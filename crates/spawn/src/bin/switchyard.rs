//! `switchyard link`: record the work items (issues, cards, PRs) the
//! current branch is for, where the inbox finds them. Agents and John run it
//! in a task's worktree on any host; it writes the branch's git config
//! (`branch.BRANCH.switchyard-link`, one ref per value), which the task
//! check on the config host reads over SSH (see core's `links`).

use std::process::{Command, ExitCode, Stdio};

use switchyard_core::links::ref_shape_ok;

const USAGE: &str = "usage: switchyard link [REF...]
       switchyard unlink REF...

Links the current branch to work items, which the agents inbox shows on its
task: REF is #42 or 42 (an issue, or a card when the project's tracker is a
Fizzy board), OWNER/REPO#42, fizzy:123, pr:57, or a GitHub issue or PR URL
or Fizzy card URL. With no REF, lists the branch's links. unlink removes
them. The refs live in the branch's git config, branch.BRANCH.switchyard-link.";

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
    for r in refs {
        let r = r.trim();
        if unlink {
            if existing.iter().any(|e| e == r) {
                git(&["config", "--fixed-value", "--unset-all", &key, r])?;
                println!("unlinked {r}");
            } else {
                eprintln!("switchyard: {r} is not linked");
            }
        } else if !ref_shape_ok(r) {
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

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.first().map(String::as_str), Some("-h" | "--help")) {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("switchyard: {e}");
            ExitCode::FAILURE
        }
    }
}
