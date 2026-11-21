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
//! - Antigravity (`agy`): an Agent Client Protocol `session/new`, after
//!   which `agy acp` announces the session's commands. No model turn starts,
//!   but the session is saved, so the probe deletes it again. The list has
//!   no built-in flag: the probe also prints the names of the skills
//!   Antigravity ships (its `builtin` folder), and `AGY_BUILTINS` names its
//!   own commands.
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
/// names a skill anywhere, Pi and Antigravity only at the start.
#[must_use]
pub fn anywhere(harness: &str) -> bool {
    !matches!(harness, "pi" | "agy")
}

/// Antigravity's own commands, which it lists among John's: they have no
/// file to find them by, unlike the skills it ships.
const AGY_BUILTINS: [&str; 7] = [
    "browser", "compact", "goal", "grill-me", "learn", "plan", "schedule",
];

/// The line prefix the Antigravity probe names a shipped skill with.
const AGY_SHIPPED: &str = "switchyard-builtin ";

/// The Antigravity probe (see the module doc). The FIFO, opened read-write
/// so opening it never blocks, holds `agy acp`'s input open until the
/// commands arrive (at most 15 s); closing it ends `agy`. The session it
/// made, and that alone, is then removed: its conversation, artifact
/// folder, presence lock and index rows. Without sqlite3 to drop the index
/// rows, everything stays, so the index never names a missing conversation.
const AGY_PROBE: &str = r#"command -v agy >/dev/null || exit 1
c=~/.gemini/antigravity-cli
d=$(mktemp -d) || exit 1
mkfifo "$d/in" || exit 1
exec 3<>"$d/in"
$t agy acp <"$d/in" >"$d/out" 2>/dev/null 3>&- &
p=$!
w=$(printf '%s' "$PWD" | sed 's/[\\"]/\\&/g')
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}' \
  '{"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"'"$w"'","mcpServers":[]}}' >&3
i=0
while [ $i -lt 150 ] && ! grep -q available_commands_update "$d/out"; do sleep 0.1; i=$((i+1)); done
exec 3>&-
wait $p
grep available_commands_update "$d/out"
id=$(grep -o '"sessionId":"[0-9a-f-]*"' "$d/out" | head -1 | cut -d'"' -f4)
rm -rf "$d"
for f in "$c"/builtin/skills/*/SKILL.md "$c"/builtin/plugins/*/skills/*/SKILL.md; do
  [ -f "$f" ] && sed -n 's/^name:[[:space:]]*//p' "$f" | head -1 | tr -d "\"'" | sed 's/^/switchyard-builtin /'
done
if [ -n "$id" ] && command -v sqlite3 >/dev/null &&
  sqlite3 "$c/conversation_summaries.db" "delete from conversation_summaries where conversation_id='$id'; delete from conversations_fts where conversation_id='$id';" 2>/dev/null; then
  rm -rf "$c/conversations/$id.db" "$c/brain/$id" "$c/presence/$id.lock"
fi
"#;

/// The probe's request id, which picks its response out of the output.
const REQUEST: &str = "switchyard-commands";

/// The commands `harness` offers in `dir` on `host` under `profile`.
///
/// # Errors
///
/// Fails when `host` can't be reached or the harness's output lists no
/// commands.
pub fn fetch(
    host: &str,
    profile: &str,
    harness: &str,
    dir: &str,
) -> Result<Vec<SlashCommand>, String> {
    let out = host_bash(host, &script(profile, harness, dir))
        .ok_or_else(|| format!("can't reach {host}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    if harness == "agy" {
        return parse_agy(&text).ok_or_else(|| format!("agy on {host} listed no commands"));
    }
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
        "agy" => AGY_PROBE.to_string(),
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

/// John's own commands in the Antigravity probe's output (see `AGY_PROBE`):
/// the session's commands less Antigravity's own and the skills it ships,
/// each name once, by name. None when the output has no command list.
fn parse_agy(text: &str) -> Option<Vec<SlashCommand>> {
    let shipped: Vec<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix(AGY_SHIPPED))
        .map(str::trim)
        .collect();
    let v: Value = text
        .lines()
        .filter(|l| l.contains("available_commands_update"))
        .find_map(|l| serde_json::from_str(l).ok())?;
    let list = v["params"]["update"]["availableCommands"].as_array()?;
    let s = |c: &Value, k: &str| c[k].as_str().unwrap_or("").trim().to_string();
    let mut out: Vec<SlashCommand> = Vec::new();
    for c in list {
        let name = s(c, "name");
        if name.is_empty()
            || AGY_BUILTINS.contains(&name.as_str())
            || shipped.contains(&name.as_str())
            || out.iter().any(|o| o.name == name)
        {
            continue;
        }
        out.push(SlashCommand {
            name,
            description: s(c, "description"),
            hint: c["input"]["hint"].as_str().unwrap_or("").trim().to_string(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Some(out)
}

/// A command's name without Pi's `skill:` prefix.
#[must_use]
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
    fn agy_lists_only_johns_commands() {
        // Trimmed from a real `agy acp` 1.5.12 run, with a workspace skill.
        let out = concat!(
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"9eee5a90-4754-48d4-a121-ca1c849c6c7e","update":{"sessionUpdate":"available_commands_update","availableCommands":["#,
            r#"{"name":"goal","description":"Run until the specified goal is completely finished."},"#,
            r#"{"name":"compact","description":"Compact the conversation now."},"#,
            r#"{"name":"grill-me","description":"Interview me to align on a plan."},"#,
            r#"{"name":"antigravity-guide","description":"Provides a comprehensive guide."},"#,
            r#"{"name":"sw-probe","description":"Probe skill for switchyard"},"#,
            r#"{"name":"fizzy","description":"Fizzy CLI","input":{"hint":"[action]"}},"#,
            r#"{"name":"swarm","description":"(Preview & Quota Heavy) Run a swarm."}"#,
            "]}}}\n",
            "switchyard-builtin antigravity-guide\n",
            "switchyard-builtin swarm\n",
        );
        let got = parse_agy(out).unwrap();
        let names: Vec<&str> = got.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["fizzy", "sw-probe"]);
        assert_eq!(got[0].hint, "[action]");
        assert_eq!(got[1].description, "Probe skill for switchyard");
        assert!(parse_agy("switchyard-builtin swarm\n").is_none());
        assert!(!anywhere("agy"));
    }

    #[test]
    fn the_agy_probe_runs_acp_and_removes_its_session() {
        let s = script("work", "agy", "/w");
        assert!(s.starts_with("cd '/w'"), "{s}");
        assert!(s.contains("mise -C ~/dev/src/amfaro env -s bash"));
        assert!(s.contains("$t agy acp"));
        assert!(s.contains("\"method\":\"session/new\""));
        assert!(s.contains("rm -rf \"$c/conversations/$id.db\""));
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
