//! Actions on a spawned task: `switchyard task act ACTION HOST SESSION WINDOW`.
//!
//! Runs on the config host, which holds the task registry and reaches every
//! Rex host; the config's tab_picker mode calls it, then jumps to the task's tab
//! after a review.
//!
//! - review: split the task's tab with lazygit in the worktree (or a plain
//!   diff where lazygit is missing).
//! - archive: refuse a worktree with uncommitted changes; otherwise record
//!   the newest agent session, close the tab, remove the worktree with
//!   Worktrunk, close the project session if that left it empty, and mark
//!   the task ended.
//! - force-archive: the same, but throw away uncommitted changes
//!   (`wt remove --force`).
//!
//!   A clean worktree whose work is merged (its PR, as last seen on
//!   GitHub, or its branch into the default branch) is safe to archive;
//!   the task ends as "merged" rather than "archived".
//!
//!   Both also work on an ended task whose tab was closed without an
//!   archive, which leaves its worktree behind: they remove the worktree
//!   and mark the task archived.
//!
//!   On a tab that is still open with no open task, they first reopen its
//!   latest ended task (one ended by mistake), or else adopt the agent in
//!   it as a task (its checkout, branch and harness from the agent's
//!   pane), so any agent tab can be archived. A primary checkout is never
//!   removed: its archive only closes the tab.
//! - link: print the URL of the task's main work item (its PR, else an
//!   issue, card or commit; see `links`), open or ended, for the caller to
//!   open on the client machine.
//! - pin: pin an open task to the top of the inbox, or unpin a pinned one
//!   (`pinned` in the registry).
//! - snooze (`task act snooze HOST SESSION WINDOW UNTIL`): hide an
//!   open task in the inbox's Snoozed group until UNTIL, Unix seconds
//!   (`snoozed_until`); 0 wakes it now.
//! - message (`task act message HOST SESSION WINDOW PROMPT_REF`): send
//!   the prompt file's text to the agent in the tab (see `message`), then
//!   remove the file.
//! - resume: for an ended task (archived, or its tab or worktree gone), open
//!   a new tab on the task's host that recreates the worktree on its branch
//!   and resumes the agent session, replace the ended entry with the new
//!   task, and print the tab as `task launch` does. SESSION is unused here.
//!
//! Archiving doesn't delete an unmerged branch: only uncommitted work can be
//! lost, and a resume finds its commits again.

use std::time::Duration;

use crate::hosts::catalog;
use crate::launch::launch;
use crate::prompts::{discard_prompt, fetch_prompt};
use crate::rex::{host_bash, now, rex, rex_json, sh_quote};
use crate::sessions::newest_session;
use crate::tasks::{MERGED_SH, in_window, load_tasks, save_tasks};

/// The tab's agent pane: the first pane of its tiled layer, which is where
/// `--run` started the agent.
fn agent_block(host: &str, session: &str, window: &str) -> Result<String, String> {
    let list = rex_json(host, &["window", "ls", "-s", session, "--json"])?;
    list["windows"]
        .as_array()
        .and_then(|ws| ws.iter().find(|w| w["window_id"].as_str() == Some(window)))
        .and_then(|w| w["layers"].as_array())
        .and_then(|ls| ls.iter().find(|l| l["kind"].as_str() == Some("tiled")))
        .and_then(|l| l["blocks"][0]["block_id"].as_str())
        .map(String::from)
        .ok_or_else(|| "the task's tab is gone".to_string())
}

/// Where the agent in a tab works, from its foreground process's folder on
/// `host`: PATH (the checkout's top), REPO (the primary checkout) and
/// BRANCH, tab separated; exit 3 when that isn't a Git checkout.
fn checkout_script(cwd: &str) -> String {
    format!(
        "cd {} 2>/dev/null || exit 3\n\
         top=$(git rev-parse --show-toplevel 2>/dev/null) || exit 3\n\
         main=$(git worktree list --porcelain | sed -n '1s/^worktree //p')\n\
         printf '%s\\t%s\\t%s\\n' \"$top\" \"${{main:-$top}}\" \"$(git symbolic-ref --quiet --short HEAD)\"\n",
        sh_quote(cwd)
    )
}

/// A task record for an agent tab Switchyard has none for (started by hand,
/// say), so it can be archived like a spawned one: its checkout, branch
/// and harness from the agent's pane. In a primary checkout, the archive
/// only closes the tab.
fn adopt(host: &str, session: &str, window: &str) -> Result<serde_json::Value, String> {
    let pane = crate::transcript::agent_pane(host, session, window)?;
    let cwd = pane["cwd"].as_str().unwrap_or("");
    if cwd.is_empty() {
        return Err("no agent in that tab to archive".into());
    }
    let out = host_bash(host, &checkout_script(cwd)).ok_or_else(|| format!("{host} can't be reached"))?;
    if !out.status.success() {
        return Err(format!("{cwd} isn't a Git checkout: nothing to archive but the tab"));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut f = text.trim_end_matches('\n').split('\t');
    let (top, repo, branch) = (f.next().unwrap_or(""), f.next().unwrap_or(""), f.next().unwrap_or(""));
    let base = |p: &str| p.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string();
    if top != repo && branch.is_empty() {
        return Err(format!("{top} is on a detached HEAD; check out its branch to archive it"));
    }
    // Rex names Claude Code `claude-code`; spawned tasks record `claude`.
    let app = pane["app"].as_str().unwrap_or("");
    let harness = app.strip_suffix("-code").unwrap_or(app);
    Ok(serde_json::json!({
        "host": host,
        "session_id": session,
        "window_id": window,
        "project": base(repo),
        "repo": repo,
        "name": base(top),
        "branch": branch,
        "worktree": top,
        "harness": harness,
        "adopted": true,
        "created_at": now(),
        "ended_at": null,
        "ended_reason": null,
    }))
}

/// A session spawn created for the project goes when its last tab does.
fn kill_if_empty(host: &str, session: &str) {
    if let Ok(list) = rex_json(host, &["window", "ls", "-s", session, "--json"])
        && list["windows"].as_array().is_some_and(|w| w.is_empty())
    {
        let _ = rex(host, &["kill", session]);
    }
}

/// Resume the ended task that had `window` on `host`.
fn resume(host: &str, window: &str) -> Result<String, String> {
    let tasks = load_tasks()?;
    let task = tasks
        .iter()
        .rev()
        .find(|t| !t["ended_at"].is_null() && in_window(t, window))
        .ok_or("no ended task for that tab")?
        .clone();
    let field = |k: &str| task[k].as_str().unwrap_or("").to_string();
    let label = format!("{}/{}", field("project"), field("name"));
    // Tasks from before sessions were recorded: ask the host for the newest.
    let agent_session = match field("agent_session") {
        id if !id.is_empty() => id,
        _ => newest_session(&task).ok_or_else(|| format!("{label}: no agent session found on {host}"))?,
    };
    // Folder tasks have no catalog entry. Their recorded repository is the
    // primary checkout, which remains after the task worktree is archived.
    let path = catalog()?
        .into_iter()
        .find(|p| crate::folders::home_relative(&p.path) == crate::folders::home_relative(&field("repo")))
        .map(|p| p.path)
        .unwrap_or_else(|| field("repo"));
    let harness = match field("harness").as_str() {
        "" => "claude".to_string(),
        h => h.to_string(),
    };
    let (host, session, window_id) = launch(host, &path, &field("name"), &harness, None, None, Some((&agent_session, &field("profile"))))?;
    // The new task takes the ended one's place in the archive, keeping its
    // work items.
    let mut tasks = load_tasks()?;
    tasks.retain(|t| t["ended_at"].is_null() || !in_window(t, window));
    if let Some(new) = tasks
        .iter_mut()
        .rev()
        .find(|t| t["ended_at"].is_null() && in_window(t, &window_id))
        && task["links"].as_array().is_some_and(|l| !l.is_empty())
    {
        new["links"] = task["links"].clone();
    }
    save_tasks(&tasks)?;
    Ok(format!("{host}\t{session}\t{window_id}"))
}

/// The archive's check of a worktree: exit 3 when it is gone, else its
/// `git status --porcelain` lines and a last `@STATE` line from MERGED_SH.
fn archive_check(worktree: &str, branch: &str) -> String {
    format!(
        "cd {} 2>/dev/null || exit 3\n\
         status=$(git status --porcelain) || exit 1\n\
         [ -n \"$status\" ] && printf '%s\\n' \"$status\"\n\
         path=$PWD\nbranch={}\n{MERGED_SH}\necho \"@$merged\"\n",
        sh_quote(worktree),
        sh_quote(branch),
    )
}

/// Pin or unpin the open task in `window`, as `pinned`; the outcome says
/// which.
fn pin(window: &str) -> Result<String, String> {
    let mut tasks = load_tasks()?;
    let task = open_task(&mut tasks, window)?;
    let pinned = !task["pinned"].as_bool().unwrap_or(false);
    set_or_remove(task, "pinned", pinned.then(|| true.into()));
    let label = label_of(task);
    save_tasks(&tasks)?;
    Ok(format!("{} {label}", if pinned { "pinned" } else { "unpinned" }))
}

/// Snooze the open task in `window` until `until` (Unix seconds), or wake
/// it for 0.
pub fn snooze(window: &str, until: u64) -> Result<String, String> {
    let mut tasks = load_tasks()?;
    let task = open_task(&mut tasks, window)?;
    set_or_remove(task, "snoozed_until", (until > 0).then(|| until.into()));
    let label = label_of(task);
    save_tasks(&tasks)?;
    crate::server::refresh();
    Ok(if until > 0 { format!("snoozed {label}") } else { format!("woke {label}") })
}

/// How long to let the agent take a paste before pressing Enter.
const PASTE_SETTLE: Duration = Duration::from_millis(150);

/// Send `text` to the agent in `window` on `host`, as if John typed it in
/// the tab: one paste (so newlines stay in the message), then Enter. An
/// idle agent starts a turn on it; a working one takes it into the current
/// turn at its next chance (Claude after the running tool call, Pi as a
/// steering message). An agent waiting on a prompt (`blocked`) is refused,
/// since the keys would answer the prompt instead.
pub fn message(host: &str, session: &str, window: &str, text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("nothing to send".into());
    }
    let pane = crate::transcript::agent_pane(host, session, window)?;
    let block = pane["block"].as_str().filter(|b| !b.is_empty()).ok_or("no agent in that tab")?;
    let label = load_tasks()
        .ok()
        .and_then(|tasks| {
            tasks.iter().find(|t| t["ended_at"].is_null() && in_window(t, window)).map(label_of)
        })
        .unwrap_or_else(|| "the agent".into());
    if pane["state"].as_str() == Some("blocked") {
        return Err(format!("{label} is waiting on a prompt; answer it in its tab"));
    }
    let event = serde_json::json!({
        "session_id": session,
        "block_id": block,
        "event": { "type": "paste", "payload": { "text": text } },
    })
    .to_string();
    rex(host, &["api", "call", "block.event", &event])?;
    std::thread::sleep(PASTE_SETTLE);
    rex(host, &["send-key", "-s", session, "-b", block, "enter"])?;
    Ok(format!("sent to {label}"))
}

/// `message` with the text of a prompt file (`LABEL:ID`, see `prompts`),
/// which is removed once sent.
pub fn message_ref(host: &str, session: &str, window: &str, prompt_ref: &str) -> Result<String, String> {
    let text = fetch_prompt(prompt_ref)?;
    let sent = message(host, session, window, &text)?;
    discard_prompt(prompt_ref);
    Ok(sent)
}

fn open_task<'a>(tasks: &'a mut [serde_json::Value], window: &str) -> Result<&'a mut serde_json::Value, String> {
    tasks
        .iter_mut()
        .find(|t| t["ended_at"].is_null() && in_window(t, window))
        .ok_or_else(|| "no open task for that tab".to_string())
}

fn label_of(task: &serde_json::Value) -> String {
    format!("{}/{}", task["project"].as_str().unwrap_or(""), task["name"].as_str().unwrap_or(""))
}

fn set_or_remove(task: &mut serde_json::Value, key: &str, value: Option<serde_json::Value>) {
    match (value, task.as_object_mut()) {
        (Some(v), Some(obj)) => {
            obj.insert(key.into(), v);
        }
        (None, Some(obj)) => {
            obj.remove(key);
        }
        _ => {}
    }
}

pub fn act(action: &str, host: &str, session: &str, window: &str) -> Result<String, String> {
    let result = match action {
        "pin" => pin(window),
        _ => run_action(action, host, session, window),
    };
    // Even a failed archive may have changed the registry or closed the
    // tab: have switchyard-server check now rather than at its next poll.
    // Reviewing and opening a work item change nothing.
    if !matches!(action, "review" | "link") {
        crate::server::refresh();
    }
    result
}

fn run_action(action: &str, host: &str, session: &str, window: &str) -> Result<String, String> {
    if action == "resume" {
        return resume(host, window);
    }
    let mut tasks = load_tasks()?;
    let here = |t: &serde_json::Value| in_window(t, window);
    let archiving = matches!(action, "archive" | "force-archive");
    // An open task. For an archive of a tab that is still open, the latest
    // ended task reopened (it ended by mistake), or else one adopted for
    // the agent in it, so any agent tab can be archived. Otherwise the
    // latest ended task (closing its tab left the worktree).
    let i = match tasks.iter().position(|t| t["ended_at"].is_null() && here(t)) {
        Some(i) => i,
        None if archiving && agent_block(host, session, window).is_ok() => {
            match tasks.iter().rposition(here) {
                Some(i) => {
                    let task = tasks[i].as_object_mut().ok_or("bad task record")?;
                    task.insert("ended_at".into(), serde_json::Value::Null);
                    task.insert("ended_reason".into(), serde_json::Value::Null);
                    i
                }
                None => {
                    tasks.push(adopt(host, session, window)?);
                    tasks.len() - 1
                }
            }
            // Saved now, so a refusal (uncommitted changes) still leaves
            // the picker a task to show them on.
        }
        None if action != "review" => tasks.iter().rposition(here).ok_or("no task for that tab")?,
        None => return Err("no open task for that tab".into()),
    };
    if archiving && tasks[i]["ended_at"].is_null() {
        save_tasks(&tasks)?;
    }
    let ended = !tasks[i]["ended_at"].is_null();
    let field = |k: &str| tasks[i][k].as_str().unwrap_or("").to_string();
    let (label, worktree, repo, branch) =
        (format!("{}/{}", field("project"), field("name")), field("worktree"), field("repo"), field("branch"));

    match action {
        // The task's main work item (see `links::primary`), for the caller
        // to open where John is.
        "link" => crate::links::primary(&tasks[i])
            .and_then(|l| l["url"].as_str())
            .map(String::from)
            .ok_or_else(|| format!("{label} has no linked work item")),
        "review" => {
            let block = agent_block(host, session, window)?;
            let script = "if command -v lazygit >/dev/null 2>&1; then exec lazygit; fi; \
                base=$(git symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null || echo main); \
                { git log --oneline \"$base..HEAD\"; git status --short; git diff \"$base\"; } | less -R";
            rex(host, &[
                "split", "-s", session, "-w", window, "-b", &block, "--split", "right", "--ratio", "50",
                "-c", &worktree, "--label", "review", "--", "sh", "-c", script,
            ])?;
            Ok(format!("reviewing {label}"))
        }
        // A primary checkout is never removed: close the tab and end the task.
        "archive" | "force-archive" if !worktree.is_empty() && worktree.trim_end_matches('/') == repo.trim_end_matches('/') => {
            if !ended {
                let _ = rex(host, &["window", "close", "-s", session, window]);
                tasks[i]["ended_at"] = now().into();
            }
            kill_if_empty(host, session);
            tasks[i]["ended_reason"] = "archived".into();
            save_tasks(&tasks)?;
            Ok(format!("archived {label}; kept the primary checkout {worktree}"))
        }
        "archive" | "force-archive" => {
            let force = action == "force-archive";
            // Exit 3: the worktree is already gone. Any status line:
            // uncommitted changes. The last line, `@STATE`, says whether the
            // branch is merged (see MERGED_SH).
            let out = host_bash(host, &archive_check(&worktree, &branch)).ok_or_else(|| format!("{host} can't be reached"))?;
            let exists = out.status.code() != Some(3);
            if exists && !force && !out.status.success() {
                return Err(format!("{label}: can't read the worktree's status on {host}"));
            }
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            let status: Vec<&str> = text.lines().filter(|l| !l.is_empty() && !l.starts_with('@')).collect();
            let branch_merged = text.lines().any(|l| l == "@merged");
            // Safe to archive: the work is merged (its PR, or the branch
            // itself) and nothing is left uncommitted.
            let merged = exists
                && out.status.success()
                && status.is_empty()
                && (branch_merged || crate::links::pr_merged(&tasks[i]));
            if exists && !force && !status.is_empty() {
                let files: Vec<&str> = status.iter().map(|l| l.get(3..).unwrap_or(l)).collect();
                let shown = files.iter().take(3).copied().collect::<Vec<_>>().join(", ");
                let more = if files.len() > 3 { format!(" and {} more", files.len() - 3) } else { String::new() };
                return Err(format!(
                    "Archive refused: {label} has uncommitted changes in {shown}{more}. \
                     Press ⌃X, Enter, Enter in the tab picker (⌘K) to throw them away."
                ));
            }
            if ended && !exists {
                tasks[i]["ended_reason"] = "worktree removed".into();
                save_tasks(&tasks)?;
                return Err(format!("{label}: its worktree is already gone"));
            }
            // The newest session, not spawn's choice: a /clear starts
            // another. An ended task recorded it when it ended.
            if !ended && let Some(id) = newest_session(&tasks[i]) {
                tasks[i]["agent_session"] = id.into();
            }
            // Close the tab first so the agent isn't left in a deleted
            // directory; a tab that is already gone is fine.
            if !ended {
                let _ = rex(host, &["window", "close", "-s", session, window]);
            }
            let mut note = String::new();
            let mut failed = false;
            if exists {
                let remove = format!(
                    // Worktrunk deletes the files in the background (its log
                    // is under .git/wt/logs), so a big worktree doesn't hold
                    // up the picker; it still refuses an unsafe removal here.
                    "wt -C {} remove {}{} 2>&1",
                    sh_quote(&repo),
                    if force { "--force " } else { "" },
                    sh_quote(&branch)
                );
                let out = host_bash(host, &remove).ok_or_else(|| format!("{host} can't be reached"))?;
                let text = String::from_utf8_lossy(&out.stdout);
                let last = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
                if !out.status.success() {
                    failed = true;
                    note = format!("; wt remove failed: {last}");
                } else {
                    // Worktrunk's own lines, not the indented files it lists.
                    let summary: Vec<&str> = text
                        .lines()
                        .filter(|l| !l.trim().is_empty() && !l.starts_with(char::is_whitespace))
                        .map(|l| l.trim().trim_end_matches(':'))
                        .collect();
                    if !summary.is_empty() {
                        note = format!("; {}", summary.join(" · "));
                    }
                }
            }
            kill_if_empty(host, session);
            if !ended {
                tasks[i]["ended_at"] = now().into();
            }
            tasks[i]["ended_reason"] = if merged {
                "merged"
            } else if force {
                "force-archived"
            } else {
                "archived"
            }
            .into();
            save_tasks(&tasks)?;
            if failed {
                return Err(format!("{label}: tab closed{note}"));
            }
            Ok(format!("archived {label}{}{note}", if merged { " (merged)" } else { "" }))
        }
        _ => Err(format!("unknown action {action}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn checkout_script_finds_the_checkout_repo_and_branch() {
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("switchyard-adopt-{}-{unique}", std::process::id()));
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |dir: &std::path::Path, args: &[&str]| {
            assert!(Command::new("git").arg("-C").arg(dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
                .args(args).output().unwrap().status.success(), "git {args:?}");
        };
        git(&repo, &["init", "--quiet", "-b", "main"]);
        git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "base"]);
        let wt = root.join("wt");
        git(&repo, &["worktree", "add", "--quiet", "-b", "feature/x", wt.to_str().unwrap(), "main"]);
        std::fs::create_dir_all(wt.join("sub")).unwrap();
        let run = |dir: &std::path::Path| {
            let out = Command::new("bash").args(["-c", &checkout_script(dir.to_str().unwrap())]).output().unwrap();
            (out.status.code(), String::from_utf8_lossy(&out.stdout).into_owned())
        };
        let (r, w) = (repo.to_str().unwrap(), wt.to_str().unwrap());
        // From a subfolder of a linked worktree.
        assert_eq!(run(&wt.join("sub")), (Some(0), format!("{w}\t{r}\tfeature/x\n")));
        // The primary checkout is its own repo, which the archive keeps.
        assert_eq!(run(&repo), (Some(0), format!("{r}\t{r}\tmain\n")));
        assert_eq!(run(&root).0, Some(3));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn archive_check_reports_status_and_merge_state() {
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("switchyard-archive-{}-{unique}", std::process::id()));
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |dir: &std::path::Path, args: &[&str]| {
            assert!(Command::new("git").arg("-C").arg(dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
                .args(args).output().unwrap().status.success(), "git {args:?}");
        };
        git(&repo, &["init", "--quiet", "-b", "main"]);
        git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "base"]);
        let wt = root.join("wt");
        git(&repo, &["worktree", "add", "--quiet", "-b", "feature/x", wt.to_str().unwrap(), "main"]);
        git(&wt, &["commit", "--quiet", "--allow-empty", "-m", "work"]);
        let run = || {
            let out = Command::new("bash").args(["-c", &archive_check(wt.to_str().unwrap(), "feature/x")]).output().unwrap();
            (out.status.code(), String::from_utf8_lossy(&out.stdout).into_owned())
        };
        assert_eq!(run(), (Some(0), "@unmerged\n".into()));
        git(&repo, &["merge", "--quiet", "--ff-only", "feature/x"]);
        assert_eq!(run(), (Some(0), "@merged\n".into()));
        std::fs::write(wt.join("f"), "x").unwrap();
        assert_eq!(run(), (Some(0), "?? f\n@merged\n".into()));
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(run().0, Some(3));
    }
}
