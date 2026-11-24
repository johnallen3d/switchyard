//! Restoring a resumed task's agent session from Atuin Threads when the
//! harness no longer has its transcript (Claude removes old ones after
//! `cleanupPeriodDays`).
//!
//! Atuin keeps a synced copy of each captured thread under the agent's own
//! session ID. `atuin threads resume ID --print` writes a missing transcript
//! back out, at the directory the session ran in and under the profile's
//! `CLAUDE_CONFIG_DIR` or `PI_CODING_AGENT_DIR`, and prints the resume
//! command instead of running it, so the harness then resumes it as usual.
//! The personal profile's threads are in the default store (`atuin`), the
//! work profile's in its own (`atuin-work`, which system-config installs with
//! its `ATUIN_HOME`). Antigravity isn't captured.

use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long a restore may take, sync included, before the task starts a new
/// session instead.
const TIMEOUT: Duration = Duration::from_secs(30);

/// The Atuin command holding `profile`'s threads, and Atuin's name for
/// `harness`; None when Atuin captures none of that harness.
fn store(profile: &str, harness: &str) -> Option<(&'static str, &'static str)> {
    let atuin_harness = match harness {
        "claude" => "claude-code",
        "pi" => "pi",
        _ => return None,
    };
    let program = if profile == "work" {
        "atuin-work"
    } else {
        "atuin"
    };
    Some((program, atuin_harness))
}

/// Write agent session `id` back out from Atuin. `env` is the profile's own
/// environment (it says where the harness keeps sessions); `worktree` is
/// where the agent runs.
///
/// # Errors
///
/// Fails when Atuin doesn't capture the harness, isn't installed, has no
/// such thread, can't restore it without a choice, or takes too long.
pub fn restore(
    profile: &str,
    harness: &str,
    env: &[(String, String)],
    worktree: &Path,
    id: &str,
) -> Result<(), String> {
    let (program, atuin_harness) =
        store(profile, harness).ok_or_else(|| format!("Atuin doesn't capture {harness}"))?;
    // Atuin takes an ID starting with - as a flag; no harness makes one.
    if id.starts_with('-') {
        return Err(format!("{id} isn't a session ID"));
    }
    let mut child = Command::new(program)
        .args(["threads", "resume", "--print", id])
        .envs(env.iter().map(|(k, v)| (k, v)))
        .current_dir(worktree)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{program} took over {}s", TIMEOUT.as_secs()));
            }
            Err(e) => return Err(format!("{program}: {e}")),
        }
    };
    if status.success() {
        return Ok(());
    }
    let mut stderr = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }
    Err(first_error(&stderr).map_or_else(
        || format!("no {atuin_harness} thread {id} in {program}"),
        |e| format!("{program}: {e}"),
    ))
}

/// The first line of Atuin's error output, without its `Error: ` prefix;
/// None when it has none, or when it only says no thread matched (Atuin
/// takes an ID it doesn't know as a search and asks for a terminal to pick
/// from).
fn first_error(stderr: &str) -> Option<&str> {
    stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.strip_prefix("Error: ").unwrap_or(l))
        .filter(|l| !l.contains("picker needs a terminal"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_profile_reads_its_own_store() {
        assert_eq!(store("personal", "claude"), Some(("atuin", "claude-code")));
        assert_eq!(store("personal", "pi"), Some(("atuin", "pi")));
        assert_eq!(store("work", "claude"), Some(("atuin-work", "claude-code")));
        assert_eq!(store("work", "pi"), Some(("atuin-work", "pi")));
    }

    #[test]
    fn antigravity_is_not_captured() {
        assert_eq!(store("personal", "agy"), None);
        assert!(restore("personal", "agy", &[], Path::new("/"), "x").is_err());
    }

    #[test]
    fn an_error_names_atuins_first_line() {
        let err = "Error: no thread matches x\n\nLocation:\n    resume.rs:1:1\n";
        assert_eq!(first_error(err), Some("no thread matches x"));
        assert_eq!(first_error("\n"), None);
        let unknown = "Error: the picker needs a terminal; to resume without one, name the thread";
        assert_eq!(first_error(unknown), None);
    }
}
