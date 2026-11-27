//! What an agent has been saying: the end of its session log, for the
//! tab picker's preview.
//!
//! `fetch` runs on the config host for a tab (host, session, window). It
//! takes what the task registry says of the tab's task (profile, harness,
//! worktree, agent session) and asks the tab's Rex server for its agent
//! pane (`PANE_SCRIPT`: the reporting program, its process ID and the pane's
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
//! - Antigravity (`agy`): on Linux, the conversation open in the running
//!   agent's `/proc/<pid>/fd` (`presence/<id>.lock` primary,
//!   `conversations/<id>.db` secondary); when `/proc/<pid>/fd` is unavailable
//!   (non-Linux or exited process), `agent_session` then the conversation it
//!   last used in the directory. Its log is
//!   `brain/ID/.system_generated/logs/transcript_full.jsonl`.
//!
//! It reads only the end of the log and returns its last messages, tool
//! calls and questions as `{harness, entries: [{kind, text}], tokens, model}`,
//! where kind is `user`, `assistant`, `tool` or `ask` and tokens is the
//! context the agent's last turn used and model the model it ran on (each
//! absent when the log doesn't say), `log`, the log's path on the agent's
//! host, and `wake: {at, reason}` when a Claude
//! agent is waiting on a timer or background job to resume it, `at` 0 when
//! its time isn't known (see `wake`), and `title` when a Claude session
//! has one of its own (see `title`); or `{error}`. `fetch` adds
//! `agent: {host, profile, harness, dir}` once it knows them, and
//! `suggestion` when an idle Claude agent's screen shows the prompt it
//! suggests next (see `suggestion`).

use std::fmt::Write as _;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// One question from an interactive agent prompt (`agy`'s `ask_question`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptQuestion {
    pub question: String,
    pub options: Vec<String>,
    #[serde(default)]
    pub multi_select: bool,
}

use crate::rex::{host_bash, local_label, rex_json, sh_quote};
use crate::sessions::{agy_conversation_for_pid, newest_session_here, session_dir};
use crate::tasks::{in_window, load_tasks, rfc3339_secs};

/// How much of the end of a log to read.
const TAIL: u64 = 2 * 1024 * 1024;
/// How many entries to return, and the longest text of one (in characters).
const ENTRIES: usize = 150;
const TEXT: usize = 64 * 1024;
/// The most text a transcript carries all told.
const BYTES: usize = 256 * 1024;

/// A tab's agent pane, run as `rex [-S HOST] do -e SCRIPT --json --args
/// '{"session":ID,"window":ID}'`: the tiled pane with the most urgent
/// program status, as `host_rows.lua` picks a tab's, with its block, status,
/// status kind, program, that program's process ID and the foreground
/// process's directory; `{}` when no pane reports one. With `"screen":true` in
/// the args, an idle or done pane also gives the end of its screen as escape
/// sequences (`screen`), for `suggestion`, and a pane blocked on a `question`
/// gives the end of its plain screen (`screen`), for answering `agy`'s
/// `ask_question` prompt.
const PANE_SCRIPT: &str = r#"
local T = "com.superlogical.terminal"
local urgency = { idle = 1, done = 2, working = 3, error = 4, blocked = 5 }
local sid, wid = rex.args.session, rex.args.window
local SCREEN_TAIL = 16384
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
    local screen
    local fmt = nil
    if rex.args.screen then
      if r.state == "idle" or r.state == "done" then
        fmt = "vt"
      elseif r.state == "blocked" and r.kind == "question" then
        fmt = "plain"
      end
    end
    if fmt ~= nil then
      -- The end of the screen: the total length first, then only its last bytes.
      local head = rex.block.call(T, "format", { session_id = sid, block_id = best.block,
        format = fmt, unwrap = true, max_bytes = 1 })
      if head and head.id and head.total then
        local tail = rex.block.call(T, "blob_read", { session_id = sid, block_id = best.block,
          id = head.id, offset = math.max(0, head.total - SCREEN_TAIL) })
        screen = tail and tail.data
      elseif head then
        screen = head.content
      end
    end
    return { block = best.block, state = r.state or "", kind = r.kind or "",
      app = r.effective_app or r.app or "",
      pid = (r.owner and r.owner.pid) or fg.pid or 0, cwd = fg.cwd or "", screen = screen }
  end
end
error("tab not found", 0)
"#;

/// The agent pane of a tab on `host` (see `PANE_SCRIPT`).
///
/// # Errors
///
/// Fails when Rex can't run the lookup or the tab isn't found.
pub fn agent_pane(host: &str, session: &str, window: &str) -> Result<Value, String> {
    pane_lookup(host, session, window, false)
}

/// The agent pane of a tab on `host`, including the tail of its screen when
/// idle/done (`vt`) or blocked on a question (`plain`).
///
/// # Errors
///
/// Fails when Rex can't run the lookup or the tab isn't found.
pub fn pane_with_screen(host: &str, session: &str, window: &str) -> Result<Value, String> {
    pane_lookup(host, session, window, true)
}

fn pane_lookup(host: &str, session: &str, window: &str, screen: bool) -> Result<Value, String> {
    let args = json!({ "session": session, "window": window, "screen": screen }).to_string();
    rex_json(host, &["do", "-e", PANE_SCRIPT, "--json", "--args", &args])
}

/// The prompt Claude Code suggests next, from the end of an idle agent's
/// screen as escape sequences: the text after the input box's `❯` when all
/// of it is dim (Claude's ghost text). Claude keeps the suggestion out of
/// its log, so the screen is the only place to read it. Text typed in the
/// box isn't dim, so there is no suggestion then.
#[must_use]
pub fn suggestion(screen: &str) -> Option<String> {
    let lines: Vec<&str> = screen.lines().collect();
    let line = lines
        .iter()
        .rev()
        .take(15)
        .find_map(|l| l.split_once("\u{276f}\u{a0}").map(|(_, rest)| rest))?;
    let mut text = String::new();
    let mut dim = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                Some('[') => {
                    let mut params = String::new();
                    let fin = loop {
                        match chars.next() {
                            Some(c) if c.is_ascii_digit() || c == ';' || c == ':' || c == '?' => {
                                params.push(c);
                            }
                            other => break other,
                        }
                    };
                    match fin {
                        Some('m') => dim = sgr_dim(&params, dim),
                        Some('C') => {
                            let n = params.parse::<usize>().unwrap_or(1).max(1);
                            text.extend(std::iter::repeat_n(' ', n));
                        }
                        _ => {}
                    }
                }
                // OSC (titles, links): up to BEL or ST.
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' => {}
            c if c.is_whitespace() => text.push(' '),
            _ if !dim => return None,
            c => text.push(c),
        }
    }
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then_some(text)
}

/// Whether text is dim after the SGR sequence `params` (`ESC [ params m`),
/// from `dim` before it.
fn sgr_dim(params: &str, mut dim: bool) -> bool {
    let mut it = params.split(';');
    while let Some(p) = it.next() {
        match p {
            "" | "0" | "22" => dim = false,
            "2" => dim = true,
            // A colour's own numbers (`38;5;N`, `38;2;R;G;B`) aren't styles.
            "38" | "48" | "58" => match it.next() {
                Some("5") => {
                    it.next();
                }
                Some("2") => {
                    it.nth(2);
                }
                _ => {}
            },
            _ => {}
        }
    }
    dim
}

/// The transcript of the agent in a tab, as JSON (see the module docs).
/// `target` names the tab's host as the picker's title does: `-` for this
/// one, else the hex-encoded label.
#[must_use]
pub fn fetch(target: &str, session: &str, window: &str) -> Value {
    let host = if target == "-" {
        local_label()
    } else {
        unhex(target)
    };
    fetch_on(&host, session, window)
}

/// `fetch` for a tab on the host with Rex label `host`.
#[must_use]
pub fn fetch_on(host: &str, session: &str, window: &str) -> Value {
    if crate::server::remote_url().is_some() {
        return crate::server::remote_transcript(host, session, window)
            .unwrap_or_else(|e| json!({ "error": e }));
    }
    fetch_inner(host, session, window).unwrap_or_else(|e| json!({ "error": e }))
}

pub(crate) fn unhex(s: &str) -> String {
    let bytes: Vec<u8> = (0..s.len() / 2)
        .filter_map(|i| u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok())
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn fetch_inner(host: &str, session: &str, window: &str) -> Result<Value, String> {
    let task = load_tasks().ok().and_then(|tasks| {
        tasks
            .into_iter()
            .find(|t| t["ended_at"].is_null() && in_window(t, window))
    });
    let f = |k: &str| {
        task.as_ref()
            .and_then(|t| t[k].as_str())
            .unwrap_or("")
            .to_string()
    };
    let pane = match (pane_lookup(host, session, window, true), &task) {
        (Ok(p), _) => p,
        (Err(_), Some(_)) => json!({}),
        (Err(e), None) => return Err(e),
    };
    let app = pane["app"].as_str().unwrap_or("");
    let harness = match (f("harness").as_str(), app) {
        ("", "claude-code" | "claude") => "claude".to_string(),
        ("", "pi") => "pi".to_string(),
        // Rex names Antigravity's CLI `antigravity`; tasks record `agy`.
        ("", "antigravity" | "agy") => "agy".to_string(),
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
    // Where the agent runs, for the reply box's slash commands.
    let agent = json!({ "host": host, "profile": profile, "harness": harness, "dir": dir });
    let mut v = if host == local_label() {
        local(&profile, &harness, &dir, &agent_session, pid)
    } else {
        remote(host, &profile, &harness, &dir, &agent_session, pid)
            .unwrap_or_else(|e| json!({ "error": e }))
    };
    if let Some(o) = v.as_object_mut() {
        o.insert("agent".into(), agent);
        let last_question = o.remove("last_question");
        if harness == "claude"
            && let Some(s) = pane["screen"].as_str().and_then(suggestion)
        {
            o.insert("suggestion".into(), s.into());
        }
        if harness == "agy"
            && pane["state"].as_str() == Some("blocked")
            && pane["kind"].as_str() == Some("question")
            && let Some(q) = last_question
        {
            o.insert("question".into(), q);
        }
    }
    Ok(v)
}

/// `local` on `host`, over SSH.
fn remote(
    host: &str,
    profile: &str,
    harness: &str,
    dir: &str,
    agent_session: &str,
    pid: u64,
) -> Result<Value, String> {
    let script = format!(
        "switchyard transcript --local {} {} {} {} {pid}",
        sh_quote(profile),
        sh_quote(harness),
        sh_quote(dir),
        sh_quote(agent_session),
    );
    let out = host_bash(host, &script).ok_or_else(|| format!("can't reach {host}"))?;
    if !out.status.success() {
        return Err(format!(
            "no transcript reader on {host} (install matching Switchyard there)"
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|_| format!("unreadable transcript from {host}"))
}

/// The transcript of an agent on this host (see the module docs); `pid` is
/// 0 and `agent_session` empty when unknown.
#[must_use]
pub fn local(profile: &str, harness: &str, dir: &str, agent_session: &str, pid: u64) -> Value {
    let Some(path) = find(profile, harness, dir, agent_session, pid) else {
        return json!({ "error": format!("no {harness} session log for {dir}") });
    };
    match read(&path, harness, started(profile, harness, dir, pid)) {
        Ok(tail) => {
            let mut v = json!({
                "harness": harness,
                "log": path.display().to_string(),
                "entries": tail.entries.into_iter().map(|(kind, text)| json!({ "kind": kind, "text": text })).collect::<Vec<_>>(),
            });
            if let Some(t) = tail.tokens {
                v["tokens"] = json!(t);
            }
            if let Some(m) = tail.model {
                v["model"] = json!(m);
            }
            if let Some((at, reason)) = tail.wake {
                v["wake"] = json!({ "at": at, "reason": reason });
            }
            if let Some(t) = tail.title {
                v["title"] = json!(t);
            }
            if let Some(q) = tail.question {
                v["last_question"] = json!(q);
            }
            v
        }
        Err(e) => json!({
            "error": format!("{}: {e}", path.display()),
            "log": path.display().to_string(),
        }),
    }
}

/// When the running Claude agent `pid` started (Unix seconds), from its
/// `CONFIG/sessions/PID.json`; 0 when unknown.
fn started(profile: &str, harness: &str, dir: &str, pid: u64) -> u64 {
    if harness != "claude" || pid == 0 {
        return 0;
    }
    let sessions = session_dir(profile, harness, dir);
    let Some(config) = sessions.parent().and_then(Path::parent) else {
        return 0;
    };
    std::fs::read_to_string(config.join("sessions").join(format!("{pid}.json")))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v["startedAt"].as_u64())
        .map_or(0, |ms| ms / 1000)
}

fn find(profile: &str, harness: &str, dir: &str, agent_session: &str, pid: u64) -> Option<PathBuf> {
    if harness == "agy" {
        return agy_log(profile, dir, agent_session, pid);
    }
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
    running
        .as_deref()
        .and_then(named)
        .or_else(|| named(agent_session))
        .or_else(newest)
}

/// Antigravity's log for a tab: on Linux, from the live process's
/// `/proc/<pid>/fd` (`presence/<id>.lock` primary, `conversations/<id>.db`
/// secondary) so concurrent `agy` tabs in the same directory never collide and
/// a newly started or `/clear`ing process doesn't flash stale history from
/// `last_conversations.json`. Falls back to `agent_session` then
/// `agy_last_conversation(dir)` only when `/proc/<pid>/fd` is unavailable.
fn agy_log(profile: &str, dir: &str, agent_session: &str, pid: u64) -> Option<PathBuf> {
    let root = crate::rex::home().join(".gemini/antigravity-cli");
    agy_log_with(
        &root,
        agy_conversation_for_pid(pid),
        || newest_session_here(profile, "agy", dir),
        agent_session,
    )
}

fn agy_log_with(
    agy_root: &Path,
    fd_lookup: std::io::Result<Option<String>>,
    last_conv: impl FnOnce() -> Option<String>,
    agent_session: &str,
) -> Option<PathBuf> {
    let log_for = |id: &str| -> Option<PathBuf> {
        if id.is_empty() || id.contains('/') {
            return None;
        }
        let logs = agy_root
            .join("brain")
            .join(id)
            .join(".system_generated/logs");
        if !logs.is_dir() {
            return None;
        }
        ["transcript_full.jsonl", "transcript.jsonl"]
            .into_iter()
            .map(|name| logs.join(name))
            .find(|p| p.is_file())
    };
    match fd_lookup {
        // Live process with an open presence lock or conversation db: read ONLY
        // that conversation's log.
        Ok(Some(id)) => log_for(&id),
        // Live process readable via /proc/<pid>/fd, but no lock/db opened yet
        // (just started or mid-/clear): do NOT fall back to stale history.
        Ok(None) => None,
        // /proc/<pid>/fd unavailable (non-Linux, pid == 0, or exited process):
        // fall back to the recorded agent_session, then last_conversations.json.
        Err(_) => (!agent_session.is_empty())
            .then(|| agent_session.to_string())
            .into_iter()
            .chain(last_conv())
            .find_map(|id| log_for(&id)),
    }
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

/// The last entries of a log, oldest first, the context its last turn used,
/// the model it ran on, what it waits on to resume (see `wake`), the
/// session's title (see `title`), and any unanswered `agy` `ask_question`.
struct Tail {
    entries: Vec<(&'static str, String)>,
    tokens: Option<u64>,
    model: Option<String>,
    wake: Option<(u64, String)>,
    title: Option<String>,
    question: Option<Vec<PromptQuestion>>,
}

fn read(path: &Path, harness: &str, since: u64) -> std::io::Result<Tail> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(TAIL);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    // A cut mid-line: start at the next whole one.
    let text = if start > 0 {
        text.split_once('\n').map_or("", |(_, rest)| rest)
    } else {
        &text
    };
    Ok(Tail {
        entries: entries(text, harness),
        tokens: tokens(text, harness),
        model: model(text, harness),
        wake: wake(text, harness, since),
        title: title(text, harness),
        question: (harness == "agy")
            .then(|| last_agy_question(text))
            .flatten(),
    })
}

/// The most recent unanswered `ask_question` call in an Antigravity log, if
/// the agent's latest planner response asked one and no later user turn or
/// planner response has followed it.
#[must_use]
pub fn last_agy_question(log: &str) -> Option<Vec<PromptQuestion>> {
    let mut pending: Option<Vec<PromptQuestion>> = None;
    for line in log.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match v["type"].as_str() {
            Some("USER_INPUT") => pending = None,
            Some("PLANNER_RESPONSE") => {
                let asked = v["tool_calls"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .rev()
                    .find(|c| c["name"] == "ask_question")
                    .and_then(|c| parse_agy_questions(&c["args"]));
                pending = asked;
            }
            _ => {}
        }
    }
    pending
}

fn parse_agy_questions(args: &Value) -> Option<Vec<PromptQuestion>> {
    let items = args["questions"].as_array()?;
    let qs: Vec<PromptQuestion> = items
        .iter()
        .filter_map(|q| {
            let question = q["question"].as_str()?.trim().to_string();
            if question.is_empty() {
                return None;
            }
            let options: Vec<String> = q["options"]
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if options.is_empty() {
                return None;
            }
            let multi_select = q["is_multi_select"].as_bool().unwrap_or(false);
            Some(PromptQuestion {
                question,
                options,
                multi_select,
            })
        })
        .collect();
    (!qs.is_empty()).then_some(qs)
}

/// A Claude session's title: the last one given with `/rename`, else the
/// last Claude made up for it. None for Pi and Antigravity, whose logs
/// have none, or before Claude has titled the session.
#[must_use]
pub fn title(log: &str, harness: &str) -> Option<String> {
    if harness != "claude" {
        return None;
    }
    let mut made_up = None;
    for line in log.lines().rev() {
        // Titles are short lines; skip parsing the rest.
        if !line.contains("-title\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let text = |k: &str| {
            v[k].as_str()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(String::from)
        };
        match v["type"].as_str() {
            Some("custom-title") => {
                if let Some(t) = text("customTitle") {
                    return Some(t);
                }
            }
            Some("ai-title") if made_up.is_none() => made_up = text("aiTitle"),
            _ => {}
        }
    }
    made_up
}

/// The model the agent's last turn ran on, or Pi's last switch to one since.
/// None when the log doesn't say.
#[must_use]
pub fn model(log: &str, harness: &str) -> Option<String> {
    log.lines().rev().find_map(|line| {
        let v = serde_json::from_str::<Value>(line).ok()?;
        let m = if harness == "agy" {
            if v["type"] != "PLANNER_RESPONSE" {
                return None;
            }
            &v["model"]
        } else if harness == "pi" {
            match v["type"].as_str()? {
                "model_change" => &v["modelId"],
                "message" if v["message"]["role"] == "assistant" => &v["message"]["model"],
                _ => return None,
            }
        } else {
            // Sidechains are subagents, which may run on other models.
            if v["type"] != "assistant" || v["isSidechain"] == true {
                return None;
            }
            &v["message"]["model"]
        };
        // Claude marks the turns it writes itself, such as API errors.
        m.as_str()
            .filter(|m| !m.is_empty() && *m != "<synthetic>")
            .map(String::from)
    })
}

/// The context the last turn in a log used: its input, cached and written
/// cache tokens and its output, as the agent's own context meter counts.
/// None when no turn in the log reports usage.
#[must_use]
pub fn tokens(log: &str, harness: &str) -> Option<u64> {
    log.lines().rev().find_map(|line| {
        let v = serde_json::from_str::<Value>(line).ok()?;
        let n = |u: &Value, k: &str| u[k].as_u64().unwrap_or(0);
        if harness == "agy" {
            // Each response counts its own input apart from cached input.
            if v["type"] != "PLANNER_RESPONSE" {
                return None;
            }
            let sum = n(&v, "input_tokens") + n(&v, "cache_read_tokens") + n(&v, "output_tokens");
            (sum > 0).then_some(sum)
        } else if harness == "pi" {
            let m = &v["message"];
            if v["type"] != "message" || m["role"] != "assistant" {
                return None;
            }
            let u = m["usage"].as_object().map(|_| &m["usage"])?;
            let sum = n(u, "input") + n(u, "output") + n(u, "cacheRead") + n(u, "cacheWrite");
            (sum > 0).then_some(sum)
        } else {
            // Sidechains are subagents, with contexts of their own.
            if v["type"] != "assistant" || v["isSidechain"] == true {
                return None;
            }
            let u = v["message"]["usage"]
                .as_object()
                .map(|_| &v["message"]["usage"])?;
            let sum = n(u, "input_tokens")
                + n(u, "output_tokens")
                + n(u, "cache_read_input_tokens")
                + n(u, "cache_creation_input_tokens");
            (sum > 0).then_some(sum)
        }
    })
}

/// The tool a Claude agent calls to resume itself later (`/loop`).
const WAKEUP_TOOL: &str = "ScheduleWakeup";

/// What a Claude agent that has ended its turn is waiting on to resume it:
/// when that happens if known (Unix seconds, 0 when not) and why.
///
/// - A timer: its last `ScheduleWakeup` call, when nothing has prompted it
///   since. The call's result says when Claude actually scheduled it
///   ("Next wakeup scheduled for 14:15:00 (in 68s)"), which may be later
///   than asked; failing that, the call's time plus its delay. A prompt
///   (John's, or the timer's own when it fires) means it has gone off.
/// - A background job (a Bash command or an agent run with
///   `run_in_background`) with no `<task-notification>` for it yet: Claude
///   resumes the agent when one arrives. Its time is known when the command
///   is a `sleep`, the usual way to wait a while. Many at once give the one
///   due first, else the first started.
///
/// Calls before `since` (the agent process's start; 0 when unknown) are
/// ignored: a resumed session's old timers and jobs died with its process.
/// None for Pi and Antigravity, whose logs say neither.
pub fn wake(log: &str, harness: &str, since: u64) -> Option<(u64, String)> {
    if harness != "claude" {
        return None;
    }
    let mut pending = Pending::default();
    for line in log.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v["isSidechain"] == true || v["isMeta"] == true {
            continue;
        }
        let at = v["timestamp"].as_str().and_then(rfc3339_secs).unwrap_or(0);
        let content = &v["message"]["content"];
        match v["type"].as_str() {
            // A notification is queued as soon as its job ends.
            Some("queue-operation") => pending.notified(v["content"].as_str().unwrap_or("")),
            Some("assistant") if at >= since => {
                for call in content
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|p| p["type"] == "tool_use")
                {
                    pending.call(call, at);
                }
            }
            Some("user") => match content {
                Value::String(s) => pending.text(s),
                Value::Array(parts) => parts.iter().for_each(|part| pending.part(part, at)),
                _ => {}
            },
            _ => {}
        }
    }
    pending.next()
}

/// What `wake` has seen pending so far, reading a log forward.
#[derive(Default)]
struct Pending {
    /// The timer's call id, time, input and scheduled time (from its result).
    timer: Option<(String, u64, Value, Option<u64>)>,
    /// Background jobs' call ids, when they're due (0 when unknown) and why.
    jobs: Vec<(String, u64, String)>,
}

impl Pending {
    /// A tool call at `at`: a timer, a background job, or neither.
    fn call(&mut self, call: &Value, at: u64) {
        let id = call["id"].as_str().unwrap_or("").to_string();
        let input = &call["input"];
        if call["name"] == WAKEUP_TOOL {
            self.timer = Some((id, at, input.clone(), None));
        } else if input["run_in_background"] == true {
            let command = input["command"].as_str().unwrap_or("");
            let when = sleep_secs(command).map_or(0, |s| at + s);
            let reason = input["description"]
                .as_str()
                .or_else(|| command.lines().next())
                .unwrap_or("");
            self.jobs.push((id, when, reason.to_string()));
        }
    }

    /// A part of a user message at `at`: a tool's result, else text.
    fn part(&mut self, part: &Value, at: u64) {
        if part["type"] != "tool_result" {
            return self.text(part["text"].as_str().unwrap_or(""));
        }
        let id = part["tool_use_id"].as_str().unwrap_or("");
        if part["is_error"] == true {
            self.jobs.retain(|(j, _, _)| j != id);
            self.timer.take_if(|t| t.0 == id);
            return;
        }
        if let Some(t) = self.timer.as_mut().filter(|t| t.0 == id) {
            let text: String = match &part["content"] {
                Value::String(s) => s.clone(),
                Value::Array(a) => a.iter().filter_map(|p| p["text"].as_str()).collect(),
                _ => String::new(),
            };
            t.3 = seconds_in(&text).map(|s| at + s);
        }
    }

    /// User text: a job's notification, else a prompt, which sets off the
    /// timer (John's, or the timer's own).
    fn text(&mut self, text: &str) {
        if text.contains("<task-notification>") {
            self.notified(text);
        } else {
            self.timer = None;
        }
    }

    /// The jobs a notification names have ended.
    fn notified(&mut self, text: &str) {
        for id in text
            .split("<tool-use-id>")
            .skip(1)
            .filter_map(|s| s.split_once("</tool-use-id>"))
        {
            self.jobs.retain(|(j, _, _)| j != id.0);
        }
    }

    /// The timer if one is pending, else the job due first, else the first.
    fn next(self) -> Option<(u64, String)> {
        if let Some((_, at, input, scheduled)) = self.timer {
            let asked = input["delaySeconds"]
                .as_u64()
                .filter(|_| at > 0)
                .map(|d| at + d);
            if let Some(when) = scheduled.or(asked) {
                return Some((when, input["reason"].as_str().unwrap_or("").to_string()));
            }
        }
        let jobs = &self.jobs;
        let first = jobs
            .iter()
            .filter(|j| j.1 > 0)
            .min_by_key(|j| j.1)
            .or(jobs.first())?;
        let reason = match jobs.len() {
            1 => first.2.clone(),
            n => format!("{} (+{} more)", first.2, n - 1),
        };
        Some((first.1, reason))
    }
}

/// N from the "(in Ns)" of a `ScheduleWakeup` result.
fn seconds_in(text: &str) -> Option<u64> {
    let rest = &text[text.find("(in ")? + 4..];
    let digits = &rest[..rest.find(|c: char| !c.is_ascii_digit())?];
    rest[digits.len()..]
        .starts_with("s)")
        .then(|| digits.parse().ok())
        .flatten()
}

/// The seconds a command that waits with `sleep N` (N in seconds, or with
/// an s, m or h suffix) sleeps first; None for any other command.
fn sleep_secs(command: &str) -> Option<u64> {
    let mut words = command.split(|c: char| c.is_whitespace() || c == ';' || c == '&' || c == '|');
    words.find(|w| *w == "sleep")?;
    let n = words.find(|w| !w.is_empty())?;
    let (digits, unit) = n.split_at(n.find(|c: char| !c.is_ascii_digit()).unwrap_or(n.len()));
    let n: u64 = digits.parse().ok()?;
    match unit {
        "" | "s" => Some(n),
        "m" => Some(n * 60),
        "h" => Some(n * 3600),
        _ => None,
    }
}

/// A log's entries (JSON lines), oldest first, the last ENTRIES of them.
#[must_use]
pub fn entries(log: &str, harness: &str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for line in log.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if harness == "agy" {
            agy_entry(&v, &mut out);
        } else if harness == "pi" {
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
            Some("tool_use") => {
                push_tool(out, part["name"].as_str().unwrap_or("tool"), &part["input"]);
            }
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
            Some("toolCall") => push_tool(
                out,
                part["name"].as_str().unwrap_or("tool"),
                &part["arguments"],
            ),
            _ => {}
        }
    }
}

/// An Antigravity step: John's request (without the metadata Antigravity
/// wraps it in), or a response's text and tool calls. Tool results and
/// errors are left out, as for the other harnesses.
fn agy_entry(v: &Value, out: &mut Vec<(&'static str, String)>) {
    let content = v["content"].as_str().unwrap_or("");
    match v["type"].as_str() {
        Some("USER_INPUT") => {
            let request = content
                .split_once("<USER_REQUEST>")
                .and_then(|(_, rest)| rest.split_once("</USER_REQUEST>"))
                .map_or(content, |(request, _)| request);
            push_text(out, "user", request);
        }
        Some("PLANNER_RESPONSE") => {
            push_text(out, "assistant", content);
            for call in v["tool_calls"].as_array().into_iter().flatten() {
                push_tool(
                    out,
                    call["name"].as_str().unwrap_or("tool"),
                    &agy_input(&call["args"]),
                );
            }
        }
        _ => {}
    }
}

/// An Antigravity tool call's arguments under the names `push_tool` looks
/// for: its own summary as the description, `CommandLine` as the command,
/// and the rest (`Path`, `Query`, `Url`, ...) lowercased.
fn agy_input(args: &Value) -> Value {
    let Some(args) = args.as_object() else {
        return Value::Null;
    };
    let renamed = args.iter().map(|(k, v)| {
        let key = match k.as_str() {
            "toolSummary" => "description".to_string(),
            "CommandLine" => "command".to_string(),
            k => k.to_lowercase(),
        };
        (key, v.clone())
    });
    Value::Object(renamed.collect())
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
    if name == "AskUserQuestion" || name == "ask_question" {
        let mut text = String::new();
        for q in input["questions"].as_array().into_iter().flatten() {
            text.push_str(q["question"].as_str().unwrap_or(""));
            for o in q["options"].as_array().into_iter().flatten() {
                if let Some(opt) = o.as_str() {
                    let _ = write!(text, "\n  - {opt}");
                } else {
                    let _ = write!(text, "\n  - {}", o["label"].as_str().unwrap_or(""));
                    if let Some(d) = o["description"].as_str().filter(|d| !d.is_empty()) {
                        let _ = write!(text, ": {d}");
                    }
                }
            }
            text.push('\n');
        }
        out.push(("ask", clip(text.trim(), TEXT)));
        return;
    }
    let detail = [
        "description",
        "command",
        "file_path",
        "path",
        "pattern",
        "url",
        "query",
        "prompt",
        "skill",
    ]
    .iter()
    .find_map(|k| input[*k].as_str().filter(|s| !s.is_empty()))
    .unwrap_or("");
    let detail = detail.lines().next().unwrap_or("");
    let text = if detail.is_empty() {
        name.to_string()
    } else {
        format!("{name}: {}", clip(detail, 200))
    };
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

    /// The end of an idle Claude Code screen with `input` in its box.
    fn claude_screen(input: &str) -> String {
        [
            "\x1b[0m\x1b[38;2;153;153;153m\u{273b}\x1b[0m Brewed for 2m\r",
            "\r",
            "\x1b[0m\x1b[38;2;136;136;136m\u{2500}\u{2500}\u{2500}\r",
            &format!("\u{276f}\u{a0}{input}\r"),
            "\x1b[0m\x1b[38;2;136;136;136m\u{2500}\u{2500}\u{2500}\r",
            "  \x1b[0m\x1b[38;2;153;153;153mctx 58k/1.0M 6% \x1b[0m\x1b[2m\x1b[38;2;153;153;153m|\x1b[0m\r",
        ]
        .join("\n")
    }

    #[test]
    fn dim_text_in_claudes_input_box_is_its_suggestion() {
        let screen = claude_screen("\x1b[0m\x1b[2mpoint rex at\x1b[1Cthe worktree\x1b[0m");
        assert_eq!(
            suggestion(&screen).as_deref(),
            Some("point rex at the worktree")
        );
    }

    #[test]
    fn typed_or_empty_input_is_no_suggestion() {
        assert_eq!(suggestion(&claude_screen("\x1b[0mfix the bug")), None);
        assert_eq!(suggestion(&claude_screen("\x1b[0m")), None);
        assert_eq!(suggestion("no prompt here"), None);
        // A 2 inside a colour isn't dim.
        assert_eq!(suggestion(&claude_screen("\x1b[38;2;2;2;2mtyped")), None);
    }

    /// Steps of a real Antigravity `transcript_full.jsonl`.
    const AGY_LOG: &str = concat!(
        r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-11-21T14:39:11Z","content":"<USER_REQUEST>\nRun 'ls' with the shell tool, then reply with one word.\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nThe current local time is: 2026-11-21T09:39:11-05:00.\n</ADDITIONAL_METADATA>"}"#,
        "\n",
        r#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-11-21T14:39:11Z","model":"gemini-3.8-flash-low","input_tokens":8761,"cache_read_tokens":0,"output_tokens":34,"tool_calls":[{"name":"run_command","args":{"CommandLine":"ls","WaitMsBeforeAsync":5000,"toolSummary":"Run ls"}},{"name":"read_file","args":{"Path":"/r/a.txt"}}]}"#,
        "\n",
        r#"{"step_index":2,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-11-21T14:39:13Z","content":"Command exited with code 0.\n\nCommand output:\na.txt\r\n"}"#,
        "\n",
        r#"{"step_index":3,"source":"SYSTEM","type":"ERROR_MESSAGE","status":"DONE","created_at":"2026-11-21T14:39:13Z","error":"UNAVAILABLE"}"#,
        "\n",
        r#"{"step_index":4,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-11-21T14:39:13Z","model":"claude-sonnet-4-6","input_tokens":2164,"cache_read_tokens":6720,"output_tokens":1,"content":"Done"}"#,
        "\n",
    );

    #[test]
    fn agy_logs_give_requests_responses_and_tool_calls() {
        assert_eq!(
            entries(AGY_LOG, "agy"),
            vec![
                (
                    "user",
                    "Run 'ls' with the shell tool, then reply with one word.".to_string()
                ),
                ("tool", "run_command: Run ls".to_string()),
                ("tool", "read_file: /r/a.txt".to_string()),
                ("assistant", "Done".to_string()),
            ]
        );
        assert_eq!(tokens(AGY_LOG, "agy"), Some(2164 + 6720 + 1));
        assert_eq!(model(AGY_LOG, "agy").as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(title(AGY_LOG, "agy"), None);
        assert_eq!(wake(AGY_LOG, "agy", 0), None);
    }

    #[test]
    fn long_agy_responses_are_not_truncated_at_old_limit() {
        let reply = "a".repeat(10_000);
        let log = json!({
            "step_index": 0,
            "source": "MODEL",
            "type": "PLANNER_RESPONSE",
            "status": "DONE",
            "content": reply,
        })
        .to_string();
        let got = entries(&log, "agy");
        assert_eq!(got, vec![("assistant", reply)]);

        let huge = "b".repeat(TEXT + 50);
        let huge_log = json!({
            "step_index": 1,
            "source": "MODEL",
            "type": "PLANNER_RESPONSE",
            "status": "DONE",
            "content": huge,
        })
        .to_string();
        let got_huge = entries(&huge_log, "agy");
        assert_eq!(got_huge.len(), 1);
        assert_eq!(got_huge[0].0, "assistant");
        assert!(got_huge[0].1.ends_with('…'));
        assert_eq!(got_huge[0].1.chars().count(), TEXT + 1);
    }

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
    fn the_last_turn_gives_the_context_used() {
        let claude = [
            json!({"type":"assistant","message":{"usage":{"input_tokens":1,"output_tokens":1}}}),
            json!({"type":"assistant","message":{"usage":{
                "input_tokens":10,"output_tokens":5,"cache_read_input_tokens":1000,"cache_creation_input_tokens":200}}}),
            json!({"type":"assistant","isSidechain":true,"message":{"usage":{"input_tokens":99}}}),
            json!({"type":"user","message":{"content":"next"}}),
        ]
        .map(|v| v.to_string())
        .join("\n");
        assert_eq!(tokens(&claude, "claude"), Some(1215));
        let pi = json!({"type":"message","message":{"role":"assistant","usage":{
            "input":3,"output":4,"cacheRead":50,"cacheWrite":6,"totalTokens":63}}})
        .to_string();
        assert_eq!(tokens(&pi, "pi"), Some(63));
        assert_eq!(tokens("{\"type\":\"user\"}", "claude"), None);
    }

    #[test]
    fn a_rename_wins_over_claudes_own_title() {
        let ai = |t: &str| format!(r#"{{"type":"ai-title","aiTitle":"{t}","sessionId":"s"}}"#);
        let log = [
            ai("Old"),
            ai("Slack thread issue"),
            r#"{"type":"user"}"#.to_string(),
        ]
        .join("\n");
        assert_eq!(title(&log, "claude").as_deref(), Some("Slack thread issue"));
        let renamed = format!(
            "{log}\n{}\n{}",
            r#"{"type":"custom-title","customTitle":"My name","sessionId":"s"}"#,
            ai("Later")
        );
        assert_eq!(title(&renamed, "claude").as_deref(), Some("My name"));
        assert_eq!(title(&log, "pi"), None);
        assert_eq!(title(r#"{"type":"user"}"#, "claude"), None);
    }

    #[test]
    fn the_last_turn_gives_the_model() {
        let claude = [
            json!({"type":"assistant","message":{"model":"claude-sonnet-5-5"}}),
            json!({"type":"assistant","message":{"model":"claude-opus-5-5"}}),
            json!({"type":"assistant","isSidechain":true,"message":{"model":"claude-haiku-5-5"}}),
            json!({"type":"assistant","message":{"model":"<synthetic>"}}),
        ]
        .map(|v| v.to_string())
        .join("\n");
        assert_eq!(model(&claude, "claude").as_deref(), Some("claude-opus-5-5"));
        let pi = [
            json!({"type":"message","message":{"role":"assistant","model":"gpt-6.1-sol"}}),
            json!({"type":"model_change","modelId":"claude-opus-5-5"}),
        ]
        .map(|v| v.to_string())
        .join("\n");
        assert_eq!(model(&pi, "pi").as_deref(), Some("claude-opus-5-5"));
        assert_eq!(model("{\"type\":\"user\"}", "claude"), None);
    }

    #[test]
    fn a_wakeup_with_no_prompt_since_is_pending() {
        let call = |id: &str, ts: &str| {
            json!({"type":"assistant","timestamp":ts,"message":{"content":[{"type":"tool_use","id":id,
                "name":"ScheduleWakeup","input":{"delaySeconds":60,"reason":"CI is running"}}]}})
        };
        let result = |id: &str, ts: &str, text: &str| {
            json!({"type":"user","timestamp":ts,"message":{"content":[
                {"type":"tool_result","tool_use_id":id,"content":text}]}})
        };
        let text = json!({"type":"assistant","timestamp":"2026-11-14T20:00:05Z","message":{"content":[
            {"type":"text","text":"I'll check back."}]}});
        let log = |lines: &[Value]| {
            lines
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        };
        let at = rfc3339_secs("2026-11-14T20:00:00Z").unwrap();
        // The result's time wins over the delay asked for.
        let scheduled = log(&[
            call("a", "2026-11-14T20:00:00Z"),
            result(
                "a",
                "2026-11-14T20:00:01Z",
                "Next wakeup scheduled for 20:01:09 (in 68s). Nothing more to do",
            ),
            text.clone(),
        ]);
        assert_eq!(
            wake(&scheduled, "claude", 0),
            Some((at + 1 + 68, "CI is running".into()))
        );
        // No result yet: the delay asked for.
        assert_eq!(
            wake(&log(&[call("a", "2026-11-14T20:00:00Z")]), "claude", 0),
            Some((at + 60, "CI is running".into()))
        );
        // A prompt since (the timer's own, say) leaves nothing pending.
        let prompted = log(&[
            call("a", "2026-11-14T20:00:00Z"),
            result("a", "2026-11-14T20:00:01Z", "(in 68s)"),
            json!({"type":"user","timestamp":"2026-11-14T20:01:09Z","message":{"content":"check CI"}}),
            text.clone(),
        ]);
        assert_eq!(wake(&prompted, "claude", 0), None);
        // A failed call set nothing.
        let failed = log(&[
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"a",
            "is_error":true,"content":"no"}]}}),
        ]);
        assert_eq!(
            wake(
                &(log(&[call("a", "2026-11-14T20:00:00Z")]) + "\n" + &failed),
                "claude",
                0
            ),
            None
        );
        assert_eq!(wake(&log(std::slice::from_ref(&text)), "claude", 0), None);
        assert_eq!(wake(&scheduled, "pi", 0), None);
        // A timer set before this agent process started died with the last.
        assert_eq!(wake(&scheduled, "claude", at + 1), None);
    }

    #[test]
    fn a_background_job_with_no_notification_yet_is_pending() {
        let job = |id: &str, ts: &str, command: &str, description: &str| {
            json!({"type":"assistant","timestamp":ts,"message":{"content":[{"type":"tool_use","id":id,"name":"Bash",
                "input":{"command":command,"description":description,"run_in_background":true}}]}})
        };
        let notified = |id: &str| {
            json!({"type":"user","message":{"content":format!(
                "<task-notification>\n<task-id>x</task-id>\n<tool-use-id>{id}</tool-use-id>\n<status>completed</status>")}})
        };
        let log = |lines: &[Value]| {
            lines
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        };
        let at = rfc3339_secs("2026-11-14T20:00:00Z").unwrap();
        let wait = job(
            "a",
            "2026-11-14T20:00:00Z",
            "sleep 300 && gh pr checks",
            "Wait five minutes",
        );
        let build = job("b", "2026-11-14T20:00:10Z", "cargo build", "Build");
        // A sleep says when; the one due first wins.
        assert_eq!(
            wake(&log(&[build.clone(), wait.clone()]), "claude", 0),
            Some((at + 300, "Wait five minutes (+1 more)".into()))
        );
        // Any other job has no time.
        assert_eq!(
            wake(
                &log(&[wait.clone(), build.clone(), notified("a")]),
                "claude",
                0
            ),
            Some((0, "Build".into()))
        );
        // A notification, queued or delivered, ends it; a prompt from John doesn't.
        let queued = json!({"type":"queue-operation","operation":"enqueue","content":"<tool-use-id>b</tool-use-id>"});
        let prompt = json!({"type":"user","message":{"content":"how's it going?"}});
        assert_eq!(
            wake(
                &log(&[wait.clone(), build.clone(), queued, prompt, notified("a")]),
                "claude",
                0
            ),
            None
        );
        assert_eq!(
            wake(&log(std::slice::from_ref(&build)), "claude", at + 60),
            None
        );
        assert_eq!(sleep_secs("sleep 5m"), Some(300));
        assert_eq!(sleep_secs("cd x; sleep 2h"), Some(7200));
        assert_eq!(sleep_secs("echo sleepy 3"), None);
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
            [
                ("user", "Hi".to_string()),
                ("assistant", "Reading.".to_string()),
                ("tool", "read: /a/b".to_string())
            ]
        );
    }

    #[test]
    fn a_running_claude_agent_is_found_by_its_process() {
        let root =
            std::env::temp_dir().join(format!("switchyard-transcript-{}", std::process::id()));
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

    #[test]
    fn agy_log_isolates_concurrent_live_processes_and_falls_back_when_unavailable() {
        use std::os::unix::fs::symlink;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "switchyard-agy-log-{}-{unique}",
            std::process::id()
        ));
        let make_log = |id: &str| {
            let dir = root.join("brain").join(id).join(".system_generated/logs");
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("transcript_full.jsonl");
            std::fs::write(&path, "").unwrap();
            path
        };
        let log_a = make_log("conv-a");
        let log_b = make_log("conv-b");
        let log_stale = make_log("conv-stale");

        let fd_a = root.join("fd-a");
        let fd_empty = root.join("fd-empty");
        std::fs::create_dir_all(&fd_a).unwrap();
        std::fs::create_dir_all(&fd_empty).unwrap();
        symlink(root.join("presence/conv-a.lock"), fd_a.join("15")).unwrap();

        // Live process with open presence lock resolves its exact log even when
        // last_conversations.json and agent_session point elsewhere.
        let found = agy_log_with(
            &root,
            crate::sessions::agy_conversation_from_fd_dir(&fd_a),
            || Some("conv-stale".into()),
            "conv-b",
        );
        assert_eq!(found, Some(log_a));

        // Live readable /proc/<pid>/fd with no lock/db yet (starting or /clear)
        // must NOT fall back to stale last_conversations.json or agent_session.
        let found = agy_log_with(
            &root,
            crate::sessions::agy_conversation_from_fd_dir(&fd_empty),
            || Some("conv-stale".into()),
            "conv-b",
        );
        assert_eq!(found, None);

        // Unavailable /proc/<pid>/fd (exited process / non-Linux) falls back to
        // agent_session first, then last_conversations.json.
        let missing = root.join("no-such-fd");
        let found = agy_log_with(
            &root,
            crate::sessions::agy_conversation_from_fd_dir(&missing),
            || Some("conv-stale".into()),
            "conv-b",
        );
        assert_eq!(found, Some(log_b));

        let found = agy_log_with(
            &root,
            crate::sessions::agy_conversation_from_fd_dir(&missing),
            || Some("conv-stale".into()),
            "",
        );
        assert_eq!(found, Some(log_stale));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agy_ask_question_renders_as_ask_and_tracks_pending_question() {
        let ask_step = json!({
            "step_index": 1,
            "source": "MODEL",
            "type": "PLANNER_RESPONSE",
            "status": "DONE",
            "content": "Two design questions:",
            "tool_calls": [{
                "name": "ask_question",
                "args": {
                    "questions": [
                        {
                            "question": "Pick one",
                            "options": ["(Recommended) Alpha", "Beta"],
                            "is_multi_select": false
                        },
                        {
                            "question": "Pick many",
                            "options": ["One", "Two"],
                            "is_multi_select": true
                        }
                    ]
                }
            }]
        })
        .to_string();

        assert_eq!(
            entries(&ask_step, "agy"),
            vec![
                ("assistant", "Two design questions:".to_string()),
                (
                    "ask",
                    "Pick one\n  - (Recommended) Alpha\n  - Beta\nPick many\n  - One\n  - Two"
                        .to_string()
                ),
            ]
        );
        assert_eq!(
            last_agy_question(&ask_step),
            Some(vec![
                PromptQuestion {
                    question: "Pick one".into(),
                    options: vec!["(Recommended) Alpha".into(), "Beta".into()],
                    multi_select: false,
                },
                PromptQuestion {
                    question: "Pick many".into(),
                    options: vec!["One".into(), "Two".into()],
                    multi_select: true,
                },
            ])
        );

        // Once the planner responds again (or a user message follows), the
        // earlier ask_question is no longer pending.
        let answered = format!(
            "{ask_step}\n{}\n{}",
            json!({"step_index": 2, "source": "MODEL", "type": "GENERIC", "status": "DONE", "content": "A1: Alpha"}),
            json!({"step_index": 3, "source": "MODEL", "type": "PLANNER_RESPONSE", "status": "DONE", "content": "Got it."}),
        );
        assert_eq!(last_agy_question(&answered), None);
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
