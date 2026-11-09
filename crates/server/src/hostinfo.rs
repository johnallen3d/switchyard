//! What the spawn picker and `auto` ask the hosts for, kept in the
//! background: each host's zoxide folders (this machine's and the dev
//! hosts', the picker's host chips) and each dev host's load and memory
//! (the hosts an `auto` launch can choose from). Each is one SSH call over
//! the shared connection (see core's `rex::ssh`), so a picker or launch
//! never waits on one. Busy agents and the project's session come from the
//! watchers' rows instead (see `state::State::probes_reply`).
//!
//! Load is measured every PROBE_EVERY, which also keeps the SSH connection
//! to those hosts open; folders every FOLDERS_EVERY, or sooner once a
//! client is served a listing older than FOLDERS_STALE. A host whose last
//! call is still running is skipped, so an unreachable or hung host holds
//! up nothing else. The host lists come from the config, read again each
//! round, so an activation that changes them needs no restart.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use switchyard_core::config::{self, Config};
use switchyard_core::folders::{zoxide_here, zoxide_on};
use switchyard_core::hosts::{CONFIG, metrics, pool_in};
use switchyard_core::rex::local_label;

use crate::state::Shared;

pub const PROBE_EVERY: Duration = Duration::from_secs(30);
pub const FOLDERS_EVERY: Duration = Duration::from_secs(120);
/// A client served folders this old asks for a new listing.
pub const FOLDERS_STALE: Duration = Duration::from_secs(60);
/// How often to look for due work, between wakes from clients.
const TICK: Duration = Duration::from_secs(5);

/// The hosts to keep folders for (this machine first, then the dev hosts)
/// and to measure (every host in a pool `auto` chooses from, that is one
/// with more than one host, except this machine, which has no /proc).
pub fn tracked(config: &Config, local: &str) -> (Vec<String>, Vec<String>) {
    let mut folders = vec![local.to_string()];
    for h in &config.hosts {
        if !folders.contains(h) {
            folders.push(h.clone());
        }
    }
    let mut probes: Vec<String> = Vec::new();
    for p in &config.projects {
        let pool = pool_in(config, &p.hosts, &p.profile);
        if pool.len() < 2 {
            continue;
        }
        for h in pool {
            let h = if h == CONFIG { local.to_string() } else { h };
            if h != local && !probes.contains(&h) {
                probes.push(h);
            }
        }
    }
    (folders, probes)
}

/// Keep the listings current until the server stops. A wake on `wake`
/// (a client found folders stale) runs the due work at once.
pub fn run(shared: Arc<Shared>, wake: Receiver<()>) {
    let local = local_label();
    loop {
        match config::read() {
            Ok(c) => {
                let (folders, probes) = tracked(&c, &local);
                shared.lock().info.track(&local, &folders, &probes);
            }
            Err(e) => crate::log(&format!("hosts: {e}")),
        }
        start_due(&shared, &local);
        match wake.recv_timeout(TICK) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        while wake.try_recv().is_ok() {}
    }
}

/// Start a thread for each listing that is due, marking it running.
fn start_due(shared: &Arc<Shared>, local: &str) {
    let now = Instant::now();
    let (folders, probes) = {
        let mut state = shared.lock();
        let mut folders = Vec::new();
        for (host, l) in state.info.folders.iter_mut().filter(|(_, l)| l.due(now, FOLDERS_EVERY)) {
            l.running = true;
            l.wanted = false;
            folders.push(host.clone());
        }
        let mut probes = Vec::new();
        for (host, l) in state.info.metrics.iter_mut().filter(|(_, l)| l.due(now, PROBE_EVERY)) {
            l.running = true;
            l.wanted = false;
            probes.push(host.clone());
        }
        (folders, probes)
    };
    for host in folders {
        let (shared, here) = (shared.clone(), host == local);
        thread::spawn(move || {
            let got = if here { zoxide_here() } else { zoxide_on(&host) };
            if let Err(e) = &got {
                crate::log(&format!("folders on {host}: {e}"));
            }
            if let Some(l) = shared.lock().info.folders.get_mut(&host) {
                l.set(got, Instant::now());
            }
        });
    }
    for host in probes {
        let shared = shared.clone();
        thread::spawn(move || {
            let got = metrics(&host);
            if let Some(l) = shared.lock().info.metrics.get_mut(&host) {
                l.set(got, Instant::now());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use switchyard_core::config::ProjectEntry;

    #[test]
    fn tracks_the_picker_hosts_and_the_pools_auto_chooses_from() {
        let project = |profile: &str, hosts: &[&str]| ProjectEntry {
            profile: profile.into(),
            hosts: hosts.iter().map(|h| h.to_string()).collect(),
            ..ProjectEntry::default()
        };
        let mut config = Config {
            hosts: vec!["miller".into(), "drummer".into(), "omarchy".into()],
            projects: vec![project("personal", &["config"]), project("personal", &[]), project("work", &[])],
            ..Config::default()
        };
        config.pools.insert("personal".into(), vec!["omarchy".into()]);
        config.pools.insert("work".into(), vec!["miller".into(), "drummer".into()]);
        let (folders, probes) = tracked(&config, "mac");
        assert_eq!(folders, ["mac", "miller", "drummer", "omarchy"]);
        // Single-host pools never get probed.
        assert_eq!(probes, ["miller", "drummer"]);
        // A pool with the config host probes the others; no pool for the
        // profile means every dev host.
        config.projects.push(project("other", &[]));
        config.projects.push(project("work", &["config", "nas"]));
        let (_, probes) = tracked(&config, "mac");
        assert_eq!(probes, ["miller", "drummer", "omarchy", "nas"]);
    }
}
