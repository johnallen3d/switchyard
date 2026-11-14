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
//!
//! It reads only the end of the log and returns its last messages, tool
//! calls and questions as `{harness, entries: [{kind, text}], tokens, model}`,
//! where kind is `user`, `assistant`, `tool` or `ask` and tokens is the
//! context the agent's last turn used and model the model it ran on (each
//! absent when the log doesn't say), and `wake: {at, reason}` when a Claude
//! agent is waiting on a timer or background job to resume it, `at` 0 when
//! its time isn't known (see `wake`); or `{error}`. `fetch` adds
//! `agent: {host, profile, harness, dir}` once it knows them.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::rex::{host_bash, local_label, rex_json, sh_quote};
use crate::sessions::session_dir;
use crate::tasks::{in_window, load_tasks, rfc3339_secs};

/// How much of the end of a log to read.
const TAIL: u64 = 2 * 1024 * 1024;
/// How many entries to return, and the longest text of one (in characters).
const ENTRIES: usize = 150;
const TEXT: usize = 6000;
/// The most text a transcript carries, so its paste stays small.
const BYTES: usize = 96 * 1024;

/// A tab's agent pane, run as `rex [-S HOST] do -e SCRIPT --json --args
/// '{"session":ID,"window":ID}'`: the tiled pane with the most urgent
/// program status, as `host_rows.lua` picks a tab's, with its block, status,
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

/// The agent pane of a tab on `host` (see `PANE_SCRIPT`).
///
/// # Errors
///
/// Fails when Rex can't run the lookup or the tab isn't found.
pub fn agent_pane(host: &str, session: &str, window: &str) -> Result<Value, String> {
    let args = json!({ "session": session, "window": window }).to_string();
    rex_json(host, &["do", "-e", PANE_SCRIPT, "--json", "--args", &args])
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
    fetch_inner(&host, session, window).unwrap_or_else(|e| json!({ "error": e }))
}

/// `fetch` for a tab on the host with Rex label `host`.
#[must_use]
pub fn fetch_on(host: &str, session: &str, window: &str) -> Value {
    fetch_inner(host, session, window).unwrap_or_else(|e| json!({ "error": e }))
}

fn unhex(s: &str) -> String {
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
        Ok((entries, tokens, model, wake)) => {
            let mut v = json!({
                "harness": harness,
                "entries": entries.into_iter().map(|(kind, text)| json!({ "kind": kind, "text": text })).collect::<Vec<_>>(),
            });
            if let Some(t) = tokens {
                v["tokens"] = json!(t);
            }
            if let Some(m) = model {
                v["model"] = json!(m);
            }
            if let Some((at, reason)) = wake {
                v["wake"] = json!({ "at": at, "reason": reason });
            }
            v
        }
        Err(e) => json!({ "error": format!("{}: {e}", path.display()) }),
    }
}

/// When the running Claude agent `pid` started (Unix seconds), from its
/// `CONFIG/sessions/PID.json`; 0 when unknown.
fn started(profile: &str, harness: &str, dir: &str, pid: u64) -> u64 {
    if harness == "pi" || pid == 0 {
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
/// the model it ran on and what it waits on to resume (see `wake`).
type Tail = (
    Vec<(&'static str, String)>,
    Option<u64>,
    Option<String>,
    Option<(u64, String)>,
);

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
    Ok((
        entries(text, harness),
        tokens(text, harness),
        model(text, harness),
        wake(text, harness, since),
    ))
}

/// The model the agent's last turn ran on, or Pi's last switch to one since.
/// None when the log doesn't say.
#[must_use]
pub fn model(log: &str, harness: &str) -> Option<String> {
    log.lines().rev().find_map(|line| {
        let v = serde_json::from_str::<Value>(line).ok()?;
        let m = if harness == "pi" {
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
        if harness == "pi" {
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
/// None for Pi, which has neither.
pub fn wake(log: &str, harness: &str, since: u64) -> Option<(u64, String)> {
    if harness == "pi" {
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
                let _ = write!(text, "\n  - {}", o["label"].as_str().unwrap_or(""));
                if let Some(d) = o["description"].as_str().filter(|d| !d.is_empty()) {
                    let _ = write!(text, ": {d}");
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
