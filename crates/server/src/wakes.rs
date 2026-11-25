//! What agents wait on to resume them. A Claude agent that sets a timer
//! (`ScheduleWakeup`) or leaves a background job running (often a `sleep`)
//! ends its turn and reports `idle` or `done` like any other, and Rex's
//! status protocol has no state for "resumes by itself later". Its session
//! log says, though (see core's `transcript::wake`), so the server reads it
//! once each time an open task's tab settles: when its status becomes idle
//! or done, keyed by that status's time, so the log is read again only
//! after the next change. A task with one pending shows as `scheduled`,
//! with `wake_at` (Unix seconds) when its time is known and its reason as
//! the message, in snapshots (see `apply`).
//!
//! The same read gives the session's own title (see core's
//! `transcript::title`), kept until a later read finds another and shown as
//! the task's `title`: what the agent says the work is, often clearer than
//! its name.
//!
//! The lookup asks the tab's Rex server for its agent pane and reads the
//! log there, over SSH for another host, so it runs on a thread of its own
//! and never under the lock.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use switchyard_core::tasks::TabStatus;
use switchyard_core::transcript;

use crate::state::Shared;

/// A timer that went off this long ago without the agent waking (its
/// status would have changed) is taken as gone: the agent was interrupted
/// or exited.
const STALE_SECS: u64 = 3600;

/// What the log said for a tab's status at `at`: when the agent resumes (0
/// when unknown) and why, or None (nothing pending, or the lookup is still
/// running or failed); and the session's title, from any read.
#[derive(Clone, Debug, PartialEq)]
pub struct Wake {
    pub at: u64,
    pub pending: Option<(u64, String)>,
    pub title: Option<String>,
}

/// A log to read: the tab's window, its status time, and where it is.
pub struct Job {
    window: String,
    at: u64,
    host: String,
    session: String,
}

fn settled(status: &TabStatus) -> bool {
    matches!(status.state.as_str(), "idle" | "done") && status.at > 0
}

fn is_claude(task: &Value) -> bool {
    matches!(task["harness"].as_str(), None | Some("" | "claude"))
}

/// The open tasks whose tab has settled since its log was last read,
/// marked as looked up so they aren't queued twice. Entries for tabs no
/// open task has are dropped.
pub fn due(
    waits: &mut HashMap<String, Wake>,
    tasks: &[Value],
    live: &HashMap<String, TabStatus>,
) -> Vec<Job> {
    let field = |t: &Value, k: &str| t[k].as_str().unwrap_or("").to_string();
    let open: Vec<&Value> = tasks.iter().filter(|t| t["ended_at"].is_null()).collect();
    waits.retain(|window, w| {
        let Some(task) = open
            .iter()
            .find(|t| t["window_id"].as_str() == Some(window))
        else {
            return false;
        };
        if !is_claude(task) {
            w.pending = None;
        }
        true
    });
    let mut jobs = Vec::new();
    for task in open.into_iter().filter(|t| is_claude(t)) {
        let window = field(task, "window_id");
        let Some(status) = live.get(&window).filter(|s| settled(s)) else {
            continue;
        };
        if waits.get(&window).is_some_and(|w| w.at == status.at) {
            continue;
        }
        let title = waits.get(&window).and_then(|w| w.title.clone());
        waits.insert(
            window.clone(),
            Wake {
                at: status.at,
                pending: None,
                title,
            },
        );
        jobs.push(Job {
            window,
            at: status.at,
            host: field(task, "host"),
            session: field(task, "session_id"),
        });
    }
    jobs
}

/// Read each job's log on a thread of its own and record what it says.
pub fn start(shared: &Arc<Shared>, jobs: Vec<Job>) {
    for job in jobs {
        let shared = shared.clone();
        thread::spawn(move || {
            let v = transcript::fetch_on(&job.host, &job.session, &job.window);
            let w = &v["wake"];
            let wake = w.is_object().then(|| {
                (
                    w["at"].as_u64().unwrap_or(0),
                    w["reason"].as_str().unwrap_or("").to_string(),
                )
            });
            let title = v["title"].as_str().map(String::from);
            shared.update(|s| {
                s.set_wake(&job.window, job.at, wake) | s.set_title(&job.window, title)
            });
        });
    }
}

/// Give each open task its session's title, when a read found one, and
/// mark each whose tab is still in the status its log was read for, with
/// something pending, as `scheduled`: `wake_at` when known, and its reason
/// as `status_msg` when it has one. Run after `apply_live`.
pub fn apply(tasks: &mut [Value], waits: &HashMap<String, Wake>) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    for task in tasks.iter_mut().filter(|t| t["ended_at"].is_null()) {
        let Some(w) = task["window_id"].as_str().and_then(|w| waits.get(w)) else {
            continue;
        };
        if let Some(title) = &w.title {
            task["title"] = title.clone().into();
        }
        if !is_claude(task) {
            continue;
        }
        let Wake {
            at,
            pending: Some((wake_at, reason)),
            ..
        } = w
        else {
            continue;
        };
        let status = TabStatus {
            state: task["status"].as_str().unwrap_or("").to_string(),
            at: task["status_at"].as_u64().unwrap_or(0),
            msg: String::new(),
        };
        if !settled(&status) || status.at != *at || (*wake_at > 0 && wake_at + STALE_SECS < now) {
            continue;
        }
        task["status"] = "scheduled".into();
        if *wake_at > 0 {
            task["wake_at"] = (*wake_at).into();
        }
        if !reason.is_empty() {
            task["status_msg"] = reason.clone().into();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn live(state: &str, at: u64) -> HashMap<String, TabStatus> {
        HashMap::from([(
            "w".to_string(),
            TabStatus {
                state: state.into(),
                at,
                msg: String::new(),
            },
        )])
    }

    #[test]
    fn a_settled_tab_is_read_once_per_status() {
        let tasks = vec![json!({"window_id":"w","host":"h","session_id":"s"})];
        let mut waits = HashMap::new();
        assert!(due(&mut waits, &tasks, &live("working", 5)).is_empty());
        assert_eq!(due(&mut waits, &tasks, &live("idle", 5)).len(), 1);
        assert!(due(&mut waits, &tasks, &live("idle", 5)).is_empty());
        assert_eq!(due(&mut waits, &tasks, &live("done", 9)).len(), 1);
        // Pi has no timers; switching a window to pi clears pending Claude wakes.
        waits.get_mut("w").unwrap().pending = Some((u64::MAX, "CI".into()));
        let pi = vec![json!({"window_id":"w","harness":"pi"})];
        assert!(due(&mut waits, &pi, &live("idle", 12)).is_empty());
        assert_eq!(waits["w"].pending, None);
        let mut pi_list =
            vec![json!({"window_id":"w","harness":"pi","status":"idle","status_at":12})];
        apply(&mut pi_list, &waits);
        assert_eq!(pi_list[0]["status"], "idle");
        due(
            &mut waits,
            &[json!({"window_id":"w","ended_at":1})],
            &live("idle", 13),
        );
        assert!(waits.is_empty());
    }

    #[test]
    fn a_title_outlives_the_status_it_was_read_for() {
        let tasks = vec![json!({"window_id":"w","host":"h","session_id":"s"})];
        let mut waits = HashMap::new();
        due(&mut waits, &tasks, &live("idle", 5));
        waits.get_mut("w").unwrap().title = Some("Slack thread issue".into());
        due(&mut waits, &tasks, &live("done", 9));
        assert_eq!(waits["w"].title.as_deref(), Some("Slack thread issue"));
        let mut list = vec![json!({"window_id":"w","status":"working","status_at":10})];
        apply(&mut list, &waits);
        assert_eq!(list[0]["title"], "Slack thread issue");
        assert_eq!(list[0]["status"], "working");
    }

    #[test]
    fn a_pending_timer_makes_its_task_scheduled_until_the_status_moves() {
        let soon = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 60;
        let waits = HashMap::from([(
            "w".to_string(),
            Wake {
                at: 5,
                pending: Some((soon, "CI".into())),
                title: None,
            },
        )]);
        let task = |state: &str, at: u64| json!({"window_id":"w","status":state,"status_at":at,"status_msg":"m"});
        let mut tasks = vec![task("idle", 5), task("working", 5), task("idle", 6)];
        apply(&mut tasks, &waits);
        assert_eq!(
            (tasks[0]["status"].as_str(), tasks[0]["status_msg"].as_str()),
            (Some("scheduled"), Some("CI"))
        );
        assert_eq!(tasks[0]["wake_at"], soon);
        assert_eq!(tasks[1]["status"], "working");
        assert_eq!(tasks[2]["status"], "idle");
        // Long overdue: gone.
        let old = HashMap::from([(
            "w".to_string(),
            Wake {
                at: 5,
                pending: Some((10, String::new())),
                title: None,
            },
        )]);
        let mut tasks = vec![task("idle", 5)];
        apply(&mut tasks, &old);
        assert_eq!(tasks[0]["status"], "idle");
        // A background job with no known time never goes stale.
        let job = HashMap::from([(
            "w".to_string(),
            Wake {
                at: 5,
                pending: Some((0, "Build".into())),
                title: None,
            },
        )]);
        let mut tasks = vec![task("idle", 5)];
        apply(&mut tasks, &job);
        assert!(tasks[0]["title"].is_null());
        assert_eq!(
            (tasks[0]["status"].as_str(), tasks[0]["status_msg"].as_str()),
            (Some("scheduled"), Some("Build"))
        );
        assert!(tasks[0]["wake_at"].is_null());
    }
}
