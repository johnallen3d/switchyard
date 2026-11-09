//! Where switchyard-server listens and how a client asks it, so the server
//! and the pickers agree on both. The server (crates/server) keeps every
//! Rex server's rows and the task registry up to date from events; a client
//! asks it rather than fanning out to every host itself, and falls back to
//! the direct path when nothing answers.
//!
//! The protocol is newline-delimited JSON over a Unix socket. A request is
//! one object with a `cmd`, and each gets one reply line unless noted; a
//! reply with `error` (and nothing else) means the server can't answer it,
//! and the client falls back:
//!
//! - `{"cmd":"snapshot"}`: the snapshot (rows, tasks, errors; see the
//!   server's state module for its fields).
//! - `{"cmd":"subscribe"}`: a snapshot at once and another after each
//!   change, until the client hangs up.
//! - `{"cmd":"folders","host":HOST}`: HOST's zoxide folders as the server
//!   last listed them, `{"host","folders","age_ms"}` with `folders` as
//!   `zoxide query -l` prints them. An error when HOST isn't one the server
//!   lists (the config host and the dev hosts), hasn't been listed yet, or
//!   its last listing failed. A listing older than a minute is listed again
//!   in the background after the reply.
//! - `{"cmd":"probes","project":NAME}`: `{"probes":[...]}`, one entry per
//!   dev host `auto` can choose: `host`, `load` (per CPU), `mem_gib` (free),
//!   `age_ms` and `error` of the last measurement (age null before the
//!   first), and from the host's rows `agents` (panes working or blocked)
//!   and `warm` (it has a session named NAME), both null without rows. See
//!   `hosts::choose_host`.
//! - `{"cmd":"refresh"}`: check the task registry against the hosts now
//!   rather than at the next trigger, after an action changed it; the reply,
//!   `{"ok":true}`, comes at once and the result arrives as usual (pushed
//!   to subscribers).
//!
//! `pastes` turns a snapshot into the float's data pastes (see the
//! tab-picker), so the float's fill script (through `switchyard-server
//! pastes`) and the standalone picker show the same thing from it.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::rex::runtime_dir;

/// The server's socket, in this user's runtime directory.
pub fn socket_path() -> Option<PathBuf> {
    runtime_dir().map(|d| d.join("server.sock"))
}

/// How long a client waits for a reply before falling back. A snapshot is
/// served from memory, so anything slower means a stuck server.
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

fn send(cmd: &str) -> Result<BufReader<UnixStream>, String> {
    request(serde_json::json!({ "cmd": cmd }))
}

fn request(req: Value) -> Result<BufReader<UnixStream>, String> {
    let path = socket_path().ok_or("no runtime directory for the server's socket")?;
    let mut stream = UnixStream::connect(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    stream.set_read_timeout(Some(REPLY_TIMEOUT)).map_err(|e| e.to_string())?;
    writeln!(stream, "{req}").map_err(|e| format!("switchyard-server: {e}"))?;
    Ok(BufReader::new(stream))
}

fn read_reply(reader: &mut BufReader<UnixStream>) -> Result<Value, String> {
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => Err("switchyard-server hung up".into()),
        Ok(_) => {
            let reply: Value = serde_json::from_str(&line).map_err(|e| format!("switchyard-server: {e}"))?;
            match reply["error"].as_str() {
                Some(e) => Err(format!("switchyard-server: {e}")),
                None => Ok(reply),
            }
        }
        Err(e) => Err(format!("switchyard-server: {e}")),
    }
}

/// The server's current snapshot, or why it couldn't be had (most often:
/// no server running).
pub fn snapshot() -> Result<Value, String> {
    read_reply(&mut send("snapshot")?)
}

/// `host`'s zoxide folders from the server's cache, one per line, or why
/// not (no server, or it has no good listing for `host`).
pub fn folders(host: &str) -> Result<String, String> {
    let reply = read_reply(&mut request(serde_json::json!({ "cmd": "folders", "host": host }))?)?;
    reply["folders"].as_str().map(String::from).ok_or_else(|| "switchyard-server: no folders".into())
}

/// The server's cached host probes for `project` (see the module docs), or
/// why not.
pub fn probes(project: &str) -> Result<Vec<Value>, String> {
    let reply = read_reply(&mut request(serde_json::json!({ "cmd": "probes", "project": project }))?)?;
    reply["probes"].as_array().cloned().ok_or_else(|| "switchyard-server: no probes".into())
}

/// Ask the server to check the task registry now, after an action changed
/// it. Quietly does nothing without a server: the registry is read fresh
/// on the direct path anyway.
pub fn refresh() {
    if let Ok(mut reader) = send("refresh") {
        let _ = read_reply(&mut reader);
    }
}

/// Snapshots as they change, starting with the current one. Iteration ends
/// at the first error, such as the server stopping.
pub fn subscribe() -> Result<Subscription, String> {
    let mut reader = send("subscribe")?;
    // The first reply comes at once, so it gets the usual timeout ...
    let first = read_reply(&mut reader)?;
    // ... but updates come only on changes, and a quiet minute is normal.
    reader.get_ref().set_read_timeout(None).map_err(|e| e.to_string())?;
    Ok(Subscription { first: Some(first), reader: Some(reader) })
}

/// The first snapshot `ok` accepts, waiting up to `timeout` for the server
/// to get there; for a client that would rather not show half an answer.
pub fn wait_for(timeout: Duration, ok: impl Fn(&Value) -> bool) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    let mut sub = subscribe()?;
    loop {
        let snap = sub.next().unwrap_or_else(|| Err("switchyard-server hung up".into()))?;
        if ok(&snap) {
            return Ok(snap);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("switchyard-server: still gathering".into());
        }
        if let Some(reader) = &sub.reader {
            reader.get_ref().set_read_timeout(Some(left)).map_err(|e| e.to_string())?;
        }
    }
}

pub struct Subscription {
    first: Option<Value>,
    reader: Option<BufReader<UnixStream>>,
}

impl Iterator for Subscription {
    type Item = Result<Value, String>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(first) = self.first.take() {
            return Some(Ok(first));
        }
        let reply = read_reply(self.reader.as_mut()?);
        if reply.is_err() {
            self.reader = None;
        }
        Some(reply)
    }
}

/// Whether every Rex server has answered (with rows or an error) and the
/// registry has been read: a snapshot the float can show whole. A server
/// that has only just started has neither.
pub fn is_ready(snap: &Value) -> bool {
    let hosts = snap["hosts"].as_array().map(Vec::as_slice).unwrap_or_default();
    !hosts.is_empty()
        && hosts.iter().all(|h| !h["age_ms"].is_null() || !h["error"].is_null())
        && (!snap["tasks"].is_null() || !snap["tasks_error"].is_null())
}

/// Whether the snapshot's tasks are as good as they will get: checked
/// against their hosts, or the check failed.
pub fn tasks_settled(snap: &Value) -> bool {
    snap["tasks_checked"].as_bool() == Some(true) || !snap["tasks_error"].is_null()
}

/// The float's data pastes for a snapshot, as (kind, body) in delivery
/// order, leaving out what the server doesn't know yet:
///
/// - `rows`: every host's rows, the config host first, then an `E` row
///   (`E HOST MESSAGE`) for each host that failed, whose last rows (if any)
///   stay, or hasn't answered yet. The picker shows the rows it has and
///   names the hosts missing. Only when no host has ever answered and one
///   failed is it `error`, as when the direct fan-out fails everywhere.
/// - `tasks` once the registry has been checked against the hosts, else
///   `live` (the same with live status), which the float shows until a
///   checked `tasks` replaces it.
/// - `tasks-error` when the registry couldn't be read or checked; after
///   `live`, the picker keeps those tasks, as after the direct path's.
pub fn pastes(snap: &Value) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let hosts = snap["hosts"].as_array().map(Vec::as_slice).unwrap_or_default();
    if !hosts.is_empty() {
        let (mut rows, mut failed, mut first_error) = (String::new(), String::new(), None);
        let mut answered = false;
        for h in hosts {
            let name = h["host"].as_str().unwrap_or("?");
            rows.push_str(h["rows"].as_str().unwrap_or(""));
            answered |= !h["age_ms"].is_null();
            let why = match h["error"].as_str() {
                Some(e) => {
                    first_error.get_or_insert_with(|| format!("{name}: {e}"));
                    e
                }
                None if h["age_ms"].is_null() => "not listed yet",
                None => continue,
            };
            failed.push_str(&failed_row(name, why));
        }
        match first_error {
            Some(e) if !answered => out.push(("error", e)),
            _ if !answered => {}
            _ => out.push(("rows", rows + &failed)),
        }
    }
    let checked = snap["tasks_checked"].as_bool() == Some(true);
    let error = snap["tasks_error"].as_str();
    match &snap["tasks"] {
        Value::Array(_) => {
            out.push((if checked { "tasks" } else { "live" }, snap["tasks"].to_string()));
            if let (false, Some(e)) = (checked, error) {
                out.push(("tasks-error", e.to_string()));
            }
        }
        _ => {
            if let Some(e) = error {
                out.push(("tasks-error", e.to_string()));
            }
        }
    }
    out
}

/// The rows' note that a host's tabs are missing or stale, kept to one
/// row: tabs and newlines in the message become spaces.
fn failed_row(host: &str, why: &str) -> String {
    let why: String = why.trim().chars().map(|c| if c == '\t' || c == '\n' { ' ' } else { c }).collect();
    format!("E\t{host}\t{why}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn host(name: &str, rows: &str, age: Option<u64>, error: Option<&str>) -> Value {
        json!({ "host": name, "rows": rows, "age_ms": age, "error": error, "watching": true })
    }

    #[test]
    fn pastes_carry_every_host_and_checked_tasks() {
        let snap = json!({
            "hosts": [host("local", "S\t-\tlocal\ts1\ta\n", Some(5), None), host("miller", "S\tx\tmiller\ts2\tb\n", Some(9), None)],
            "tasks": [{"window_id": "w"}], "tasks_checked": true, "tasks_error": null,
        });
        assert!(is_ready(&snap) && tasks_settled(&snap));
        assert_eq!(pastes(&snap), vec![
            ("rows", "S\t-\tlocal\ts1\ta\nS\tx\tmiller\ts2\tb\n".to_string()),
            ("tasks", r#"[{"window_id":"w"}]"#.to_string()),
        ]);
    }

    #[test]
    fn failed_hosts_keep_their_rows_and_get_a_note() {
        let snap = json!({
            "hosts": [
                host("local", "S\t-\tlocal\ts1\ta\n", Some(5), None),
                host("miller", "S\tx\tmiller\ts2\tb\n", Some(60000), Some("watcher exited (1)\nssh: gone")),
                host("drummer", "", None, None),
            ],
            "tasks": [], "tasks_checked": false, "tasks_error": null,
        });
        // drummer hasn't answered yet: not one to show whole.
        assert!(!is_ready(&snap) && !tasks_settled(&snap));
        assert_eq!(pastes(&snap), vec![
            ("rows", "S\t-\tlocal\ts1\ta\nS\tx\tmiller\ts2\tb\n\
                E\tmiller\twatcher exited (1) ssh: gone\n\
                E\tdrummer\tnot listed yet\n".to_string()),
            ("live", "[]".to_string()),
        ]);
    }

    #[test]
    fn nothing_listed_anywhere_is_an_error_or_nothing() {
        let mut snap = json!({
            "hosts": [host("local", "", None, Some("can't start rex")), host("miller", "", None, None)],
            "tasks": null, "tasks_checked": false, "tasks_error": "bad json",
        });
        assert!(!is_ready(&snap));
        assert_eq!(pastes(&snap), vec![
            ("error", "local: can't start rex".to_string()),
            ("tasks-error", "bad json".to_string()),
        ]);
        // A server that has only just started knows nothing yet.
        snap["hosts"][0]["error"] = Value::Null;
        snap["tasks_error"] = Value::Null;
        assert!(pastes(&snap).is_empty());
        assert!(pastes(&json!({ "hosts": [], "tasks": null })).is_empty());
    }

    #[test]
    fn unchecked_tasks_that_failed_their_check_stay_live() {
        let snap = json!({
            "hosts": [host("local", "", Some(1), None)],
            "tasks": [], "tasks_checked": false, "tasks_error": "ssh: timeout",
        });
        assert!(is_ready(&snap) && tasks_settled(&snap));
        assert_eq!(pastes(&snap), vec![
            ("rows", String::new()),
            ("live", "[]".to_string()),
            ("tasks-error", "ssh: timeout".to_string()),
        ]);
    }
}
