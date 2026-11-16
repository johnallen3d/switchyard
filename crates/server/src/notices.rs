//! Telling a spawning agent about its child tasks, so it needn't poll them.
//!
//! An agent that spawns a task through the MCP endpoint is recorded as its
//! parent (`parent_host`, `parent_session`, `parent_window`; see core's
//! `launch`). When the child's agent stops working (its tab goes from
//! `working` to `idle`, `done` or `blocked`) the server types a short
//! notice into the parent's agent, as `message_task` would: the child's
//! status and its last assistant message, so the parent can carry on or
//! `read_task` for more. A child that has set itself a timer or left a
//! background job running will resume by itself, so it gets no notice
//! until it settles for good. `notify_parent: false` on the task turns
//! notices off.
//!
//! A child can also write to its parent itself, mid-work, with the MCP
//! tool `message_parent` (see `send`).
//!
//! Transitions are taken from the live status in the rows, seen at each
//! check (new rows or tasks). The first status seen for a task is only
//! recorded, so restarting the server sends nothing. Sending reads the
//! child's log and types into the parent's tab, over SSH and Rex, so it
//! runs on a thread of its own and never under the lock. A parent waiting
//! on a prompt of its own refuses the notice; it is logged and dropped.

use std::collections::HashMap;
use std::fmt::Write;
use std::thread;

use serde_json::Value;
use switchyard_core::actions;
use switchyard_core::tasks::TabStatus;
use switchyard_core::transcript;

/// The most of the child's last message a notice quotes.
const QUOTE: usize = 600;

/// A notice to send: the child task and the status it settled in.
#[derive(Debug)]
pub struct Job {
    task: Value,
    state: String,
    msg: String,
}

/// The parent's tab a task records, when it has one.
fn parent(task: &Value) -> Option<(&str, &str, &str)> {
    let f = |k: &str| task[k].as_str().filter(|s| !s.is_empty());
    Some((f("parent_host")?, f("parent_session")?, f("parent_window")?))
}

fn label(task: &Value) -> String {
    format!(
        "{}/{}",
        task["project"].as_str().unwrap_or(""),
        task["name"].as_str().unwrap_or("")
    )
}

/// The open tasks with a parent to tell whose agent has just stopped
/// working, recording each one's status in `seen`. Entries for tabs no
/// open task has are dropped.
pub fn due(
    seen: &mut HashMap<String, String>,
    tasks: &[Value],
    live: &HashMap<String, TabStatus>,
) -> Vec<Job> {
    let open: Vec<&Value> = tasks
        .iter()
        .filter(|t| t["ended_at"].is_null() && parent(t).is_some())
        .filter(|t| t["notify_parent"].as_bool() != Some(false))
        .collect();
    seen.retain(|window, _| open.iter().any(|t| t["window_id"].as_str() == Some(window)));
    let mut jobs = Vec::new();
    for task in open {
        let Some(window) = task["window_id"].as_str() else {
            continue;
        };
        let Some(status) = live.get(window) else {
            continue;
        };
        let before = seen.insert(window.to_string(), status.state.clone());
        let settled = matches!(status.state.as_str(), "idle" | "done" | "blocked");
        if before.as_deref() == Some("working") && settled {
            jobs.push(Job {
                task: task.clone(),
                state: status.state.clone(),
                msg: status.msg.clone(),
            });
        }
    }
    jobs
}

/// Send each notice on a thread of its own.
pub fn start(jobs: Vec<Job>) {
    for job in jobs {
        thread::spawn(move || {
            let who = label(&job.task);
            match notice(&job) {
                Some(text) => match send(&job.task, &text) {
                    Ok(_) => crate::log(&format!(
                        "notice: told the parent of {who} it is {}",
                        job.state
                    )),
                    Err(e) => crate::log(&format!("notice: {who}: {e}")),
                },
                None => crate::log(&format!("notice: {who} will resume by itself; not told")),
            }
        });
    }
}

/// The notice for `job`, or None when the child will resume by itself.
fn notice(job: &Job) -> Option<String> {
    let who = label(&job.task);
    if job.state == "blocked" {
        let what = if job.msg.is_empty() {
            String::new()
        } else {
            format!(" ({})", job.msg)
        };
        return Some(format!(
            "[switchyard] Your child task {who} is waiting on a prompt{what}. \
             Only John can answer it, in its tab."
        ));
    }
    let f = |k: &str| job.task[k].as_str().unwrap_or("");
    let log = transcript::fetch_on(f("host"), f("session_id"), f("window_id"));
    if log["wake"].is_object() {
        return None;
    }
    Some(compose(&who, &job.state, &log))
}

/// A settled child's notice from its log (see core's `transcript`).
fn compose(who: &str, state: &str, log: &Value) -> String {
    let last = log["entries"].as_array().and_then(|entries| {
        entries
            .iter()
            .rev()
            .find(|e| e["kind"] == "assistant")
            .and_then(|e| e["text"].as_str())
    });
    let mut text = format!("[switchyard] Your child task {who} is {state}.");
    if let Some(last) = last.map(str::trim).filter(|s| !s.is_empty()) {
        let mut quote: String = last.chars().take(QUOTE).collect();
        if quote.len() < last.len() {
            quote.push('…');
        }
        let _ = write!(text, " Its last message: \"{quote}\"");
    }
    text.push_str(" Use read_task for more, or message_task to reply.");
    text
}

/// Type `text` into the agent that spawned `task`.
///
/// # Errors
///
/// Fails when the task has no parent on record, or as core's
/// `actions::message` does (the parent's tab is gone, or it waits on a
/// prompt).
pub fn send(task: &Value, text: &str) -> Result<String, String> {
    let (host, session, window) = parent(task).ok_or_else(|| {
        format!(
            "{} has no parent agent on record (it wasn't spawned by an agent through spawn_task with its REX_BLOCK)",
            label(task)
        )
    })?;
    actions::message(host, session, window, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn live(state: &str) -> HashMap<String, TabStatus> {
        HashMap::from([(
            "w".to_string(),
            TabStatus {
                state: state.into(),
                at: 1,
                msg: "m".into(),
            },
        )])
    }

    fn child() -> Value {
        json!({"project":"p","name":"n","window_id":"w",
            "parent_host":"h","parent_session":"s","parent_window":"pw"})
    }

    #[test]
    fn a_child_that_stops_working_is_noticed_once() {
        let tasks = vec![child()];
        let mut seen = HashMap::new();
        // The first status seen is only recorded, even a settled one.
        assert!(due(&mut seen, &tasks, &live("idle")).is_empty());
        assert!(due(&mut seen, &tasks, &live("working")).is_empty());
        let jobs = due(&mut seen, &tasks, &live("blocked"));
        assert_eq!(jobs.len(), 1);
        assert_eq!(
            (jobs[0].state.as_str(), jobs[0].msg.as_str()),
            ("blocked", "m")
        );
        assert!(due(&mut seen, &tasks, &live("idle")).is_empty());
        assert!(due(&mut seen, &tasks, &live("working")).is_empty());
        assert_eq!(due(&mut seen, &tasks, &live("done")).len(), 1);
    }

    #[test]
    fn tasks_without_a_parent_or_with_notices_off_are_left_alone() {
        let mut seen = HashMap::new();
        let orphan = vec![json!({"window_id":"w"})];
        let mut off = child();
        off["notify_parent"] = false.into();
        for tasks in [orphan, vec![off]] {
            due(&mut seen, &tasks, &live("working"));
            assert!(due(&mut seen, &tasks, &live("idle")).is_empty());
        }
        assert!(seen.is_empty());
        // A closed task's entry goes.
        due(&mut seen, &[child()], &live("working"));
        let mut ended = child();
        ended["ended_at"] = 1.into();
        due(&mut seen, &[ended], &live("idle"));
        assert!(seen.is_empty());
    }

    #[test]
    fn a_notice_quotes_the_last_assistant_message() {
        let log = json!({"entries":[
            {"kind":"assistant","text":"first"},
            {"kind":"assistant","text":" done: PR opened "},
            {"kind":"tool","text":"Bash"},
        ]});
        assert_eq!(
            compose("p/n", "idle", &log),
            "[switchyard] Your child task p/n is idle. Its last message: \"done: PR opened\" \
             Use read_task for more, or message_task to reply."
        );
        let truncated = json!({"entries":[{"kind":"assistant","text":"x".repeat(QUOTE + 5)}]});
        assert!(compose("p/n", "done", &truncated).contains(&format!("{}…\"", "x".repeat(QUOTE))));
        assert!(!compose("p/n", "idle", &json!({"error":"no log"})).contains("last message"));
    }

    #[test]
    fn sending_needs_a_parent_on_record() {
        let err = send(&json!({"project":"p","name":"n"}), "hi").unwrap_err();
        assert!(err.starts_with("p/n has no parent agent"));
    }
}
