//! The slash commands an agent offers: the prompt boxes complete them after
//! `/`. Only John's own are listed (his commands, skills, prompt templates,
//! plugins, packages and extensions), not the harness's built-ins.
//!
//! Rather than reading command files, which would miss plugins, packages
//! and extension commands, `fetch` asks the harness itself, on the agent's
//! host and in its folder, with the profile's environment:
//!
//! - Claude: an Agent SDK `initialize` request over stream-json, whose
//!   response lists every command with a `builtin` flag. No model turn
//!   starts and, with `--no-session-persistence`, no session is saved.
//! - Pi: the RPC `get_commands` call, which lists extension commands,
//!   prompt templates and skills (as `skill:NAME`) but not the TUI's own.
//!
//! The probe is a Bash script run through `host_bash`, so it needs no
//! Switchyard of the same version on the agent's host.

use serde_json::Value;

use crate::rex::{host_bash, sh_quote};

/// A command as the prompt boxes show it: its name without the `/`, what it
/// does, and the arguments it takes (Claude's `argument-hint`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: String,
    pub description: String,
    pub hint: String,
}

/// What a harness's commands take in a message: Claude runs a `/command` or
/// names a skill anywhere, Pi only at the start.
pub fn anywhere(harness: &str) -> bool {
    harness != "pi"
}

/// The probe's request id, which picks its response out of the output.
const REQUEST: &str = "switchyard-commands";

/// The commands `harness` offers in `dir` on `host` under `profile`.
pub fn fetch(
    host: &str,
    profile: &str,
    harness: &str,
    dir: &str,
) -> Result<Vec<SlashCommand>, String> {
    let out = host_bash(host, &script(profile, harness, dir))
        .ok_or_else(|| format!("can't reach {host}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text
        .lines()
        .find(|l| l.contains(REQUEST) || l.contains("\"get_commands\""));
    let v: Value = line
        .and_then(|l| serde_json::from_str(l).ok())
        .ok_or_else(|| format!("{harness} on {host} listed no commands"))?;
    parse(harness, &v)
}

/// The probe for `fetch`: in the agent's folder (else home), with the work
/// profile's mise environment (as `task run` gives a work agent; personal
/// agents' wrappers default their own config folders), ask the harness and
/// print its response line.
fn script(profile: &str, harness: &str, dir: &str) -> String {
    let probe = match harness {
        "pi" => format!(
            "printf '%s\\n' '{{\"type\":\"get_commands\",\"id\":\"{REQUEST}\"}}' \
             | $t pi --mode rpc --no-session 2>/dev/null"
        ),
        _ => format!(
            "printf '%s\\n' '{{\"type\":\"control_request\",\"request_id\":\"{REQUEST}\",\"request\":{{\"subtype\":\"initialize\"}}}}' \
             | $t claude -p --input-format stream-json --output-format stream-json --verbose --no-session-persistence 2>/dev/null"
        ),
    };
    let work = if profile == "work" {
        "[ -d ~/dev/src/amfaro ] && eval \"$(mise -C ~/dev/src/amfaro env -s bash 2>/dev/null)\"\n"
    } else {
        ""
    };
    // Catalog paths start `~/`, which quoting would keep from expanding.
    let dir = match dir.strip_prefix("~/") {
        Some(rest) => format!("~/{}", sh_quote(rest)),
        None => sh_quote(dir),
    };
    format!(
        "cd {dir} 2>/dev/null || cd ~ || exit 1\n{work}t=\ncommand -v timeout >/dev/null && t='timeout 20'\n{probe}\n"
    )
}

/// The commands in a probe's response, John's own only, each name once, by
/// name (Pi's skills by the name after `skill:`).
fn parse(harness: &str, v: &Value) -> Result<Vec<SlashCommand>, String> {
    let list = if harness == "pi" {
        if v["success"] == Value::Bool(false) {
            return Err(format!(
                "pi: {}",
                v["error"].as_str().unwrap_or("get_commands failed")
            ));
        }
        &v["data"]["commands"]
    } else {
        if v["response"]["subtype"] == "error" {
            return Err(format!(
                "claude: {}",
                v["response"]["error"]
                    .as_str()
                    .unwrap_or("initialize failed")
            ));
        }
        &v["response"]["response"]["commands"]
    };
    let s = |c: &Value, k: &str| c[k].as_str().unwrap_or("").trim().to_string();
    let mut out: Vec<SlashCommand> = Vec::new();
    for c in list.as_array().into_iter().flatten() {
        let name = s(c, "name");
        if name.is_empty()
            || c["builtin"] == Value::Bool(true)
            || out.iter().any(|o| o.name == name)
        {
            continue;
        }
        out.push(SlashCommand {
            name,
            description: s(c, "description"),
            hint: s(c, "argumentHint"),
        });
    }
    out.sort_by(|a, b| bare(&a.name).cmp(bare(&b.name)).then(a.name.cmp(&b.name)));
    Ok(out)
}

/// A command's name without Pi's `skill:` prefix.
pub fn bare(name: &str) -> &str {
    name.strip_prefix("skill:").unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_lists_only_johns_commands() {
        let v = json!({ "type": "control_response", "response": { "subtype": "success", "request_id": REQUEST, "response": {
            "commands": [
                { "name": "wrap", "description": "Commit and clean up", "argumentHint": "" },
                { "name": "clear", "description": "Start a new session", "builtin": true },
                { "name": "fizzy", "description": "Fizzy CLI", "argumentHint": "[action]" },
                { "name": "wrap", "description": "again" },
            ]
        }}});
        let names: Vec<String> = parse("claude", &v)
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["fizzy", "wrap"]);
        assert_eq!(parse("claude", &v).unwrap()[0].hint, "[action]");
    }

    #[test]
    fn pi_lists_everything_sorted_by_bare_name() {
        let v = json!({ "type": "response", "command": "get_commands", "success": true, "data": { "commands": [
            { "name": "issue-create", "description": "Create an issue", "source": "prompt" },
            { "name": "skill:fizzy", "description": "Fizzy CLI", "source": "skill" },
            { "name": "implement", "source": "extension" },
            { "name": "implement", "description": "dup", "source": "prompt" },
        ]}});
        let names: Vec<String> = parse("pi", &v)
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["skill:fizzy", "implement", "issue-create"]);
    }

    #[test]
    fn failures_are_errors() {
        assert!(parse("pi", &json!({ "success": false, "error": "nope" })).is_err());
        assert!(
            parse(
                "claude",
                &json!({ "response": { "subtype": "error", "error": "nope" } })
            )
            .is_err()
        );
    }

    #[test]
    fn the_probe_quotes_the_folder_and_takes_the_work_env() {
        let s = script("work", "pi", "/w/it's");
        assert!(s.starts_with("cd '/w/it'\\''s'"), "{s}");
        assert!(s.contains("mise -C ~/dev/src/amfaro env -s bash"));
        assert!(s.contains("pi --mode rpc --no-session"));
        assert!(!script("personal", "claude", "/w").contains("mise"));
        assert!(script("personal", "claude", "~/dev/src/x").starts_with("cd ~/'dev/src/x'"));
    }
}
