//! The project catalog and dev hosts, from the runtime config (see
//! `config`), and choosing the least busy host for `auto`.

use std::process::Stdio;

use crate::config;
use crate::rex::{host_bash, local_label, rex_command, rex_json};

#[derive(Clone)]
pub struct Project {
    pub profile: String,
    pub path: String,
    pub clone_url: String,
    pub name: String,
    /// Pool override from projects.toml's `hosts`; empty uses the group's.
    pub hosts: Vec<String>,
    /// The catalog's `tracker` (see `links::Tracker`); may be empty.
    pub tracker: String,
    /// Whether tasks create a Git worktree (`true`) or run in-place in `path`.
    pub worktree: bool,
}

/// The catalog projects (from projects.toml), or why the config is unreadable.
///
/// # Errors
///
/// Fails when the config can't be loaded (see `config::load`).
pub fn catalog() -> Result<Vec<Project>, String> {
    Ok(config::load()?
        .projects
        .iter()
        .map(|p| {
            let name = p
                .path
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or(&p.path);
            Project {
                profile: p.profile.clone(),
                path: p.path.clone(),
                clone_url: p.clone_url.clone(),
                name: name.into(),
                hosts: p.hosts.clone(),
                tracker: p.tracker.clone(),
                worktree: p.worktree,
            }
        })
        .collect())
}

pub const AUTO: &str = "auto";

/// Pool entry for the config host (the Mac), resolved to its own label.
pub const CONFIG: &str = "config";

/// Linux dev hosts the picker offers (none when the config is unreadable).
#[must_use]
pub fn dev_hosts() -> Vec<String> {
    config::load().map(|c| c.hosts.clone()).unwrap_or_default()
}

/// Hosts `auto` chooses from for a project: its own `hosts`, else its
/// group's pool, else every dev host. "config" stays unresolved here; see
/// `resolve`.
#[must_use]
pub fn pool(project: &Project) -> Vec<String> {
    match config::load() {
        Ok(c) => pool_in(c, &project.hosts, &project.profile),
        Err(_) => project.hosts.clone(),
    }
}

/// `pool` for a project's `hosts` override and profile under `config`.
#[must_use]
pub fn pool_in(config: &config::Config, hosts: &[String], profile: &str) -> Vec<String> {
    if !hosts.is_empty() {
        return hosts.to_vec();
    }
    config
        .pools
        .get(profile)
        .filter(|hosts| !hosts.is_empty())
        .cloned()
        .unwrap_or_else(|| config.hosts.clone())
}

/// Hosts a project may be moved to by hand: its own pool, plus every host
/// its profile's pool or catalog projects use, so personal work offers the
/// personal hosts (and the Mac where personal projects pin "config") and
/// work stays on the work hosts. "config" stays unresolved, as in `pool`.
#[must_use]
pub fn compatible(project: &Project) -> Vec<String> {
    match config::load() {
        Ok(c) => compatible_in(c, &project.hosts, &project.profile),
        Err(_) => project.hosts.clone(),
    }
}

/// `compatible` for a project's `hosts` override and profile under `config`.
#[must_use]
pub fn compatible_in(config: &config::Config, hosts: &[String], profile: &str) -> Vec<String> {
    let mut out = pool_in(config, hosts, profile);
    let group = pool_in(config, &[], profile);
    let pinned = config
        .projects
        .iter()
        .filter(|p| p.profile == profile)
        .flat_map(|p| p.hosts.iter().cloned());
    for h in group.into_iter().chain(pinned) {
        if !out.contains(&h) {
            out.push(h);
        }
    }
    out
}

/// A pool entry as a Rex host label, resolving `"config"` to the configured
/// control-plane host (or this host when unset).
#[must_use]
pub fn resolve(host: &str) -> String {
    if host == CONFIG {
        config::control_host()
    } else {
        host.to_string()
    }
}

/// Resolved compatible hosts for `project` other than `exclude_host`,
/// deduplicated in preference order.
#[must_use]
pub fn other_compatible(project: &Project, exclude_host: &str) -> Vec<String> {
    let control = config::control_host();
    if let Ok(c) = config::load() {
        return other_compatible_in(c, &project.hosts, &project.profile, exclude_host, &control);
    }
    let mut out = Vec::new();
    for h in &project.hosts {
        let resolved = if h == CONFIG {
            control.clone()
        } else {
            h.clone()
        };
        if resolved != exclude_host && !out.contains(&resolved) {
            out.push(resolved);
        }
    }
    out
}

/// `other_compatible` under `config` with `local` as the config host's label.
#[must_use]
pub fn other_compatible_in(
    config: &config::Config,
    hosts: &[String],
    profile: &str,
    exclude_host: &str,
    local: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    for h in compatible_in(config, hosts, profile) {
        let resolved = if h == CONFIG { local.to_string() } else { h };
        if resolved != exclude_host && !out.contains(&resolved) {
            out.push(resolved);
        }
    }
    out
}

/// Fresh cached probe measurements from switchyard-server for `project`,
/// keyed by host label. Empty when the server isn't answering.
#[must_use]
pub fn cached_probe_summary(project: &str) -> std::collections::HashMap<String, Probe> {
    let mut out = std::collections::HashMap::new();
    for entry in crate::server::probes(project).unwrap_or_default() {
        if let Some(Cached::Measured(p)) = cached_probe(Some(&entry)) {
            out.insert(p.host.clone(), p);
        }
    }
    out
}

/// Agents busy on a host (panes reporting working or blocked) and whether it
/// already has a session for the project. Runs on that host's Rex server.
const PROBE_SCRIPT: &str = r#"
local T = "com.superlogical.terminal"
local agents, has = 0, 0
local list = rex.call("session.list")
for _, s in ipairs((list and list.sessions) or {}) do
  if s.label == rex.args.project then has = 1 end
  rex.session.attach{ session_id = s.session_id }
  local view = rex.session.view{ session_id = s.session_id }
  for _, w in ipairs((view and view.windows) or {}) do
    for _, l in ipairs(w.layers or {}) do
      for _, b in ipairs(l.blocks or {}) do
        local st = rex.block.call(T, "program_status", { session_id = s.session_id, block_id = b.block_id })
        for _, r in ipairs((st and st.records) or {}) do
          if r.state == "working" or r.state == "blocked" then agents = agents + 1; break end
        end
      end
    end
  end
end
return agents .. " " .. has
"#;

/// A dev host's load per CPU and free memory, from one SSH call.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub load: f64,
    pub mem_gib: f64,
}

/// What `metrics` runs on a dev host (Linux).
const METRICS_SCRIPT: &str = "cat /proc/loadavg; nproc; grep MemAvailable /proc/meminfo";

/// `METRICS_SCRIPT`'s output as metrics; None when it isn't all there.
fn parse_metrics(text: &str) -> Option<Metrics> {
    let mut lines = text.lines();
    let load1: f64 = lines.next()?.split_whitespace().next()?.parse().ok()?;
    let cpus: f64 = lines.next()?.trim().parse().ok()?;
    let mem_kib: f64 = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
    Some(Metrics {
        load: load1 / cpus.max(1.0),
        mem_gib: mem_kib / 1024.0 / 1024.0,
    })
}

/// Load per CPU and free memory of `host` (locally on a Linux control plane,
/// else over SSH by Rex label); Err when it can't be reached or read (macOS
/// hosts have no `/proc`). switchyard-server keeps these for every probed host.
///
/// # Errors
///
/// Fails when `host` can't be reached or its output has no load or memory
/// figures.
pub fn metrics(host: &str) -> Result<Metrics, String> {
    let out = host_bash(host, METRICS_SCRIPT).ok_or_else(|| format!("couldn't reach {host}"))?;
    parse_metrics(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| format!("no load or memory from {host}"))
}

#[derive(Debug, PartialEq)]
pub struct Probe {
    pub host: String,
    pub score: f64,
    pub load: f64,
    pub mem_gib: f64,
    pub agents: u32,
    pub warm: bool,
    /// From switchyard-server's cache rather than measured now.
    pub cached: bool,
}

impl Probe {
    /// Lower is better. Each busy agent counts like half the CPUs being
    /// busy; a host already holding the project is slightly preferred (warm
    /// checkout and build cache); low memory pushes away.
    #[must_use]
    pub fn new(host: &str, m: Metrics, agents: u32, warm: bool) -> Probe {
        let score = m.load + 0.5 * f64::from(agents) - if warm { 0.25 } else { 0.0 }
            + if m.mem_gib < 4.0 { 1.0 } else { 0.0 };
        Probe {
            host: host.to_string(),
            score,
            load: m.load,
            mem_gib: m.mem_gib,
            agents,
            warm,
            cached: false,
        }
    }
}

/// Measure one dev host now: load and memory via `metrics`, busy agents and an
/// existing project session over Rex. None when the host can't be reached.
fn probe(host: &str, project: &str) -> Option<Probe> {
    let m = metrics(host).ok()?;
    let mut cmd = rex_command();
    if host != local_label() {
        cmd.args(["-S", host]);
    }
    let rex = cmd
        .args(["do", "-e", PROBE_SCRIPT, &format!("project={project}")])
        .stdin(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let reply: String = serde_json::from_slice(&rex.stdout).ok()?;
    let mut f = reply.split_whitespace();
    let agents: u32 = f.next()?.parse().ok()?;
    let warm = f.next() == Some("1");
    Some(Probe::new(host, m, agents, warm))
}

/// How old a cached probe may be for `choose_host` to trust it; older ones
/// are measured again.
const PROBE_FRESH_MS: u64 = 60_000;

/// Fresh word about a host from switchyard-server's cache.
#[derive(Debug, PartialEq)]
enum Cached {
    /// A fresh measurement.
    Measured(Probe),
    /// Fresh word that it can't be reached.
    Unreachable,
}

/// What switchyard-server's cached probe (one entry of its `probes` reply,
/// see core's `server`) says about a host, or None for nothing to trust
/// (missing, stale, or no rows for its agents), so measure it now.
fn cached_probe(entry: Option<&serde_json::Value>) -> Option<Cached> {
    let e = entry?;
    if e["age_ms"].as_u64()? > PROBE_FRESH_MS {
        return None;
    }
    if !e["error"].is_null() {
        return Some(Cached::Unreachable);
    }
    let m = Metrics {
        load: e["load"].as_f64()?,
        mem_gib: e["mem_gib"].as_f64()?,
    };
    let agents = u32::try_from(e["agents"].as_u64()?).ok()?;
    let mut p = Probe::new(e["host"].as_str()?, m, agents, e["warm"].as_bool()?);
    p.cached = true;
    Some(Cached::Measured(p))
}

/// Probes for `hosts`: switchyard-server's where fresh (`use_cache`), the
/// rest measured now, in parallel. Unreachable hosts are left out.
fn probe_all(hosts: &[String], project: &str, use_cache: bool) -> Vec<Probe> {
    let cached = if use_cache {
        crate::server::probes(project).unwrap_or_default()
    } else {
        Vec::new()
    };
    let entry = |host: &str| cached.iter().find(|e| e["host"].as_str() == Some(host));
    let mut probes = Vec::new();
    let mut handles = Vec::new();
    for host in hosts {
        if let Some(cached) = cached_probe(entry(host)) {
            if let Cached::Measured(p) = cached {
                probes.push(p);
            }
        } else {
            let (host, project) = (host.clone(), project.to_string());
            handles.push(std::thread::spawn(move || probe(&host, &project)));
        }
    }
    probes.extend(handles.into_iter().filter_map(|h| h.join().ok().flatten()));
    probes
}

/// The project's host, or the least busy reachable one when its pool has
/// several (probing is Linux only), with a one-line reason for the log.
///
/// # Errors
///
/// Fails when no host in the project's pool is reachable.
pub fn choose_host(project: &Project) -> Result<(String, String), String> {
    choose(project, true)
}

/// `choose_host`, with or without switchyard-server's cached probes.
///
/// # Errors
///
/// Fails when no host in the project's pool is reachable.
pub fn choose(project: &Project, use_cache: bool) -> Result<(String, String), String> {
    let hosts = pool(project);
    if let [only] = hosts.as_slice() {
        return Ok((resolve(only), format!("the only host for {}", project.name)));
    }
    let hosts: Vec<String> = hosts.iter().map(|h| resolve(h)).collect();
    let mut probes = probe_all(&hosts, &project.name, use_cache);
    probes.sort_by(|a, b| a.score.total_cmp(&b.score));
    let best = probes
        .first()
        .ok_or_else(|| format!("no host for {} is reachable", project.name))?;
    let why = probes
        .iter()
        .map(|p| {
            format!(
                "{} {:.2} (load {:.2}/cpu, {:.0} GiB free, {} busy agent{}{}{})",
                p.host,
                p.score,
                p.load,
                p.mem_gib,
                p.agents,
                if p.agents == 1 { "" } else { "s" },
                if p.warm { ", has session" } else { "" },
                if p.cached { ", cached" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    Ok((best.host.clone(), why))
}

/// Choose the least busy compatible host for `project` other than
/// `exclude_host`, with a one-line reason for the log.
///
/// # Errors
///
/// Fails when `project` has no other compatible host or none is reachable.
pub fn choose_other_host(
    project: &Project,
    exclude_host: &str,
) -> Result<(String, String), String> {
    let candidates = other_compatible(project, exclude_host);
    if candidates.is_empty() {
        return Err(format!("no other compatible host for {}", project.name));
    }
    if let [only] = candidates.as_slice() {
        return Ok((
            only.clone(),
            format!("the only other compatible host for {}", project.name),
        ));
    }
    let mut probes = probe_all(&candidates, &project.name, true);
    probes.sort_by(|a, b| a.score.total_cmp(&b.score));
    if let Some(best) = probes.first() {
        let why = probes
            .iter()
            .map(|p| {
                format!(
                    "{} {:.2} (load {:.2}/cpu, {:.0} GiB free, {} busy agent{}{}{})",
                    p.host,
                    p.score,
                    p.load,
                    p.mem_gib,
                    p.agents,
                    if p.agents == 1 { "" } else { "s" },
                    if p.warm { ", has session" } else { "" },
                    if p.cached { ", cached" } else { "" }
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Ok((best.host.clone(), why));
    }
    // Non-Linux hosts (e.g. macOS) have no /proc metrics, so fall back to the
    // first candidate whose Rex server answers.
    for host in candidates {
        if (cfg!(target_os = "macos") && host == local_label())
            || rex_json(&host, &["api", "call", "server.whoami"]).is_ok()
        {
            return Ok((
                host,
                format!("the only reachable compatible host for {}", project.name),
            ));
        }
    }
    Err(format!(
        "no other compatible host for {} is reachable",
        project.name
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn metrics_are_per_cpu_load_and_free_gib() {
        let m =
            parse_metrics("3.20 2.10 1.00 2/900 12345\n16\nMemAvailable:   33554432 kB\n").unwrap();
        assert_eq!(
            m,
            Metrics {
                load: 0.2,
                mem_gib: 32.0
            }
        );
        assert_eq!(parse_metrics("3.20 2.10 1.00 2/900 12345\n"), None);
    }

    #[test]
    fn scores_prefer_idle_warm_hosts_with_memory() {
        let m = Metrics {
            load: 0.5,
            mem_gib: 32.0,
        };
        let close = |score: f64, want: f64| (score - want).abs() < 1e-9;
        assert!(close(Probe::new("a", m, 2, false).score, 1.5));
        assert!(close(Probe::new("a", m, 0, true).score, 0.25));
        assert!(close(
            Probe::new("a", Metrics { mem_gib: 2.0, ..m }, 0, false).score,
            1.5
        ));
    }

    #[test]
    fn compatible_hosts_stay_within_the_profile() {
        let config: config::Config = toml::from_str(
            r#"
            hosts = ["miller", "drummer", "omarchy"]
            [pools]
            personal = ["omarchy"]
            work = ["miller", "drummer"]
            [[projects]]
            profile = "personal"
            hosts = ["config"]
            [[projects]]
            profile = "personal"
            [[projects]]
            profile = "work"
            "#,
        )
        .unwrap();
        let s = |v: &[&str]| v.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(
            compatible_in(&config, &[], "personal"),
            s(&["omarchy", "config"])
        );
        assert_eq!(
            compatible_in(&config, &s(&["config"]), "personal"),
            s(&["config", "omarchy"])
        );
        assert_eq!(
            compatible_in(&config, &[], "work"),
            s(&["miller", "drummer"])
        );
        assert_eq!(
            other_compatible_in(&config, &s(&["config"]), "personal", "mac", "mac"),
            s(&["omarchy"])
        );
        assert_eq!(
            other_compatible_in(&config, &[], "personal", "omarchy", "mac"),
            s(&["mac"])
        );
        assert_eq!(
            other_compatible_in(&config, &[], "work", "miller", "mac"),
            s(&["drummer"])
        );
    }

    #[test]
    fn cached_probes_count_only_when_fresh_and_whole() {
        let entry = json!({ "host": "miller", "load": 0.5, "mem_gib": 32.0, "agents": 1, "warm": true,
            "age_ms": 1000, "error": null });
        let Some(Cached::Measured(p)) = cached_probe(Some(&entry)) else {
            panic!("a fresh measurement");
        };
        assert_eq!((p.host.as_str(), p.score, p.cached), ("miller", 0.75, true));
        // Fresh word that the host is down: leave it out without asking again.
        let down = json!({ "host": "miller", "age_ms": 1000, "error": "couldn't reach miller" });
        assert_eq!(cached_probe(Some(&down)), Some(Cached::Unreachable));
        // Stale, unmeasured or without rows for its agents: measure it now.
        let mut stale = entry.clone();
        stale["age_ms"] = json!(PROBE_FRESH_MS + 1);
        assert_eq!(cached_probe(Some(&stale)), None);
        let mut new = entry.clone();
        new["age_ms"] = json!(null);
        assert_eq!(cached_probe(Some(&new)), None);
        let mut no_rows = entry.clone();
        no_rows["agents"] = json!(null);
        assert_eq!(cached_probe(Some(&no_rows)), None);
        assert_eq!(cached_probe(None), None);
    }

    /// `auto`'s choice for each catalog project with a pool to choose from,
    /// from switchyard-server's cached probes and measured live, without
    /// launching anything.
    #[test]
    #[ignore = "probes the real dev hosts; run with --ignored on the config host"]
    fn cached_and_live_choices() {
        for project in catalog().unwrap().iter().filter(|p| pool(p).len() > 1) {
            for use_cache in [true, false] {
                let started = std::time::Instant::now();
                let choice = choose(project, use_cache);
                let ms = started.elapsed().as_millis();
                eprintln!(
                    "{} {} {ms} ms: {choice:?}",
                    project.name,
                    if use_cache { "cached" } else { "live" }
                );
            }
        }
    }
}
