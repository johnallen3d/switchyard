//! The picker run bare from a shell (`rex-tab-picker`, or `rex-inbox` for
//! the Agents view), without the config's float.
//!
//! It gets what the float's fill script would paste, in the background (see
//! `Feed`): from switchyard-server when one answers, as pushed updates,
//! else directly, every few seconds: the rows from one `rex do -e` on this
//! host that runs host_rows.lua on every server (as the fill script does),
//! and the tasks from the registry through switchyard-core (live status
//! first, then the worktree check). It acts on Enter, ctrl-r, ctrl-l (open
//! the task's work item) and the "ask" confirm the way the config's
//! tab_picker mode does, through core's actions and `rex do
//! session.select`, then stays open, showing the outcome and the data after
//! it. Escape or ctrl-c closes it. The registry is on the Mac; elsewhere it
//! shows tabs only.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use switchyard_core::rex::{local_label, log, select};
use switchyard_core::tasks::{live_tasks, reconcile_tasks};
use switchyard_core::{actions, server};
use switchyard_ui::protocol::{DATA, unhex};

/// One server's rows: the script the float's tab-picker.lua reads.
const HOST_ROWS: &str = include_str!("../../../lua/switchyard/host_rows.lua");

/// Runs HOST_ROWS on this host's server and every server it knows, at
/// once, and returns their rows together, as the fill script does: a host
/// that fails gets an `E` row naming it, and only when every host fails is
/// it an error.
const ALL_ROWS: &str = r#"
local jobs = { { host = "local", job = rex.exec.start({ "rex", "do", "-e", host_rows_script, "host=local" }) } }
for _, host in ipairs(rex.servers()) do
  jobs[#jobs + 1] = { host = host, job = rex.exec.start({ "rex", "-S", host, "do", "-e", host_rows_script,
    "host=" .. host }) }
end
local rows, failed, listed = {}, nil, false
for _, j in ipairs(jobs) do
  local r = j.job:wait()
  if r.ok then
    listed = true
    rows[#rows + 1] = rex.json.decode(r.stdout)
  else
    local err = j.host .. ": " .. (r.stderr:gsub("%s+$", ""))
    failed = failed or err
    rows[#rows + 1] = "E\t" .. j.host .. "\t" .. err:gsub("[\t\n]", " ") .. "\n"
  end
end
if not listed then error(failed, 0) end
return table.concat(rows)
"#;

/// Clear the Rex session variables a shell in Rex carries, so rex CLI calls
/// name their own targets as the float's do. Call before any thread starts.
pub fn detach_env() {
    for var in ["REX_SERVER", "REX_CLIENT", "REX_SESSION", "REX_BLOCK"] {
        // SAFETY: called from main before any thread starts.
        unsafe { std::env::remove_var(var) };
    }
}

/// How long the direct path waits after a fetch before the next.
const REFRESH: Duration = Duration::from_secs(5);

/// How often to try for a server while fetching directly.
const RETRY: Duration = Duration::from_secs(5);

/// The picker's data in the background, as the float's pastes would carry
/// it (`rx`).
///
/// With switchyard-server answering, a subscription forwards each snapshot
/// it pushes (see core's `server::pastes`), so the picker changes when Rex
/// or the registry does and costs nothing in between. Without one, or once
/// it goes, the picker fetches directly, at once and every REFRESH after,
/// and tries for a server every RETRY. A snapshot carries the whole state,
/// so switching either way needs no catching up.
///
/// `poke` asks for the data now, after an action: from the server, one
/// snapshot (the action's own effects follow as pushes when the server
/// hears of them); directly, a fetch.
pub struct Feed {
    pub rx: Receiver<String>,
    poke: Sender<()>,
}

impl Feed {
    pub fn start() -> Feed {
        let (tx, rx) = mpsc::channel();
        let (poke, poked) = mpsc::channel();
        let on_server = Arc::new(AtomicBool::new(false));
        // Try the server before anything starts, so a running one fills the
        // picker without a direct fetch beside it.
        let first = server::subscribe().ok();
        on_server.store(first.is_some(), Ordering::Relaxed);
        {
            let (tx, poke, on_server) = (tx.clone(), poke.clone(), on_server.clone());
            thread::spawn(move || follow_server(first, tx, poke, on_server));
        }
        thread::spawn(move || fetch_when_needed(tx, poked, on_server));
        Feed { rx, poke }
    }

    pub fn poke(&self) {
        let _ = self.poke.send(());
    }
}

/// Forward the server's snapshots for as long as it answers, then say so
/// (a poke, which starts a direct fetch at once) and try again every RETRY.
fn follow_server(mut sub: Option<server::Subscription>, tx: Sender<String>, poke: Sender<()>, on_server: Arc<AtomicBool>) {
    loop {
        if let Some(sub) = sub.take() {
            on_server.store(true, Ordering::Relaxed);
            for snap in sub {
                let Ok(snap) = snap else { break };
                if !send_pastes(&tx, &snap) {
                    return;
                }
            }
            on_server.store(false, Ordering::Relaxed);
            if poke.send(()).is_err() {
                return;
            }
        }
        thread::sleep(RETRY);
        sub = server::subscribe().ok();
    }
}

/// Send a snapshot's pastes; false once the picker has gone.
fn send_pastes(tx: &Sender<String>, snap: &serde_json::Value) -> bool {
    let mut pastes = server::pastes(snap);
    if !cfg!(target_os = "macos") {
        // A server elsewhere has no registry worth showing.
        pastes.retain(|(kind, _)| !matches!(*kind, "live" | "tasks" | "tasks-error"));
        pastes.push(("tasks-error", NO_REGISTRY.into()));
    }
    pastes.into_iter().all(|(kind, body)| tx.send(format!("{DATA}{kind}\n{body}")).is_ok())
}

const NO_REGISTRY: &str = "the task registry is on the Mac";

/// Fetch directly whenever the server isn't answering: at once, after
/// REFRESH and on a poke. With it answering, a poke gets one snapshot.
fn fetch_when_needed(tx: Sender<String>, poked: Receiver<()>, on_server: Arc<AtomicBool>) {
    let mut due = !on_server.load(Ordering::Relaxed);
    loop {
        if due {
            let sent = if on_server.load(Ordering::Relaxed) {
                // Should the server have just gone, follow_server's poke
                // fetches directly.
                server::snapshot().map_or(true, |snap| send_pastes(&tx, &snap))
            } else {
                fetch(&tx)
            };
            if !sent {
                return;
            }
        }
        due = match poked.recv_timeout(REFRESH) {
            Ok(()) => {
                // One fetch covers a burst of pokes.
                while poked.try_recv().is_ok() {}
                true
            }
            Err(RecvTimeoutError::Timeout) => !on_server.load(Ordering::Relaxed),
            Err(RecvTimeoutError::Disconnected) => return,
        };
    }
}

/// Fetch the rows and tasks directly, at once, sending each as it comes;
/// false once the picker has gone.
fn fetch(tx: &Sender<String>) -> bool {
    let rows_tx = tx.clone();
    let rows = thread::spawn(move || {
        rows_tx
            .send(match all_rows() {
                Ok(rows) => format!("{DATA}rows\n{rows}"),
                Err(e) => format!("{DATA}error\n{e}"),
            })
            .is_ok()
    });
    let tasks = fetch_tasks(tx);
    rows.join().unwrap_or(false) && tasks
}

fn fetch_tasks(tx: &Sender<String>) -> bool {
    if !cfg!(target_os = "macos") {
        return tx.send(format!("{DATA}tasks-error\n{NO_REGISTRY}")).is_ok();
    }
    for kind in ["live", "tasks"] {
        let got = if kind == "live" { live_tasks() } else { reconcile_tasks() };
        let (msg, failed) = match got {
            Ok(tasks) => (format!("{DATA}{kind}\n{}", serde_json::Value::from(tasks)), false),
            Err(e) => (format!("{DATA}tasks-error\n{e}"), true),
        };
        if tx.send(msg).is_err() {
            return false;
        }
        if failed {
            break;
        }
    }
    true
}

fn all_rows() -> Result<String, String> {
    let script = format!("local host_rows_script = [=====[\n{HOST_ROWS}]=====]\n{ALL_ROWS}");
    let out = std::process::Command::new("rex")
        .args(["do", "-e", &script])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("rex: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    serde_json::from_slice::<String>(&out.stdout).map_err(|e| format!("rex do: {e}"))
}

/// A pick from the title protocol: ACTION, host, session and window.
pub struct Pick {
    pub action: String,
    pub host: String,
    pub session: String,
    pub window: String,
}

pub fn parse_pick(pick: &str) -> Option<Pick> {
    let (action, target) = pick.split_once(':')?;
    // A fourth field, the count of links ctrl-l chooses from, is for the
    // config's mode.
    let mut parts = target.split('/');
    let (host, session, window) = (parts.next()?, parts.next()?, parts.next()?);
    Some(Pick {
        action: action.to_string(),
        host: if host == "-" { local_label() } else { unhex(host) },
        session: session.to_string(),
        window: window.to_string(),
    })
}

/// Show the pick's tab.
pub fn go(p: &Pick) -> Result<(), String> {
    select(&p.host, &p.session, &p.window)
}

/// Archive (or force-archive) the pick's task; Ok is the outcome to show.
pub fn archive(action: &str, p: &Pick) -> Result<String, String> {
    logged(action, actions::act(action, &p.host, &p.session, &p.window))
}

/// Open lazygit beside the task's agent, then show its tab.
pub fn review(p: &Pick) -> Result<(), String> {
    logged("review", actions::act("review", &p.host, &p.session, &p.window))?;
    go(p)
}

/// Resume an ended task in a new tab and show it.
pub fn resume(p: &Pick) -> Result<(), String> {
    let out = logged("resume", actions::act("resume", &p.host, &p.session, &p.window))?;
    let mut f = out.trim_end().splitn(3, '\t');
    match (f.next(), f.next(), f.next()) {
        (Some(h), Some(s), Some(w)) => select(h, s, w),
        _ => Err(format!("resume printed no tab: {out}")),
    }
}

/// Open the pick's task's main work item in the browser here (the
/// standalone picker runs where John is); Ok is the URL.
pub fn open_link(p: &Pick) -> Result<String, String> {
    let url = actions::act("link", &p.host, &p.session, &p.window)?.trim().to_string();
    open_url(&url)?;
    Ok(url)
}

/// Open `url` in the browser here.
pub fn open_url(url: &str) -> Result<(), String> {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let status = std::process::Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("{opener}: {e}"))?;
    if !status.success() {
        return Err(format!("{opener} {url} failed ({status})"));
    }
    Ok(())
}

/// Log an action's outcome to the state log, as `rex-spawn --act` does.
fn logged(action: &str, r: Result<String, String>) -> Result<String, String> {
    match &r {
        Ok(out) => log(out),
        Err(e) => log(&format!("{action} failed: {e}")),
    }
    r
}
