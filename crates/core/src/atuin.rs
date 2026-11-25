//! Restoring and converting agent sessions through Atuin Threads.
//!
//! Atuin keeps a synced copy of each captured thread under the agent's own
//! session ID:
//! - `restore`: `atuin threads resume --print ID` writes a missing transcript
//!   back out at the directory the session ran in and under the profile's
//!   `CLAUDE_CONFIG_DIR` or `PI_CODING_AGENT_DIR`, so the harness can resume
//!   it as usual (used when a resumed task's transcript was cleaned up).
//! - `convert`: `atuin threads resume --print --in TARGET ID` converts a
//!   captured thread into a new session for `TARGET`, carrying over messages
//!   and tool calls with a note naming the original session, and prints the
//!   command naming the new session ID.
//!
//! The personal profile's threads live in the default store (`atuin`), the
//! work profile's in its own (`atuin-work`, which system-config installs with
//! its `ATUIN_HOME`). Other profiles (such as `notes` / `pi-notes`) and
//! Antigravity (`agy`) are neither captured nor valid `--in` targets.

use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::sessions::{session_exists_with_env, work_env};

/// How long an Atuin Threads call may take, sync included, before giving up.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Atuin's harness name for a Switchyard harness, when Atuin supports it both
/// for thread capture and as a `--in` target. Antigravity (`agy`) is neither.
#[must_use]
pub fn harness_name(harness: &str) -> Option<&'static str> {
    match harness {
        "claude" => Some("claude-code"),
        "pi" => Some("pi"),
        _ => None,
    }
}

/// The Atuin CLI command holding `profile`'s threads: `atuin` for personal
/// (or an older task with no recorded profile), `atuin-work` for work. Other
/// profiles (such as `notes`) are not captured.
#[must_use]
pub fn store_program(profile: &str) -> Option<&'static str> {
    match profile {
        "work" => Some("atuin-work"),
        "" | "personal" => Some("atuin"),
        _ => None,
    }
}

/// Validate moving a task of `profile` and `harness` to another host through
/// Atuin Threads, returning `(program, atuin_harness)`.
///
/// The personal store (`atuin`) syncs through nas, so threads can be restored
/// on any pool host. The work store (`atuin-work`) is local-only and not
/// synced, so moving a work task across hosts is refused clearly rather than
/// dropping the conversation or crossing profiles.
///
/// # Errors
///
/// Fails with a clear explanation when `profile` is `work` (unsynced store),
/// Atuin doesn't capture `profile`, or Atuin doesn't capture `harness`.
pub fn move_plan(profile: &str, harness: &str) -> Result<(&'static str, &'static str), String> {
    if profile == "work" {
        return Err(
            "work tasks use the local-only atuin-work store (not synced), so they can't be moved across hosts"
                .into(),
        );
    }
    let program = store_program(profile)
        .ok_or_else(|| format!("Atuin doesn't capture the {profile} profile"))?;
    let atuin_harness =
        harness_name(harness).ok_or_else(|| format!("Atuin doesn't capture {harness} sessions"))?;
    Ok((program, atuin_harness))
}

/// Sync `profile`'s Atuin store on this host. Only synced stores (personal)
/// are allowed; work's local-only store is refused.
///
/// # Errors
///
/// Fails when `profile` is unsynced or uncaptured, or when `atuin sync`
/// fails or times out.
pub fn sync_here(profile: &str) -> Result<(), String> {
    if profile == "work" {
        return Err("atuin-work is local-only and cannot be synced".into());
    }
    let program = store_program(profile)
        .ok_or_else(|| format!("Atuin doesn't capture the {profile} profile"))?;
    run_atuin(program, &["sync"], &[], &crate::rex::home(), "sync failed")?;
    Ok(())
}

/// This host's Atuin host ID from `atuin info`, used as `@<host id>` when
/// resolving a diverged thread branch during a cross-host move.
#[must_use]
pub fn host_id_here(profile: &str) -> Option<String> {
    if profile == "work" {
        return None;
    }
    let program = store_program(profile)?;
    let out = run_atuin(program, &["info"], &[], &crate::rex::home(), "info failed").ok()?;
    parse_host_id(&out)
}

/// Extract `host id: ...` from `atuin info` output.
#[must_use]
pub fn parse_host_id(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let id = line.trim().strip_prefix("host id:")?.trim();
        (!id.is_empty() && !id.chars().any(char::is_whitespace)).then(|| id.to_string())
    })
}

/// Whether an Atuin error says the local copy of the thread diverged from
/// sync and requires `--switch` / `--branch` / `--as-is`.
#[must_use]
pub fn needs_branch_switch(err: &str) -> bool {
    err.contains("--switch")
        || err.contains("--branch")
        || err.contains("--as-is")
        || err.contains("diverged")
}

/// Restore agent session `id` on this host after syncing for a cross-host
/// move, into `worktree`.
///
/// Unlike `restore`, this also handles a thread that already exists on this
/// host and diverged from `source_host_id` by switching it in place to
/// `@<source_host_id>` via `--switch` (never silently falling back to
/// `--as-is`, which would drop the source host's newer turns), and verifies
/// that the harness transcript now exists for `worktree`.
///
/// # Errors
///
/// Fails when `move_plan` refuses the profile/harness, `worktree` is missing,
/// Atuin fails or has no such thread, or the restored session isn't found for
/// `worktree`.
pub fn restore_for_move(
    profile: &str,
    harness: &str,
    env: &[(String, String)],
    worktree: &Path,
    id: &str,
    source_host_id: Option<&str>,
) -> Result<(), String> {
    let (program, atuin_harness) = move_plan(profile, harness)?;
    if id.is_empty() || id.starts_with('-') {
        return Err(format!("{id} isn't a session ID"));
    }
    if !worktree.is_dir() {
        return Err(format!("worktree {} is missing", worktree.display()));
    }
    let missing = format!("no {atuin_harness} thread {id} in {program}");
    let first = run_atuin(
        program,
        &["threads", "resume", "--print", id],
        env,
        worktree,
        &missing,
    );
    match first {
        Ok(_) => {}
        Err(err) if needs_branch_switch(&err) => {
            let branch_ref = source_host_id
                .filter(|s| !s.is_empty())
                .map(|s| format!("@{s}"));
            let args: Vec<&str> = match branch_ref.as_deref() {
                Some(b) => vec![
                    "threads", "resume", "--print", "--switch", "--branch", b, id,
                ],
                None => vec!["threads", "resume", "--print", "--switch", id],
            };
            run_atuin(program, &args, env, worktree, &missing)
                .map_err(|second| if second == missing { err } else { second })?;
        }
        Err(err) => return Err(err),
    }
    let here = worktree.display().to_string();
    if !session_exists_with_env(harness, env, &here, id) {
        return Err(format!(
            "{program} restored {harness} session {id}, but not for {here}"
        ));
    }
    Ok(())
}

/// `restore_for_move` on this host.
///
/// # Errors
///
/// Fails as `restore_for_move` does.
pub fn restore_for_move_here(
    profile: &str,
    harness: &str,
    worktree: &Path,
    id: &str,
    source_host_id: Option<&str>,
) -> Result<(), String> {
    restore_for_move(profile, harness, &[], worktree, id, source_host_id)
}

/// Validate continuing a task of `profile` from `source` to `target` through
/// Atuin Threads, returning `(program, source_atuin, target_atuin)`.
///
/// # Errors
///
/// Fails with a clear explanation when Atuin doesn't capture `profile` or
/// `source`, doesn't support `target` as a `--in` target, or `source == target`.
pub fn switch_plan(
    profile: &str,
    source: &str,
    target: &str,
) -> Result<(&'static str, &'static str, &'static str), String> {
    let program = store_program(profile)
        .ok_or_else(|| format!("Atuin doesn't capture the {profile} profile"))?;
    let src =
        harness_name(source).ok_or_else(|| format!("Atuin doesn't capture {source} sessions"))?;
    let dst = harness_name(target).ok_or_else(|| {
        format!("Atuin doesn't support {target} as a target harness (claude and pi only)")
    })?;
    if source == target {
        return Err(format!("already running in {source}"));
    }
    Ok((program, src, dst))
}

/// Write agent session `id` back out from Atuin. `env` is the profile's own
/// environment (it says where the harness keeps sessions); `worktree` is
/// where the agent runs.
///
/// # Errors
///
/// Fails when Atuin doesn't capture the profile or harness, isn't installed,
/// has no such thread, can't restore it without a choice, or takes too long.
pub fn restore(
    profile: &str,
    harness: &str,
    env: &[(String, String)],
    worktree: &Path,
    id: &str,
) -> Result<(), String> {
    let program = store_program(profile)
        .ok_or_else(|| format!("Atuin doesn't capture the {profile} profile"))?;
    let atuin_harness =
        harness_name(harness).ok_or_else(|| format!("Atuin doesn't capture {harness}"))?;
    if id.is_empty() || id.starts_with('-') {
        return Err(format!("{id} isn't a session ID"));
    }
    run_atuin(
        program,
        &["threads", "resume", "--print", id],
        env,
        worktree,
        &format!("no {atuin_harness} thread {id} in {program}"),
    )?;
    Ok(())
}

/// Convert agent session `id` (from `source_harness`) into a new session for
/// `target_harness` via `atuin threads resume --print --in TARGET ID`. `env`
/// is the profile's own environment (`CLAUDE_CONFIG_DIR`,
/// `PI_CODING_AGENT_DIR`), and `worktree` must already exist on disk so Atuin
/// writes the new session under it rather than falling back elsewhere.
///
/// Returns the new session ID that Atuin created in `target_harness`.
///
/// # Errors
///
/// Fails when Atuin doesn't support the profile or harnesses, `worktree` is
/// missing, Atuin isn't installed or has no such thread, the command times
/// out, or the created session can't be found for `worktree`.
pub fn convert(
    profile: &str,
    source_harness: &str,
    target_harness: &str,
    env: &[(String, String)],
    worktree: &Path,
    id: &str,
) -> Result<String, String> {
    let (program, src_atuin, dst_atuin) = switch_plan(profile, source_harness, target_harness)?;
    if id.is_empty() || id.starts_with('-') {
        return Err(format!("{id} isn't a session ID"));
    }
    if !worktree.is_dir() {
        return Err(format!("worktree {} is missing", worktree.display()));
    }
    let missing = format!("no {src_atuin} thread {id} in {program}");
    let stdout = match run_atuin(
        program,
        &["threads", "resume", "--print", "--in", dst_atuin, id],
        env,
        worktree,
        &missing,
    ) {
        Ok(out) => out,
        // A thread that diverged on another machine asks for --branch; pick
        // this machine's branch before giving up.
        Err(first) if first.contains("--branch") => run_atuin(
            program,
            &[
                "threads", "resume", "--print", "--in", dst_atuin, "--branch", "this", id,
            ],
            env,
            worktree,
            &missing,
        )
        .map_err(|_| first)?,
        Err(e) => return Err(e),
    };
    let new_id = parse_printed_session(&stdout, target_harness)?;
    let here = worktree.display().to_string();
    if !session_exists_with_env(target_harness, env, &here, &new_id) {
        return Err(format!(
            "{program} wrote {target_harness} session {new_id}, but not for {here}"
        ));
    }
    Ok(new_id)
}

/// `convert` on this host, loading `work_env()` for the work profile.
///
/// # Errors
///
/// Fails when the work profile's environment can't be loaded, or as `convert`
/// does.
pub fn convert_here(
    profile: &str,
    source_harness: &str,
    target_harness: &str,
    worktree: &Path,
    id: &str,
) -> Result<String, String> {
    let env = if profile == "work" {
        work_env()?
    } else {
        Vec::new()
    };
    convert(profile, source_harness, target_harness, &env, worktree, id)
}

/// Run `program` with `args` and `env` in `cwd`, returning its stdout on
/// success or a concise one-line error on failure or timeout.
fn run_atuin(
    program: &str,
    args: &[&str],
    env: &[(String, String)],
    cwd: &Path,
    not_found: &str,
) -> Result<String, String> {
    let mut child = Command::new(program)
        .args(args)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    let drain = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_string(&mut s);
            }
            s
        })
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    );
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
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    if status.success() {
        return Ok(stdout);
    }
    Err(first_error(&stderr).map_or_else(|| not_found.to_string(), |e| format!("{program}: {e}")))
}

/// Extract the new session ID from `atuin threads resume --print --in ...`
/// stdout: `claude --resume ID` or `pi --session PATH` (whose filename stem
/// ends with `_ID`).
fn parse_printed_session(stdout: &str, target_harness: &str) -> Result<String, String> {
    let flag = match target_harness {
        "claude" => "--resume",
        "pi" => "--session",
        _ => return Err(format!("unsupported target harness {target_harness}")),
    };
    let value = flag_arg(stdout, flag)
        .ok_or_else(|| format!("Atuin printed no {flag} argument for {target_harness}"))?;
    let id = if target_harness == "pi" {
        let name = value.rsplit('/').next().unwrap_or(&value);
        let stem = name.strip_suffix(".jsonl").unwrap_or(name);
        stem.rsplit_once('_').map_or(stem, |(_, id)| id).to_string()
    } else {
        value
    };
    if id.is_empty() || id.contains('/') || id.chars().any(char::is_whitespace) {
        return Err(format!(
            "Atuin printed an unreadable {target_harness} session ID: {id:?}"
        ));
    }
    Ok(id)
}

/// The shell-unquoted argument immediately following `flag` in `command`.
fn flag_arg(command: &str, flag: &str) -> Option<String> {
    let mut rest = command;
    loop {
        let i = rest.find(flag)?;
        let before_ok = i == 0 || rest[..i].ends_with(char::is_whitespace);
        let after = &rest[i + flag.len()..];
        let after_ok = after.starts_with(char::is_whitespace);
        if before_ok && after_ok {
            return next_shell_word(after.trim_start());
        }
        rest = &rest[i + flag.len()..];
    }
}

/// Parse one POSIX shell word at the start of `s` (single-quoted,
/// double-quoted, or backslash-escaped).
fn next_shell_word(s: &str) -> Option<String> {
    if s.is_empty() {
        return None;
    }
    let mut out = String::new();
    let mut chars = s.chars();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                } else {
                    out.push(c);
                }
            }
            Some('"') => match c {
                '"' => quote = None,
                '\\' => {
                    if let Some(next) = chars.next() {
                        out.push(next);
                    }
                }
                _ => out.push(c),
            },
            Some(_) => unreachable!(),
            None => match c {
                c if c.is_whitespace() => break,
                '\'' | '"' => quote = Some(c),
                '\\' => {
                    if let Some(next) = chars.next() {
                        out.push(next);
                    }
                }
                _ => out.push(c),
            },
        }
    }
    (!out.is_empty()).then_some(out)
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
        let store = |p, h| Some((store_program(p)?, harness_name(h)?));
        assert_eq!(store("personal", "claude"), Some(("atuin", "claude-code")));
        assert_eq!(store("personal", "pi"), Some(("atuin", "pi")));
        assert_eq!(store("", "claude"), Some(("atuin", "claude-code")));
        assert_eq!(store("work", "claude"), Some(("atuin-work", "claude-code")));
        assert_eq!(store("work", "pi"), Some(("atuin-work", "pi")));
        assert_eq!(store("notes", "pi"), None);
    }

    #[test]
    fn antigravity_is_neither_captured_nor_a_target() {
        assert_eq!(harness_name("agy"), None);
        assert!(restore("personal", "agy", &[], Path::new("/"), "x").is_err());
        assert!(
            switch_plan("personal", "agy", "pi")
                .unwrap_err()
                .contains("doesn't capture agy")
        );
        assert!(
            switch_plan("personal", "claude", "agy")
                .unwrap_err()
                .contains("doesn't support agy as a target")
        );
        assert!(
            switch_plan("notes", "pi", "claude")
                .unwrap_err()
                .contains("doesn't capture the notes profile")
        );
        assert!(
            switch_plan("personal", "claude", "claude")
                .unwrap_err()
                .contains("already running in claude")
        );
        assert_eq!(
            switch_plan("personal", "claude", "pi"),
            Ok(("atuin", "claude-code", "pi"))
        );
        assert_eq!(
            switch_plan("work", "pi", "claude"),
            Ok(("atuin-work", "pi", "claude-code"))
        );
    }

    #[test]
    fn parses_printed_resume_commands() {
        let claude =
            "cd -- /r/.worktrees/x && claude --resume 67dbf6e4-b352-4ac9-b9f1-ca054f306791\n";
        assert_eq!(
            parse_printed_session(claude, "claude").unwrap(),
            "67dbf6e4-b352-4ac9-b9f1-ca054f306791"
        );
        let pi = "cd -- /r/.worktrees/x && pi --session /p/sessions/--r-.worktrees-x--/2026-11-24T20-00-00-000Z_01a20fe7-3310-74c5-b7e6-8adaea3290af.jsonl\n";
        assert_eq!(
            parse_printed_session(pi, "pi").unwrap(),
            "01a20fe7-3310-74c5-b7e6-8adaea3290af"
        );
        let quoted_pi = "cd -- '/r/my repo' && pi --session '/p/sessions/--r-my repo--/2026-11-24_abc-123.jsonl'\n";
        assert_eq!(parse_printed_session(quoted_pi, "pi").unwrap(), "abc-123");
        let escaped_pi = "cd -- /r && pi --session /p/my\\ dir/2026-11-24_def-456.jsonl\n";
        assert_eq!(parse_printed_session(escaped_pi, "pi").unwrap(), "def-456");
        assert!(parse_printed_session("cd -- /r && claude", "claude").is_err());
        assert!(parse_printed_session(pi, "agy").is_err());
    }

    #[test]
    fn an_error_names_atuins_first_line() {
        let err = "Error: no thread matches x\n\nLocation:\n    resume.rs:1:1\n";
        assert_eq!(first_error(err), Some("no thread matches x"));
        assert_eq!(first_error("\n"), None);
        let unknown = "Error: the picker needs a terminal; to resume without one, name the thread";
        assert_eq!(first_error(unknown), None);
    }

    #[test]
    fn move_plan_allows_personal_captured_harnesses_and_refuses_work_notes_agy() {
        assert_eq!(
            move_plan("personal", "claude"),
            Ok(("atuin", "claude-code"))
        );
        assert_eq!(move_plan("personal", "pi"), Ok(("atuin", "pi")));
        assert_eq!(move_plan("", "claude"), Ok(("atuin", "claude-code")));
        let work_err = move_plan("work", "claude").unwrap_err();
        assert!(
            work_err.contains("atuin-work") && work_err.contains("not synced"),
            "{work_err}"
        );
        assert!(move_plan("notes", "pi").unwrap_err().contains("notes"));
        assert!(move_plan("personal", "agy").unwrap_err().contains("agy"));
    }

    #[test]
    fn parses_host_id_and_detects_divergence_errors() {
        let info = "Version info:\nversion: 18.24.8\n\nHost info:\nhost id: 01a20f5f535e75018dc129311465cb7c\n";
        assert_eq!(
            parse_host_id(info).as_deref(),
            Some("01a20f5f535e75018dc129311465cb7c")
        );
        assert_eq!(parse_host_id("Host info:\nhost id: \n"), None);
        assert!(needs_branch_switch("use --switch or --as-is"));
        assert!(needs_branch_switch(
            "thread diverged on another host; pass --branch"
        ));
        assert!(!needs_branch_switch("no claude-code thread abc in atuin"));
    }
}
