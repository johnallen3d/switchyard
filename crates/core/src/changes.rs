//! What a task has changed: its diff against the base branch and its local
//! commits, for the tab picker's preview (ctrl-l in Inbox).
//!
//! `fetch` runs on the config host for a tab (host, session, window), as
//! `transcript::fetch` does: the task registry's worktree for the tab's
//! task, else the agent pane's directory, then `local` on the agent's host:
//! here, or there with `switchyard diff --local DIR` over SSH.
//!
//! `local` reads the checkout with Git, taking no optional locks so it
//! never contends with the agent's own Git commands, and with no external
//! diff tool and the plain `a/` and `b/` prefixes, whatever Git's config. The base is
//! whichever of `main`, `master`, `origin/HEAD`'s branch and origin's `main`
//! and `master` HEAD has the fewest commits beyond, so a stale local `main`
//! doesn't pull in the upstream work since; the diff runs from where the
//! branch left it (the merge base) to the working tree, so uncommitted work
//! counts, with untracked files as new ones. It returns
//! `{base, branch, dirty, commits: [{sha, subject, at}], files: [{path,
//! added, removed, binary, untracked}], patch, truncated}`, commits newest
//! first and `dirty` the number of paths `git status` lists; or `{error}`.
//!
//! `pull_request` gives a GitHub pull request's changes in the same shape,
//! from `gh` rather than a checkout: its base and head branches, its
//! commits, files and patch (`dirty` 0), for the Work view's preview.

use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

use serde_json::{Value, json};

use crate::item_actions::run_bounded;
use crate::rex::{home, host_bash, local_label, sh_quote};
use crate::sessions::work_env;
use crate::tasks::{in_window, load_tasks, rfc3339_secs};
use crate::transcript::{agent_pane, unhex};

/// The most patch text to return, so the picker's preview stays quick.
const PATCH: usize = 128 * 1024;
/// The most commits to list.
const COMMITS: usize = 200;
/// The most untracked files to diff as new ones.
const UNTRACKED: usize = 50;

/// What the task in the tab (`target` as the picker names it: `-` for this
/// host, else the hex-encoded label) has changed (see the module docs).
#[must_use]
pub fn fetch(target: &str, session: &str, window: &str) -> Value {
    let host = if target == "-" {
        local_label()
    } else {
        unhex(target)
    };
    fetch_inner(&host, session, window).unwrap_or_else(|e| json!({ "error": e }))
}

fn fetch_inner(host: &str, session: &str, window: &str) -> Result<Value, String> {
    let dir = task_dir(host, session, window)?;
    if host == local_label() {
        return Ok(local(&dir));
    }
    let out = host_bash(host, &format!("switchyard diff --local {}", sh_quote(&dir)))
        .ok_or_else(|| format!("can't reach {host}"))?;
    if !out.status.success() {
        return Err(format!(
            "no diff reader on {host} (install matching Switchyard there)"
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|_| format!("unreadable diff from {host}"))
}

/// The checkout of the task in the tab (`window` in `session` on `host`):
/// the task registry's worktree for it, else the agent pane's directory.
///
/// # Errors
///
/// Fails when neither is known, or the agent pane can't be looked up.
pub fn task_dir(host: &str, session: &str, window: &str) -> Result<String, String> {
    let worktree = load_tasks().ok().and_then(|tasks| {
        tasks
            .into_iter()
            .find(|t| t["ended_at"].is_null() && in_window(t, window))
            .and_then(|t| t["worktree"].as_str().map(String::from))
            .filter(|w| !w.is_empty())
    });
    match worktree {
        Some(w) => Ok(w),
        None => Ok(agent_pane(host, session, window)?["cwd"]
            .as_str()
            .filter(|d| !d.is_empty())
            .ok_or("no folder known for this tab")?
            .to_string()),
    }
}

/// How long one gh call for a pull request may take.
const GH_TIMEOUT: Duration = Duration::from_secs(30);

/// The changes of the GitHub pull request `reference` names
/// (`owner/repo#N`), as `gh` sees them under `profile` (see the module
/// docs).
#[must_use]
pub fn pull_request(reference: &str, profile: &str) -> Value {
    pr_inner(reference, profile).unwrap_or_else(|e| json!({ "error": e }))
}

fn pr_inner(reference: &str, profile: &str) -> Result<Value, String> {
    let (repo, number) = reference
        .split_once('#')
        .filter(|(r, n)| r.contains('/') && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(|| format!("{reference}: not an owner/repo#N reference"))?;
    let env = if profile == "work" {
        work_env()?
    } else {
        Vec::new()
    };
    let gh = |args: &[&str]| {
        let mut cmd = Command::new("gh");
        cmd.args(args)
            .env("GH_PROMPT_DISABLED", "1")
            .envs(env.iter().cloned())
            .current_dir(home());
        run_bounded(&mut cmd, "gh", GH_TIMEOUT)
    };
    let view = gh(&[
        "pr",
        "view",
        number,
        "-R",
        repo,
        "--json",
        "baseRefName,headRefName,commits,files",
    ])?;
    let patch = gh(&["pr", "diff", number, "-R", repo, "--color", "never"])?;
    let v: Value = serde_json::from_str(view.trim()).map_err(|e| format!("gh pr view: {e}"))?;
    Ok(pr_value(&v, patch))
}

/// A pull request's changes from `gh pr view`'s JSON and `gh pr diff`'s
/// patch: commits newest first, as `log` gives a checkout's.
fn pr_value(v: &Value, mut patch: String) -> Value {
    let list = |k: &str| v[k].as_array().cloned().unwrap_or_default();
    let mut commits: Vec<Value> = list("commits")
        .iter()
        .map(|c| {
            let sha = c["oid"].as_str().unwrap_or("");
            json!({
                "sha": sha.get(..7).unwrap_or(sha),
                "at": c["committedDate"].as_str().and_then(rfc3339_secs).unwrap_or(0),
                "subject": c["messageHeadline"].as_str().unwrap_or(""),
            })
        })
        .collect();
    commits.reverse();
    commits.truncate(COMMITS);
    let files: Vec<Value> = list("files")
        .iter()
        .map(|f| {
            json!({
                "path": f["path"].as_str().unwrap_or(""),
                "added": f["additions"].as_u64().unwrap_or(0),
                "removed": f["deletions"].as_u64().unwrap_or(0),
                "binary": false,
                "untracked": false,
            })
        })
        .collect();
    let truncated = truncate(&mut patch);
    json!({
        "base": v["baseRefName"].as_str().unwrap_or(""),
        "branch": v["headRefName"].as_str().unwrap_or(""),
        "dirty": 0,
        "commits": commits,
        "files": files,
        "patch": patch,
        "truncated": truncated,
    })
}

/// Cut `patch` to at most `PATCH` bytes, on a whole line; whether it was.
fn truncate(patch: &mut String) -> bool {
    if patch.len() <= PATCH {
        return false;
    }
    let mut end = PATCH;
    while !patch.is_char_boundary(end) {
        end -= 1;
    }
    // End on a whole line.
    end = patch[..end].rfind('\n').map_or(end, |i| i + 1);
    patch.truncate(end);
    true
}

/// The changes in the checkout at `dir` on this host (see the module docs).
#[must_use]
pub fn local(dir: &str) -> Value {
    read(Path::new(dir)).unwrap_or_else(|e| json!({ "error": e }))
}

fn git(dir: &Path, args: &[&str]) -> Option<Output> {
    Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()
}

/// A Git command's output when it succeeds.
fn git_ok(dir: &Path, args: &[&str]) -> Option<String> {
    git(dir, args)
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
}

fn read(dir: &Path) -> Result<Value, String> {
    if !dir.is_dir() {
        return Err(format!("no folder {}", dir.display()));
    }
    if git_ok(dir, &["rev-parse", "--git-dir"]).is_none() {
        return Err("not a Git checkout".into());
    }
    let base = base_branch(dir).ok_or("no main, master or origin/HEAD to diff against")?;
    let branch = git_ok(dir, &["branch", "--show-current"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let merge_base = git_ok(dir, &["merge-base", &base, "HEAD"])
        .map(|s| s.trim().to_string())
        .ok_or_else(|| format!("no common history with {base}"))?;
    let dirty = git_ok(dir, &["status", "--porcelain"])
        .map_or(0, |s| s.lines().filter(|l| !l.is_empty()).count());
    let commits = commits(dir, &base);
    let mut files = numstat(dir, &merge_base);
    let mut patch = git_ok(
        dir,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "-M",
            &merge_base,
            "--",
        ],
    )
    .unwrap_or_default();
    let untracked =
        git_ok(dir, &["ls-files", "--others", "--exclude-standard", "-z"]).unwrap_or_default();
    for path in untracked
        .split('\0')
        .filter(|p| !p.is_empty())
        .take(UNTRACKED)
    {
        if patch.len() > PATCH {
            break;
        }
        let Some(out) = git(
            dir,
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--src-prefix=a/",
                "--dst-prefix=b/",
                "--no-index",
                "--",
                "/dev/null",
                path,
            ],
        ) else {
            continue;
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let added = text
            .lines()
            .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
            .count();
        files.push(json!({
            "path": path,
            "added": added,
            "removed": 0,
            "binary": text
                .lines()
                .any(|l| l.starts_with("Binary files ") && l.ends_with(" differ")),
            "untracked": true,
        }));
        patch.push_str(&text);
    }
    let truncated = truncate(&mut patch);
    Ok(json!({
        "base": base,
        "branch": branch,
        "dirty": dirty,
        "commits": commits,
        "files": files,
        "patch": patch,
        "truncated": truncated,
    }))
}

/// The branch to diff against: of `main`, `master`, `origin/HEAD`'s branch
/// and origin's `main` and `master`, the one HEAD has the fewest commits
/// beyond, so a stale local `main` loses to the origin branch the task
/// started from; on a tie, the first of them. `BASE_SH` is the same in shell.
fn base_branch(dir: &Path) -> Option<String> {
    let origin_head = git_ok(
        dir,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty());
    let mut best: Option<(u64, String)> = None;
    for b in ["main", "master"]
        .into_iter()
        .map(String::from)
        .chain(origin_head)
        .chain(
            ["origin/main", "origin/master"]
                .into_iter()
                .map(String::from),
        )
    {
        let Some(ahead) = git_ok(dir, &["rev-list", "--count", &format!("{b}..HEAD")])
            .and_then(|s| s.trim().parse::<u64>().ok())
        else {
            continue;
        };
        if best.as_ref().is_none_or(|(n, _)| ahead < *n) {
            best = Some((ahead, b));
        }
    }
    best.map(|(_, b)| b)
}

/// Shell that sets `base` the way `base_branch` picks it, in the current
/// checkout, or leaves it empty, for the review script.
pub(crate) const BASE_SH: &str = r#"base=
best=
for b in main master $(git symbolic-ref -q --short refs/remotes/origin/HEAD) origin/main origin/master; do
  n=$(git rev-list --count "$b..HEAD" 2>/dev/null) || continue
  if [ -z "$base" ] || [ "$n" -lt "$best" ]; then base=$b; best=$n; fi
done
"#;

/// The commits on HEAD that `base` lacks, newest first.
fn commits(dir: &Path, base: &str) -> Vec<Value> {
    let range = format!("{base}..HEAD");
    let max = format!("--max-count={COMMITS}");
    git_ok(
        dir,
        &[
            "log",
            "--no-color",
            &max,
            "--format=%h%x09%ct%x09%s",
            &range,
        ],
    )
    .unwrap_or_default()
    .lines()
    .filter_map(|l| {
        let mut f = l.splitn(3, '\t');
        let sha = f.next()?;
        let at: u64 = f.next()?.parse().ok()?;
        Some(json!({ "sha": sha, "at": at, "subject": f.next().unwrap_or("") }))
    })
    .collect()
}

/// Each tracked file changed since `merge_base`, in the working tree.
fn numstat(dir: &Path, merge_base: &str) -> Vec<Value> {
    git_ok(dir, &["diff", "--numstat", "-M", merge_base, "--"])
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut f = l.splitn(3, '\t');
            let added = f.next()?;
            let removed = f.next()?;
            let path = f.next()?;
            Some(json!({
                "path": path,
                "added": added.parse::<u64>().unwrap_or(0),
                "removed": removed.parse::<u64>().unwrap_or(0),
                "binary": added == "-",
                "untracked": false,
            }))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn a_branch_shows_its_commits_and_everything_since_main() {
        let tmp = std::env::temp_dir().join(format!("switchyard-changes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        run(&tmp, &["init", "-q", "-b", "main"]);
        std::fs::write(tmp.join("a.txt"), "one\n").unwrap();
        run(&tmp, &["add", "."]);
        run(&tmp, &["commit", "-qm", "first"]);
        run(&tmp, &["switch", "-qc", "feature/x"]);
        std::fs::write(tmp.join("a.txt"), "one\ntwo\n").unwrap();
        run(&tmp, &["commit", "-qam", "add two"]);
        // Uncommitted and untracked work counts too.
        std::fs::write(tmp.join("a.txt"), "uno\ntwo\n").unwrap();
        // A text file that mentions binaries is still text.
        std::fs::write(tmp.join("new.txt"), "fresh\nBinary files a and b differ\n").unwrap();
        run(&tmp, &["config", "diff.mnemonicPrefix", "true"]);

        let v = local(tmp.to_str().unwrap());
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(v["error"], Value::Null, "{v}");
        assert_eq!(v["base"], "main");
        assert_eq!(v["branch"], "feature/x");
        assert_eq!(v["dirty"], 2);
        assert_eq!(v["commits"][0]["subject"], "add two");
        assert_eq!(v["commits"].as_array().unwrap().len(), 1);
        let files = v["files"].as_array().unwrap();
        assert_eq!(files[0]["path"], "a.txt");
        assert_eq!(
            (files[0]["added"].as_u64(), files[0]["removed"].as_u64()),
            (Some(2), Some(1))
        );
        assert_eq!(files[1]["path"], "new.txt");
        assert_eq!(files[1]["untracked"], true);
        assert_eq!(files[1]["binary"], false);
        let patch = v["patch"].as_str().unwrap();
        assert!(patch.contains("+uno") && patch.contains("+two") && patch.contains("+fresh"));
        assert!(patch.contains("diff --git a/a.txt b/a.txt"), "{patch}");
        assert!(patch.contains("diff --git a/new.txt b/new.txt"), "{patch}");
        assert_eq!(v["truncated"], false);
    }

    #[test]
    fn a_stale_local_main_loses_to_origins() {
        let tmp = std::env::temp_dir().join(format!("switchyard-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        run(&tmp, &["init", "-q", "-b", "main"]);
        std::fs::write(tmp.join("a.txt"), "one\n").unwrap();
        run(&tmp, &["add", "."]);
        run(&tmp, &["commit", "-qm", "first"]);
        // Upstream moves on while local main stays put.
        run(&tmp, &["switch", "-qc", "up"]);
        std::fs::write(tmp.join("up.txt"), "upstream\n").unwrap();
        run(&tmp, &["add", "."]);
        run(&tmp, &["commit", "-qm", "upstream work"]);
        run(&tmp, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        // The task branches from origin/main and changes one file.
        run(&tmp, &["switch", "-qc", "chore/x", "origin/main"]);
        std::fs::write(tmp.join("a.txt"), "one\ntwo\n").unwrap();

        let v = local(tmp.to_str().unwrap());
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(v["base"], "origin/main", "{v}");
        assert_eq!(v["commits"].as_array().unwrap().len(), 0);
        let files = v["files"].as_array().unwrap();
        assert_eq!(files.len(), 1, "{v}");
        assert_eq!(files[0]["path"], "a.txt");
    }

    #[test]
    fn a_pull_request_reads_as_a_branch_does() {
        let v = json!({
            "baseRefName": "main",
            "headRefName": "feature/x",
            "commits": [
                {"oid": "aaaaaaa1111", "committedDate": "2026-11-15T10:00:00Z", "messageHeadline": "first"},
                {"oid": "bbbbbbb2222", "committedDate": "2026-11-16T10:00:00Z", "messageHeadline": "second"}
            ],
            "files": [{"path": "a.rs", "additions": 3, "deletions": 1}]
        });
        let out = pr_value(&v, "diff --git a/a.rs b/a.rs\n".into());
        assert_eq!(
            (out["base"].as_str(), out["branch"].as_str()),
            (Some("main"), Some("feature/x"))
        );
        assert_eq!(out["commits"][0]["subject"], "second");
        assert_eq!(out["commits"][0]["sha"], "bbbbbbb");
        assert!(out["commits"][0]["at"].as_u64().unwrap() > 0);
        assert_eq!(out["files"][0]["added"], 3);
        assert_eq!(out["truncated"], false);
        assert!(pull_request("not-a-ref", "")["error"].is_string());
    }

    #[test]
    fn a_folder_outside_git_says_so() {
        let tmp = std::env::temp_dir();
        let v = local(tmp.join("switchyard-no-such-dir").to_str().unwrap());
        assert!(v["error"].as_str().unwrap().starts_with("no folder"));
    }
}
