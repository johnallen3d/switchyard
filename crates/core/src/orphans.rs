//! Orphan worktrees: linked worktrees of catalog projects that no task
//! owns, such as one an agent made by hand outside any Switchyard task,
//! which no archive would ever remove.
//!
//! switchyard-server scans for them every `SCAN_SECS` (`record_orphans`):
//! each linked worktree of a catalog project, on the config host and each
//! dev host, that isn't the worktree of a task the check follows (open,
//! closed with its worktree left, or an orphan already recorded) and is
//! older than `GRACE_MINUTES`, so a launch in progress isn't one. Each
//! becomes an ended task, `ended_reason: "orphaned"`, with a made-up window
//! ID (`orphan:HOST:PATH`) for actions to address it. The regular check
//! then reads its uncommitted files and merge state like any task's, the
//! tab picker lists it under Orphans, ctrl-x removes it with Worktrunk as
//! for a closed task's worktree, and `actions::auto_archive` removes a
//! clean, landed one on its own (see `auto_removable`).
//!
//! The primary checkout, bare and prunable entries, and locked worktrees
//! are never orphans.

use std::collections::HashSet;
use std::fmt::Write as _;

use crate::folders::home_relative;
use crate::hosts::{catalog, dev_hosts};
use crate::rex::{host_bash, local_label, now, rex_command};
use crate::tasks::{checked, update_tasks};

/// How often switchyard-server scans for orphans.
pub const SCAN_SECS: u64 = 600;

/// How old a worktree must be (by its `.git` file) to count as an orphan.
const GRACE_MINUTES: u32 = 10;

/// Bash that reads catalog paths (`~/` allowed) from the `RSP_REPOS`
/// heredoc that follows it and prints `REPO<TAB>WORKTREE<TAB>BRANCH` for
/// each linked worktree older than the grace period (BRANCH empty for a
/// detached HEAD). REPO is the primary checkout as Git reports it.
const SCAN_SH: &str = r#"while IFS= read -r repo; do
  case $repo in "~/"*) repo=$HOME/${repo#"~/"} ;; esac
  list=$(git -C "$repo" worktree list --porcelain 2>/dev/null) || continue
  main= wt= branch= skip=
  while IFS= read -r line; do
    case $line in
      "worktree "*) wt=${line#worktree } branch= skip= ;;
      "branch refs/heads/"*) branch=${line#branch refs/heads/} ;;
      bare|prunable*|locked*) skip=1 ;;
      "")
        if [ -z "$main" ]; then main=$wt; continue; fi
        [ -n "$skip" ] && continue
        [ -d "$wt" ] || continue
        [ -n "$(find "$wt/.git" -maxdepth 0 -mmin +GRACE 2>/dev/null)" ] || continue
        printf '%s\t%s\t%s\n' "$main" "$wt" "$branch" ;;
    esac
  done <<<"$list"$'\n'
done <<'RSP_REPOS'
"#;

/// A linked worktree found by `scan`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub host: String,
    /// The primary checkout.
    pub repo: String,
    pub worktree: String,
    /// Empty for a detached HEAD.
    pub branch: String,
}

/// The hosts to scan: the config host and each dev host.
fn hosts() -> Vec<String> {
    let mut hosts = vec![local_label()];
    for h in dev_hosts() {
        if !hosts.contains(&h) {
            hosts.push(h);
        }
    }
    hosts
}

/// The linked worktrees of `repos` on `host` past the grace period; None
/// when the host can't be reached.
fn scan_host(host: &str, repos: &[String]) -> Option<Vec<Found>> {
    let mut script = SCAN_SH.replace("GRACE", &GRACE_MINUTES.to_string());
    for repo in repos {
        let _ = writeln!(script, "{repo}");
    }
    script.push_str("RSP_REPOS\n");
    let out = host_bash(host, &script).filter(|o| o.status.success())?;
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let mut f = l.split('\t');
                let (repo, worktree) = (f.next()?, f.next()?);
                Some(Found {
                    host: host.to_string(),
                    repo: repo.to_string(),
                    worktree: worktree.to_string(),
                    branch: f.next().unwrap_or("").to_string(),
                })
            })
            .collect(),
    )
}

/// Every linked worktree of a catalog project past the grace period, on
/// each host that could be reached, and the hosts that couldn't. Reads
/// only.
///
/// # Errors
///
/// Fails when the catalog can't be read.
pub fn scan() -> Result<(Vec<Found>, Vec<String>), String> {
    let repos: Vec<String> = catalog()?
        .into_iter()
        .filter(|p| p.worktree)
        .map(|p| p.path)
        .collect();
    let results: Vec<(String, Option<Vec<Found>>)> = std::thread::scope(|s| {
        let handles: Vec<_> = hosts()
            .into_iter()
            .map(|host| {
                let repos = &repos;
                s.spawn(move || {
                    let found = scan_host(&host, repos);
                    (host, found)
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    let mut found = Vec::new();
    let mut unreachable = Vec::new();
    for (host, f) in results {
        match f {
            Some(f) => found.extend(f),
            None => unreachable.push(host),
        }
    }
    Ok((found, unreachable))
}

/// A worktree path for comparing: without a trailing slash.
fn norm(path: &str) -> &str {
    path.trim_end_matches('/')
}

/// The worktrees in `found` that no task in `tasks` the check follows owns
/// (matched by host and path).
#[must_use]
pub fn unowned<'a>(tasks: &[serde_json::Value], found: &'a [Found]) -> Vec<&'a Found> {
    let s = |t: &serde_json::Value, k: &str| t[k].as_str().unwrap_or("").to_string();
    let owned: HashSet<(String, String)> = tasks
        .iter()
        .filter(|t| checked(t))
        .map(|t| (s(t, "host"), norm(&s(t, "worktree")).to_string()))
        .collect();
    found
        .iter()
        .filter(|f| !owned.contains(&(f.host.clone(), norm(&f.worktree).to_string())))
        .collect()
}

/// The registry record for orphan `f`, ended now.
fn record(f: &Found, projects: &[crate::hosts::Project], stamp: u64) -> serde_json::Value {
    let base = |p: &str| norm(p).rsplit('/').next().unwrap_or("").to_string();
    let project = projects
        .iter()
        .find(|p| home_relative(norm(&p.path)) == home_relative(norm(&f.repo)));
    serde_json::json!({
        "host": f.host,
        "session_id": "",
        "window_id": format!("orphan:{}:{}", f.host, norm(&f.worktree)),
        "project": project.map_or_else(|| base(&f.repo), |p| p.name.clone()),
        "profile": project.map_or("", |p| p.profile.as_str()),
        "repo": f.repo,
        "name": base(&f.worktree),
        "branch": f.branch,
        "worktree": f.worktree,
        "orphan": true,
        "created_at": stamp,
        "ended_at": stamp,
        "ended_reason": "orphaned",
    })
}

/// Scan for orphans and record each new one in the registry (see the
/// module doc). Returns each new orphan as `HOST:PATH`, and the hosts that
/// couldn't be reached.
///
/// # Errors
///
/// Fails when the catalog or registry can't be read or saved.
pub fn record_orphans() -> Result<(Vec<String>, Vec<String>), String> {
    let (found, unreachable) = scan()?;
    let projects = catalog()?;
    let stamp = now();
    let new = update_tasks(|tasks| {
        let fresh: Vec<serde_json::Value> = unowned(tasks, &found)
            .into_iter()
            .map(|f| record(f, &projects, stamp))
            .collect();
        let labels = fresh
            .iter()
            .map(|t| {
                format!(
                    "{}:{}",
                    t["host"].as_str().unwrap_or(""),
                    t["worktree"].as_str().unwrap_or("")
                )
            })
            .collect();
        tasks.extend(fresh);
        Ok(labels)
    })?;
    Ok((new, unreachable))
}

/// Whether reconciled task `t` is an orphan due to be removed on its own:
/// its worktree is still there, separate from the primary checkout, on a
/// branch (not a detached HEAD), with nothing uncommitted, and its work has
/// landed: the branch head is in the default branch (local or origin's)
/// with commits of its own (`branch_merged` is `merged`, not `new`), or a
/// PR GitHub reported merged has the branch head as its head. A Rex pane
/// sitting in it is checked separately (see `pane_cwds`), as that needs
/// the host.
#[must_use]
pub fn auto_removable(t: &serde_json::Value) -> bool {
    let s = |k: &str| t[k].as_str().unwrap_or("");
    let own_worktree = !s("worktree").is_empty() && norm(s("worktree")) != norm(s("repo"));
    let landed = match s("branch_merged") {
        "merged" => true,
        "new" => false,
        _ => crate::links::pr_merged_at(t, s("branch_head")),
    };
    s("ended_reason") == "orphaned"
        && t["worktree_left"].as_bool() == Some(true)
        && own_worktree
        && !s("branch").is_empty()
        && t["dirty_count"].as_u64() == Some(0)
        && landed
}

/// Lua for `rex do` that lists each pane's working directories (its
/// foreground process's and its shell's), a line each.
const CWDS_LUA: &str = r#"
local T = "com.superlogical.terminal"
local out = {}
local list = rex.call("session.list")
for _, s in ipairs((list and list.sessions) or {}) do
  rex.session.attach{ session_id = s.session_id }
  local view = rex.session.view{ session_id = s.session_id }
  for _, w in ipairs((view and view.windows) or {}) do
    for _, l in ipairs(w.layers or {}) do
      for _, b in ipairs(l.blocks or {}) do
        local p = rex.block.call(T, "process", { session_id = s.session_id, block_id = b.block_id }) or {}
        for _, k in ipairs({ "foreground", "child" }) do
          if p[k] and p[k].cwd and p[k].cwd ~= "" then out[#out + 1] = p[k].cwd end
        end
      end
    end
  end
end
return table.concat(out, "\n")
"#;

/// The working directories of every Rex pane on `host`, or None when they
/// can't be listed (then nothing there may be removed).
#[must_use]
pub fn pane_cwds(host: &str) -> Option<Vec<String>> {
    let mut cmd = rex_command();
    cmd.arg("--autostart=false");
    if host != local_label() {
        cmd.args(["-S", host]);
    }
    let out = cmd
        .args(["do", "-e", CWDS_LUA])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let reply: String = serde_json::from_slice(&out.stdout).ok()?;
    Some(reply.lines().map(String::from).collect())
}

/// Whether any of `cwds` is `worktree` or inside it.
#[must_use]
pub fn occupied(cwds: &[String], worktree: &str) -> bool {
    let wt = norm(worktree);
    cwds.iter().any(|c| {
        let c = norm(c);
        c == wt || c.strip_prefix(wt).is_some_and(|rest| rest.starts_with('/'))
    })
}
