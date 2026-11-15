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
    let branch = git(&["symbolic-ref", "--quiet", "--short", "HEAD"])
        .map_err(|_| "not on a branch: run this in the task's worktree".to_string())?;
    let key = format!("branch.{}.switchyard-link", branch.trim());
    // --get-all exits 1 when the key has no values.
    let existing: Vec<String> = git(&["config", "--get-all", &key])
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect();
    if refs.is_empty() {
        for r in &existing {
            println!("{r}");
        }
        return Ok(());
    }
    if unlink {
        unlink_refs(&key, &existing, refs)?;
        switchyard_core::server::refresh();
        return Ok(());
    }
    for r in refs {
        let r = r.trim();
        if !ref_shape_ok(r) {
            return Err(format!("{r}: not a work item ref\n\n{USAGE}"));
        } else if existing.iter().any(|e| e == r) {
            println!("already linked {r}");
        } else {
            git(&["config", "--add", &key, r])?;
            println!("linked {r}");
        }
    }
    // On the config host, have switchyard-server check the tasks now rather
    // than at its next timer, so the inbox shows the change; elsewhere
    // there is no server socket and this does nothing.
    switchyard_core::server::refresh();
    Ok(())
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
