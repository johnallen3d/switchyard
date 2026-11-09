//! The task registry in the background. Checking tasks against their hosts
//! (core's `reconcile_tasks`: worktrees over SSH, tabs over Rex, ending the
//! gone ones) takes a few hundred milliseconds, so the server never does it
//! for a request. It runs when a watcher hears a tab, pane or session
//! close, when the registry file changes (a launch, an archive), and every
//! minute for what no event shows, such as a worktree removed by hand. A
//! client that has just archived, resumed or launched asks for one at once
//! (`refresh`), rather than waiting for the registry poll.
//! Between checks the tasks get live status from the watchers' rows.

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use switchyard_core::tasks::{load_tasks, reconcile_tasks, tasks_path};

use crate::state::Shared;

/// Checks with nothing to prompt them.
const EVERY: Duration = Duration::from_secs(60);

/// Wait this long after a trigger for the rest of its burst (closing a tab
/// closes its panes too; an archive closes the tab, then rewrites the
/// registry).
const SETTLE: Duration = Duration::from_millis(500);

/// How often to look at the registry file; std has no portable file events,
/// and a stat costs nothing.
const POLL: Duration = Duration::from_secs(1);

/// The registry file's modification time and size, to notice changes.
type Stamp = Option<(SystemTime, u64)>;

fn stamp(path: &Path) -> Stamp {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// Read the registry for a first answer, then reconcile on each trigger
/// from `triggers` (and the timer) until every sender is gone.
pub fn run(shared: Arc<Shared>, triggers: Receiver<&'static str>) {
    let read_at = Instant::now();
    shared.update(|s| s.set_tasks(load_tasks(), false, read_at));
    let mut why = "start";
    loop {
        let started = Instant::now();
        let got = reconcile_tasks();
        if let Err(e) = &got {
            crate::log(&format!("tasks ({why}): {e}"));
        }
        shared.update(|s| s.set_tasks(got, true, Instant::now()));
        if started.elapsed() > Duration::from_secs(5) {
            crate::log(&format!("tasks ({why}): checking took {:.1} s", started.elapsed().as_secs_f64()));
        }
        why = match triggers.recv_timeout(EVERY) {
            Ok(why) => why,
            Err(RecvTimeoutError::Timeout) => "timer",
            Err(RecvTimeoutError::Disconnected) => return,
        };
        // Let the burst finish, and fold it into this one check. A client's
        // refresh comes after its action has finished: check at once.
        let mut now = why == "refresh";
        while let Ok(w) = triggers.try_recv() {
            now |= w == "refresh";
        }
        if now {
            why = "refresh";
        } else {
            thread::sleep(SETTLE);
            while triggers.try_recv().is_ok() {}
        }
    }
}

/// Trigger a check when the registry file changes. A check that saves the
/// registry changes it too, which costs one more check that saves nothing.
pub fn watch_registry(trigger: Sender<&'static str>) {
    let path = tasks_path();
    let mut last = stamp(&path);
    loop {
        thread::sleep(POLL);
        let now = stamp(&path);
        if now != last {
            last = now;
            if trigger.send("registry").is_err() {
                return;
            }
        }
    }
}
