//! Actions on a spawned task: `rex-spawn --act ACTION HOST SESSION WINDOW`.
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
//! - link: print the URL of the task's main work item (its PR, else an
//!   issue, card or commit; see `links`), open or ended, for the caller to
//!   open on the client machine.
//! - resume: for an ended task (archived, or its tab or worktree gone), open
//!   a new tab on the task's host that recreates the worktree on its branch
//!   and resumes the agent session, replace the ended entry with the new
//!   task, and print the tab as `--launch` does. SESSION is unused here.
//!
//! Archiving doesn't delete an unmerged branch: only uncommitted work can be
//! lost, and a resume finds its commits again.

use crate::hosts::catalog;
use crate::launch::launch;
use crate::rex::{host_bash, now, rex, rex_json, sh_quote};
use crate::sessions::newest_session;
use crate::tasks::{MERGED_SH, load_tasks, save_tasks};

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

/// Resume the ended task that had `window` on `host`.
fn resume(host: &str, window: &str) -> Result<String, String> {
    let tasks = load_tasks()?;
    let task = tasks
        .iter()
        .rev()
        .find(|t| !t["ended_at"].is_null() && t["host"].as_str() == Some(host) && t["window_id"].as_str() == Some(window))
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
    let (host, session, window_id) = launch(host, &path, &field("name"), &harness, None, Some((&agent_session, &field("profile"))))?;
    // The new task takes the ended one's place in the archive, keeping its
    // work items.
    let mut tasks = load_tasks()?;
    tasks.retain(|t| {
        !(!t["ended_at"].is_null()
            && t["host"].as_str() == Some(host.as_str())
            && t["window_id"].as_str() == Some(window))
    });
    if let Some(new) = tasks
        .iter_mut()
        .rev()
        .find(|t| t["ended_at"].is_null() && t["host"].as_str() == Some(host.as_str()) && t["window_id"].as_str() == Some(window_id.as_str()))
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

pub fn act(action: &str, host: &str, session: &str, window: &str) -> Result<String, String> {
    let result = run_action(action, host, session, window);
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
    let here = |t: &serde_json::Value| t["host"].as_str() == Some(host) && t["window_id"].as_str() == Some(window);
    // An open task, or for an archive, the latest ended one (closing its tab
    // left the worktree).
    let i = match tasks.iter().position(|t| t["ended_at"].is_null() && here(t)) {
        Some(i) => i,
        None if action != "review" => tasks.iter().rposition(here).ok_or("no task for that tab")?,
        None => return Err("no open task for that tab".into()),
    };
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
                    "wt -C {} remove --foreground {}{} 2>&1",
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
                } else if !last.is_empty() {
                    note = format!("; {last}");
                }
            }
            // A session spawn created for the project goes when its last tab does.
            if let Ok(list) = rex_json(host, &["window", "ls", "-s", session, "--json"])
                && list["windows"].as_array().is_some_and(|w| w.is_empty())
            {
                let _ = rex(host, &["kill", session]);
            }
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
