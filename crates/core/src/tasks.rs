//! The task registry. It lives on the config host, the only one that
//! reaches every Rex host: a JSON array in $XDG_STATE_HOME/rex/spawn-tasks.json,
//! one object per spawned task (see `launch` for the fields). Rex has no
//! custom metadata for sessions or tabs, so this is what maps a tab back to
//! its project, branch and worktree. Ended tasks are kept a while so the
//! inbox can show recent finishes, then pruned. Saves are atomic and synced
//! to disk, and keep the previous file as `spawn-tasks.json.bak`, which
//! readers fall back to when the registry won't parse (see `read_at`).

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::hash::BuildHasher;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::links::{
    FIZZY_BOARD_SH, Lookup, Pr, Tracker, Unlinked, add_link, add_live, branch_number, find_prs,
    github_repo, parse_ref, pr_key, set_agent_links, set_derived_items, set_derived_pr,
    tracker_or_fizzy, unlink,
};
use crate::rex::{home, host_bash, local_label, log, now, rex_command};
use crate::sessions::newest_session;

/// How long ended tasks stay in the registry, and so can be resumed from the
/// inbox: Claude's default transcript cleanup (`cleanupPeriodDays`).
const ENDED_KEEP_SECS: u64 = 30 * 24 * 60 * 60;

/// How long a new task may go without its worktree: `task launch` records
/// it before `task run`, in the new tab, fetches (or clones) and creates
/// the worktree, so a missing one only ends the task after this.
const LAUNCH_GRACE_SECS: u64 = 10 * 60;

/// Whether an open task is still within its launch grace at `now`, when a
/// missing worktree may not have been created yet.
fn launching(task: &serde_json::Value, now: u64) -> bool {
    task["created_at"]
        .as_u64()
        .is_some_and(|created| now.saturating_sub(created) < LAUNCH_GRACE_SECS)
}

pub fn tasks_path() -> PathBuf {
    std::env::var("XDG_STATE_HOME")
        .map_or_else(|_| home().join(".local/state"), PathBuf::from)
        .join("rex/spawn-tasks.json")
}

/// The task registry's records.
///
/// # Errors
///
/// Fails when the registry exists but can't be read or recovered.
pub fn load_tasks() -> Result<Vec<serde_json::Value>, String> {
    load_at(&tasks_path())
}

fn load_at(path: &Path) -> Result<Vec<serde_json::Value>, String> {
    read_at(path).map(|(tasks, _)| tasks)
}

/// Log a registry storage problem to the spawn log, but not from tests,
/// which damage registries on purpose.
fn note(line: &str) {
    if !cfg!(test) {
        log(line);
    }
}

/// The last good registry: `save_at` links the file it replaces here, so it
/// is at most one save behind.
fn backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
}

/// The registry at `path`, and whether it came from the backup because the
/// file itself won't parse. Saves are atomic, so a reader never sees half a
/// file; one that won't parse was damaged, such as by a crash before the
/// disk had it. A missing file is an empty registry, not a reason to bring
/// back the backup.
fn read_at(path: &Path) -> Result<(Vec<serde_json::Value>, bool), String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((Vec::new(), false)),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let err = match serde_json::from_slice(&bytes) {
        Ok(tasks) => return Ok((tasks, false)),
        Err(e) => format!("{}: {e}", path.display()),
    };
    let backup = backup_path(path);
    match std::fs::read(&backup)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
    {
        Some(tasks) => {
            note(&format!("{err}; using {}", backup.display()));
            Ok((tasks, true))
        }
        None => Err(err),
    }
}

/// Replace the registry atomically, so a reader never sees half a file, and
/// durably: the new file and its directory entry reach the disk before this
/// returns, so a crash leaves the old registry or the new one, not an empty
/// file. With `backup`, the file replaced becomes the backup; pass false when
/// it is the damaged file being repaired, so the good backup stays.
fn save_at(path: &Path, tasks: &[serde_json::Value], backup: bool) -> Result<(), String> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = path.with_extension(format!("json.{}", std::process::id()));
    let mut text = serde_json::to_string_pretty(tasks).map_err(|e| e.to_string())?;
    text.push('\n');
    write_synced(&tmp, text.as_bytes()).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", tmp.display())
    })?;
    if backup && let Err(e) = link_over(path, &backup_path(path)) {
        // The save matters more than its backup.
        if e.kind() != io::ErrorKind::NotFound {
            note(&format!("could not back up {}: {e}", path.display()));
        }
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
    // Some filesystems refuse to sync a directory; the rename has happened.
    let _ = File::open(dir).and_then(|d| d.sync_all());
    Ok(())
}

/// Write `bytes` to a new file at `path` and flush it to the disk.
fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Make `to` another name for `from`'s file, replacing what `to` was in one
/// step, so `to` is always whole. A hard link: `from` is about to be renamed
/// over, and the link keeps its old contents without copying them.
fn link_over(from: &Path, to: &Path) -> io::Result<()> {
    let tmp = to.with_extension(format!("tmp.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    std::fs::hard_link(from, &tmp)?;
    std::fs::rename(&tmp, to).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Read-modify-write the registry under its lock. `f` changes the tasks it
/// is given, and they are saved only if they changed. Every writer goes
/// through here, so two of them can't interleave: neither overwrites the
/// other's save with a copy read before it. `f` must not touch the registry
/// again (the lock is not reentrant); an error from `f` saves nothing.
///
/// # Errors
///
/// Fails when the registry can't be locked, read, or saved, or with `f`'s
/// error.
pub fn update_tasks<T>(
    f: impl FnOnce(&mut Vec<serde_json::Value>) -> Result<T, String>,
) -> Result<T, String> {
    update_at(&tasks_path(), f)
}

fn update_at<T>(
    path: &Path,
    f: impl FnOnce(&mut Vec<serde_json::Value>) -> Result<T, String>,
) -> Result<T, String> {
    let _lock = lock_at(path)?;
    let (mut tasks, recovered) = read_at(path)?;
    let before = serde_json::to_string(&tasks).map_err(|e| e.to_string())?;
    let out = f(&mut tasks)?;
    if recovered {
        // Repair the damaged file from the backup, keeping it to look at.
        let damaged = path.with_extension(format!("json.damaged.{}", now()));
        match link_over(path, &damaged) {
            Ok(()) => note(&format!(
                "restored {} from its backup; the damaged file is {}",
                path.display(),
                damaged.display()
            )),
            Err(e) => note(&format!(
                "restored {} from its backup; could not keep the damaged file: {e}",
                path.display()
            )),
        }
        save_at(path, &tasks, false)?;
    } else if serde_json::to_string(&tasks).map_err(|e| e.to_string())? != before {
        save_at(path, &tasks, true)?;
    }
    Ok(out)
}

/// An exclusive lock on the registry at `path`, held until the file is
/// dropped. It is an flock on a sibling file, so the CLI and the picker,
/// which are other processes, wait on it too.
fn lock_at(path: &Path) -> Result<File, String> {
    let lock = path.with_extension("json.lock");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock)
        .map_err(|e| format!("{}: {e}", lock.display()))?;
    file.lock()
        .map_err(|e| format!("{}: {e}", lock.display()))?;
    Ok(file)
}

/// What identifies a task: its host, window and start, as JSON so that
/// values of different types can't collide.
fn task_key(task: &serde_json::Value) -> String {
    format!(
        "{}\t{}\t{}",
        task["host"], task["window_id"], task["created_at"]
    )
}

/// Whether `a` and `b` are the same task: the same host, window and start.
fn same_task(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    task_key(a) == task_key(b)
}

/// `tasks` by `task_key`, for merges that would otherwise scan the registry
/// once per task.
fn by_key(tasks: &[serde_json::Value]) -> HashMap<String, &serde_json::Value> {
    tasks.iter().map(|t| (task_key(t), t)).collect()
}

/// Save `task` as the registry's record of its task, leaving the other
/// records as the registry has them now. For a writer that changes only its
/// own task (an archive), so it doesn't write back a copy of the rest that it
/// read earlier. The inbox state the registry has (pins, snoozes) is kept.
///
/// # Errors
///
/// Fails as `update_tasks` does.
pub fn save_record(task: &serde_json::Value) -> Result<(), String> {
    save_record_at(&tasks_path(), task)
}

fn save_record_at(path: &Path, task: &serde_json::Value) -> Result<(), String> {
    update_at(path, |tasks| {
        let mut one = [task.clone()];
        carry_inbox_state(&mut one, &by_key(tasks));
        let [record] = one;
        match tasks.iter().position(|t| same_task(t, task)) {
            Some(i) => tasks[i] = record,
            None => tasks.push(record),
        }
        Ok(())
    })
}

/// Record `handoff` on `task`'s entry in the registry (matched by
/// `same_task`), leaving every other record untouched.
///
/// # Errors
///
/// Fails as `update_tasks` does.
pub fn set_handoff(task: &serde_json::Value, handoff: &str) -> Result<(), String> {
    set_handoff_at(&tasks_path(), task, handoff)
}

fn set_handoff_at(path: &Path, task: &serde_json::Value, handoff: &str) -> Result<(), String> {
    update_at(path, |tasks| {
        if let Some(entry) = tasks.iter_mut().find(|t| same_task(t, task)) {
            entry["handoff"] = handoff.into();
        }
        Ok(())
    })
}

/// Whether `task` was spawned in `window`. Matched on the window alone:
/// Rex window IDs are unique across servers, but a host's label
/// (`hostname -s`) can change under a task when macOS takes its name from
/// the network, which would leave the task unreachable by host.
#[must_use]
pub fn in_window(task: &serde_json::Value, window: &str) -> bool {
    task["window_id"].as_str() == Some(window)
}

/// Add `task` to the registry.
///
/// # Errors
///
/// Fails as `update_tasks` does.
pub fn record_task(task: serde_json::Value) -> Result<(), String> {
    update_tasks(|tasks| {
        tasks.push(task);
        Ok(())
    })
}

/// Remove each of `refs` (see `links::unlink`) from the links of the open
/// tasks on this host whose worktree is `worktree`, the CLI's `unlink`.
/// Gives what happened for each ref; on a host without the registry, which
/// lives on the config host, every ref is `Absent`.
///
/// # Errors
///
/// Fails as `update_tasks` does.
pub fn unlink_task_refs(worktree: &Path, refs: &[&str]) -> Result<Vec<Unlinked>, String> {
    if crate::server::remote_url().is_some() {
        return crate::server::remote_unlink(&local_label(), &worktree.display().to_string(), refs);
    }
    let path = tasks_path();
    if !path.exists() {
        return Ok(refs.iter().map(|_| Unlinked::Absent).collect());
    }
    unlink_at(&path, &local_label(), &home(), worktree, refs)
}

/// Remove each of `refs` from the links of the open tasks on `host` whose
/// worktree is `worktree` (used by the control-plane RPC endpoint for remote
/// `switchyard unlink` callers).
///
/// # Errors
///
/// Fails as `update_tasks` does.
pub fn unlink_task_refs_on(
    host: &str,
    worktree: &str,
    refs: &[&str],
) -> Result<Vec<Unlinked>, String> {
    let path = tasks_path();
    if !path.exists() {
        return Ok(refs.iter().map(|_| Unlinked::Absent).collect());
    }
    if host == local_label() {
        let wt = Path::new(worktree);
        let canon = std::fs::canonicalize(wt).unwrap_or_else(|_| wt.to_path_buf());
        return unlink_at(&path, host, &home(), &canon, refs);
    }
    unlink_remote_at(&path, host, worktree, refs)
}

fn unlink_remote_at(
    path: &Path,
    host: &str,
    worktree: &str,
    refs: &[&str],
) -> Result<Vec<Unlinked>, String> {
    let want = worktree.trim_end_matches('/');
    let want_rel = crate::folders::home_relative(want);
    update_at(path, |tasks| {
        let mut mine: Vec<&mut serde_json::Value> = tasks
            .iter_mut()
            .filter(|t| t["ended_at"].is_null() && t["host"].as_str() == Some(host))
            .filter(|t| {
                let w = field(t, "worktree");
                let w = w.trim_end_matches('/');
                w == want || crate::folders::home_relative(w) == want_rel
            })
            .collect();
        Ok(refs
            .iter()
            .map(|r| {
                let outcomes: Vec<Unlinked> = mine.iter_mut().map(|t| unlink(t, r)).collect();
                if outcomes.contains(&Unlinked::Removed) {
                    Unlinked::Removed
                } else if outcomes.contains(&Unlinked::Derived) {
                    Unlinked::Derived
                } else {
                    Unlinked::Absent
                }
            })
            .collect())
    })
}

fn unlink_at(
    path: &Path,
    host: &str,
    home: &Path,
    worktree: &Path,
    refs: &[&str],
) -> Result<Vec<Unlinked>, String> {
    update_at(path, |tasks| {
        let mut mine: Vec<&mut serde_json::Value> = tasks
            .iter_mut()
            .filter(|t| t["ended_at"].is_null() && t["host"].as_str() == Some(host))
            .filter(|t| {
                let w = field(t, "worktree");
                let w = match w.strip_prefix("~/") {
                    Some(rest) => home.join(rest),
                    None => PathBuf::from(w),
                };
                std::fs::canonicalize(&w).unwrap_or(w) == worktree
            })
            .collect();
        Ok(refs
            .iter()
            .map(|r| {
                let outcomes: Vec<Unlinked> = mine.iter_mut().map(|t| unlink(t, r)).collect();
                if outcomes.contains(&Unlinked::Removed) {
                    Unlinked::Removed
                } else if outcomes.contains(&Unlinked::Derived) {
                    Unlinked::Derived
                } else {
                    Unlinked::Absent
                }
            })
            .collect())
    })
}

/// Every window on a host's Rex server with its most urgent agent status, as
/// `WINDOW_ID<TAB>STATE<TAB>UPDATED_AT<TAB>MSG` lines (the last three empty
/// for a tab with no status), after an `S<TAB>SESSION_ID` line for each
/// session whose windows were read. A tab shows its most urgent pane, as in
/// the tab picker (cmd+i, cmd+k).
const WINDOWS_SCRIPT: &str = r#"
local T = "com.superlogical.terminal"
local urgency = { idle = 1, done = 2, working = 3, error = 4, blocked = 5 }
local lines = {}
local function clean(s) return (tostring(s or ""):gsub("[\t\r\n]", " ")) end
local list = rex.call("session.list")
for _, s in ipairs((list and list.sessions) or {}) do
  -- Every session the server has, whether or not its view reads below.
  lines[#lines + 1] = "L\t" .. s.session_id
  rex.session.attach{ session_id = s.session_id }
  local view = rex.session.view{ session_id = s.session_id }
  if view and view.windows then lines[#lines + 1] = "S\t" .. s.session_id end
  for _, w in ipairs((view and view.windows) or {}) do
    local best
    for _, l in ipairs(w.layers or {}) do
      if l.kind == "tiled" then
        for _, b in ipairs(l.blocks or {}) do
          local st = rex.block.call(T, "program_status", { session_id = s.session_id, block_id = b.block_id })
          for _, r in ipairs((st and st.records) or {}) do
            if best == nil or (urgency[r.state] or 0) > (urgency[best.state] or 0) then best = r end
          end
        end
      end
    end
    best = best or {}
    lines[#lines + 1] = table.concat({ w.window_id, clean(best.state), clean(best.updated_at), clean(best.msg) }, "\t")
  end
end
return table.concat(lines, "\n")
"#;

/// A tab's live agent status: state, when it last changed (Unix seconds,
/// 0 when unknown) and the agent's message.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TabStatus {
    pub state: String,
    pub at: u64,
    pub msg: String,
}

/// A `WINDOW_ID<TAB>STATE<TAB>UPDATED_AT<TAB>MSG` line, as `WINDOWS_SCRIPT`
/// prints them and `host_rows.lua`'s W rows carry them after the W.
#[must_use]
pub fn parse_window_status(line: &str) -> (String, TabStatus) {
    let mut f = line.split('\t');
    let id = f.next().unwrap_or("").to_string();
    let state = f.next().unwrap_or("").to_string();
    let at = rfc3339_secs(f.next().unwrap_or("")).unwrap_or(0);
    let msg = f.next().unwrap_or("").to_string();
    (id, TabStatus { state, at, msg })
}

/// Give each open task its tab's live agent status (`status`, `status_at`,
/// `status_msg`) from `live`, by window ID; a task whose tab isn't there
/// keeps what it had.
pub fn apply_live<S: BuildHasher>(
    tasks: &mut [serde_json::Value],
    live: &HashMap<String, TabStatus, S>,
) {
    for task in tasks.iter_mut().filter(|t| t["ended_at"].is_null()) {
        if let Some(status) = task["window_id"].as_str().and_then(|id| live.get(id)) {
            task["status"] = status.state.clone().into();
            task["status_at"] = status.at.into();
            task["status_msg"] = status.msg.clone().into();
        }
    }
}

/// Seconds since the Unix epoch for an RFC 3339 timestamp such as
/// `2026-11-07T15:40:18.294391-05:00`, as Rex reports status changes.
#[must_use]
pub fn rfc3339_secs(s: &str) -> Option<u64> {
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let mut rest = &s[19..];
    if let Some(frac) = rest.strip_prefix('.') {
        rest = frac.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let offset = match rest.as_bytes().first()? {
        b'Z' | b'z' => 0,
        sign @ (b'+' | b'-') => {
            let oh: i64 = rest.get(1..3)?.parse().ok()?;
            let om: i64 = rest.get(4..6)?.parse().ok()?;
            (oh * 3600 + om * 60) * if *sign == b'-' { -1 } else { 1 }
        }
        _ => return None,
    };
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86400 + h * 3600 + mi * 60 + sec - offset).ok()
}

/// What a host's Rex server listed: every session it has, the sessions
/// whose windows it read, and those windows' live status by window ID.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HostWindows {
    pub all_sessions: std::collections::HashSet<String>,
    pub sessions: std::collections::HashSet<String>,
    pub tabs: HashMap<String, TabStatus>,
}

impl HostWindows {
    /// Parse `WINDOWS_SCRIPT`'s reply.
    #[must_use]
    pub fn parse(reply: &str) -> Self {
        let mut listed = Self::default();
        for line in reply.lines().filter(|l| !l.is_empty()) {
            if let Some(session) = line.strip_prefix("L\t") {
                listed.all_sessions.insert(session.to_string());
            } else if let Some(session) = line.strip_prefix("S\t") {
                listed.sessions.insert(session.to_string());
            } else {
                let (id, status) = parse_window_status(line);
                listed.tabs.insert(id, status);
            }
        }
        listed
    }

    /// Whether this listing shows the task's tab closed: only when it read
    /// the task's own session and that session has no such window. A
    /// session it didn't list (another or a fresh server, as a temporary
    /// `XDG_RUNTIME_DIR` reaches, or a failed view) proves nothing.
    #[must_use]
    pub fn tab_gone(&self, session_id: &str, window_id: &str) -> bool {
        !session_id.is_empty()
            && !window_id.is_empty()
            && self.sessions.contains(session_id)
            && !self.tabs.contains_key(window_id)
    }

    /// Whether this listing shows the task's whole session gone, as when a
    /// hidden task's block was its session's last and Rex dropped the
    /// session with it. A server without the session could be another or
    /// a fresh one (see `tab_gone`), so it counts only when the server also
    /// has one of `peers`, sessions the registry knows on this host,
    /// proving it is the server the task ran on.
    #[must_use]
    pub fn session_gone<'a>(
        &self,
        session_id: &str,
        mut peers: impl Iterator<Item = &'a str>,
    ) -> bool {
        !session_id.is_empty()
            && !self.all_sessions.contains(session_id)
            && peers.any(|p| p != session_id && self.all_sessions.contains(p))
    }
}

/// `host`'s Rex windows, or None when they can't be listed. It never starts
/// a server: a missing one is unreachable, not empty.
fn host_windows(host: &str) -> Option<HostWindows> {
    let mut cmd = rex_command();
    cmd.arg("--autostart=false");
    if host != local_label() {
        cmd.args(["-S", host]);
    }
    let out = cmd
        .args(["do", "-e", WINDOWS_SCRIPT])
        .stdin(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let reply: String = serde_json::from_slice(&out.stdout).ok()?;
    Some(HostWindows::parse(&reply))
}

/// Bash that sets `head` (the full branch head), `baseref` (the default
/// branch's first ref found), `baserefs` (all of them, space separated:
/// the local default branch and origin's, which may be ahead of it) and
/// `merged` for `$branch` in the checkout at `$path`: `merged` when the
/// head is in the default branch (local or origin's), `new` when it is but
/// the branch's reflog shows no commits made on it (it was only created or
/// fast-forwarded, as when it pulls the default branch), `unmerged`, or
/// empty when it can't tell. A caller that already knows them can set
/// `wt_head` (the head) and `wt_base` (origin/HEAD's short name, possibly
/// empty) to skip those lookups.
pub(crate) const MERGED_SH: &str = r#"
  head=${wt_head:-$(git -C "$path" rev-parse --verify --quiet "refs/heads/$branch" 2>/dev/null)}
  base=${wt_base-$(git -C "$path" symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null)}
  base=${base#origin/}
  merged=
  baseref=
  baserefs=
  for ref in "refs/heads/${base:-main}" "refs/remotes/origin/${base:-main}"; do
    [ -n "$head" ] || break
    git -C "$path" rev-parse --verify --quiet "$ref" >/dev/null 2>&1 || continue
    baseref=${baseref:-$ref}
    baserefs="$baserefs $ref"
    [ "$merged" = merged ] && continue
    if git -C "$path" merge-base --is-ancestor "$head" "$ref" 2>/dev/null; then merged=merged; else merged=unmerged; fi
  done
  if [ "$merged" = merged ]; then
    # Without a reflog it can't tell, and stays merged.
    reflog= own=
    while IFS= read -r line; do
      reflog=1
      # Only the branch's creation and fast-forwards to a default branch
      # are not its own work; anything else (commit, rebase, reset) is.
      case $line in branch:*|merge\ *:\ Fast-forward) ;; *) own=1; break ;; esac
    done < <(git -C "$path" reflog show --format=%gs "refs/heads/$branch" 2>/dev/null)
    [ -n "$reflog" ] && [ -z "$own" ] && merged=new
  fi
"#;

/// The loop of `host_worktrees`' script, reading `REPO<US>BRANCH<US>WANT` (US: the unit separator, \x1f)
/// lines from the `RSP_PAIRS` heredoc that follows it.
const WORKTREES_LOOP: &str = r#"repos=() lists=() origins=() bases=() configs=() boards=()
while IFS=$'\x1f' read -r repo branch want; do
  list= n=
  for i in "${!repos[@]}"; do
    if [ "${repos[$i]}" = "$repo" ]; then list=${lists[$i]} n=$i; break; fi
  done
  if [ -z "$n" ]; then
    n=${#repos[@]}
    list=$(git -C "$repo" worktree list --porcelain 2>/dev/null) || list="?"
    repos[$n]=$repo lists[$n]=$list origins[$n]= bases[$n]= configs[$n]= boards[$n]=
    if [ "$list" != "?" ]; then
      origins[$n]=$(git -C "$repo" remote get-url origin 2>/dev/null)
      bases[$n]=$(git -C "$repo" symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null)
      configs[$n]=$(git -C "$repo" config --get-regexp '^branch\..*\.switchyard-link$' 2>/dev/null)
      boards[$n]=$(fizzy_board "$repo")
      boards[$n]=${boards[$n]//$'\t'/}
    fi
  fi
  if [ "$list" = "?" ]; then echo "?"; continue; fi
  path=- wt= wt_head= sha= moved= moved_head= moved_branch=
  while IFS= read -r line; do
    case $line in
      "worktree "*) wt=${line#worktree } sha= ;;
      "HEAD "*) sha=${line#HEAD } ;;
      "branch refs/heads/$branch") path=$wt wt_head=$sha; break ;;
      "branch refs/heads/"*)
        # The recorded worktree on another branch: the agent renamed it.
        if [ -n "$want" ] && [ "$wt" -ef "$want" ]; then
          moved=$wt moved_head=$sha moved_branch=${line#branch refs/heads/}
        fi ;;
      detached) if [ -n "$want" ] && [ "$wt" -ef "$want" ]; then path=$wt wt_head=$sha; fi ;;
    esac
  done <<<"$list"
  if [ "$path" = "-" ] && [ -n "$moved" ]; then
    path=$moved wt_head=$moved_head branch=$moved_branch
  fi
  if [ "$path" = "-" ]; then printf -- '-\t%s\n' "${boards[$n]}"; continue; fi
  count=0 files=
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    count=$((count + 1))
    if [ "$count" -le 5 ]; then files=${files:+$files$'\t'}${line:3}; fi
  done < <(git -C "$path" status --porcelain 2>/dev/null)
  wt_base=${bases[$n]}
  merged_state
  origin=${origins[$n]//$'\t'/}
  links=
  while IFS= read -r line; do
    case $line in
      "branch.$branch.switchyard-link "*) links="$links ${line#"branch.$branch.switchyard-link "}" ;;
    esac
  done <<<"${configs[$n]}"
  links=${links//$'\t'/ }
  fixes=
  if [ -n "$baseref" ]; then
    shopt -s nocasematch
    # Commits in neither default branch ref, not just since the local one:
    # a branch that pulled origin's newer one would claim what they close.
    while IFS= read -r line; do
      case $line in *'#'*) ;; *) continue ;; esac
      case $line in *fix*|*clos*|*resolv*) fixes="$fixes ${line//$'\t'/ }" ;; esac
    done < <(git -C "$path" log --format=%B "$head" --not $baserefs 2>/dev/null)
    shopt -u nocasematch
    fixes=${fixes:0:4000}
  fi
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$path" "$count" "${head:0:12}" "$merged" "$origin" "$links" "$fixes" "${boards[$n]}" "$branch" "$files"
done <<'RSP_PAIRS'
"#;

/// For each (repo, branch), the branch's worktree on `host` with its
/// uncommitted files (count, and the first few paths), its branch head,
/// whether that is merged into the default branch, and the repo's origin
/// URL, or an empty path when
/// the repo is there but the branch has no worktree; None for a repo that
/// can't be read. None overall when the host can't be reached. One script
/// per host on stdin: `bash -s` over SSH by Rex label (as `probe` reaches
/// hosts; their login shell is Fish), or locally for the config host.
fn host_worktrees(host: &str, pairs: &[(String, String, String)]) -> Option<Vec<Option<Worktree>>> {
    // Pairs are REPO, BRANCH and the task's recorded worktree, which still
    // counts while detached from BRANCH, as a rebase leaves it until it
    // finishes: a check in that moment would otherwise end the task as
    // "worktree removed". With no worktree on BRANCH, the recorded one on
    // another branch counts too, as that branch: the agent renamed it, and
    // `git branch -m` took its link config along. The pairs go in separated
    // by the unit separator, not tabs: bash merges adjacent tabs (IFS
    // whitespace), so an orphan's empty branch (a detached HEAD) would shift
    // its worktree into BRANCH and it would never be found. One line out per
    // pair: PATH<TAB>COUNT<TAB>HEAD<TAB>MERGED<TAB>ORIGIN<TAB>LINKS<TAB>FIXES<TAB>FIZZY<TAB>BRANCH<TAB>FILE...,
    // "-<TAB>FIZZY" for no worktree, "?" for no repo. MERGED is as MERGED_SH
    // sets it; LINKS are the branch's `switchyard-link` git config values,
    // FIXES its commit message lines that may close issues, space separated;
    // FIZZY the repo's `.fizzy.yaml` board (see `links::FIZZY_BOARD_SH`).
    // Each repo's worktree list, origin URL, default branch and link config
    // are read once, the branch head comes from the worktree list, and the
    // rest is shell builtins where it can be: starting processes is most of
    // the cost, on macOS especially. Bash 3.2 (macOS's /bin/bash) runs it too.
    let mut script = String::from(
        r"merged_state() {
",
    );
    script.push_str(MERGED_SH);
    script.push_str("}\n");
    script.push_str(FIZZY_BOARD_SH);
    script.push_str(WORKTREES_LOOP);
    for (repo, branch, worktree) in pairs {
        let _ = writeln!(script, "{repo}\x1f{branch}\x1f{worktree}");
    }
    script.push_str("RSP_PAIRS\n");
    let out = host_bash(host, &script).filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<&str> = text.lines().collect();
    if rows.len() != pairs.len() {
        return None;
    }
    Some(
        rows.into_iter()
            .map(|l| {
                if l == "?" {
                    None
                } else {
                    Some(Worktree::parse(l))
                }
            })
            .collect(),
    )
}

/// A task's worktree on its host; an empty path means it has none.
#[derive(Clone, Default)]
struct Worktree {
    path: String,
    /// Uncommitted files, and the first few of their paths.
    dirty: u64,
    files: Vec<String>,
    /// The branch head (abbreviated), whether it is merged (see
    /// `host_worktrees`), and the repo's origin URL.
    head: String,
    merged: String,
    origin: String,
    /// The branch's `switchyard-link` refs, and the issues its commits fix.
    links: Vec<String>,
    fixes: Vec<String>,
    /// The repo's `.fizzy.yaml` board, `ACCOUNT/BOARD`, or empty.
    fizzy: String,
    /// The branch the worktree is on: the task's, or what it was renamed
    /// to (see `host_worktrees`).
    branch: String,
}

impl Worktree {
    /// One of `host_worktrees`' lines other than `?`: a worktree's fields,
    /// or `-<TAB>FIZZY` for none.
    fn parse(line: &str) -> Self {
        if line == "-" || line.starts_with("-\t") {
            let fizzy = line.split('\t').nth(1).unwrap_or("").to_string();
            return Worktree {
                fizzy,
                ..Worktree::default()
            };
        }
        let mut f = line.split('\t');
        let path = f.next().unwrap_or("").to_string();
        let dirty = f.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        let head = f.next().unwrap_or("").to_string();
        let merged = f.next().unwrap_or("").to_string();
        let origin = f.next().unwrap_or("").to_string();
        let links = f
            .next()
            .unwrap_or("")
            .split_whitespace()
            .map(String::from)
            .collect();
        let fixes = crate::links::fixed_refs(f.next().unwrap_or(""));
        let fizzy = f.next().unwrap_or("").to_string();
        let branch = f.next().unwrap_or("").to_string();
        let changed = f.filter(|s| !s.is_empty()).map(String::from).collect();
        Worktree {
            path,
            dirty,
            files: changed,
            head,
            merged,
            origin,
            links,
            fixes,
            fizzy,
            branch,
        }
    }
}

/// The registry with each open task's live agent status from its host's Rex
/// server, as `reconcile_tasks` gives it but without checking worktrees over
/// SSH or ending anything, so the inbox can show tasks before that check.
///
/// # Errors
///
/// Fails when the registry can't be loaded.
pub fn live_tasks() -> Result<Vec<serde_json::Value>, String> {
    if crate::server::remote_url().is_some() {
        return crate::server::remote_tasks(true);
    }
    let mut tasks = load_tasks()?;
    let mut hosts: Vec<String> = tasks
        .iter()
        .filter(|t| t["ended_at"].is_null())
        .filter_map(|t| t["host"].as_str().map(String::from))
        .collect();
    hosts.sort();
    hosts.dedup();
    let checks: Vec<_> = hosts
        .into_iter()
        .map(|host| std::thread::spawn(move || host_windows(&host)))
        .collect();
    let mut live: HashMap<String, TabStatus> = HashMap::new();
    for handle in checks {
        if let Ok(Some(windows)) = handle.join() {
            live.extend(windows.tabs);
        }
    }
    apply_live(&mut tasks, &live);
    crate::titles::apply(&mut tasks);
    Ok(tasks)
}

/// Check the open tasks against their hosts, end the ones whose tab or
/// worktree is gone, prune old ended ones, and return the registry. Open
/// tasks also get their tab's live agent status (`status`, `status_at`,
/// `status_msg`) and their worktree's uncommitted files (`dirty_count`,
/// `dirty_files`), which are not saved. Tasks that ended with their tab
/// closed are checked too, since closing the tab leaves the worktree: one
/// still there gets `worktree_left` and its uncommitted files (not saved),
/// so the archive can clean it up, and one gone becomes "worktree removed".
/// Checked tasks also get their branch head and merge state
/// (`branch_head`, `branch_merged`, not saved) and their work items as
/// links (saved): the branch's git config links, and derived ones such as
/// their PR, with its live `state` and `checks` (not saved); see `links`.
/// Their links get their work items' `title` too (not saved; see `titles`).
/// Their Fizzy cards get live `state`, `column` and `assignee` (not
/// saved); see `cards`.
/// A host that can't be reached leaves its tasks as they were, without
/// status, and a tab ends its task only when the host lists the task's
/// session without it. A new task's missing worktree ends it only after
/// `LAUNCH_GRACE_SECS`, since `task run` may still be creating it.
///
/// # Errors
///
/// Fails when the registry can't be loaded, or the check's changes can't be
/// saved.
pub fn reconcile_tasks() -> Result<Vec<serde_json::Value>, String> {
    if crate::server::remote_url().is_some() {
        return crate::server::remote_tasks(false);
    }
    let mut tasks = load_tasks()?;
    let checks = check_hosts(&tasks);
    let stamp = now();
    let (live, dirty) = apply_checks(&mut tasks, checks, stamp);
    keep_newest_sessions(&mut tasks, stamp);
    let plans = task_trackers(&tasks, &dirty);
    let live_prs = update_links(&mut tasks, &plans);
    refresh_titles(&tasks, &plans);
    let cards = crate::cards::find_cards(tasks.iter().filter(|t| checked(t)));
    let live_prs: Vec<(String, String, Pr)> = live_prs
        .into_iter()
        .map(|(i, repo, pr)| (window_key(&tasks[i]), repo, pr))
        .collect();
    let mut tasks = commit_checked_at(&tasks_path(), tasks, stamp)?;
    apply_live(&mut tasks, &live);
    crate::titles::apply(&mut tasks);
    for task in &mut tasks {
        add_unsaved(task, &dirty, &live_prs, &cards);
    }
    Ok(tasks)
}

/// Field `k` of task `t` as a string, or empty.
fn field(t: &serde_json::Value, k: &str) -> String {
    t[k].as_str().unwrap_or("").to_string()
}

/// Whether `task` runs directly in its project directory (`worktree = false`)
/// rather than in a Git worktree.
#[must_use]
pub fn is_in_place(task: &serde_json::Value) -> bool {
    task["in_place"].as_bool() == Some(true)
}

/// Whether `reconcile_tasks` checks task `t`: it is open, or (for worktree
/// tasks) ended with only its tab closed or an orphan worktree (see `orphans`).
pub(crate) fn checked(t: &serde_json::Value) -> bool {
    t["ended_at"].is_null()
        || (!is_in_place(t)
            && matches!(t["ended_reason"].as_str(), Some("tab closed" | "orphaned")))
}

/// A task's `HOST/WINDOW_ID`, which keys a check's worktrees and PRs.
fn window_key(t: &serde_json::Value) -> String {
    format!("{}/{}", field(t, "host"), field(t, "window_id"))
}

/// One host's check: its tabs (None when Rex can't be asked) and the
/// worktrees of its checked tasks, in their order (None when it can't be
/// reached).
type HostCheck = (String, Option<HostWindows>, Option<Vec<Option<Worktree>>>);

/// Start checking each host of the checked tasks, in parallel.
fn check_hosts(tasks: &[serde_json::Value]) -> Vec<std::thread::JoinHandle<HostCheck>> {
    let mut hosts: Vec<String> = tasks
        .iter()
        .filter(|t| checked(t))
        .map(|t| field(t, "host"))
        .collect();
    hosts.sort();
    hosts.dedup();
    hosts
        .into_iter()
        .map(|host| {
            let mut has_worktree = Vec::new();
            let mut pairs: Vec<(String, String, String)> = Vec::new();
            for t in tasks
                .iter()
                .filter(|t| checked(t) && field(t, "host") == host)
            {
                if is_in_place(t) {
                    has_worktree.push(false);
                } else {
                    has_worktree.push(true);
                    pairs.push((field(t, "repo"), field(t, "branch"), field(t, "worktree")));
                }
            }
            // The tab listing (Rex) and the worktree check (SSH) don't depend
            // on each other, so a host costs the slower of the two. In-place
            // tasks skip Git worktree checks completely.
            std::thread::spawn(move || {
                let (windows, worktrees) = std::thread::scope(|s| {
                    let windows = s.spawn(|| host_windows(&host));
                    let worktrees = if pairs.is_empty() {
                        Some(vec![None; has_worktree.len()])
                    } else {
                        host_worktrees(&host, &pairs).map(|found| {
                            let mut it = found.into_iter();
                            has_worktree
                                .iter()
                                .map(|&check_wt| if check_wt { it.next().flatten() } else { None })
                                .collect()
                        })
                    };
                    (windows.join().ok().flatten(), worktrees)
                });
                (host, windows, worktrees)
            })
        })
        .collect()
}

/// End the checked tasks whose tab or worktree the host checks found gone,
/// and return the open tabs' live status by window ID and the worktrees
/// found, by `HOST/WINDOW_ID`.
fn apply_checks(
    tasks: &mut [serde_json::Value],
    checks: Vec<std::thread::JoinHandle<HostCheck>>,
    stamp: u64,
) -> (HashMap<String, TabStatus>, HashMap<String, Worktree>) {
    let mut live: HashMap<String, TabStatus> = HashMap::new();
    let mut dirty: HashMap<String, Worktree> = HashMap::new();
    for handle in checks {
        let Ok((host, windows, worktrees)) = handle.join() else {
            continue;
        };
        let peers: std::collections::HashSet<String> = tasks
            .iter()
            .filter(|t| t["host"].as_str() == Some(host.as_str()))
            .map(|t| field(t, "session_id"))
            .filter(|s| !s.is_empty())
            .collect();
        let mine = tasks
            .iter_mut()
            .filter(|t| checked(t) && t["host"].as_str() == Some(host.as_str()));
        for (i, task) in mine.enumerate() {
            let window_id = task["window_id"].as_str().unwrap_or("").to_string();
            let window_id = window_id.as_str();
            let session_id = field(task, "session_id");
            let key = format!("{host}/{window_id}");
            let worktree = if is_in_place(task) {
                None
            } else {
                worktrees.as_ref().and_then(|w| w.get(i).cloned().flatten())
            };
            if !task["ended_at"].is_null() {
                match worktree {
                    Some(w) if w.path.is_empty() => {
                        task["ended_reason"] = "worktree removed".into();
                    }
                    Some(w) => {
                        follow_rename(task, &w);
                        dirty.insert(key, w);
                    }
                    None => {}
                }
                continue;
            }
            // Ended only when Rex positively lists the task's session
            // without its tab, or the task's server without its session;
            // see `HostWindows::tab_gone` and `HostWindows::session_gone`.
            let tab_gone = windows.as_ref().is_some_and(|w| {
                w.tab_gone(&session_id, window_id)
                    || w.session_gone(&session_id, peers.iter().map(String::as_str))
            });
            if let Some(status) = windows.as_ref().and_then(|w| w.tabs.get(window_id)) {
                live.insert(window_id.to_string(), status.clone());
            }
            match worktree {
                Some(w) if w.path.is_empty() && launching(task, stamp) => {}
                Some(w) if w.path.is_empty() => {
                    task["ended_at"] = stamp.into();
                    task["ended_reason"] = "worktree removed".into();
                }
                Some(w) => {
                    follow_rename(task, &w);
                    dirty.insert(key, w);
                }
                None => {}
            }
            if tab_gone && task["ended_at"].is_null() {
                task["ended_at"] = stamp.into();
                task["ended_reason"] = "tab closed".into();
            }
        }
    }
    (live, dirty)
}

/// Record `w`, the worktree found for `task`, as its worktree, and the
/// branch it is on as its branch when the agent renamed it.
fn follow_rename(task: &mut serde_json::Value, w: &Worktree) {
    task["worktree"] = w.path.clone().into();
    if !w.branch.is_empty() && task["branch"].as_str() != Some(w.branch.as_str()) {
        task["branch"] = w.branch.clone().into();
    }
}

/// Tasks that ended on their own just now (at `stamp`) keep their newest
/// agent session (after a /clear it is no longer the one spawn chose), for
/// a resume.
fn keep_newest_sessions(tasks: &mut [serde_json::Value], stamp: u64) {
    let ended_now: Vec<usize> = (0..tasks.len())
        .filter(|&i| tasks[i]["ended_at"].as_u64() == Some(stamp))
        .collect();
    let found: Vec<_> = ended_now
        .iter()
        .map(|&i| {
            let task = tasks[i].clone();
            std::thread::spawn(move || newest_session(&task))
        })
        .collect();
    for (&i, handle) in ended_now.iter().zip(found) {
        if let Ok(Some(id)) = handle.join() {
            tasks[i]["agent_session"] = id.into();
        }
    }
}

/// Each checked task's index, tracker and worktree (from `dirty`, by
/// `HOST/WINDOW_ID`).
fn task_trackers(
    tasks: &[serde_json::Value],
    dirty: &HashMap<String, Worktree>,
) -> Vec<(usize, Tracker, Option<Worktree>)> {
    let projects = crate::hosts::catalog().unwrap_or_default();
    tasks
        .iter()
        .enumerate()
        .filter(|(_, t)| checked(t))
        .map(|(i, task)| {
            let worktree = dirty.get(&window_key(task)).cloned();
            let tracker = task_tracker(task, worktree.as_ref(), &projects);
            (i, tracker, worktree)
        })
        .collect()
}

/// The tracker of `task`'s work items: its catalog project's, or its
/// checkout's `.fizzy.yaml` board, with the GitHub repo of its origin, its
/// project's clone URL, or else a PR link it already has.
fn task_tracker(
    task: &serde_json::Value,
    worktree: Option<&Worktree>,
    projects: &[crate::hosts::Project],
) -> Tracker {
    let repo = crate::folders::home_relative(&field(task, "repo"));
    let project = projects
        .iter()
        .find(|p| crate::folders::home_relative(p.path.trim_end_matches('/')) == repo);
    let remote = worktree
        .map(|w| w.origin.clone())
        .filter(|o| github_repo(o).is_some())
        .or_else(|| project.map(|p| p.clone_url.clone()))
        .unwrap_or_default();
    let board = worktree.map_or("", |w| w.fizzy.as_str());
    let catalog = project.map_or("", |p| p.tracker.as_str());
    let mut tracker = Tracker::new(&tracker_or_fizzy(catalog, board), &remote);
    if tracker.github.is_none() {
        tracker.github = task["links"].as_array().and_then(|links| {
            links
                .iter()
                .filter(|l| l["kind"] == "pr" && l["system"] == "github")
                .find_map(|l| {
                    l["ref"]
                        .as_str()?
                        .split_once('#')
                        .map(|(repo, _)| repo.to_string())
                })
        });
    }
    tracker
}

/// Update each planned task's work items (see `links`): its branch's git
/// config links, issues its branch name and commits name, and its PR from
/// GitHub, which is saved. Returns the PRs found, by task index and repo,
/// for their live state to be added once saved.
fn update_links(
    tasks: &mut [serde_json::Value],
    plans: &[(usize, Tracker, Option<Worktree>)],
) -> Vec<(usize, String, Pr)> {
    let mut lookups: Vec<(usize, &Tracker, Lookup)> = Vec::new();
    // The derived issue and card links of tasks whose worktree was read,
    // rebuilt rather than added to, so one the branch no longer implies
    // goes. Without a worktree (or a PR answer) they are only added to.
    let mut rebuilt: HashMap<usize, Vec<serde_json::Value>> = HashMap::new();
    for (i, tracker, worktree) in plans {
        let task = &mut tasks[*i];
        let branch = field(task, "branch");
        let mut items = Vec::new();
        if let Some(w) = worktree {
            set_agent_links(task, &w.links, tracker);
            items.extend(w.fixes.iter().filter_map(|r| parse_ref(r, tracker)));
        }
        items.extend(branch_number(&branch).and_then(|n| parse_ref(n, tracker)));
        if worktree.as_ref().is_some_and(|w| !w.path.is_empty()) {
            rebuilt.insert(*i, items);
        } else {
            for link in items {
                add_link(task, link, "derived");
            }
        }
        if let (Some(repo), false) = (&tracker.github, branch.is_empty()) {
            lookups.push((
                *i,
                tracker,
                Lookup {
                    profile: field(task, "profile"),
                    repo: repo.clone(),
                    branch,
                },
            ));
        }
    }
    let prs = find_prs(
        &lookups
            .iter()
            .map(|(_, _, l)| l.clone())
            .collect::<Vec<_>>(),
    );
    let mut live_prs: Vec<(usize, String, Pr)> = Vec::new();
    for (i, tracker, l) in &lookups {
        let Some(pr) = prs.get(&pr_key(&l.repo, &l.branch)) else {
            // GitHub couldn't be asked, so the PR body's links stay.
            for link in rebuilt.remove(i).unwrap_or_default() {
                add_link(&mut tasks[*i], link, "derived");
            }
            continue;
        };
        set_derived_pr(&mut tasks[*i], &l.repo, pr);
        if let Some(pr) = pr {
            let items = pr.fixes.iter().filter_map(|r| parse_ref(r, tracker));
            match rebuilt.get_mut(i) {
                Some(rebuilt) => rebuilt.extend(items),
                None => {
                    for link in items {
                        add_link(&mut tasks[*i], link, "derived");
                    }
                }
            }
            live_prs.push((*i, l.repo.clone(), pr.clone()));
        }
    }
    for (i, items) in rebuilt {
        set_derived_items(&mut tasks[i], items);
    }
    live_prs
}

/// Refresh every task's work items' titles (see `titles`), added on read.
/// Ended tasks' links already name their repo or card account, so they need
/// no tracker.
fn refresh_titles(tasks: &[serde_json::Value], plans: &[(usize, Tracker, Option<Worktree>)]) {
    let asks = tasks.iter().enumerate().flat_map(|(i, task)| {
        let tracker = plans
            .iter()
            .find(|(k, _, _)| *k == i)
            .map(|(_, t, _)| t.clone())
            .unwrap_or_default();
        let profile = field(task, "profile");
        let links = task["links"].as_array().cloned().unwrap_or_default();
        links.into_iter().map(move |link| crate::titles::Ask {
            profile: profile.clone(),
            link,
            tracker: tracker.clone(),
        })
    });
    crate::titles::refresh(asks.collect());
}

/// Add what a check found that isn't saved to `task`: its worktree's state
/// (from `dirty`), its PRs' live state, and its cards'.
fn add_unsaved(
    task: &mut serde_json::Value,
    dirty: &HashMap<String, Worktree>,
    live_prs: &[(String, String, Pr)],
    cards: &HashMap<String, crate::cards::Card>,
) {
    let open = task["ended_at"].is_null();
    let key = window_key(task);
    if let Some(w) = dirty.get(&key) {
        task["dirty_count"] = w.dirty.into();
        task["dirty_files"] = w.files.clone().into();
        if !w.head.is_empty() {
            task["branch_head"] = w.head.clone().into();
        }
        if !w.merged.is_empty() {
            task["branch_merged"] = w.merged.clone().into();
        }
        if !open {
            task["worktree_left"] = true.into();
        }
    }
    for (_, repo, pr) in live_prs.iter().filter(|(k, _, _)| *k == key) {
        add_live(task, repo, pr);
    }
    crate::cards::add_live(task, cards);
}

/// The inbox state John sets on a task (see actions' pin and snooze).
const INBOX_KEYS: [&str; 2] = ["pinned", "snoozed_until"];

/// Save what a check changed. `checked` is the registry as the check read it,
/// with the check's changes, and it is merged onto the registry as it is now,
/// under the lock. The check takes seconds, so meanwhile an archive may have
/// ended one of its tasks, and a launch or pin may have been saved. The
/// registry's end of a task is kept (an archive's reason, not "worktree
/// removed" from a check that found the worktree gone), and tasks it has
/// that the check didn't read are kept too. Returns the merged tasks.
fn commit_checked_at(
    path: &Path,
    checked: Vec<serde_json::Value>,
    stamp: u64,
) -> Result<Vec<serde_json::Value>, String> {
    update_at(path, |fresh| {
        let mut merged = checked;
        let newer = by_key(fresh);
        carry_inbox_state(&mut merged, &newer);
        for task in &mut merged {
            let Some(newer) = newer.get(&task_key(task)) else {
                continue;
            };
            // An orphan whose worktree the check found gone is removed,
            // not still an orphan.
            let orphan_gone =
                newer["ended_reason"] == "orphaned" && task["ended_reason"] == "worktree removed";
            if !newer["ended_at"].is_null() && !orphan_gone {
                task["ended_at"] = newer["ended_at"].clone();
                task["ended_reason"] = newer["ended_reason"].clone();
                if let Some(handoff) = newer.get("handoff").filter(|v| !v.is_null()) {
                    task["handoff"] = handoff.clone();
                }
            }
            keep_unlinked(task, newer);
        }
        let checked: HashSet<String> = merged.iter().map(task_key).collect();
        let launched: Vec<serde_json::Value> = fresh
            .iter()
            .filter(|f| !checked.contains(&task_key(f)))
            .cloned()
            .collect();
        merged.extend(launched);
        // Pruned after the merge, so an old ended task the registry still
        // has doesn't come back once this check has dropped it.
        merged.retain(|t| {
            t["ended_at"]
                .as_u64()
                .is_none_or(|ended| stamp.saturating_sub(ended) < ENDED_KEEP_SECS)
        });
        fresh.clone_from(&merged);
        Ok(merged)
    })
}

/// Drop the explicit links (spawn, manual) of a checked `task` that its
/// `newer` record no longer has: `unlink` removed them during the check,
/// which only sets derived and agent links, so it must not bring them back.
fn keep_unlinked(task: &mut serde_json::Value, newer: &serde_json::Value) {
    let key = |l: &serde_json::Value| format!("{}\t{}\t{}", l["kind"], l["system"], l["ref"]);
    let kept: HashSet<_> = newer["links"]
        .as_array()
        .into_iter()
        .flatten()
        .map(key)
        .collect();
    if let Some(links) = task["links"].as_array_mut() {
        links.retain(|l| {
            l["source"] == "derived" || l["source"] == "agent" || kept.contains(&key(l))
        });
    }
}

/// Copy each task's inbox state from `fresh`, a later read of the registry
/// (see `by_key`), matching tasks by host, window and start.
fn carry_inbox_state(tasks: &mut [serde_json::Value], fresh: &HashMap<String, &serde_json::Value>) {
    for task in tasks.iter_mut() {
        let Some(newer) = fresh.get(&task_key(task)) else {
            continue;
        };
        let Some(obj) = task.as_object_mut() else {
            continue;
        };
        for key in INBOX_KEYS {
            match newer.get(key) {
                Some(v) => obj.insert(key.into(), v.clone()),
                None => obj.remove(key),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn inbox_state_saved_during_a_check_survives_it() {
        let mut tasks = vec![
            serde_json::json!({"host":"h", "window_id":"a", "created_at":1, "status":"x", "pinned":true}),
            serde_json::json!({"host":"h", "window_id":"b", "created_at":2}),
            serde_json::json!({"host":"h", "window_id":"c", "created_at":3, "pinned":true}),
        ];
        let fresh = vec![
            serde_json::json!({"host":"h", "window_id":"a", "created_at":1}),
            serde_json::json!({"host":"h", "window_id":"b", "created_at":2, "pinned":true, "snoozed_until":9}),
        ];
        carry_inbox_state(&mut tasks, &by_key(&fresh));
        assert_eq!(
            tasks[0],
            serde_json::json!({"host":"h", "window_id":"a", "created_at":1, "status":"x"})
        );
        assert_eq!(tasks[1]["snoozed_until"], 9);
        assert_eq!(tasks[1]["pinned"], true);
        // Gone from the later read (pruned or replaced): left alone.
        assert_eq!(tasks[2]["pinned"], true);
    }

    #[test]
    fn only_a_listed_session_without_the_tab_closes_it() {
        let listed = HostWindows::parse("S\tsession:a\nwindow:1\tworking\t\tbusy\n");
        assert_eq!(listed.tabs["window:1"].state, "working");
        assert!(!listed.tab_gone("session:a", "window:1"));
        assert!(listed.tab_gone("session:a", "window:2"));
        // A session the server didn't list proves nothing.
        assert!(!listed.tab_gone("session:b", "window:2"));
        assert!(!listed.tab_gone("", "window:2"));
        assert!(!listed.tab_gone("session:a", ""));
    }

    #[test]
    fn a_fresh_or_wrong_server_closes_no_tabs() {
        // As a temporary XDG_RUNTIME_DIR's autostarted server answers:
        // nothing, or only its own new session.
        for reply in [
            "",
            "S\tsession:fresh\n",
            "S\tsession:fresh\nwindow:9\t\t\t\n",
        ] {
            let listed = HostWindows::parse(reply);
            assert!(
                !listed.tab_gone("session:01a1db3e", "window:01a1db57"),
                "{reply:?}"
            );
        }
    }

    #[test]
    fn a_session_gone_from_the_tasks_server_closes_its_tab() {
        let listed =
            HostWindows::parse("L\tsession:a\nS\tsession:a\nL\tsession:b\nblock:1\tidle\t\t\n");
        assert_eq!(
            listed.all_sessions,
            ["session:a", "session:b"].map(String::from).into()
        );
        // session:a, which the registry knows, proves it the task's server.
        assert!(listed.session_gone("session:gone", ["session:a"].into_iter()));
        // Its own session listed, unread or not, isn't gone.
        assert!(!listed.session_gone("session:a", ["session:a"].into_iter()));
        assert!(!listed.session_gone("session:b", ["session:a"].into_iter()));
        // No known peer listed, or none at all: another server, maybe.
        assert!(!listed.session_gone("session:gone", ["session:x"].into_iter()));
        assert!(!listed.session_gone("session:gone", ["session:gone"].into_iter()));
        assert!(!listed.session_gone("session:gone", std::iter::empty()));
        assert!(!listed.session_gone("", ["session:a"].into_iter()));
    }

    #[test]
    fn a_fresh_server_shows_no_session_gone() {
        for reply in [
            "",
            "L\tsession:fresh\n",
            "L\tsession:fresh\nS\tsession:fresh\n",
        ] {
            let listed = HostWindows::parse(reply);
            assert!(
                !listed.session_gone("session:01a1db3e", ["session:01a1e66a"].into_iter()),
                "{reply:?}"
            );
        }
    }

    #[test]
    fn a_new_task_waits_for_its_worktree() {
        let task = serde_json::json!({"created_at": 1000});
        assert!(launching(&task, 1000));
        assert!(launching(&task, 1000 + LAUNCH_GRACE_SECS - 1));
        assert!(!launching(&task, 1000 + LAUNCH_GRACE_SECS));
        // An entry without a creation time gets no grace.
        assert!(!launching(&serde_json::json!({}), 1000));
    }

    /// A registry file for one test, in a directory of its own.
    fn registry(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "switchyard-registry-{name}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("spawn-tasks.json")
    }

    fn task(window: &str, created: u64) -> serde_json::Value {
        serde_json::json!({"host":"h", "window_id":window, "created_at":created, "project":"p", "name":window})
    }

    #[test]
    fn a_check_that_an_archive_overtook_keeps_the_archive_end() {
        let path = registry("overtaken");
        save_at(&path, &[task("a", 1)], true).unwrap();
        // The check reads the registry, and while it checks the worktree the
        // archive ends the task and saves that.
        let mut checked = load_at(&path).unwrap();
        update_at(&path, |tasks| {
            tasks[0]["ended_at"] = 50.into();
            tasks[0]["ended_reason"] = "archived".into();
            Ok(())
        })
        .unwrap();
        // The check, which read the task as open, found the worktree gone.
        checked[0]["ended_at"] = 60.into();
        checked[0]["ended_reason"] = "worktree removed".into();
        commit_checked_at(&path, checked, 60).unwrap();
        let saved = load_at(&path).unwrap();
        assert_eq!(saved[0]["ended_reason"], "archived");
        assert_eq!(saved[0]["ended_at"], 50);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn an_archive_saved_after_a_check_still_wins() {
        let path = registry("archive-last");
        save_at(&path, &[task("a", 1)], true).unwrap();
        let mut archived = load_at(&path).unwrap();
        archived[0]["ended_at"] = 50.into();
        archived[0]["ended_reason"] = "archived".into();
        let mut checked = load_at(&path).unwrap();
        checked[0]["ended_at"] = 40.into();
        checked[0]["ended_reason"] = "worktree removed".into();
        commit_checked_at(&path, checked, 40).unwrap();
        save_record_at(&path, &archived[0]).unwrap();
        assert_eq!(load_at(&path).unwrap()[0]["ended_reason"], "archived");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn unlink_removes_registry_links_and_a_check_does_not_restore_them() {
        let path = registry("unlink");
        let mut mine = task("a", 1);
        mine["worktree"] = "~/repo/.worktrees/a".into();
        mine["links"] = serde_json::json!([
            {"kind":"card","system":"fizzy","ref":"1201","url":"","source":"spawn"},
            {"kind":"card","system":"fizzy","ref":"4528","url":"","source":"spawn"},
        ]);
        let mut other = task("b", 2);
        other["worktree"] = "/elsewhere".into();
        other["links"] = mine["links"].clone();
        save_at(&path, &[mine, other], true).unwrap();
        // A check reads the registry before the unlink saves.
        let checked = load_at(&path).unwrap();
        let home = Path::new("/home/u");
        let worktree = Path::new("/home/u/repo/.worktrees/a");
        assert_eq!(
            unlink_at(&path, "h", home, worktree, &["#4528", "#9"]).unwrap(),
            [Unlinked::Removed, Unlinked::Absent]
        );
        assert_eq!(
            unlink_at(&path, "other-host", home, worktree, &["#1201"]).unwrap(),
            [Unlinked::Absent]
        );
        commit_checked_at(&path, checked, 10).unwrap();
        let refs = |t: &serde_json::Value| -> Vec<String> {
            t["links"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| l["ref"].as_str().unwrap().to_string())
                .collect()
        };
        let saved = load_at(&path).unwrap();
        assert_eq!(refs(&saved[0]), ["1201"]);
        assert_eq!(refs(&saved[1]), ["1201", "4528"], "another worktree's task");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_task_launched_during_a_check_survives_it() {
        let path = registry("launched-check");
        save_at(&path, &[task("a", 1)], true).unwrap();
        let checked = load_at(&path).unwrap();
        update_at(&path, |tasks| {
            tasks.push(task("b", 2));
            Ok(())
        })
        .unwrap();
        let merged = commit_checked_at(&path, checked, 100).unwrap();
        assert_eq!(merged.len(), 2);
        assert_eq!(load_at(&path).unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn an_archive_keeps_what_was_saved_while_it_ran() {
        let path = registry("archive-meanwhile");
        save_at(&path, &[task("a", 1)], true).unwrap();
        // The archive's copy of its task, read before it ran.
        let mut archived = load_at(&path).unwrap()[0].clone();
        // Meanwhile: a launch, and a pin on the archived task.
        update_at(&path, |tasks| {
            tasks.push(task("b", 2));
            tasks[0]["pinned"] = true.into();
            Ok(())
        })
        .unwrap();
        archived["ended_at"] = 70.into();
        archived["ended_reason"] = "archived".into();
        save_record_at(&path, &archived).unwrap();
        let saved = load_at(&path).unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[0]["ended_reason"], "archived");
        assert_eq!(saved[0]["pinned"], true);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn set_handoff_records_summary_and_survives_commit_checked() {
        let path = registry("handoff");
        let original = task("a", 1);
        save_at(&path, std::slice::from_ref(&original), true).unwrap();
        let mut checked = vec![original.clone()];
        checked[0]["ended_at"] = 40.into();
        checked[0]["ended_reason"] = "tab closed".into();

        let mut archived = original.clone();
        archived["ended_at"] = 50.into();
        archived["ended_reason"] = "archived".into();
        save_record_at(&path, &archived).unwrap();
        set_handoff_at(&path, &original, "Committed abc123 and closed #42.").unwrap();

        let merged = commit_checked_at(&path, checked, 60).unwrap();
        assert_eq!(merged[0]["ended_reason"], "archived");
        assert_eq!(merged[0]["handoff"], "Committed abc123 and closed #42.");
        let saved = load_at(&path).unwrap();
        assert_eq!(saved[0]["handoff"], "Committed abc123 and closed #42.");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn concurrent_writers_lose_no_updates() {
        let path = registry("writers");
        save_at(&path, &[], true).unwrap();
        let writers: Vec<_> = (0..4)
            .map(|w| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for n in 0..10 {
                        update_at(&path, |tasks| {
                            tasks.push(task(&format!("{w}-{n}"), n));
                            Ok(())
                        })
                        .unwrap();
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        assert_eq!(load_at(&path).unwrap().len(), 40);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn each_save_backs_up_the_registry_it_replaces() {
        let path = registry("backup");
        save_at(&path, &[task("a", 1)], true).unwrap();
        assert!(!backup_path(&path).exists());
        record_at(&path, task("b", 2));
        let backup = std::fs::read(backup_path(&path)).unwrap();
        let backup: Vec<serde_json::Value> = serde_json::from_slice(&backup).unwrap();
        assert_eq!(backup, [task("a", 1)]);
        assert_eq!(load_at(&path).unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    fn record_at(path: &Path, t: serde_json::Value) {
        update_at(path, |tasks| {
            tasks.push(t);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn a_damaged_registry_is_read_from_its_backup_and_repaired() {
        let path = registry("damaged");
        save_at(&path, &[task("a", 1)], true).unwrap();
        record_at(&path, task("b", 2));
        // A crash left the registry truncated.
        std::fs::write(&path, "[{\"host\":").unwrap();
        // Readers get the backup, one save behind.
        assert_eq!(load_at(&path).unwrap(), [task("a", 1)]);
        // The next writer repairs the file from it and keeps the damaged one.
        record_at(&path, task("c", 3));
        assert_eq!(load_at(&path).unwrap(), [task("a", 1), task("c", 3)]);
        assert!(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(&path).unwrap()).is_ok()
        );
        let dir = path.parent().unwrap();
        let damaged: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("spawn-tasks.json.damaged.")
            })
            .collect();
        assert_eq!(damaged.len(), 1);
        assert_eq!(
            std::fs::read_to_string(damaged[0].path()).unwrap(),
            "[{\"host\":"
        );
        // The repair didn't back up the damaged file over the good backup.
        let backup: Vec<serde_json::Value> =
            serde_json::from_slice(&std::fs::read(backup_path(&path)).unwrap()).unwrap();
        assert_eq!(backup, [task("a", 1)]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_damaged_registry_without_a_backup_is_an_error() {
        let path = registry("no-backup");
        std::fs::write(&path, "").unwrap();
        assert!(load_at(&path).is_err());
        assert!(update_at(&path, |_| Ok(())).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_missing_registry_is_empty_even_with_a_backup() {
        let path = registry("missing");
        save_at(&path, &[task("a", 1)], true).unwrap();
        record_at(&path, task("b", 2));
        std::fs::remove_file(&path).unwrap();
        assert_eq!(load_at(&path).unwrap(), [] as [serde_json::Value; 0]);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_check_merge_matches_tasks_by_host_window_and_start() {
        let path = registry("keys");
        // Same window, different start: a different task.
        save_at(&path, &[task("a", 1), task("a", 2)], true).unwrap();
        let mut checked = load_at(&path).unwrap();
        update_at(&path, |tasks| {
            tasks[1]["pinned"] = true.into();
            tasks[1]["ended_at"] = 5.into();
            tasks[1]["ended_reason"] = "archived".into();
            Ok(())
        })
        .unwrap();
        checked.truncate(1);
        let merged = commit_checked_at(&path, checked, 10).unwrap();
        assert_eq!(merged.len(), 2);
        assert!(merged[0].get("pinned").is_none() && merged[0]["ended_at"].is_null());
        assert_eq!(
            (
                merged[1]["pinned"].clone(),
                merged[1]["ended_reason"].clone()
            ),
            (true.into(), "archived".into())
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
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
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    /// A repo under a fresh temp root with worktrees for branches `fresh`
    /// (new), `done` (merged), `open` (unmerged, dirty, linked, with fixes),
    /// `pulled` (only origin's newer commit) and `rebasing` (detached), and a
    /// `.fizzy.yaml` board. Returns the root and the repo.
    fn worktree_fixture() -> (PathBuf, PathBuf) {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("switchyard-wt-{}-{unique}", std::process::id()));
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "--quiet", "-b", "main"]);
        git(
            &repo,
            &["remote", "add", "origin", "git@github.com:o/r.git"],
        );
        git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "base"]);
        for b in ["fresh", "done", "open", "pulled", "rebasing"] {
            git(
                &repo,
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    "-b",
                    b,
                    root.join(b).to_str().unwrap(),
                    "main",
                ],
            );
        }
        git(
            &root.join("done"),
            &["commit", "--quiet", "--allow-empty", "-m", "done"],
        );
        git(&repo, &["merge", "--quiet", "--ff-only", "done"]);
        git(
            &root.join("open"),
            &[
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "open\n\nFixes #9, closes o/r#10",
            ],
        );
        git(
            &repo,
            &["config", "--add", "branch.open.switchyard-link", "fizzy:5"],
        );
        git(
            &repo,
            &[
                "config",
                "--add",
                "branch.open.switchyard-link",
                "https://github.com/o/r/issues/3",
            ],
        );
        // Origin's default branch moves on with someone else's fix, and a
        // branch with no commits of its own pulls it; local main lags.
        git(
            &repo,
            &[
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "other (#8)\n\nResolves #7",
            ],
        );
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(&repo, &["reset", "--quiet", "--hard", "HEAD~1"]);
        git(
            &root.join("pulled"),
            &["merge", "--quiet", "--ff-only", "origin/main"],
        );
        // Mid-rebase, a worktree is detached from its branch.
        git(&root.join("rebasing"), &["checkout", "--quiet", "--detach"]);
        std::fs::write(root.join("open/new.txt"), "x").unwrap();
        std::fs::write(
            repo.join(".fizzy.yaml"),
            "account: \"6284043\"\napi_url: https://app.fizzy.do\nboard: b1\n",
        )
        .unwrap();
        (root, repo)
    }

    #[test]
    fn worktree_script_reports_head_merge_state_and_origin() {
        let (root, repo) = worktree_fixture();
        let r = repo.to_str().unwrap().to_string();
        let mut pairs: Vec<(String, String, String)> = ["fresh", "done", "open", "gone", "pulled"]
            .iter()
            .map(|b| (r.clone(), b.to_string(), String::new()))
            .collect();
        // Detached: found at the task's recorded worktree, and not without it.
        let rebasing = root.join("rebasing").to_str().unwrap().to_string();
        pairs.push((r.clone(), "rebasing".into(), rebasing.clone()));
        pairs.push((r.clone(), "rebasing".into(), String::new()));
        // A detached orphan has no branch at all, only its worktree.
        pairs.push((r.clone(), String::new(), rebasing.clone()));
        let got = host_worktrees(&local_label(), &pairs).unwrap();
        let w: Vec<Worktree> = got.into_iter().map(Option::unwrap).collect();
        assert_eq!(w[0].merged, "new");
        assert_eq!(w[1].merged, "merged");
        assert_eq!(w[2].merged, "unmerged");
        assert_eq!(
            (w[2].dirty, w[2].files.clone()),
            (1, vec!["new.txt".to_string()])
        );
        assert_eq!(w[2].links, ["fizzy:5", "https://github.com/o/r/issues/3"]);
        assert_eq!(w[2].fixes, ["#9", "o/r#10"]);
        assert!(w[0].links.is_empty() && w[0].fixes.is_empty());
        assert_eq!(w[3].path, "");
        assert_eq!(w[4].merged, "new");
        assert!(w[4].fixes.is_empty(), "{:?}", w[4].fixes);
        assert!(
            std::path::Path::new(&w[5].path) == std::fs::canonicalize(&rebasing).unwrap()
                || w[5].path == rebasing,
            "{}",
            w[5].path
        );
        assert_eq!(w[6].path, "");
        assert_eq!(w[7].path, w[5].path);
        // The repo's `.fizzy.yaml` board, with or without a worktree.
        assert!(w.iter().all(|w| w.fizzy == "6284043/b1"));
        assert!(
            w[..3]
                .iter()
                .chain(&w[4..6])
                .all(|w| w.head.len() == 12 && w.origin == "git@github.com:o/r.git")
        );
        assert_eq!(w[2].branch, "open");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn worktree_script_follows_a_renamed_task_branch() {
        let (root, repo) = worktree_fixture();
        let r = repo.to_str().unwrap().to_string();
        let open = root.join("open");
        // `git branch -m` takes the branch's link config along.
        git(&open, &["branch", "-m", "feature/open"]);
        let recorded = open.to_str().unwrap().to_string();
        let pairs = [
            (r.clone(), "open".to_string(), recorded),
            (r.clone(), "open".to_string(), String::new()),
        ];
        let got = host_worktrees(&local_label(), &pairs).unwrap();
        let w: Vec<Worktree> = got.into_iter().map(Option::unwrap).collect();
        assert_eq!(w[0].branch, "feature/open");
        assert_eq!(w[0].merged, "unmerged");
        assert_eq!(w[0].links, ["fizzy:5", "https://github.com/o/r/issues/3"]);
        // Without the recorded worktree, the old branch is just gone.
        assert_eq!(w[1].path, "");
        let mut task = serde_json::json!({"branch": "open"});
        follow_rename(&mut task, &w[0]);
        assert_eq!(task["branch"], "feature/open");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unlink_remote_at_matches_host_and_home_relative_worktree() {
        let dir = std::env::temp_dir().join(format!(
            "switchyard-remote-unlink-{}-{}",
            std::process::id(),
            now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("spawn-tasks.json");
        let records = vec![serde_json::json!({
            "host": "m4-mbp",
            "window_id": "w1",
            "created_at": 1,
            "ended_at": null,
            "worktree": "/Users/john.allen/dev/repo/.worktrees/feat",
            "links": [
                {"system": "fizzy", "kind": "card", "ref": "1519", "source": "spawn"}
            ]
        })];
        save_at(&path, &records, false).unwrap();
        let outcomes = unlink_remote_at(
            &path,
            "m4-mbp",
            "~/dev/repo/.worktrees/feat/",
            &["1519", "9999"],
        )
        .unwrap();
        assert_eq!(outcomes, [Unlinked::Removed, Unlinked::Absent]);
        let after = load_at(&path).unwrap();
        assert_eq!(after[0]["links"], serde_json::json!([]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn in_place_tasks_skip_worktree_checks_and_end_only_when_tab_closes() {
        let open_in_place = serde_json::json!({
            "host": "omarchy",
            "session_id": "s1",
            "window_id": "w1",
            "project": "notes",
            "name": "chat-1",
            "repo": "/home/johna/notes",
            "worktree": "/home/johna/notes",
            "branch": "",
            "in_place": true,
            "created_at": 10,
            "ended_at": null,
            "ended_reason": null,
        });
        assert!(is_in_place(&open_in_place));
        assert!(checked(&open_in_place));

        // Even if a bogus empty Worktree were somehow passed, apply_checks
        // ignores worktree state for in-place tasks and keeps live Rex status.
        let mut tasks = vec![open_in_place.clone()];
        let windows = HostWindows::parse("L\ts1\nS\ts1\nw1\tidle\t50\tready\n");
        let bogus_gone = Worktree {
            path: String::new(),
            branch: String::new(),
            dirty: 0,
            files: Vec::new(),
            merged: String::new(),
            head: String::new(),
            origin: String::new(),
            fizzy: String::new(),
            links: Vec::new(),
            fixes: Vec::new(),
        };
        let handle = std::thread::spawn(move || {
            (
                "omarchy".to_string(),
                Some(windows),
                Some(vec![Some(bogus_gone)]),
            )
        });
        let (live, dirty) = apply_checks(&mut tasks, vec![handle], 10_000);
        assert!(tasks[0]["ended_at"].is_null());
        assert_eq!(live["w1"].state, "idle");
        assert!(dirty.is_empty());

        // When Rex reports the tab gone, the in-place task ends as "tab closed"
        // and is no longer checked (no leftover worktree to clean up).
        let windows_gone = HostWindows::parse("L\ts1\nS\ts1\n");
        let handle = std::thread::spawn(move || {
            ("omarchy".to_string(), Some(windows_gone), Some(vec![None]))
        });
        apply_checks(&mut tasks, vec![handle], 10_001);
        assert_eq!(tasks[0]["ended_at"], 10_001);
        assert_eq!(tasks[0]["ended_reason"], "tab closed");
        assert!(!checked(&tasks[0]));
    }
}
