//! `switchyard link`: record the work items (issues, cards, PRs) the
//! current branch is for, where the inbox finds them. Agents and John run it
//! in a task's worktree on any host; it writes the branch's git config
//! (`branch.BRANCH.switchyard-link`, one ref per value), which the task
//! check on the config host reads over SSH (see core's `links`).

use std::process::{Command, ExitCode, Stdio};

use switchyard_core::links::{Unlinked, ref_shape_ok};
use switchyard_core::tasks::unlink_task_refs;

const USAGE: &str = "usage: switchyard link [REF...]
       switchyard unlink REF...

Links the current branch to work items, which the agents inbox shows on its
task: REF is #42 or 42 (an issue, or a card when the project's tracker is a
Fizzy board), OWNER/REPO#42, fizzy:123, pr:57, or a GitHub issue or PR URL
or Fizzy card URL. With no REF, lists the branch's links. unlink removes
them. The refs live in the branch's git config, branch.BRANCH.switchyard-link;
unlink also removes those the task was spawned with from the task registry.";

fn git(args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn run(args: &[String]) -> Result<(), String> {
    let (unlink, refs) = match args {
        [cmd, refs @ ..] if cmd == "link" => (false, refs),
        [cmd, refs @ ..] if cmd == "unlink" && !refs.is_empty() => (true, refs),
        _ => return Err(USAGE.into()),
    };
    let branch = git(&["symbolic-ref", "--quiet", "--short", "HEAD"]);
    let task = task_branch(branch.as_deref().ok().map(str::trim));
    let branch = match branch {
        Ok(b) => b.trim().to_string(),
        // Off a branch in a task's own repo, link the task's branch.
        Err(_) => match &task {
            Some(TaskBranch::SameRepo(b)) => b.clone(),
            _ => return Err("not on a branch: run this in the task's worktree".into()),
        },
    };
    let key = link_key(&branch);
    let existing = links_of(&key);
    if refs.is_empty() {
        for r in &existing {
            println!("{r}");
        }
        task_hint(task.as_ref(), &branch);
        return Ok(());
    }
    if unlink {
        unlink_refs(&key, &existing, refs)?;
        switchyard_core::server::refresh();
        return Ok(());
    }
    if let Some(r) = refs.iter().find(|r| !ref_shape_ok(r.trim())) {
        return Err(format!("{}: not a work item ref\n\n{USAGE}", r.trim()));
    }
    add_links(&key, &existing, refs, "")?;
    match &task {
        Some(TaskBranch::SameRepo(b)) if *b != branch => {
            eprintln!(
                "switchyard: {branch} is not this task's branch, {b}, where Switchyard \
                 looks; linked there too"
            );
            let key = link_key(b);
            add_links(&key, &links_of(&key), refs, &format!(" on {b}"))?;
        }
        other => task_hint(other.as_ref(), &branch),
    }
    // On the config host, have switchyard-server check the tasks now rather
    // than at its next timer, so the inbox shows the change; elsewhere
    // there is no server socket and this does nothing.
    switchyard_core::server::refresh();
    Ok(())
}

fn link_key(branch: &str) -> String {
    format!("branch.{branch}.switchyard-link")
}

/// The refs linked under `key`.
fn links_of(key: &str) -> Vec<String> {
    // --get-all exits 1 when the key has no values.
    git(&["config", "--get-all", key])
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

/// Add `refs` under `key`, skipping those in `existing`; `on` follows each
/// report.
fn add_links(key: &str, existing: &[String], refs: &[String], on: &str) -> Result<(), String> {
    for r in refs {
        let r = r.trim();
        if existing.iter().any(|e| e == r) {
            println!("already linked {r}{on}");
        } else {
            git(&["config", "--add", key, r])?;
            println!("linked {r}{on}");
        }
    }
    Ok(())
}

/// The branch of the Switchyard task this runs under, from the environment
/// `task run` gives its agent, when it is not the current branch.
enum TaskBranch {
    /// In this repository, whose config can hold its links too.
    SameRepo(String),
    /// In another repository: the branch and its worktree.
    Elsewhere(String, String),
}

fn task_branch(current: Option<&str>) -> Option<TaskBranch> {
    let branch = std::env::var("SWITCHYARD_TASK_BRANCH").ok()?;
    let worktree = std::env::var("SWITCHYARD_TASK_WORKTREE").ok()?;
    if branch.is_empty() || current == Some(branch.as_str()) {
        return None;
    }
    // In the task's worktree, its branch is the current one, renamed since
    // `task run` set the environment: Switchyard follows the rename.
    let canon = |d: &str| std::fs::canonicalize(d.trim()).ok();
    let top = git(&["rev-parse", "--show-toplevel"]).ok();
    if current.is_some()
        && top
            .as_deref()
            .and_then(canon)
            .is_some_and(|t| Some(t) == canon(&worktree))
    {
        return None;
    }
    let common = |dir: Option<&str>| {
        let mut args = vec![];
        if let Some(dir) = dir {
            args.extend(["-C", dir]);
        }
        args.extend(["rev-parse", "--path-format=absolute", "--git-common-dir"]);
        git(&args)
            .ok()
            .map(|d| std::fs::canonicalize(d.trim()).unwrap_or_else(|_| d.trim().into()))
    };
    match (common(None), common(Some(&worktree))) {
        (Some(here), Some(there)) if here == there => Some(TaskBranch::SameRepo(branch)),
        _ => Some(TaskBranch::Elsewhere(branch, worktree)),
    }
}

/// Warn that `branch` is not the task's branch, where Switchyard looks.
fn task_hint(task: Option<&TaskBranch>, branch: &str) {
    match task {
        Some(TaskBranch::Elsewhere(b, worktree)) => eprintln!(
            "switchyard: {branch} is not this task's branch; Switchyard looks for its links \
             on {b}: run `switchyard link` in {worktree} for work items this task is for"
        ),
        Some(TaskBranch::SameRepo(b)) => eprintln!(
            "switchyard: {branch} is not this task's branch; Switchyard looks for its links \
             on {b}"
        ),
        None => {}
    }
}

/// Remove `refs` from the branch's git config and from its task's links in
/// the registry, where spawn records the work items it was started for.
fn unlink_refs(key: &str, existing: &[String], refs: &[String]) -> Result<(), String> {
    let refs: Vec<&str> = refs.iter().map(|r| r.trim()).collect();
    let top = git(&["rev-parse", "--show-toplevel"])?;
    let top = std::fs::canonicalize(top.trim()).unwrap_or_else(|_| top.trim().into());
    // The branch's config is the source of truth for agent links, so it is
    // unlinked even when the registry can't be.
    let registry = unlink_task_refs(&top, &refs).unwrap_or_else(|e| {
        eprintln!("switchyard: could not unlink in the task registry: {e}");
        refs.iter().map(|_| Unlinked::Absent).collect()
    });
    for (r, in_registry) in refs.iter().zip(registry) {
        let in_config = existing.iter().any(|e| e == r);
        if in_config {
            git(&["config", "--fixed-value", "--unset-all", key, r])?;
        }
        match in_registry {
            _ if in_config => println!("unlinked {r}"),
            Unlinked::Removed => println!("unlinked {r}"),
            Unlinked::Derived => eprintln!(
                "switchyard: {r} comes from the branch name, its commits or PR, \
                 so it can't be unlinked"
            ),
            Unlinked::Absent => eprintln!("switchyard: {r} is not linked"),
        }
    }
    Ok(())
}

pub fn main(args: &[String]) -> ExitCode {
    if matches!(args.get(1).map(String::as_str), Some("-h" | "--help")) {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("switchyard: {e}");
            ExitCode::FAILURE
        }
    }
}
