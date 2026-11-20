//! `switchyard task run`: runs in the work's new tab. Clones a missing project,
//! creates or reuses the worktree with Worktrunk, then becomes the harness.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use switchyard_core::naming::{branch_and_slug, split_harness};
use switchyard_core::rex::home;
use switchyard_core::sessions::{new_session_id, session_exists_here, work_env};

use crate::trust::{claude_config, trust_at};

fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None if path == "~" => home(),
        None => PathBuf::from(path),
    }
}

fn status(msg: &str) {
    println!("\x1b[38;5;183m▸\x1b[0m {msg}");
}

fn check(cmd: &mut Command, what: &str) -> Result<(), String> {
    match cmd.status() {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("{what} failed ({s})")),
        Err(e) => Err(format!("{what}: {e}")),
    }
}

fn worktree_for(repo: &Path, branch: &str) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let want = format!("branch refs/heads/{branch}");
    let mut current: Option<&str> = None;
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current = Some(path);
        } else if line == want {
            return current.map(PathBuf::from);
        }
    }
    None
}

/// Fetch origin's default branch and return it (e.g. `origin/main`) as the
/// base for a new branch, so work doesn't start from a stale local `main`.
/// None, falling back to Worktrunk's local default, when the repo has no
/// `origin/HEAD` or the fetch fails (offline).
fn fresh_remote_base(repo: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ])
        .output()
        .ok()?;
    let base = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let branch = base
        .strip_prefix("origin/")
        .filter(|b| out.status.success() && !b.is_empty())?;
    status(&format!("Fetching {base}"));
    match check(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["fetch", "--quiet", "origin", branch]),
        "git fetch",
    ) {
        Ok(()) => Some(base),
        Err(e) => {
            eprintln!("\x1b[33mswitchyard: {e}; branching from local {branch}\x1b[0m");
            None
        }
    }
}

/// Set up the worktree; returns it, or the error and the best directory to
/// leave the shell in.
fn prepare(path: &str, name: &str, clone_url: &str) -> Result<PathBuf, (String, PathBuf)> {
    let repo = expand(path);
    let (branch, slug) = branch_and_slug(name);
    if !repo.exists() {
        if clone_url.is_empty() {
            return Err((format!("Git folder {path} no longer exists"), home()));
        }
        status(&format!("Cloning {clone_url} into {path}"));
        if let Some(parent) = repo.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        check(
            Command::new("git")
                .arg("clone")
                .arg(expand(clone_url))
                .arg(&repo),
            "git clone",
        )
        .map_err(|e| (e, home()))?;
    }
    let fail = |e: String| (e, repo.clone());
    if let Some(existing) = worktree_for(&repo, &branch) {
        status(&format!("Reusing {branch} at {}", existing.display()));
        return Ok(existing);
    }
    let exists = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    status(&format!(
        "{} {branch} in .worktrees/{slug}",
        if exists { "Checking out" } else { "Creating" }
    ));
    // Override the path template per call for the short-name convention;
    // the global Worktrunk config keeps its own.
    let template = format!("worktree-path = \"{{{{ repo_path }}}}/.worktrees/{slug}\"");
    let mut wt = Command::new("wt");
    wt.arg("-C")
        .arg(&repo)
        .args(["--config-set", &template, "switch"]);
    if !exists {
        wt.arg("--create");
        if let Some(base) = fresh_remote_base(&repo) {
            wt.args(["--base", &base]);
        }
    }
    wt.args([branch.as_str(), "--no-cd", "--yes"]);
    check(&mut wt, "wt switch").map_err(fail)?;
    worktree_for(&repo, &branch).ok_or_else(|| fail(format!("wt made no worktree for {branch}")))
}

/// The system-prompt note that keeps a task's agent on the worktree and
/// branch Switchyard made for it. Switchyard learns a task's commits and
/// links only from its branch, so work an agent moves to a worktree of its
/// own disappears from the task.
fn task_note(worktree: &Path, branch: &str) -> String {
    let worktree = worktree.display();
    format!(
        "You are running as a Switchyard task. This task's worktree is {worktree}, on \
         branch {branch}; you start in it. Do all of this task's work there: edits, \
         commits, pushes, pull requests, and `switchyard link`. Do not create another \
         worktree or branch for this task, even when a skill or instruction says to make \
         one: this worktree is the task worktree such a workflow asks for, so reuse it. \
         Keep the branch name too; if a required check insists on another, rename it \
         in place with `git branch -m` rather than making a new branch. Switchyard \
         tracks the task's commits, issues and pull requests only on this branch."
    )
}

/// Mark the repository, and a worktree outside it, trusted in the Claude
/// profile's config (see `trust`), so the agent doesn't stop on Claude's
/// trust dialog. `env` is the profile's own environment, if any. A failure
/// is only shown: the dialog then asks as before.
fn trust_folders(env: &[(String, String)], repo: &Path, worktree: &Path) {
    let dir = env
        .iter()
        .find(|(k, _)| k == "CLAUDE_CONFIG_DIR")
        .map(|(_, v)| v.clone())
        .or_else(|| std::env::var("CLAUDE_CONFIG_DIR").ok());
    let config = claude_config(dir.as_deref());
    match trust_at(&config, &[repo.to_path_buf(), worktree.to_path_buf()]) {
        Ok(marked) => {
            for dir in marked {
                status(&format!("Trusting {} for Claude", dir.display()));
            }
        }
        Err(e) => eprintln!("\x1b[33mswitchyard: trusting the folder: {e}\x1b[0m"),
    }
}

/// `session` is the agent session to start (`mode` "new") or resume
/// ("resume"); None when the command gave none. A new session
/// starts on `prompt` when there is one. `harness` is `HARNESS` or
/// `HARNESS:MODEL` (see `split_harness`).
pub fn run(
    profile: &str,
    path: &str,
    name: &str,
    harness: &str,
    clone_url: &str,
    session: Option<(&str, &str)>,
    prompt: &str,
) -> ! {
    let dir = match prepare(path, name, clone_url) {
        Ok(worktree) => {
            // Move this process too: the agent it becomes is the tab's
            // program, and a split starts where the tab's program is.
            let _ = std::env::set_current_dir(&worktree);
            // Work gets amfaro's mise env (CLAUDE_CONFIG_DIR and
            // PI_CODING_AGENT_DIR); personal uses the login defaults. Read
            // the env rather than `mise -C … exec`, which would also run
            // the agent in ~/dev/src/amfaro instead of the worktree.
            let (harness, model) = split_harness(harness);
            let mut agent = Command::new(harness);
            // Both harnesses take --model (Claude an alias or ID, pi a
            // pattern), on a resume too.
            if let Some(model) = model {
                agent.args(["--model", model]);
            }
            // The inherited $PWD still names the tab's start directory.
            agent.env("PWD", &worktree);
            let mut env = Vec::new();
            if profile == "work" {
                match work_env() {
                    Ok(work) => env = work,
                    Err(e) => eprintln!("\x1b[31mswitchyard: work profile: {e}\x1b[0m"),
                }
            }
            agent.envs(env.iter().map(|(k, v)| (k, v)));
            // Spawned Claude tasks always run without permission prompts,
            // and in folders they trust.
            if harness == "claude" {
                agent.arg("--dangerously-skip-permissions");
                trust_folders(&env, &expand(path), &worktree);
            }
            // On resume too: an older session never had the note.
            let branch = branch_and_slug(name).0;
            agent
                .arg("--append-system-prompt")
                .arg(task_note(&worktree, &branch));
            // `switchyard link` run from another branch also links these
            // (see the cli's `link`).
            agent
                .env("SWITCHYARD_TASK_WORKTREE", &worktree)
                .env("SWITCHYARD_TASK_BRANCH", &branch);
            let mut resuming = false;
            if let Some((id, mode)) = session {
                let here = worktree.display().to_string();
                if mode == "resume" && !session_exists_here(profile, harness, &here, id) {
                    eprintln!(
                        "\x1b[33mswitchyard: agent session {id} is gone; starting a new one\x1b[0m"
                    );
                    agent.args(["--session-id", &new_session_id()]);
                } else if mode == "resume" && harness == "claude" {
                    resuming = true;
                    agent.args(["--resume", id]);
                } else {
                    // Pi's --session-id reopens the directory's session with
                    // that ID or creates it; Claude's starts a new one.
                    resuming = mode == "resume";
                    agent.args(["--session-id", id]);
                }
            }
            // Both harnesses start interactively with a positional prompt as
            // the first message; `--` keeps one starting with - as text. Pi
            // reads an argument starting with @ as a file even after `--`,
            // so a leading space keeps that one text too.
            if !prompt.is_empty() && !resuming {
                let prompt = if harness == "pi" && prompt.starts_with('@') {
                    format!(" {prompt}")
                } else {
                    prompt.to_string()
                };
                agent.arg("--").arg(prompt);
            }
            // The agent replaces this process, so quitting it closes the tab.
            let e = agent.current_dir(&worktree).exec();
            eprintln!("\x1b[31m{harness}: {e}\x1b[0m");
            worktree
        }
        Err((e, dir)) => {
            eprintln!("\x1b[31mswitchyard: {e}\x1b[0m");
            dir
        }
    };
    // Only reached on failure: keep a shell so the error stays readable.
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".into());
    let err = Command::new(&shell)
        .current_dir(&dir)
        .env("PWD", &dir)
        .exec();
    eprintln!("switchyard: {shell}: {err}");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_note_names_the_worktree_and_branch() {
        let note = task_note(Path::new("/r/.worktrees/x"), "fix/x");
        assert!(note.contains("worktree is /r/.worktrees/x, on branch fix/x"));
        assert!(note.contains("Do not create another worktree or branch"));
    }
}
