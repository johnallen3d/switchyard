//! One watcher per Rex server: a long-lived `rex [-S HOST] do --all -e
//! SCRIPT` that holds one connection to the server and hears every
//! session's events. Its Lua runs here, on the server's machine, and is
//! host_rows.lua (the row logic, kept in one place) wrapped in a
//! function it calls at start, shortly after relevant events, and on a slow
//! timer in case an event is missed. Each run prints the host's rows as one
//! line; closes and host list changes print a line of their own, for the
//! reconcile and the host list.
//!
//! The supervisor here restarts a watcher that exits or goes quiet, backing
//! off while its host stays unreachable, and records why in the state.

use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::state::{Shared, split_rows};

/// One server's rows (see host_rows.lua).
const HOST_ROWS: &str = include_str!("../../../lua/switchyard/host_rows.lua");

/// Seconds between runs when no event says the rows changed.
const SAFETY_SECS: u64 = 30;

/// A watcher that prints nothing for this long has lost its connection
/// without noticing (the safety timer prints every SAFETY_SECS).
const QUIET: Duration = Duration::from_secs(3 * SAFETY_SECS + 10);

/// Lines a watcher prints start with this, then a kind and a JSON string,
/// tab separated; rex do's own lines (such as "listening: ...") don't.
const PREFIX: &str = "switchyard\t";

/// The rest of the watcher's Lua, after host_rows.lua as `host_rows`.
/// Events come in bursts (a layout change, an agent updating its status),
/// so a run waits a moment for the rest: 0.1 s for changes to statuses,
/// tabs and sessions, 0.5 s for titles, folders and layout, which change
/// often while agents work and matter less. A pending run isn't pushed back
/// by later events, so a steady stream can't starve it.
///
/// A watcher whose server is killed outright (no chance to stop its
/// watchers, see main) notices at its next write, within SAFETY_SECS.
const WATCH: &str = r#"
-- A failed write means the server is gone (even killed outright), so stop.
local function emit(kind, body)
  local ok = io.write("switchyard\t", kind, "\t", rex.json.encode(body), "\n") and io.stdout:flush()
  if not ok then rex.stop(1) end
end

local fast, slow
local function refresh()
  if fast ~= nil then fast:cancel(); fast = nil end
  if slow ~= nil then slow:cancel(); slow = nil end
  local ok, rows = pcall(host_rows)
  if ok then emit("rows", rows) else emit("error", tostring(rows)) end
end
local function soon()
  if fast == nil then fast = rex.after(0.1, function() fast = nil; refresh() end) end
end
local function later()
  if slow == nil and fast == nil then slow = rex.after(0.5, function() slow = nil; refresh() end) end
end

for _, name in ipairs({ "terminal.program_status_changed", "terminal.program_status_removed", "window_created",
  "window_label_changed", "block_created", "session_created", "session_label_changed" }) do
  rex.on(name, soon)
end
for _, name in ipairs({ "terminal.title_changed", "terminal.pwd_changed", "session_view_changed" }) do
  rex.on(name, later)
end
-- A close can end a task, so the server checks the registry.
for _, name in ipairs({ "window_closed", "block_closed", "session_destroyed" }) do
  rex.on(name, function() emit("closed", name); soon() end)
end
rex.on("hosts_changed", function() emit("hosts", "") end)
rex.every(SAFETY_SECS, refresh)
refresh()
"#;

pub fn script() -> String {
    let watch = WATCH.replace("SAFETY_SECS", &SAFETY_SECS.to_string());
    format!("local function host_rows()\n{HOST_ROWS}\nend\n{watch}")
}

/// Running watchers' PIDs (0 is a free slot), for `stop_all`: a signal
/// handler can't take a lock.
static PIDS: [AtomicI32; 64] = [const { AtomicI32::new(0) }; 64];

/// A watcher's slot in PIDS, freed on drop.
struct Slot(Option<&'static AtomicI32>);

impl Slot {
    fn take(pid: u32) -> Slot {
        let pid = pid as i32;
        Slot(PIDS.iter().find(|p| p.compare_exchange(0, pid, Ordering::SeqCst, Ordering::SeqCst).is_ok()))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let Some(p) = self.0 {
            p.store(0, Ordering::SeqCst);
        }
    }
}

/// End every running watcher. Async-signal-safe.
pub fn stop_all() {
    for p in &PIDS {
        let pid = p.load(Ordering::SeqCst);
        if pid > 0 {
            // SAFETY: kill is async-signal-safe.
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
    }
}

/// A line a watcher printed.
#[derive(Debug, PartialEq)]
pub enum Line {
    Rows(String),
    Error(String),
    Closed(String),
    Hosts,
    /// Anything else, such as rex do's "listening" line.
    Other,
}

pub fn parse_line(line: &str) -> Line {
    let Some((kind, body)) = line.strip_prefix(PREFIX).and_then(|l| l.split_once('\t')) else {
        return Line::Other;
    };
    let body = serde_json::from_str::<String>(body).unwrap_or_default();
    match kind {
        "rows" => Line::Rows(body),
        "error" => Line::Error(body),
        "closed" => Line::Closed(body),
        "hosts" => Line::Hosts,
        _ => Line::Other,
    }
}

/// The wait before restarting a watcher that ran for `ran`, after waiting
/// `last`: back to a second after a good run, otherwise twice the last wait,
/// up to a minute, so an unreachable host costs little.
pub fn backoff(last: Duration, ran: Duration) -> Duration {
    const MIN: Duration = Duration::from_secs(1);
    const MAX: Duration = Duration::from_secs(60);
    if ran >= MAX { MIN } else { (last * 2).clamp(MIN, MAX) }
}

/// Where a watcher's events go besides the state.
#[derive(Clone)]
pub struct Events {
    /// Closes, for the reconcile.
    pub closed: Sender<&'static str>,
    /// Host list changes.
    pub hosts: Sender<()>,
}

/// Keep `host`'s watcher running until `stop` is set. `host` is `local` for
/// this machine's server, else a `rex.servers()` label.
pub fn supervise(shared: Arc<Shared>, host: String, events: Events, stop: Arc<AtomicBool>) {
    let script = script();
    let mut wait = Duration::ZERO;
    while !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        match run(&shared, &host, &script, &events, &stop) {
            Ok(()) => break,
            Err(e) => {
                crate::log(&format!("{host}: {e}"));
                shared.update(|s| s.set_watching(&host, false) | s.set_error(&host, e));
            }
        }
        wait = backoff(wait, started.elapsed());
        let until = Instant::now() + wait;
        while Instant::now() < until && !stop.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(200));
        }
    }
    shared.update(|s| s.set_watching(&host, false));
}

/// Run one watcher until it exits (Err with why) or `stop` is set (Ok).
fn run(shared: &Arc<Shared>, host: &str, script: &str, events: &Events, stop: &AtomicBool) -> Result<(), String> {
    let mut cmd = Command::new("rex");
    if host != "local" {
        cmd.args(["-S", host]);
    }
    let mut child = cmd
        .args(["do", "--all", "-e", script, &format!("host={host}"), "status=yes"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("can't start rex: {e}"))?;
    let slot = Slot::take(child.id());
    shared.update(|s| s.set_watching(host, true));
    let heard = Arc::new(Mutex::new(Instant::now()));
    let stdout = child.stdout.take().expect("piped stdout");
    let reader = {
        let (shared, host, events, heard) = (shared.clone(), host.to_string(), events.clone(), heard.clone());
        thread::spawn(move || read_lines(stdout, &shared, &host, &events, &heard))
    };
    // The last thing rex said on stderr explains an exit.
    let mut stderr = child.stderr.take().expect("piped stderr");
    let complaint = thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string()
    });
    let outcome = watch_child(&mut child, &heard, stop);
    // Reaped: its PID can be reused now.
    drop(slot);
    let _ = reader.join();
    let complaint = complaint.join().unwrap_or_default();
    match outcome {
        Exit::Stopped => Ok(()),
        Exit::Quiet => Err(format!("no word from the Rex server for {} s", QUIET.as_secs())),
        Exit::Exited(status) if complaint.is_empty() => Err(format!("watcher exited ({status})")),
        Exit::Exited(_) => Err(complaint),
    }
}

enum Exit {
    Stopped,
    Quiet,
    Exited(std::process::ExitStatus),
}

fn watch_child(child: &mut Child, heard: &Mutex<Instant>, stop: &AtomicBool) -> Exit {
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Exit::Exited(status);
        }
        let quiet = heard.lock().map(|at| at.elapsed() > QUIET).unwrap_or(false);
        if quiet || stop.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return if quiet { Exit::Quiet } else { Exit::Stopped };
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn read_lines(stdout: impl Read, shared: &Shared, host: &str, events: &Events, heard: &Mutex<Instant>) {
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        if let Ok(mut at) = heard.lock() {
            *at = Instant::now();
        }
        match parse_line(&line) {
            Line::Rows(rows) => shared.update(|s| s.set_rows(host, split_rows(&rows), Instant::now())),
            Line::Error(e) => shared.update(|s| s.set_error(host, e)),
            Line::Closed(_) => {
                let _ = events.closed.send("close");
            }
            Line::Hosts => {
                let _ = events.hosts.send(());
            }
            Line::Other => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_watcher_lines() {
        assert_eq!(parse_line("switchyard\trows\t\"S\\tx\\n\""), Line::Rows("S\tx\n".into()));
        assert_eq!(parse_line("switchyard\terror\t\"miller: gone\""), Line::Error("miller: gone".into()));
        assert_eq!(parse_line("switchyard\tclosed\t\"window_closed\""), Line::Closed("window_closed".into()));
        assert_eq!(parse_line("switchyard\thosts\t\"\""), Line::Hosts);
        assert_eq!(parse_line("listening: 12 handlers, every session; Ctrl-C to stop"), Line::Other);
        assert_eq!(parse_line("switchyard\tnew-kind\t\"\""), Line::Other);
    }

    #[test]
    fn backoff_doubles_to_a_minute_and_resets_after_a_good_run() {
        let s = Duration::from_secs;
        assert_eq!(backoff(Duration::ZERO, s(0)), s(1));
        assert_eq!(backoff(s(1), s(2)), s(2));
        assert_eq!(backoff(s(40), s(2)), s(60));
        assert_eq!(backoff(s(60), s(600)), s(1));
    }

    #[test]
    fn script_wraps_host_rows_in_a_function() {
        let s = script();
        assert!(s.starts_with("local function host_rows()\n"));
        assert!(s.contains("rex.every(30, refresh)"));
        // host_rows.lua's final return ends the function, not the script.
        assert!(HOST_ROWS.trim_end().ends_with(r#"return table.concat(lines, "\n") .. "\n""#));
        assert!(s.contains(&format!("{HOST_ROWS}\nend\n")));
    }
}
