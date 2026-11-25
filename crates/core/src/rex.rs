//! Calls to Rex servers and hosts: the rex CLI (no `-S` for the local
//! server), Bash over SSH by Rex label, and the state log.
//!
//! SSH calls share one connection per host (see `ssh`): connecting costs
//! about 100 ms to a dev host, more than most of the scripts sent over it.

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where Rex keeps its current server/CLI binary on macOS, relative to
/// `$HOME` — stable across app bundle renames and updates.
const MACOS_REX_BIN_DIR: &str = "Library/Application Support/com.superlogical.rex/RexServer/Versions/Current/Rex Server.app/Contents/MacOS";

/// How `rex` was resolved, so `rex_command` knows whether to also put its
/// directory at the front of the child's `PATH` (for `rex do` scripts that
/// spawn `rex`).
#[derive(Debug, PartialEq, Eq)]
struct ResolvedRex {
    path: PathBuf,
    prepend_dir: bool,
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn find_in_path(path_var: &OsStr, name: &str) -> Option<PathBuf> {
    std::env::split_paths(path_var)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn resolve_rex(
    rex_bin_dir: Option<&OsStr>,
    path_var: Option<&OsStr>,
    home_dir: &Path,
    is_macos: bool,
) -> ResolvedRex {
    if let Some(dir) = rex_bin_dir.filter(|d| !d.is_empty()) {
        let candidate = PathBuf::from(dir).join("rex");
        if is_executable(&candidate) {
            return ResolvedRex {
                path: candidate,
                prepend_dir: true,
            };
        }
    }
    if let Some(path) = path_var
        && let Some(found) = find_in_path(path, "rex")
    {
        return ResolvedRex {
            path: found,
            prepend_dir: false,
        };
    }
    if is_macos {
        let candidate = home_dir.join(MACOS_REX_BIN_DIR).join("rex");
        if is_executable(&candidate) {
            return ResolvedRex {
                path: candidate,
                prepend_dir: true,
            };
        }
    }
    ResolvedRex {
        path: PathBuf::from("rex"),
        prepend_dir: false,
    }
}

fn prepend_path(dir: &Path, existing: Option<&OsStr>) -> Option<OsString> {
    let mut dirs: Vec<PathBuf> = existing
        .map(|p| std::env::split_paths(p).collect())
        .unwrap_or_default();
    if dirs.first().is_some_and(|first| first == dir) {
        return None;
    }
    dirs.insert(0, dir.to_path_buf());
    std::env::join_paths(dirs).ok()
}

/// Path to the `rex` CLI binary: `$REX_BIN_DIR/rex` first, then `PATH`, then
/// macOS's `Versions/Current` server bundle directory, falling back to bare
/// `"rex"`.
#[must_use]
pub fn rex_bin() -> PathBuf {
    resolve_rex(
        std::env::var_os("REX_BIN_DIR").as_deref(),
        std::env::var_os("PATH").as_deref(),
        &home(),
        cfg!(target_os = "macos"),
    )
    .path
}

/// A `Command` for the `rex` CLI, resolved through `$REX_BIN_DIR/rex`,
/// `PATH`, and the macOS `Versions/Current` fallback. When resolved via
/// `REX_BIN_DIR` or the macOS fallback, its directory is placed at the front
/// of the child's `PATH` so Lua scripts run via `rex do` that invoke `rex`
/// find the same binary.
#[must_use]
pub fn rex_command() -> Command {
    let resolved = resolve_rex(
        std::env::var_os("REX_BIN_DIR").as_deref(),
        std::env::var_os("PATH").as_deref(),
        &home(),
        cfg!(target_os = "macos"),
    );
    let mut cmd = Command::new(&resolved.path);
    if resolved.prepend_dir
        && let Some(dir) = resolved.path.parent().filter(|d| !d.as_os_str().is_empty())
        && let Some(new_path) = prepend_path(dir, std::env::var_os("PATH").as_deref())
    {
        cmd.env("PATH", new_path);
    }
    cmd
}

/// This host's Rex label: its short hostname.
#[must_use]
pub fn local_label() -> String {
    Command::new("hostname")
        .arg("-s")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Run the rex CLI against `host` (no -S for the local server) and parse
/// its JSON output.
///
/// # Errors
///
/// Fails when rex can't run, exits unsuccessfully, or prints invalid JSON.
pub fn rex_json(host: &str, args: &[&str]) -> Result<serde_json::Value, String> {
    let mut cmd = rex_command();
    if host != local_label() {
        cmd.args(["-S", host]);
    }
    let out = cmd
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("rex: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "rex {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("rex {}: {e}", args.join(" ")))
}

/// Run a Bash script on `host` from stdin: over SSH by Rex label (as `probe`
/// reaches hosts; their login shell is Fish), or locally for the config
/// host. None when it can't be started or the host can't be reached;
/// otherwise the output whatever the script's exit status.
#[must_use]
pub fn host_bash(host: &str, script: &str) -> Option<std::process::Output> {
    let mut cmd = if host == local_label() {
        let mut c = Command::new("bash");
        c.arg("-s");
        c
    } else {
        let mut c = ssh(host);
        c.arg("bash -s");
        c
    };
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(script.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    // ssh exits 255 when it can't connect.
    if host != local_label() && out.status.code() == Some(255) {
        return None;
    }
    Some(out)
}

/// An ssh command to `host` (a Rex label, as the dev hosts are named in
/// ~/.ssh/config) that never prompts, gives up connecting after 3 s, and
/// shares a connection with the other calls to that host for a few minutes
/// (`ControlMaster`), so only the first call in a while pays for connecting.
/// The control sockets live in `runtime_dir`, whose short path keeps them
/// under macOS's 104-byte socket path limit; without it, each call connects
/// on its own. Like any ssh, it exits 255 when it can't connect.
#[must_use]
pub fn ssh(host: &str) -> Command {
    ssh_with(host, false)
}

/// `ssh`, with a terminal on `host` for an interactive program (`-t`).
#[must_use]
pub fn ssh_tty(host: &str) -> Command {
    ssh_with(host, true)
}

fn ssh_with(host: &str, tty: bool) -> Command {
    let mut cmd = Command::new("ssh");
    if tty {
        cmd.arg("-t");
    }
    cmd.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=3"]);
    if let Some(dir) = runtime_dir() {
        // %C is a hash of the connection's host, port and user.
        cmd.args(["-o", "ControlMaster=auto", "-o", "ControlPersist=180"])
            .arg("-o")
            .arg(format!("ControlPath={}/ssh-%C", dir.display()));
    }
    cmd.arg(host);
    cmd
}

/// A directory only this user can use, for sockets: switchyard-server's and
/// SSH's shared connections. `$XDG_RUNTIME_DIR/switchyard` where there is
/// one (Linux), else `/tmp/switchyard-UID`: macOS's `$TMPDIR` is too long
/// for socket paths. Created on first use; None when it can't be made or
/// someone else owns it.
#[must_use]
pub fn runtime_dir() -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: getuid has no preconditions and can't fail.
    let uid = unsafe { libc::getuid() };
    let dir = match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d).join("switchyard"),
        None => PathBuf::from(format!("/tmp/switchyard-{uid}")),
    };
    if let Err(e) = std::fs::create_dir(&dir)
        && e.kind() != std::io::ErrorKind::AlreadyExists
    {
        return None;
    }
    // /tmp is shared: refuse a directory (or a link) someone else made.
    let meta = std::fs::symlink_metadata(&dir).ok()?;
    if !meta.is_dir() || meta.uid() != uid {
        return None;
    }
    if meta.mode() & 0o077 != 0 {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).ok()?;
    }
    Some(dir)
}

/// Append a line to the state log the Lua action also uses
/// (`$XDG_STATE_HOME/rex/spawn.log`); the action drops `task launch`'s stderr
/// on success.
pub fn log(line: &str) {
    let state =
        std::env::var("XDG_STATE_HOME").map_or_else(|_| home().join(".local/state"), PathBuf::from);
    let dir = state.join("rex");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("spawn.log"))
    {
        let _ = writeln!(f, "switchyard: {line}");
    }
}

/// Where `diagnose` sends its lines; unset, `log`'s state log.
static DIAGNOSTICS: std::sync::OnceLock<fn(&str)> = std::sync::OnceLock::new();

/// Send `diagnose`'s lines to `sink` instead of the state log: the server
/// sends them to its own log. Only the first call counts.
pub fn set_diagnostics(sink: fn(&str)) {
    let _ = DIAGNOSTICS.set(sink);
}

/// Report a background failure (a tracker lookup that couldn't run, say),
/// so a missing or broken CLI is diagnosable: to the server's log in the
/// server, else to the state log (never to stderr, which a picker draws on).
pub fn diagnose(line: &str) {
    match DIAGNOSTICS.get() {
        Some(sink) => sink(line),
        None => log(line),
    }
}

#[must_use]
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[must_use]
pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

#[must_use]
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Run the rex CLI against `host` and return an error with its stderr.
///
/// # Errors
///
/// Fails when rex can't run or exits unsuccessfully.
pub fn rex(host: &str, args: &[&str]) -> Result<(), String> {
    let mut cmd = rex_command();
    if host != local_label() {
        cmd.args(["-S", host]);
    }
    let out = cmd
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("rex: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "rex {}: {}",
            args[..2.min(args.len())].join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// The home directory of `host`'s account: this host's own, or a remote host's
/// `$HOME` over SSH (falling back to `/home/ACCOUNT` from `server.whoami` for
/// Linux Rex servers when SSH isn't available). Empty when it can't be read.
///
/// # Errors
///
/// Fails when neither SSH nor `host`'s Rex server can answer.
pub fn host_home(host: &str) -> Result<String, String> {
    if host == local_label() {
        return Ok(home().display().to_string());
    }
    if let Some(out) = host_bash(host, "printf '%s' \"$HOME\"\n")
        && out.status.success()
    {
        let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if dir.starts_with('/') {
            return Ok(dir);
        }
    }
    let account = rex_json(host, &["api", "call", "server.whoami"])?["account"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    Ok(if account.is_empty() {
        String::new()
    } else {
        format!("/home/{account}")
    })
}

/// Show a tab in its host's Rex app, as the config's jumps do; with no
/// window, the session's own.
///
/// # Errors
///
/// Fails when rex can't run or exits unsuccessfully.
pub fn select(host: &str, session: &str, window: &str) -> Result<(), String> {
    let mut cmd = rex_command();
    if host != local_label() {
        cmd.args(["-S", host]);
    }
    let mut args = vec![
        "do".to_string(),
        "session.select".to_string(),
        format!("session_id={session}"),
    ];
    if !window.is_empty() {
        args.push(format!("window_id={window}"));
    }
    let out = cmd
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("rex: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "can't show the tab on {host}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(name: &str) -> PathBuf {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "switchyard-rex-test-{}-{}-{seq}",
            std::process::id(),
            name
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_file(path: &Path, executable: bool) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        let mode = if executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn resolve_rex_prefers_rex_bin_dir_then_path_then_macos_fallback() {
        let root = temp_dir("order");
        let rex_dir = root.join("rex-bin");
        let path_dir = root.join("path-bin");
        let home_dir = root.join("home");
        let mac_dir = home_dir.join(MACOS_REX_BIN_DIR);

        make_file(&rex_dir.join("rex"), true);
        make_file(&path_dir.join("rex"), true);
        make_file(&mac_dir.join("rex"), true);

        let path_env = std::env::join_paths([&path_dir]).unwrap();

        // 1. REX_BIN_DIR wins over PATH and macOS fallback.
        assert_eq!(
            resolve_rex(Some(rex_dir.as_os_str()), Some(&path_env), &home_dir, true),
            ResolvedRex {
                path: rex_dir.join("rex"),
                prepend_dir: true,
            }
        );

        // 2. Without REX_BIN_DIR, PATH wins over macOS fallback.
        assert_eq!(
            resolve_rex(None, Some(&path_env), &home_dir, true),
            ResolvedRex {
                path: path_dir.join("rex"),
                prepend_dir: false,
            }
        );

        // 3. When PATH has no executable rex, macOS falls back to Versions/Current.
        let empty_dir = root.join("empty");
        std::fs::create_dir_all(&empty_dir).unwrap();
        let empty_path = std::env::join_paths([&empty_dir]).unwrap();
        assert_eq!(
            resolve_rex(None, Some(&empty_path), &home_dir, true),
            ResolvedRex {
                path: mac_dir.join("rex"),
                prepend_dir: true,
            }
        );

        // 4. On non-macOS, the macOS bundle path is not used; falls back to bare "rex".
        assert_eq!(
            resolve_rex(None, Some(&empty_path), &home_dir, false),
            ResolvedRex {
                path: PathBuf::from("rex"),
                prepend_dir: false,
            }
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolve_rex_skips_non_executable_and_broken_symlinks() {
        let root = temp_dir("broken");
        let stale_rex_dir = root.join("stale");
        let broken_path_dir = root.join("broken-path");
        let good_path_dir = root.join("good-path");
        let home_dir = root.join("home");

        make_file(&stale_rex_dir.join("rex"), false);
        std::fs::create_dir_all(&broken_path_dir).unwrap();
        symlink(root.join("does-not-exist/rex"), broken_path_dir.join("rex")).unwrap();
        make_file(&good_path_dir.join("rex"), true);

        let path_env = std::env::join_paths([&broken_path_dir, &good_path_dir]).unwrap();
        assert_eq!(
            resolve_rex(
                Some(stale_rex_dir.as_os_str()),
                Some(&path_env),
                &home_dir,
                false
            ),
            ResolvedRex {
                path: good_path_dir.join("rex"),
                prepend_dir: false,
            }
        );

        // With only a broken symlink in PATH on macOS, falls back to Versions/Current.
        let mac_rex = home_dir.join(MACOS_REX_BIN_DIR).join("rex");
        make_file(&mac_rex, true);
        let broken_only = std::env::join_paths([&broken_path_dir]).unwrap();
        assert_eq!(
            resolve_rex(None, Some(&broken_only), &home_dir, true),
            ResolvedRex {
                path: mac_rex,
                prepend_dir: true,
            }
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prepend_path_prepends_only_when_not_already_first() {
        let a = Path::new("/opt/rex/bin");
        let b = Path::new("/usr/bin");
        let existing = std::env::join_paths([b]).unwrap();
        assert_eq!(
            prepend_path(a, Some(&existing)),
            Some(std::env::join_paths([a, b]).unwrap())
        );

        let already = std::env::join_paths([a, b]).unwrap();
        assert_eq!(prepend_path(a, Some(&already)), None);

        assert_eq!(prepend_path(a, None), Some(a.as_os_str().to_os_string()));
    }
}
