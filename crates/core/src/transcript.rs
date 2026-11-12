//! What an agent has been saying: the end of its session log, for the
//! inbox's preview (`switchyard transcript`).
//!
//! `fetch` runs on the config host for a tab (host, session, window). It
//! takes what the task registry says of the tab's task (profile, harness,
//! worktree, agent session) and asks the tab's Rex server for its agent
//! pane (PANE_SCRIPT: the reporting program, its process ID and the pane's
//! directory), then reads the log on the agent's host: here, or there with
//! `switchyard transcript --local` over SSH. Either is enough: a tab with no
//! task goes by its pane, and a task whose agent has exited by the registry.
//!
//! `local` finds the log the way each harness keeps it:
//!
//! - Claude writes `CONFIG/sessions/PID.json` for each running agent, naming
//!   its current session (which changes on /clear); failing that, the
//!   task's agent session, then the newest session for the directory.
//! - Pi: the newest session for the directory, which follows /new.
//!
//! It reads only the end of the log and returns its last messages, tool
//! calls and questions as `{harness, entries: [{kind, text}]}`, where kind
//! is `user`, `assistant`, `tool` or `ask`; or `{error}`.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::rex::{host_bash, local_label, rex_json, sh_quote};
use crate::sessions::session_dir;
use crate::tasks::load_tasks;

/// How much of the end of a log to read.
const TAIL: u64 = 2 * 1024 * 1024;
/// How many entries to return, and the longest text of one (in characters).
const ENTRIES: usize = 150;
const TEXT: usize = 6000;
/// The most text a transcript carries, so its paste stays small.
const BYTES: usize = 96 * 1024;

/// A tab's agent pane, run as `rex [-S HOST] do -e SCRIPT --json --args
/// '{"session":ID,"window":ID}'`: the tiled pane with the most urgent
/// program status, as host_rows.lua picks a tab's, with its block, status,
/// program, that program's process ID and the foreground process's
/// directory; `{}` when no pane reports one.
const PANE_SCRIPT: &str = r#"
local T = "com.superlogical.terminal"
local urgency = { idle = 1, done = 2, working = 3, error = 4, blocked = 5 }
local sid, wid = rex.args.session, rex.args.window
local _, err = rex.session.attach{ session_id = sid }
if err ~= nil then error(tostring(err), 0) end
local view = rex.session.view{ session_id = sid }
for _, w in ipairs((view and view.windows) or {}) do
  if w.window_id == wid then
    local best, rank
    for _, l in ipairs(w.layers or {}) do
      if l.kind == "tiled" then
        for _, b in ipairs(l.blocks or {}) do
          local st = rex.block.call(T, "program_status", { session_id = sid, block_id = b.block_id })
          for _, r in ipairs((st and st.records) or {}) do
            local u = urgency[r.state] or 0.5
            if rank == nil or u > rank then best, rank = { block = b.block_id, record = r }, u end
          end
        end
      end
    end
    if best == nil then return {} end
    local proc = rex.block.call(T, "process", { session_id = sid, block_id = best.block }) or {}
    local fg = proc.foreground or proc.child or {}
    local r = best.record
    return { block = best.block, state = r.state or "", app = r.effective_app or r.app or "",
      pid = (r.owner and r.owner.pid) or fg.pid or 0, cwd = fg.cwd or "" }
  end
end
error("tab not found", 0)
"#;

/// The agent pane of a tab on `host` (see PANE_SCRIPT).
pub fn agent_pane(host: &str, session: &str, window: &str) -> Result<Value, String> {
    let args = json!({ "session": session, "window": window }).to_string();
    rex_json(host, &["do", "-e", PANE_SCRIPT, "--json", "--args", &args])
}

/// The transcript of the agent in a tab, as JSON (see the module docs).
/// `target` names the tab's host as the picker's title does: `-` for this
/// one, else the hex-encoded label.
pub fn fetch(target: &str, session: &str, window: &str) -> Value {
    let host = if target == "-" { local_label() } else { unhex(target) };
    fetch_inner(&host, session, window).unwrap_or_else(|e| json!({ "error": e }))
}

fn unhex(s: &str) -> String {
    let bytes: Vec<u8> = (0..s.len() / 2).filter_map(|i| u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn fetch_inner(host: &str, session: &str, window: &str) -> Result<Value, String> {
    let task = load_tasks().ok().and_then(|tasks| {
        tasks.into_iter().find(|t| {
            t["ended_at"].is_null() && t["host"].as_str() == Some(host) && t["window_id"].as_str() == Some(window)
        })
    });
    let f = |k: &str| task.as_ref().and_then(|t| t[k].as_str()).unwrap_or("").to_string();
    let pane = match (agent_pane(host, session, window), &task) {
        (Ok(p), _) => p,
        (Err(_), Some(_)) => json!({}),
        (Err(e), None) => return Err(e),
    };
    let app = pane["app"].as_str().unwrap_or("");
    let harness = match (f("harness").as_str(), app) {
        ("", "claude-code" | "claude") => "claude".to_string(),
        ("", "pi") => "pi".to_string(),
        ("", "") if task.is_some() => "claude".to_string(),
        ("", "") => return Err("no agent in this tab".into()),
        ("", other) => return Err(format!("no transcript for {other}")),
        (h, _) => h.to_string(),
    };
    let dir = match f("worktree") {
        w if !w.is_empty() => w,
        _ => pane["cwd"].as_str().unwrap_or("").to_string(),
    };
    let profile = match f("profile") {
        p if !p.is_empty() => p,
        _ if dir.contains("/dev/src/amfaro") => "work".to_string(),
        _ => "personal".to_string(),
    };
    let pid = pane["pid"].as_u64().unwrap_or(0);
    let agent_session = f("agent_session");
    if host == local_label() {
        return Ok(local(&profile, &harness, &dir, &agent_session, pid));
    }
    let script = format!(
        "switchyard transcript --local {} {} {} {} {pid}",
        sh_quote(&profile),
        sh_quote(&harness),
        sh_quote(&dir),
        sh_quote(&agent_session),
    );
    let out = host_bash(host, &script).ok_or_else(|| format!("can't reach {host}"))?;
    if !out.status.success() {
        return Err(format!("no transcript reader on {host} (install matching Switchyard there)"));
    }
    serde_json::from_slice(&out.stdout).map_err(|_| format!("unreadable transcript from {host}"))
}

/// The transcript of an agent on this host (see the module docs); `pid` is
/// 0 and `agent_session` empty when unknown.
pub fn local(profile: &str, harness: &str, dir: &str, agent_session: &str, pid: u64) -> Value {
    let Some(path) = find(profile, harness, dir, agent_session, pid) else {
        return json!({ "error": format!("no {harness} session log for {dir}") });
    };
    match read(&path, harness) {
        Ok(entries) => json!({
            "harness": harness,
            "entries": entries.into_iter().map(|(kind, text)| json!({ "kind": kind, "text": text })).collect::<Vec<_>>(),
        }),
        Err(e) => json!({ "error": format!("{}: {e}", path.display()) }),
    }
}

fn find(profile: &str, harness: &str, dir: &str, agent_session: &str, pid: u64) -> Option<PathBuf> {
    let sessions = session_dir(profile, harness, dir);
    let newest = || newest_log(&sessions);
    if harness == "pi" {
        return newest();
    }
    // CONFIG/projects/FLAT: the running agent's own record, under CONFIG.
    let config = sessions.parent()?.parent()?;
    let running = (pid > 0)
        .then(|| std::fs::read_to_string(config.join("sessions").join(format!("{pid}.json"))).ok())
        .flatten()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v["sessionId"].as_str().map(String::from));
    let named = |id: &str| -> Option<PathBuf> {
        if id.is_empty() {
            return None;
        }
        let here = sessions.join(format!("{id}.jsonl"));
        if here.is_file() {
            return Some(here);
        }
        // The agent may have started elsewhere than the directory.
        std::fs::read_dir(config.join("projects"))
            .ok()?
            .filter_map(Result::ok)
            .map(|e| e.path().join(format!("{id}.jsonl")))
            .find(|p| p.is_file())
    };
    running.as_deref().and_then(named).or_else(|| named(agent_session)).or_else(newest)
}

fn newest_log(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p)
}

/// The last entries of a log, oldest first.
fn read(path: &Path, harness: &str) -> std::io::Result<Vec<(&'static str, String)>> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(TAIL);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    // A cut mid-line: start at the next whole one.
    let text = if start > 0 { text.split_once('\n').map_or("", |(_, rest)| rest) } else { &text };
    Ok(entries(text, harness))
}

/// A log's entries (JSON lines), oldest first, the last ENTRIES of them.
pub fn entries(log: &str, harness: &str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for line in log.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if harness == "pi" {
            pi_entry(&v, &mut out);
        } else {
            claude_entry(&v, &mut out);
        }
    }
    // The last ENTRIES, and no more text than BYTES all told.
    let mut bytes = 0;
    let keep = out.iter().rev().take(ENTRIES).take_while(|(_, t)| {
        bytes += t.len();
        bytes <= BYTES
    });
    let skip = out.len() - keep.count().max(1).min(out.len());
    out.drain(..skip);
    out
}

fn claude_entry(v: &Value, out: &mut Vec<(&'static str, String)>) {
    let kind = match v["type"].as_str() {
        Some("user") if v["isMeta"] != true => "user",
        Some("assistant") => "assistant",
        _ => return,
    };
    let content = &v["message"]["content"];
    if let Some(text) = content.as_str() {
        push_text(out, kind, text);
        return;
    }
    for part in content.as_array().into_iter().flatten() {
        match part["type"].as_str() {
            Some("text") => push_text(out, kind, part["text"].as_str().unwrap_or("")),
            Some("tool_use") => push_tool(out, part["name"].as_str().unwrap_or("tool"), &part["input"]),
            _ => {}
        }
    }
}

fn pi_entry(v: &Value, out: &mut Vec<(&'static str, String)>) {
    if v["type"] != "message" {
        return;
    }
    let m = &v["message"];
    let kind = match m["role"].as_str() {
        Some("user") => "user",
        Some("assistant") => "assistant",
        _ => return,
    };
    let content = &m["content"];
    if let Some(text) = content.as_str() {
        push_text(out, kind, text);
        return;
    }
    for part in content.as_array().into_iter().flatten() {
        match part["type"].as_str() {
            Some("text") => push_text(out, kind, part["text"].as_str().unwrap_or("")),
            Some("toolCall") => push_tool(out, part["name"].as_str().unwrap_or("tool"), &part["arguments"]),
            _ => {}
        }
    }
}

/// Text John or the agent wrote; not the harness's own markup (command
/// echoes, reminders), which comes as user text in tags.
fn push_text(out: &mut Vec<(&'static str, String)>, kind: &'static str, text: &str) {
    let text = text.trim();
    if text.is_empty() || (kind == "user" && text.starts_with('<')) {
        return;
    }
    out.push((kind, clip(text, TEXT)));
}

/// A tool call in a line, or a question for John in full.
fn push_tool(out: &mut Vec<(&'static str, String)>, name: &str, input: &Value) {
    if name == "AskUserQuestion" {
        let mut text = String::new();
        for q in input["questions"].as_array().into_iter().flatten() {
            text.push_str(q["question"].as_str().unwrap_or(""));
            for o in q["options"].as_array().into_iter().flatten() {
                text.push_str(&format!("\n  - {}", o["label"].as_str().unwrap_or("")));
                if let Some(d) = o["description"].as_str().filter(|d| !d.is_empty()) {
                    text.push_str(&format!(": {d}"));
                }
            }
            text.push('\n');
        }
        out.push(("ask", clip(text.trim(), TEXT)));
        return;
    }
    let detail = ["description", "command", "file_path", "path", "pattern", "url", "query", "prompt", "skill"]
        .iter()
        .find_map(|k| input[*k].as_str().filter(|s| !s.is_empty()))
        .unwrap_or("");
    let detail = detail.lines().next().unwrap_or("");
    let text = if detail.is_empty() { name.to_string() } else { format!("{name}: {}", clip(detail, 200)) };
    out.push(("tool", text));
}

fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_logs_give_messages_tool_calls_and_questions() {
        let log = [
            json!({"type":"user","message":{"role":"user","content":"Fix the bug"}}),
            json!({"type":"user","isMeta":true,"message":{"role":"user","content":"hidden"}}),
            json!({"type":"user","message":{"role":"user","content":"<command-name>/clear</command-name>"}}),
            json!({"type":"assistant","message":{"content":[
                {"type":"thinking","thinking":"hmm"},
                {"type":"text","text":"Looking."},
                {"type":"tool_use","name":"Bash","input":{"command":"cargo test\nmore","description":"Run tests"}}]}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}),
            json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":"AskUserQuestion","input":{
                "questions":[{"question":"Which?","options":[{"label":"A","description":"first"},{"label":"B"}]}]}}]}}),
        ]
        .map(|v| v.to_string())
        .join("\n");
        assert_eq!(
            entries(&log, "claude"),
            [
                ("user", "Fix the bug".to_string()),
                ("assistant", "Looking.".to_string()),
                ("tool", "Bash: Run tests".to_string()),
                ("ask", "Which?\n  - A: first\n  - B".to_string()),
            ]
        );
    }

    #[test]
    fn pi_logs_give_messages_and_tool_calls() {
        let log = [
            json!({"type":"session","cwd":"/x"}),
            json!({"type":"message","message":{"role":"user","content":[{"type":"text","text":"Hi"}]}}),
            json!({"type":"message","message":{"role":"assistant","content":[
                {"type":"text","text":"Reading."},{"type":"toolCall","name":"read","arguments":{"path":"/a/b"}}]}}),
            json!({"type":"message","message":{"role":"toolResult","content":[{"type":"text","text":"data"}]}}),
        ]
        .map(|v| v.to_string())
        .join("\n");
        assert_eq!(
            entries(&log, "pi"),
            [("user", "Hi".to_string()), ("assistant", "Reading.".to_string()), ("tool", "read: /a/b".to_string())]
        );
    }

    #[test]
    fn a_running_claude_agent_is_found_by_its_process() {
        let root = std::env::temp_dir().join(format!("switchyard-transcript-{}", std::process::id()));
        let projects = root.join("projects");
        std::fs::create_dir_all(projects.join("elsewhere")).unwrap();
        std::fs::create_dir_all(root.join("sessions")).unwrap();
        std::fs::write(root.join("sessions/42.json"), r#"{"sessionId":"live"}"#).unwrap();
        std::fs::write(projects.join("elsewhere/live.jsonl"), "").unwrap();
        // session_dir reads CLAUDE_CONFIG_DIR for the personal profile.
        let sessions = root.join("projects/-w");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join("old.jsonl"), "").unwrap();
        let found = with_claude_config(&root, || find("personal", "claude", "/w", "old", 42));
        assert_eq!(found, Some(projects.join("elsewhere/live.jsonl")));
        let found = with_claude_config(&root, || find("personal", "claude", "/w", "old", 7));
        assert_eq!(found, Some(sessions.join("old.jsonl")));
        std::fs::remove_dir_all(root).unwrap();
    }

    fn with_claude_config<T>(root: &Path, f: impl FnOnce() -> T) -> T {
        let before = std::env::var_os("CLAUDE_CONFIG_DIR");
        // SAFETY: only this test sets it.
        unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", root) };
        let out = f();
        match before {
            Some(v) => unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", v) },
            None => unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") },
        }
        out
    }
}
