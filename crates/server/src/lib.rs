//! switchyard-server: the pickers' data, kept current on the config host.
//!
//! Without a server, every picker open fans out a fresh rex process to each
//! Rex server and SSHes into each task host, and opening a connection costs
//! more than the calls made over it. The server instead holds one
//! connection per Rex server and keeps what the pickers show in memory:
//!
//! - Rows: a watcher per Rex server (this machine's as `local`, then each
//!   of `rex.servers()`) runs host_rows.lua whenever its events say the
//!   rows changed, and on a slow timer (see `watch`).
//! - Tasks: the registry, with live agent status from the rows, checked
//!   against the hosts in the background (see `tasks`).
//! - Host info for the spawn picker and `auto`: zoxide folders and load,
//!   kept over SSH in the background (see `hostinfo`).
//!
//! Clients ask over a Unix socket (see `socket`, and core's `server` for
//! the path, protocol and client side) and fall back to the direct path
//! when no server answers.
//!
//! Modes: bare (or `serve`) runs the server in the foreground; `snapshot`
//! prints the running server's snapshot as JSON; `subscribe` prints one
//! line per change until interrupted.

mod hostinfo;
mod socket;
mod state;
mod tasks;
mod watch;

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use switchyard_core::server;

use crate::state::Shared;
use crate::watch::Events;

const USAGE: &str = "usage: switchyard server [serve]
       switchyard server snapshot
       switchyard server subscribe

serve (the default) runs the server in the foreground: it watches every Rex
server's tabs and the task registry and answers on a Unix socket. snapshot
prints the running server's rows, tasks and errors as JSON; subscribe
prints a snapshot per change. Both fail when the server isn't running, so
the caller can fetch directly.";

pub fn log(line: &str) {
    eprintln!("switchyard-server: {line}");
}

pub fn main(args: Vec<String>) {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match a.as_slice() {
        [] | ["serve"] => serve(),
        ["snapshot"] => server::snapshot().map(|snap| println!("{snap}")),
        ["subscribe"] => subscribe(),
        ["-h"] | ["--help"] => {
            println!("{USAGE}");
            Ok(())
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        log(&e);
        std::process::exit(1);
    }
}

fn subscribe() -> Result<(), String> {
    for snap in server::subscribe()? {
        println!("{}", snap?);
    }
    Ok(())
}

/// Stop the watchers with the server, rather than leaving them to notice
/// at their next write.
extern "C" fn stop(sig: libc::c_int) {
    watch::stop_all();
    // SAFETY: signal and raise are async-signal-safe; with the default
    // action back, the signal ends this process.
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

fn serve() -> Result<(), String> {
    // A server started from a shell in Rex would otherwise aim its rex calls
    // at that shell's session. SAFETY: no thread has started yet.
    for var in ["REX_SERVER", "REX_CLIENT", "REX_SESSION", "REX_BLOCK"] {
        unsafe { std::env::remove_var(var) };
    }
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: installs a handler that only makes async-signal-safe calls.
        unsafe { libc::signal(sig, stop as *const () as libc::sighandler_t) };
    }
    let path = server::socket_path().ok_or("no private runtime directory for the socket")?;
    let listener = socket::listen(&path)?;
    log(&format!("listening on {}", path.display()));

    let shared = Arc::new(Shared::new());
    let (closed, triggers) = mpsc::channel();
    let (hosts, hosts_changed) = mpsc::channel();
    let (wake, wakes) = mpsc::channel();
    let events = Events { closed: closed.clone(), hosts };
    let asks = socket::Asks { refresh: closed.clone(), folders: wake };
    {
        let shared = shared.clone();
        thread::spawn(move || tasks::run(shared, triggers));
    }
    thread::spawn(move || tasks::watch_registry(closed));
    {
        let shared = shared.clone();
        thread::spawn(move || manage_hosts(shared, events, hosts_changed));
    }
    {
        let shared = shared.clone();
        thread::spawn(move || hostinfo::run(shared, wakes));
    }
    socket::serve(listener, shared, asks);
    Ok(())
}

/// The Rex servers this machine knows besides its own.
fn list_servers() -> Result<Vec<String>, String> {
    let out = Command::new("rex")
        .args(["do", "-e", "return rex.servers()"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("rex: {e}"))?;
    if !out.status.success() {
        return Err(format!("rex.servers(): {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("rex.servers(): {e}"))
}

/// Run a watcher per Rex server, starting and stopping them as the host
/// list changes: when a watcher hears it change, and every few minutes.
fn manage_hosts(shared: Arc<Shared>, events: Events, changed: Receiver<()>) {
    let mut watchers: HashMap<String, Arc<AtomicBool>> = HashMap::new();
    loop {
        let listed = list_servers();
        if let Err(e) = &listed {
            log(e);
        }
        // Without the list, keep the watchers there are (at least local's).
        let names: Vec<String> = match &listed {
            Ok(servers) => std::iter::once("local".to_string()).chain(servers.iter().cloned()).collect(),
            Err(_) if watchers.is_empty() => vec!["local".to_string()],
            Err(_) => watchers.keys().cloned().collect(),
        };
        if listed.is_ok() || watchers.is_empty() {
            shared.update(|s| {
                s.set_hosts(&names);
                true
            });
        }
        watchers.retain(|name, stop| {
            let keep = names.contains(name);
            if !keep {
                stop.store(true, Ordering::Relaxed);
            }
            keep
        });
        for name in &names {
            if !watchers.contains_key(name) {
                let stop = Arc::new(AtomicBool::new(false));
                watchers.insert(name.clone(), stop.clone());
                let (shared, name, events) = (shared.clone(), name.clone(), events.clone());
                thread::spawn(move || watch::supervise(shared, name, events, stop));
            }
        }
        let wait = if listed.is_ok() { Duration::from_secs(600) } else { Duration::from_secs(30) };
        match changed.recv_timeout(wait) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        while changed.try_recv().is_ok() {}
    }
}
