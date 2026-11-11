//! Calls to Rex servers and hosts: the rex CLI (no `-S` for the local
//! server), Bash over SSH by Rex label, and the state log.
//!
//! SSH calls share one connection per host (see `ssh`): connecting costs
//! about 100 ms to a dev host, more than most of the scripts sent over it.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// This host's Rex label: its short hostname.
pub fn local_label() -> String {
    Command::new("hostname")
        .arg("-s")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Run the rex CLI against `host` (no -S for the local server) and parse
/// its JSON output.
pub fn rex_json(host: &str, args: &[&str]) -> Result<serde_json::Value, String> {
    let mut cmd = Command::new("rex");
    if host != local_label() {
        cmd.args(["-S", host]);
    }
    let out = cmd.args(args).stdin(Stdio::null()).output().map_err(|e| format!("rex: {e}"))?;
    if !out.status.success() {
        return Err(format!("rex {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("rex {}: {e}", args.join(" ")))
}

/// Run a Bash script on `host` from stdin: over SSH by Rex label (as `probe`
/// reaches hosts; their login shell is Fish), or locally for the config
/// host. None when it can't be started or the host can't be reached;
/// otherwise the output whatever the script's exit status.
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
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
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
/// (ControlMaster), so only the first call in a while pays for connecting.
/// The control sockets live in `runtime_dir`, whose short path keeps them
/// under macOS's 104-byte socket path limit; without it, each call connects
/// on its own. Like any ssh, it exits 255 when it can't connect.
pub fn ssh(host: &str) -> Command {
    let mut cmd = Command::new("ssh");
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
pub fn runtime_dir() -> Option<PathBuf> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
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
    let state = std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join(".local/state"));
    let dir = state.join("rex");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("spawn.log")) {
        let _ = writeln!(f, "switchyard: {line}");
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Run the rex CLI against `host` and return an error with its stderr.
pub fn rex(host: &str, args: &[&str]) -> Result<(), String> {
    let mut cmd = Command::new("rex");
    if host != local_label() {
        cmd.args(["-S", host]);
    }
    let out = cmd.args(args).stdin(Stdio::null()).output().map_err(|e| format!("rex: {e}"))?;
    if !out.status.success() {
        return Err(format!("rex {}: {}", args[..2.min(args.len())].join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

/// The home directory of `host`'s account: this host's own, or a dev host's
/// /home/ACCOUNT (the dev hosts are Linux and their Rex server's account is
/// the one that works there). Empty when the account can't be read.
pub fn host_home(host: &str) -> Result<String, String> {
    if host == local_label() {
        return Ok(home().display().to_string());
    }
    let account = rex_json(host, &["api", "call", "server.whoami"])?["account"].as_str().unwrap_or_default().to_string();
    Ok(if account.is_empty() { String::new() } else { format!("/home/{account}") })
}

/// Show a tab in its host's Rex app, as the config's jumps do; with no
/// window, the session's own.
pub fn select(host: &str, session: &str, window: &str) -> Result<(), String> {
    let mut cmd = Command::new("rex");
    if host != local_label() {
        cmd.args(["-S", host]);
    }
    let mut args = vec!["do".to_string(), "session.select".to_string(), format!("session_id={session}")];
    if !window.is_empty() {
        args.push(format!("window_id={window}"));
    }
    let out = cmd
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("rex: {e}"))?;
    if !out.status.success() {
        return Err(format!("can't show the tab on {host}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}
