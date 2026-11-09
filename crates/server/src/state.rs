//! What the server knows, shared by its threads: each Rex server's rows
//! (from its watcher) and the task registry (from the last reconcile), and
//! the subscribers to tell when either changes. Every client gets the same
//! snapshot, built here.
//!
//! A snapshot is one JSON object:
//!
//! - `seq`: counts changes since the server started.
//! - `hosts`: one object per Rex server, the config host first as `local`
//!   then `rex.servers()` order: `host`, `rows` (host_rows.lua's output, as
//!   the float's `rows` paste carries it), `age_ms` (since the rows were
//!   last read; null before the first), `error` (the last failure, cleared
//!   by the next rows; old rows stay) and `watching` (a watcher is running).
//! - `tasks`: the registry as `rex-spawn --tasks` prints it, with each open
//!   task's live agent status from the rows. Null when unreadable.
//! - `tasks_checked`: whether the tasks have been checked against their
//!   hosts (worktrees, ended tabs) yet: true is the float's `tasks` paste,
//!   false its `live` one.
//! - `tasks_age_ms`, `tasks_error`: as for a host.
//!
//! The state also keeps what the spawn picker and `auto` ask for apart from
//! snapshots (see `hostinfo`): each host's zoxide folders and each dev
//! host's load and memory. Those change no snapshot and are never pushed.

use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use serde_json::{Value, json};
use switchyard_core::hosts::Metrics;
use switchyard_core::tasks::{TabStatus, apply_live, parse_window_status};

/// The rows and live status host_rows.lua (run with `status=yes`) gave for
/// one Rex server: its W rows and B row split out of the rows the pickers
/// read.
#[derive(Debug, Default, PartialEq)]
pub struct Rows {
    pub text: String,
    pub live: HashMap<String, TabStatus>,
    /// Panes with an agent working or blocked.
    pub busy: u32,
}

pub fn split_rows(output: &str) -> Rows {
    let mut rows = Rows::default();
    for line in output.lines() {
        if let Some(status) = line.strip_prefix("W\t") {
            let (window, status) = parse_window_status(status);
            rows.live.insert(window, status);
        } else if let Some(busy) = line.strip_prefix("B\t") {
            rows.busy = busy.trim().parse().unwrap_or(0);
        } else {
            rows.text.push_str(line);
            rows.text.push('\n');
        }
    }
    rows
}

/// Whether the rows have a session labelled `label`: the project's session
/// that makes a host warm for `auto`.
fn has_session(text: &str, label: &str) -> bool {
    text.lines().any(|l| l.starts_with("S\t") && l.split('\t').nth(4) == Some(label))
}

/// The last attempt at something the server lists over SSH: the last good
/// value, when the attempt ended, and its error (None when it worked).
pub struct Listing<T> {
    pub value: Option<T>,
    pub at: Option<Instant>,
    pub error: Option<String>,
    /// An attempt is running; don't start another.
    pub running: bool,
    /// A client found the value stale; list again now.
    pub wanted: bool,
}

impl<T> Default for Listing<T> {
    fn default() -> Self {
        Listing { value: None, at: None, error: None, running: false, wanted: false }
    }
}

impl<T> Listing<T> {
    /// Whether to start an attempt: none yet, wanted, or the last is `every`
    /// old, and none running.
    pub fn due(&self, now: Instant, every: std::time::Duration) -> bool {
        !self.running && (self.wanted || self.at.is_none_or(|at| now.saturating_duration_since(at) >= every))
    }

    pub fn set(&mut self, got: Result<T, String>, now: Instant) {
        match got {
            Ok(v) => {
                self.value = Some(v);
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
        self.at = Some(now);
        self.running = false;
    }
}

/// The dev hosts' (and the config host's) listings, and which hosts the
/// config says to keep them for.
#[derive(Default)]
pub struct HostInfo {
    /// This machine's Rex label, whose rows are `local`'s.
    pub local: String,
    pub folders: HashMap<String, Listing<String>>,
    pub metrics: HashMap<String, Listing<Metrics>>,
}

impl HostInfo {
    /// Keep listings for these hosts only, keeping what's known of them.
    pub fn track(&mut self, local: &str, folder_hosts: &[String], probe_hosts: &[String]) {
        self.local = local.to_string();
        self.folders.retain(|h, _| folder_hosts.contains(h));
        self.metrics.retain(|h, _| probe_hosts.contains(h));
        for h in folder_hosts {
            self.folders.entry(h.clone()).or_default();
        }
        for h in probe_hosts {
            self.metrics.entry(h.clone()).or_default();
        }
    }
}

#[derive(Default)]
struct Host {
    name: String,
    rows: Rows,
    read_at: Option<Instant>,
    error: Option<String>,
    watching: bool,
}

#[derive(Default)]
struct Tasks {
    /// The registry as last read or reconciled, before live status.
    list: Option<Vec<Value>>,
    checked: bool,
    read_at: Option<Instant>,
    error: Option<String>,
}

#[derive(Default)]
pub struct State {
    hosts: Vec<Host>,
    tasks: Tasks,
    pub info: HostInfo,
    seq: u64,
    subscribers: Vec<Sender<String>>,
}

impl State {
    /// Set the Rex servers to watch, keeping what's known of the ones that
    /// stay, in the given order.
    pub fn set_hosts(&mut self, names: &[String]) {
        let mut old = std::mem::take(&mut self.hosts);
        self.hosts = names
            .iter()
            .map(|name| match old.iter().position(|h| &h.name == name) {
                Some(i) => old.swap_remove(i),
                None => Host { name: name.clone(), ..Host::default() },
            })
            .collect();
    }

    fn host(&mut self, name: &str) -> Option<&mut Host> {
        self.hosts.iter_mut().find(|h| h.name == name)
    }

    /// New rows from a host's watcher; whether anything a client sees
    /// changed (the safety timer mostly sends the same rows again).
    pub fn set_rows(&mut self, name: &str, rows: Rows, now: Instant) -> bool {
        let Some(host) = self.host(name) else { return false };
        host.read_at = Some(now);
        let changed = host.rows != rows || host.error.is_some();
        host.rows = rows;
        host.error = None;
        changed
    }

    pub fn set_error(&mut self, name: &str, error: String) -> bool {
        let Some(host) = self.host(name) else { return false };
        let changed = host.error.as_deref() != Some(&error);
        host.error = Some(error);
        changed
    }

    pub fn set_watching(&mut self, name: &str, watching: bool) -> bool {
        let Some(host) = self.host(name) else { return false };
        let changed = host.watching != watching;
        host.watching = watching;
        changed
    }

    /// The registry, read (`checked` false) or reconciled (true); whether
    /// a client would see a difference (most timed checks change nothing).
    /// A failed read keeps the last good list along with the error.
    pub fn set_tasks(&mut self, got: Result<Vec<Value>, String>, checked: bool, now: Instant) -> bool {
        match got {
            Ok(list) => {
                let changed = self.tasks.list.as_ref() != Some(&list)
                    || self.tasks.checked != checked
                    || self.tasks.error.is_some();
                self.tasks = Tasks { list: Some(list), checked, read_at: Some(now), error: None };
                changed
            }
            Err(e) => {
                let changed = self.tasks.error.as_deref() != Some(&e);
                self.tasks.error = Some(e);
                changed
            }
        }
    }

    /// The snapshot as clients get it.
    pub fn snapshot(&self, now: Instant) -> Value {
        let age = |at: Option<Instant>| at.map(|at| now.saturating_duration_since(at).as_millis() as u64);
        let hosts: Vec<Value> = self
            .hosts
            .iter()
            .map(|h| {
                json!({
                    "host": h.name,
                    "rows": h.rows.text,
                    "age_ms": age(h.read_at),
                    "error": h.error,
                    "watching": h.watching,
                })
            })
            .collect();
        let tasks = self.tasks.list.as_ref().map(|list| {
            let mut list = list.clone();
            let mut live = HashMap::new();
            for host in &self.hosts {
                live.extend(host.rows.live.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
            apply_live(&mut list, &live);
            list
        });
        json!({
            "seq": self.seq,
            "hosts": hosts,
            "tasks": tasks,
            "tasks_checked": self.tasks.checked,
            "tasks_age_ms": age(self.tasks.read_at),
            "tasks_error": self.tasks.error,
        })
    }

    /// The reply to `folders` for `host` (see core's `server`), and whether
    /// the listing is older than `stale`, so worth listing again.
    pub fn folders_reply(&self, host: &str, now: Instant, stale: std::time::Duration) -> (Value, bool) {
        let Some(l) = self.info.folders.get(host) else {
            return (json!({ "error": format!("no folders kept for {host}") }), false);
        };
        let age = l.at.map(|at| now.saturating_duration_since(at));
        let stale = age.is_some_and(|a| a >= stale);
        let reply = match (&l.value, &l.error, age) {
            (_, Some(e), _) => json!({ "error": format!("{host}: {e}") }),
            (Some(text), None, Some(age)) => json!({ "host": host, "folders": text, "age_ms": age.as_millis() as u64 }),
            _ => json!({ "error": format!("{host}: not listed yet") }),
        };
        (reply, stale)
    }

    /// The reply to `probes` for `project` (see core's `server`): each
    /// dev host's last measurement with busy agents and the project's
    /// session from its rows.
    pub fn probes_reply(&self, project: &str, now: Instant) -> Value {
        let mut names: Vec<&String> = self.info.metrics.keys().collect();
        names.sort();
        let probes: Vec<Value> = names
            .into_iter()
            .map(|name| {
                let l = &self.info.metrics[name];
                let rows_name = if *name == self.info.local { "local" } else { name.as_str() };
                let rows = self
                    .hosts
                    .iter()
                    .find(|h| h.name == rows_name && h.read_at.is_some() && h.error.is_none())
                    .map(|h| &h.rows);
                json!({
                    "host": name,
                    "load": l.value.map(|m| m.load),
                    "mem_gib": l.value.map(|m| m.mem_gib),
                    "age_ms": l.at.map(|at| now.saturating_duration_since(at).as_millis() as u64),
                    "error": l.error,
                    "agents": rows.map(|r| r.busy),
                    "warm": rows.map(|r| has_session(&r.text, project)),
                })
            })
            .collect();
        json!({ "probes": probes })
    }

    pub fn subscribe(&mut self, tx: Sender<String>) {
        self.subscribers.push(tx);
    }

    /// Count a change and send the new snapshot to every subscriber,
    /// dropping the ones that have gone.
    pub fn publish(&mut self, now: Instant) {
        self.seq += 1;
        if self.subscribers.is_empty() {
            return;
        }
        let line = self.snapshot(now).to_string();
        self.subscribers.retain(|tx| tx.send(line.clone()).is_ok());
    }
}

/// The state behind one lock. Holders only update fields or build a
/// snapshot, never wait on a host.
pub struct Shared(Mutex<State>);

impl Shared {
    pub fn new() -> Self {
        Shared(Mutex::new(State::default()))
    }

    pub fn lock(&self) -> MutexGuard<'_, State> {
        // A panicked holder leaves plain data behind; keep serving it.
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Run `f` on the state and publish when it reports a change.
    pub fn update(&self, f: impl FnOnce(&mut State) -> bool) {
        let mut state = self.lock();
        if f(&mut state) {
            state.publish(Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    const OUTPUT: &str = "S\t-\tlocal\tsession:1\tagents\n\
        T\twindow:a\tworking\tclaude\t~/dev\n\
        W\twindow:a\tworking\t2026-11-07T15:40:18.29-05:00\tthinking\n\
        T\twindow:b\t\tshell\t~\n\
        W\twindow:b\tidle\t\t\n\
        B\t2\n";

    #[test]
    fn split_rows_keeps_the_picker_rows_and_takes_the_status() {
        let rows = split_rows(OUTPUT);
        assert_eq!(rows.text, "S\t-\tlocal\tsession:1\tagents\nT\twindow:a\tworking\tclaude\t~/dev\nT\twindow:b\t\tshell\t~\n");
        let a = &rows.live["window:a"];
        assert_eq!((a.state.as_str(), a.at, a.msg.as_str()), ("working", 1794084018, "thinking"));
        assert_eq!(rows.live["window:b"], TabStatus { state: "idle".into(), at: 0, msg: String::new() });
        assert_eq!(rows.busy, 2);
    }

    #[test]
    fn folders_come_from_the_last_good_listing() {
        let mut s = state();
        let t0 = Instant::now();
        s.info.track("mac", &["mac".into(), "miller".into()], &[]);
        let stale = std::time::Duration::from_secs(60);
        assert_eq!(s.folders_reply("miller", t0, stale).0["error"], "miller: not listed yet");
        assert_eq!(s.folders_reply("nas", t0, stale).0["error"], "no folders kept for nas");
        s.info.folders.get_mut("miller").unwrap().set(Ok("/home/j/dev\n".into()), t0);
        let t1 = t0 + std::time::Duration::from_secs(61);
        let (reply, stale_now) = s.folders_reply("miller", t1, stale);
        assert_eq!((reply["folders"].as_str(), reply["age_ms"].as_u64(), stale_now), (Some("/home/j/dev\n"), Some(61000), true));
        // A failed listing is an error until the next good one.
        s.info.folders.get_mut("miller").unwrap().set(Err("couldn't reach miller".into()), t1);
        assert_eq!(s.folders_reply("miller", t1, stale).0["error"], "miller: couldn't reach miller");
        // Hosts the config drops go.
        s.info.track("mac", &["mac".into()], &[]);
        assert!(s.folders_reply("miller", t1, stale).0["error"].as_str().unwrap().starts_with("no folders"));
    }

    #[test]
    fn listings_are_due_when_old_wanted_or_new_but_not_twice() {
        let mut l: Listing<String> = Listing::default();
        let t0 = Instant::now();
        let every = std::time::Duration::from_secs(30);
        assert!(l.due(t0, every));
        l.running = true;
        assert!(!l.due(t0, every));
        l.set(Ok("x".into()), t0);
        assert!(!l.due(t0 + std::time::Duration::from_secs(29), every));
        assert!(l.due(t0 + every, every));
        l.wanted = true;
        assert!(l.due(t0, every));
    }

    #[test]
    fn probes_take_agents_and_sessions_from_the_rows() {
        let mut s = state();
        let now = Instant::now();
        s.info.track("mac", &[], &["miller".into(), "mac".into(), "drummer".into()]);
        s.info.metrics.get_mut("miller").unwrap().set(Ok(Metrics { load: 0.5, mem_gib: 30.0 }), now);
        s.info.metrics.get_mut("drummer").unwrap().set(Err("couldn't reach drummer".into()), now);
        s.set_rows("miller", split_rows(OUTPUT), now);
        s.set_rows("local", split_rows("S\t-\tlocal\ts\tother\nB\t0\n"), now);
        let reply = s.probes_reply("agents", now);
        let p = reply["probes"].as_array().unwrap();
        let names: Vec<_> = p.iter().map(|e| e["host"].as_str().unwrap()).collect();
        assert_eq!(names, ["drummer", "mac", "miller"]);
        assert_eq!(p[2]["load"], 0.5);
        assert_eq!((p[2]["agents"].as_u64(), p[2]["warm"].as_bool()), (Some(2), Some(true)));
        // The config host's rows are `local`'s.
        assert_eq!((p[1]["agents"].as_u64(), p[1]["warm"].as_bool()), (Some(0), Some(false)));
        assert!(p[1]["age_ms"].is_null() && p[1]["load"].is_null());
        // No rows read for drummer: nothing to say about its agents.
        assert_eq!(p[0]["error"], "couldn't reach drummer");
        assert!(p[0]["agents"].is_null() && p[0]["warm"].is_null());
    }

    fn state() -> State {
        let mut s = State::default();
        s.set_hosts(&["local".into(), "miller".into()]);
        s
    }

    #[test]
    fn unchanged_rows_refresh_the_age_without_a_change() {
        let mut s = state();
        let t0 = Instant::now();
        assert!(s.set_rows("local", split_rows(OUTPUT), t0));
        let t1 = t0 + std::time::Duration::from_secs(5);
        assert!(!s.set_rows("local", split_rows(OUTPUT), t1));
        assert_eq!(s.snapshot(t1)["hosts"][0]["age_ms"], 0);
        // A recovery clears the error, which clients see.
        assert!(s.set_error("local", "gone".into()));
        assert!(!s.set_error("local", "gone".into()));
        assert!(s.set_rows("local", split_rows(OUTPUT), t1));
        assert!(s.snapshot(t1)["hosts"][0]["error"].is_null());
        assert!(!s.set_rows("nas", Rows::default(), t1), "unknown hosts are ignored");
    }

    #[test]
    fn errors_keep_the_last_rows() {
        let mut s = state();
        let now = Instant::now();
        s.set_rows("miller", split_rows(OUTPUT), now);
        s.set_error("miller", "miller: connection lost".into());
        let snap = s.snapshot(now);
        assert_eq!(snap["hosts"][1]["error"], "miller: connection lost");
        assert!(snap["hosts"][1]["rows"].as_str().unwrap().starts_with("S\t"));
        assert!(snap["hosts"][0]["age_ms"].is_null());
    }

    #[test]
    fn set_hosts_keeps_known_hosts_in_the_new_order() {
        let mut s = state();
        s.set_rows("miller", split_rows(OUTPUT), Instant::now());
        s.set_hosts(&["local".into(), "drummer".into(), "miller".into()]);
        let snap = s.snapshot(Instant::now());
        let names: Vec<_> = snap["hosts"].as_array().unwrap().iter().map(|h| h["host"].as_str().unwrap()).collect();
        assert_eq!(names, ["local", "drummer", "miller"]);
        assert_ne!(snap["hosts"][2]["rows"], "");
    }

    #[test]
    fn tasks_get_live_status_from_the_rows() {
        let mut s = state();
        let now = Instant::now();
        let tasks = serde_json::json!([
            {"window_id": "window:a", "status": "done", "status_at": 1},
            {"window_id": "window:c", "status": "blocked", "status_at": 2},
            {"window_id": "window:b", "ended_at": 5},
        ]);
        assert!(s.set_tasks(Ok(tasks.as_array().unwrap().clone()), true, now));
        assert!(!s.set_tasks(Ok(tasks.as_array().unwrap().clone()), true, now), "the same tasks again");
        s.set_rows("local", split_rows(OUTPUT), now);
        let snap = s.snapshot(now);
        let t = &snap["tasks"];
        assert_eq!((t[0]["status"].as_str(), t[0]["status_msg"].as_str()), (Some("working"), Some("thinking")));
        // No listed tab: the reconcile's status stays.
        assert_eq!(t[1]["status"], "blocked");
        // Ended tasks get no status.
        assert!(t[2]["status"].is_null());
        assert_eq!(snap["tasks_checked"], true);
        // A failed read keeps the last list.
        assert!(s.set_tasks(Err("bad json".into()), false, now));
        let snap = s.snapshot(now);
        assert_eq!(snap["tasks_error"], "bad json");
        assert_eq!(snap["tasks"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn publish_counts_changes_and_drops_gone_subscribers() {
        let mut s = state();
        let (tx, rx) = mpsc::channel();
        let (gone, gone_rx) = mpsc::channel();
        drop(gone_rx);
        s.subscribe(tx);
        s.subscribe(gone);
        s.publish(Instant::now());
        let got: Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert_eq!(got["seq"], 1);
        assert_eq!(s.subscribers.len(), 1);
    }
}
