//! Agent sessions. Each task records its agent's session ID
//! (`agent_session`) so an ended task can be resumed in a new tab. Every
//! harness keeps sessions per working directory, which is why a resume
//! recreates the worktree at the same path.

use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::rex::{home, host_bash, sh_quote};

/// A random (v4) UUID, the form Claude requires for `--session-id`.
#[must_use]
pub fn new_session_id() -> String {
    let mut b = [0u8; 16];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| io::Read::read_exact(&mut f, &mut b))
        .is_err()
    {
        // Not expected; fall back on the clock and pid.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
            ^ (u128::from(std::process::id()) << 64);
        b = seed.to_le_bytes();
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = b.iter().fold(String::with_capacity(32), |mut h, x| {
        let _ = write!(h, "{x:02x}");
        h
    });
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// The harness's session directory for `worktree` under `profile`, on this
/// host: Claude's `$CLAUDE_CONFIG_DIR/projects/<path, non-alphanumerics as
/// ->`, Pi's `$PI_CODING_AGENT_DIR/sessions/--<path, / as ->--`.
pub(crate) fn session_dir(profile: &str, harness: &str, worktree: &str) -> PathBuf {
    let work = if profile == "work" {
        work_env().unwrap_or_default()
    } else {
        Vec::new()
    };
    session_dir_with_env(harness, &work, worktree)
}

/// `session_dir` with `env` already loaded for the profile (empty for
/// personal, `work_env()` for work).
pub(crate) fn session_dir_with_env(
    harness: &str,
    env: &[(String, String)],
    worktree: &str,
) -> PathBuf {
    let var = |k: &str| {
        env.iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var(k).ok())
    };
    if harness == "pi" {
        let root =
            var("PI_CODING_AGENT_DIR").map_or_else(|| home().join(".config/pi"), PathBuf::from);
        let flat = worktree
            .trim_start_matches('/')
            .replace(['/', '\\', ':'], "-");
        root.join("sessions").join(format!("--{flat}--"))
    } else {
        let root = var("CLAUDE_CONFIG_DIR")
            .map_or_else(|| home().join(".config/claude-personal"), PathBuf::from);
        let flat: String = worktree
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        root.join("projects").join(flat)
    }
}

/// Antigravity's data folder on this host. It has no profile variable, so
/// both profiles share it.
fn agy_dir() -> PathBuf {
    home().join(".gemini/antigravity-cli")
}

/// The conversation Antigravity last used in `worktree`, from its
/// `cache/last_conversations.json` (folder, by real path, to conversation
/// ID).
fn agy_last_conversation(worktree: &str) -> Option<String> {
    let text = std::fs::read_to_string(agy_dir().join("cache/last_conversations.json")).ok()?;
    let map: serde_json::Value = serde_json::from_str(&text).ok()?;
    let real = std::fs::canonicalize(worktree).ok();
    let real = real.as_ref().and_then(|p| p.to_str());
    [real, Some(worktree.trim_end_matches('/'))]
        .into_iter()
        .flatten()
        .find_map(|key| map[key].as_str().filter(|id| !id.is_empty()))
        .map(String::from)
}

/// Extract an Antigravity conversation ID from an open fd symlink target:
/// `presence/<id>.lock` (primary) or `conversations/<id>.db` (secondary,
/// ignoring `.db-wal`/`.db-shm` and unlinked ` (deleted)` entries).
pub(crate) fn agy_conversation_from_fd_dir(fd_dir: &Path) -> io::Result<Option<String>> {
    let mut db_id: Option<String> = None;
    for entry in std::fs::read_dir(fd_dir)? {
        let Ok(entry) = entry else { continue };
        let Ok(target) = std::fs::read_link(entry.path()) else {
            continue;
        };
        let Some(target) = target.to_str() else {
            continue;
        };
        if target.ends_with(" (deleted)") {
            continue;
        }
        if let Some(id) = parse_agy_fd_target(target, "/presence/", ".lock") {
            return Ok(Some(id.to_string()));
        }
        if db_id.is_none()
            && let Some(id) = parse_agy_fd_target(target, "/conversations/", ".db")
        {
            db_id = Some(id.to_string());
        }
    }
    Ok(db_id)
}

fn parse_agy_fd_target<'a>(target: &'a str, marker: &str, suffix: &str) -> Option<&'a str> {
    let rest = target
        .rsplit_once(marker)
        .map(|(_, rest)| rest)
        .or_else(|| target.strip_prefix(marker.trim_start_matches('/')))?;
    let id = rest.strip_suffix(suffix)?;
    (!id.is_empty() && !id.contains('/')).then_some(id)
}

/// The Antigravity conversation currently open in process `pid`, by inspecting
/// `/proc/<pid>/fd` on Linux for `presence/<id>.lock` (primary) and
/// `conversations/<id>.db` (secondary).
///
/// Returns `Ok(Some(id))` when an open lock/db is found, `Ok(None)` when
/// `/proc/<pid>/fd` is readable (the process is alive) but has not opened a
/// conversation yet, and `Err` when `/proc/<pid>/fd` is unavailable (non-Linux,
/// `pid == 0`, exited process, or permission denied).
pub(crate) fn agy_conversation_for_pid(pid: u64) -> io::Result<Option<String>> {
    #[cfg(target_os = "linux")]
    {
        if pid == 0 {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no pid"));
        }
        agy_conversation_from_fd_dir(&PathBuf::from(format!("/proc/{pid}/fd")))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "/proc/<pid>/fd is only available on Linux",
        ))
    }
}

/// The newest agent session for `worktree` on this host, by file time:
/// Claude names its files `ID.jsonl`, Pi `TIMESTAMP_ID.jsonl`; Antigravity
/// says which it last used there.
#[must_use]
pub fn newest_session_here(profile: &str, harness: &str, worktree: &str) -> Option<String> {
    newest_session_here_for_pid(profile, harness, worktree, 0)
}

/// Like `newest_session_here`, but when `harness == "agy"` and `pid > 0` on
/// Linux, resolves the exact conversation open in `/proc/<pid>/fd` without
/// falling back to `last_conversations.json` when the live process hasn't
/// opened one yet.
#[must_use]
pub fn newest_session_here_for_pid(
    profile: &str,
    harness: &str,
    worktree: &str,
    pid: u64,
) -> Option<String> {
    if harness == "agy" {
        if pid > 0
            && let Ok(found) = agy_conversation_for_pid(pid)
        {
            return found;
        }
        return agy_last_conversation(worktree);
    }
    let dir = session_dir(profile, harness, worktree);
    let newest = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max_by_key(|(t, _)| *t)?
        .1;
    let stem = newest.file_stem()?.to_str()?;
    Some(stem.rsplit_once('_').map_or(stem, |(_, id)| id).to_string())
}

/// Whether this host has the agent session for `worktree`.
#[must_use]
pub fn session_exists_here(profile: &str, harness: &str, worktree: &str, id: &str) -> bool {
    let work = if profile == "work" {
        work_env().unwrap_or_default()
    } else {
        Vec::new()
    };
    session_exists_with_env(harness, &work, worktree, id)
}

/// `session_exists_here` with `env` already loaded for the profile.
#[must_use]
pub(crate) fn session_exists_with_env(
    harness: &str,
    env: &[(String, String)],
    worktree: &str,
    id: &str,
) -> bool {
    // Antigravity keeps conversations in one folder, not per directory.
    if harness == "agy" {
        return !id.contains('/') && agy_dir().join(format!("conversations/{id}.db")).is_file();
    }
    let dir = session_dir_with_env(harness, env, worktree);
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name == format!("{id}.jsonl") || name.ends_with(&format!("_{id}.jsonl"))
        })
    })
}

/// The newest agent session for a task's worktree, asked of its host
/// (`switchyard task session-of`). None when there is none or the host can't
/// be reached.
#[must_use]
pub fn newest_session(task: &serde_json::Value) -> Option<String> {
    let f = |k: &str| task[k].as_str().unwrap_or("").to_string();
    let host = f("host");
    let harness = match f("harness").as_str() {
        "" => "claude".to_string(),
        h => h.to_string(),
    };
    let script = format!(
        "switchyard task session-of {} {} {}",
        sh_quote(&f("profile")),
        sh_quote(&harness),
        sh_quote(&f("worktree"))
    );
    if harness == "agy" && task["ended_at"].is_null() {
        let session = f("session_id");
        let window = f("window_id");
        let pid = (!session.is_empty() && !window.is_empty())
            .then(|| crate::transcript::agent_pane(&host, &session, &window).ok())
            .flatten()
            .and_then(|p| p["pid"].as_u64())
            .unwrap_or(0);
        if pid > 0 {
            let with_pid = format!("{script} {pid}");
            if let Some(out) = host_bash(&host, &with_pid).filter(|o| o.status.success()) {
                let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
                return (!id.is_empty()).then_some(id);
            }
        }
    }
    let out = host_bash(&host, &script).filter(|o| o.status.success())?;
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!id.is_empty()).then_some(id)
}

/// The environment ~/dev/src/amfaro's mise config sets, as `mise env -J`.
///
/// # Errors
///
/// Fails when mise can't run, exits unsuccessfully, or prints output that
/// isn't a JSON object.
pub fn work_env() -> Result<Vec<(String, String)>, String> {
    let out = Command::new("mise")
        .arg("-C")
        .arg(home().join("dev/src/amfaro"))
        .args(["env", "-J"])
        .output()
        .map_err(|e| format!("mise: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "mise env: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let env: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("mise env: {e}"))?;
    Ok(env
        .into_iter()
        .filter_map(|(k, v)| Some((k, v.as_str()?.to_string())))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn temp_fd_dir(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "switchyard-agy-fd-{name}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn agy_fd_prefers_presence_lock_over_conversation_db() {
        let dir = temp_fd_dir("lock-wins");
        symlink(
            "/home/u/.gemini/antigravity-cli/conversations/db-id.db",
            dir.join("10"),
        )
        .unwrap();
        symlink(
            "/home/u/.gemini/antigravity-cli/conversations/db-id.db-wal",
            dir.join("11"),
        )
        .unwrap();
        symlink(
            "/home/u/.gemini/antigravity-cli/presence/lock-id.lock",
            dir.join("12"),
        )
        .unwrap();
        assert_eq!(
            agy_conversation_from_fd_dir(&dir).unwrap(),
            Some("lock-id".into())
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn agy_fd_falls_back_to_db_and_ignores_wal_shm_and_deleted() {
        let dir = temp_fd_dir("db-only");
        symlink("/dev/pts/1", dir.join("0")).unwrap();
        symlink(
            "/home/u/.gemini/antigravity-cli/presence/old-id.lock (deleted)",
            dir.join("5"),
        )
        .unwrap();
        symlink(
            "/home/u/.gemini/antigravity-cli/conversations/wal-only.db-wal",
            dir.join("6"),
        )
        .unwrap();
        symlink(
            "/home/u/.gemini/antigravity-cli/conversations/shm-only.db-shm",
            dir.join("7"),
        )
        .unwrap();
        symlink(
            "/home/u/.gemini/antigravity-cli/conversations/active-db.db",
            dir.join("8"),
        )
        .unwrap();
        assert_eq!(
            agy_conversation_from_fd_dir(&dir).unwrap(),
            Some("active-db".into())
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn agy_fd_returns_ok_none_when_readable_without_conversation_and_err_when_missing() {
        let dir = temp_fd_dir("empty");
        symlink("/dev/pts/1", dir.join("0")).unwrap();
        assert_eq!(agy_conversation_from_fd_dir(&dir).unwrap(), None);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(agy_conversation_from_fd_dir(&dir).is_err());
    }
}
