//! The picker's data and actions (bare `switchyard`).
//!
//! It gets its data as pastes, in the background (see
//! `Feed`): from switchyard-server when one answers, as pushed updates,
//! else directly, every few seconds: the rows from one `rex do -e` on this
//! host that runs `host_rows.lua` on every server,
//! and the tasks from the registry through switchyard-core (live status
//! first, then the worktree check), and the highlighted agent's transcript
//! for the preview (see preview.rs), or its changes (see changes.rs). It acts on Enter, ctrl-b's work
//! item actions (see items.rs), ctrl-r
//! (pin), ctrl-t (review in tuicr, in a float), the snooze prompt's Enter and the
//! "ask" confirm through core's actions and `rex do session.select`, then
//! stays open, showing the outcome and the data after
//! it; closing its tab quits it. The registry is on the Mac; elsewhere it
//! shows tabs only.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use switchyard_core::item_actions::{self, Action, Item};
use switchyard_core::rex::{local_label, log, select};
use switchyard_core::tasks::{live_tasks, reconcile_tasks};
use switchyard_core::{actions, server};
use switchyard_ui::protocol::{DATA, unhex};

use crate::{changes, preview};

/// One server's rows (see `host_rows.lua`).
const HOST_ROWS: &str = include_str!("../../../lua/switchyard/host_rows.lua");

/// Runs `HOST_ROWS` on this host's server and every server it knows, at
/// once, and returns their rows together: a host
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

/// The picker's own Rex session, kept from `REX_SESSION` before
/// `detach_env` clears it, for the review float to open over.
static OWN_SESSION: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

/// Clear the Rex session variables a shell in Rex carries, so rex CLI calls
/// name their own targets, keeping the picker's own session first. Call
/// before any thread starts.
pub fn detach_env() {
    let _ = OWN_SESSION.set(std::env::var("REX_SESSION").ok().filter(|s| !s.is_empty()));
    for var in ["REX_SERVER", "REX_CLIENT", "REX_SESSION", "REX_BLOCK"] {
        // SAFETY: called from main before any thread starts.
        unsafe { std::env::remove_var(var) };
    }
}

/// How long the direct path waits after a fetch before the next.
const REFRESH: Duration = Duration::from_secs(5);

/// How often to try for a server while fetching directly.
const RETRY: Duration = Duration::from_secs(5);

/// The picker's data in the background, as data pastes (`rx`).
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
    /// The tab whose transcript to fetch for the preview (see preview.rs).
    preview: Sender<Option<preview::Pane>>,
    /// The tab whose changes to fetch for the preview (see changes.rs).
    changes: Sender<Option<preview::Pane>>,
}

impl Feed {
    /// `quick` (the cmd+k float) wants only the rows and tasks: no usage
    /// accounting, and no transcripts, as it has no preview.
    pub fn start(quick: bool) -> Feed {
        let (tx, rx) = mpsc::channel();
        let (poke, poked) = mpsc::channel();
        let on_server = Arc::new(AtomicBool::new(false));
        // Try the server before anything starts, so a running one fills the
        // picker without a direct fetch beside it.
        let first = server::subscribe().ok();
        on_server.store(first.is_some(), Ordering::Relaxed);
        {
            let (tx, poke, on_server) = (tx.clone(), poke.clone(), on_server.clone());
            thread::spawn(move || follow_server(first, &tx, &poke, &on_server));
        }
        let (preview, wants) = mpsc::channel();
        let (changes, changes_wanted) = mpsc::channel();
        if !quick {
            let usage_tx = tx.clone();
            thread::spawn(move || {
                let mut collector = switchyard_core::usage::Collector::default();
                loop {
                    let body =
                        serde_json::to_string(&collector.collect().summary()).unwrap_or_default();
                    if usage_tx.send(format!("{DATA}usage\n{body}")).is_err() {
                        return;
                    }
                    thread::sleep(switchyard_core::usage::REFRESH);
                }
            });
            let preview_tx = tx.clone();
            thread::spawn(move || preview::follow(&wants, &preview_tx));
            let changes_tx = tx.clone();
            thread::spawn(move || changes::follow(&changes_wanted, &changes_tx));
        }
        thread::spawn(move || fetch_when_needed(&tx, &poked, &on_server));
        Feed {
            rx,
            poke,
            preview,
            changes,
        }
    }

    pub fn poke(&self) {
        let _ = self.poke.send(());
    }

    /// Fetch this tab's transcript for the preview from now on (None: none).
    pub fn preview(&self, want: Option<preview::Pane>) {
        let _ = self.preview.send(want);
    }

    /// Fetch this tab's changes for the preview from now on (None: none).
    pub fn changes(&self, want: Option<preview::Pane>) {
        let _ = self.changes.send(want);
    }
}

/// Forward the server's snapshots for as long as it answers, then say so
/// (a poke, which starts a direct fetch at once) and try again every RETRY.
fn follow_server(
    mut sub: Option<server::Subscription>,
    tx: &Sender<String>,
    poke: &Sender<()>,
    on_server: &AtomicBool,
) {
    loop {
        if let Some(sub) = sub.take() {
            on_server.store(true, Ordering::Relaxed);
            for snap in sub {
                let Ok(snap) = snap else { break };
                if !send_pastes(tx, &snap) {
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
    pastes
        .into_iter()
        .all(|(kind, body)| tx.send(format!("{DATA}{kind}\n{body}")).is_ok())
}

const NO_REGISTRY: &str = "the task registry is on the Mac";

/// Fetch directly whenever the server isn't answering: at once, after
/// REFRESH and on a poke. With it answering, a poke gets one snapshot.
fn fetch_when_needed(tx: &Sender<String>, poked: &Receiver<()>, on_server: &AtomicBool) {
    let mut due = !on_server.load(Ordering::Relaxed);
    loop {
        if due {
            let sent = if on_server.load(Ordering::Relaxed) {
                // Should the server have just gone, follow_server's poke
                // fetches directly.
                server::snapshot().map_or(true, |snap| send_pastes(tx, &snap))
            } else {
                fetch(tx)
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
        let got = if kind == "live" {
            live_tasks()
        } else {
            reconcile_tasks()
        };
        let (msg, failed) = match got {
            Ok(tasks) => (
                format!("{DATA}{kind}\n{}", serde_json::Value::from(tasks)),
                false,
            ),
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

/// A pick (see the crate docs): ACTION, host, session and window.
pub struct Pick {
    pub action: String,
    pub host: String,
    pub session: String,
    pub window: String,
}

/// The window is the rest of the pick: an orphan worktree's
/// (`orphan:HOST:PATH`, see core's `orphans`) holds slashes.
pub fn parse_pick(pick: &str) -> Option<Pick> {
    let (action, target) = pick.split_once(':')?;
    let mut parts = target.splitn(3, '/');
    let (host, session, window) = (parts.next()?, parts.next()?, parts.next()?);
    Some(Pick {
        action: action.to_string(),
        host: if host == "-" {
            local_label()
        } else {
            unhex(host)
        },
        session: session.to_string(),
        window: window.to_string(),
    })
}

/// Show the pick's tab.
pub fn go(p: &Pick) -> Result<(), String> {
    select(&p.host, &p.session, &p.window)
}

/// Archive (or force-archive) the pick's task and the open tasks it
/// spawned; Ok is the outcome to show.
pub fn archive(action: &str, p: &Pick) -> Result<String, String> {
    logged(
        action,
        actions::archive_family(action, &p.host, &p.session, &p.window),
    )
}

/// Pin or unpin the pick's task; Ok is the outcome to show.
pub fn pin(p: &Pick) -> Result<String, String> {
    logged("pin", actions::act("pin", &p.host, &p.session, &p.window))
}

/// Snooze the pick's task until `until` (Unix seconds), or wake it for 0.
pub fn snooze(p: &Pick, until: u64) -> Result<String, String> {
    logged("snooze", actions::snooze(&p.window, until))
}

/// Send `text` to the agent in the pick's tab, as if typed there.
pub fn message(p: &Pick, text: &str) -> Result<String, String> {
    logged(
        "message",
        actions::message(&p.host, &p.session, &p.window, text),
    )
}

/// Interrupt the pick's agent; Ok is the outcome to show.
pub fn interrupt(p: &Pick) -> Result<String, String> {
    logged(
        "interrupt",
        actions::interrupt(&p.host, &p.session, &p.window),
    )
}

/// Resume an ended task in a new tab and show it.
pub fn resume(p: &Pick) -> Result<(), String> {
    let (host, session, window) = resume_here(p)?;
    select(&host, &session, &window)
}

/// Resume an ended task in a new tab without showing it. Ok is where it
/// opened: host, session and window.
pub fn resume_here(p: &Pick) -> Result<(String, String, String), String> {
    let out = logged(
        "resume",
        actions::act("resume", &p.host, &p.session, &p.window),
    )?;
    let mut fields = out.trim_end().splitn(3, '\t');
    match (fields.next(), fields.next(), fields.next()) {
        (Some(host), Some(session), Some(window)) => {
            Ok((host.to_string(), session.to_string(), window.to_string()))
        }
        _ => Err(format!("resume printed no tab: {out}")),
    }
}

/// Start a session from work item `url` in the catalog project at `path`:
/// auto host, claude, named from the item (see core's `launch`). Ok is
/// where it opened: host, session and window.
pub fn launch(path: &str, url: &str) -> Result<(String, String, String), String> {
    let r = switchyard_core::launch::launch_text(
        "auto",
        path,
        "",
        // The project's profile default.
        "",
        "",
        Some(url),
        switchyard_core::launch::Placement::default(),
    );
    match &r {
        Ok((host, _, window)) => log(&format!("started work on {url} on {host} ({window})")),
        Err(e) => log(&format!("launch from {url} failed: {e}")),
    }
    r
}

/// Open `url` in the browser here.
pub fn open_url(url: &str) -> Result<(), String> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
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

/// Opens the review float: a floating layer over most of the picker's
/// session (`session`), running `command` (a JSON array) in a pane
/// labeled `label`. A script attaches to the session first, as only an
/// attached connection may add to it.
const REVIEW_FLOAT: &str = r#"
local session = rex.args.session
-- rex do decodes a JSON argument itself; a string is decoded here.
local command = rex.args.command
if type(command) == "string" then command = rex.json.decode(command) end
if type(command) ~= "table" then error("no command for the review float", 0) end
local _, attach_err = rex.session.attach{ session_id = session }
if attach_err ~= nil then error(attach_err, 0) end
local _, err = rex.session.new_layer{
  session_id = session,
  bounds     = { 0.03, 0.03, 0.94, 0.94 },
  focus      = true,
  layout     = rex.layout.block{
    flavor  = "com.superlogical.terminal.shell",
    label   = rex.args.label,
    options = { command = command },
  },
}
if err ~= nil then error(err, 0) end
"#;

/// Open a float over the picker's own tab running `switchyard review
/// ARGS...` (see the CLI's review.rs), with this build's binary, so the
/// review is the picker's own version. It closes when the review ends.
///
/// # Errors
///
/// Fails outside Rex, or when Rex refuses the float.
pub fn review_float(args: &[&str]) -> Result<(), String> {
    let session = OWN_SESSION
        .get()
        .cloned()
        .flatten()
        .or_else(|| std::env::var("REX_SESSION").ok().filter(|s| !s.is_empty()))
        .ok_or("reviews open in a float over the picker's Rex tab")?;
    let exe = std::env::current_exe().map_err(|e| format!("can't find switchyard: {e}"))?;
    let mut command = vec![
        "sh".to_string(),
        "-c".to_string(),
        format!(
            "{}; exec \"$0\" \"$@\"",
            switchyard_core::review::PATH_SETUP
        ),
        exe.display().to_string(),
        "review".to_string(),
    ];
    command.extend(args.iter().map(ToString::to_string));
    let command = serde_json::to_string(&command).map_err(|e| e.to_string())?;
    let out = std::process::Command::new("rex")
        .args(["do", "-e", REVIEW_FLOAT])
        .arg(format!("session={session}"))
        .arg(format!("label={}", switchyard_core::review::LABEL))
        .arg(format!("command={command}"))
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("rex: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "can't open the review float: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// Fetch a linked item's description and comments to read in place
/// (see core's `item_view`), logging a failure.
pub fn item_thread(
    link: &crate::model::Link,
    profile: &str,
) -> Result<switchyard_core::item_view::Thread, String> {
    let r = switchyard_core::item_view::fetch(
        &link.kind,
        &link.system,
        &link.reference,
        &link.url,
        profile,
    );
    if let Err(e) = &r {
        log(&format!("read failed: {e}"));
    }
    r
}

/// Run a work item action from the ctrl-b float (see core's
/// `item_actions`); after it, forget the item's cached state (a marked PR,
/// a closed card) and have switchyard-server check again.
pub fn item_action(
    action: Action,
    item: &Item,
    body: &str,
    profile: &str,
    url: &str,
) -> Result<String, String> {
    let r = logged(
        action.verb(),
        item_actions::run(action, item, body, profile),
    );
    if r.is_ok() {
        match (action, item) {
            (Action::Ready | Action::Approve | Action::Merge, _) => {
                switchyard_core::links::forget_pr(url);
            }
            // A closed or postponed card's column and state are cached for
            // a few minutes (see core's cards); the next refresh should ask
            // Fizzy at once.
            (Action::Close | Action::Postpone, Item::Card { account, number }) => {
                switchyard_core::cards::forget(account, number);
            }
            _ => {}
        }
        server::refresh();
    }
    r
}

/// Log an action's outcome to the state log, as `switchyard task act` does.
fn logged(action: &str, r: Result<String, String>) -> Result<String, String> {
    match &r {
        Ok(out) => log(out),
        Err(e) => log(&format!("{action} failed: {e}")),
    }
    r
}
