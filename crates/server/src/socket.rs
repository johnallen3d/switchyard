//! The Unix socket clients ask (see core's `server` for the protocol and
//! path). Each connection gets a thread: every reply is built from memory,
//! so a request never waits on a host, and a subscriber's thread only waits
//! for the next change. `refresh` and a stale `folders` only prompt the
//! background work.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Instant;

use serde_json::json;

use crate::hostinfo::FOLDERS_STALE;
use crate::state::Shared;

/// Where requests prompt background work.
#[derive(Clone)]
pub struct Asks {
    /// The task check (see `tasks::run`).
    pub refresh: Sender<&'static str>,
    /// The host listings (see `hostinfo::run`).
    pub folders: Sender<()>,
}

/// Listen on `path`, replacing a socket left by a server that's gone, but
/// not one that still answers.
pub fn listen(path: &Path) -> Result<UnixListener, String> {
    if UnixStream::connect(path).is_ok() {
        return Err(format!("another switchyard-server is listening on {}", path.display()));
    }
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(format!("{}: {e}", path.display())),
        _ => {}
    }
    let listener = UnixListener::bind(path).map_err(|e| format!("{}: {e}", path.display()))?;
    // The runtime directory is private already; this is belt and braces.
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    Ok(listener)
}

pub fn serve(listener: UnixListener, shared: Arc<Shared>, asks: Asks) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let (shared, asks) = (shared.clone(), asks.clone());
                thread::spawn(move || {
                    let _ = client(stream, &shared, &asks);
                });
            }
            Err(e) => crate::log(&format!("socket: {e}")),
        }
    }
}

/// Answer one client's requests until it hangs up, or stream to it after
/// `subscribe`.
fn client(stream: UnixStream, shared: &Shared, asks: &Asks) -> io::Result<()> {
    let mut out = stream.try_clone()?;
    for line in BufReader::new(stream).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let req = serde_json::from_str::<serde_json::Value>(&line).unwrap_or_default();
        let arg = |k: &str| req[k].as_str().unwrap_or("").to_string();
        match req["cmd"].as_str() {
            Some("folders") => {
                let host = arg("host");
                let (reply, stale) = {
                    let mut state = shared.lock();
                    let (reply, stale) = state.folders_reply(&host, Instant::now(), FOLDERS_STALE);
                    if stale && let Some(l) = state.info.folders.get_mut(&host) {
                        l.wanted = true;
                    }
                    (reply, stale)
                };
                if stale {
                    let _ = asks.folders.send(());
                }
                writeln!(out, "{reply}")?;
            }
            Some("probes") => {
                let reply = shared.lock().probes_reply(&arg("project"), Instant::now());
                writeln!(out, "{reply}")?;
            }
            Some("refresh") => {
                let _ = asks.refresh.send("refresh");
                writeln!(out, "{}", json!({ "ok": true }))?;
            }
            Some("snapshot") => {
                let snap = shared.lock().snapshot(Instant::now());
                writeln!(out, "{snap}")?;
            }
            Some("subscribe") => {
                let (tx, rx) = mpsc::channel();
                let snap = {
                    let mut state = shared.lock();
                    state.subscribe(tx);
                    state.snapshot(Instant::now())
                };
                writeln!(out, "{snap}")?;
                // Ends when the client hangs up: the next write fails, and
                // the state drops this sender at the next change after.
                for update in rx {
                    writeln!(out, "{update}")?;
                }
                return Ok(());
            }
            _ => writeln!(out, "{}", json!({ "error": format!("unknown request: {line}") }))?,
        }
    }
    Ok(())
}
