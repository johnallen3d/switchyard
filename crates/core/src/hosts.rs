//! The project catalog and dev hosts, from the runtime config (see
//! `config`), and choosing the least busy host for `auto`.

use std::process::{Command, Stdio};

use crate::config;
use crate::rex::{local_label, ssh};

#[derive(Clone)]
pub struct Project {
    pub profile: String,
    pub path: String,
    pub clone_url: String,
    pub name: String,
    /// Pool override from projects.toml's `hosts`; empty uses the group's.
    pub hosts: Vec<String>,
}

/// The catalog projects (from projects.toml), or why the config is unreadable.
pub fn catalog() -> Result<Vec<Project>, String> {
    Ok(config::load()?
        .projects
        .iter()
        .map(|p| {
            let name = p.path.trim_end_matches('/').rsplit('/').next().unwrap_or(&p.path);
            Project {
                profile: p.profile.clone(),
                path: p.path.clone(),
                clone_url: p.clone_url.clone(),
                name: name.into(),
                hosts: p.hosts.clone(),
            }
        })
        .collect())
}

pub const AUTO: &str = "auto";

/// Pool entry for the config host (the Mac), resolved to its own label.
pub const CONFIG: &str = "config";

/// Linux dev hosts the picker offers (none when the config is unreadable).
pub fn dev_hosts() -> Vec<String> {
    config::load().map(|c| c.hosts.clone()).unwrap_or_default()
}

/// Hosts `auto` chooses from for a project: its own `hosts`, else its
/// group's pool, else every dev host. "config" stays unresolved here; see
/// `resolve`.
pub fn pool(project: &Project) -> Vec<String> {
    match config::load() {
        Ok(c) => pool_in(c, &project.hosts, &project.profile),
        Err(_) => project.hosts.clone(),
    }
}

/// `pool` for a project's `hosts` override and profile under `config`.
pub fn pool_in(config: &config::Config, hosts: &[String], profile: &str) -> Vec<String> {
    if !hosts.is_empty() {
        return hosts.to_vec();
    }
    config.pools.get(profile).filter(|hosts| !hosts.is_empty()).cloned().unwrap_or_else(|| config.hosts.clone())
}

/// A pool entry as a Rex host label; only called on the config host.
pub fn resolve(host: &str) -> String {
    if host == CONFIG { local_label() } else { host.to_string() }
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
    Some(Metrics { load: load1 / cpus.max(1.0), mem_gib: mem_kib / 1024.0 / 1024.0 })
}

/// Load per CPU and free memory of `host` over SSH (by its Rex label, as
/// the project picker does); Err when it can't be reached or read (the
/// config host, a Mac, has no /proc). switchyard-server keeps these for
/// every dev host.
pub fn metrics(host: &str) -> Result<Metrics, String> {
    let out = ssh(host)
        .arg(METRICS_SCRIPT)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("ssh: {e}"))?;
    if out.status.code() == Some(255) {
        return Err(format!("couldn't reach {host}"));
    }
    parse_metrics(&String::from_utf8_lossy(&out.stdout)).ok_or_else(|| format!("no load or memory from {host}"))
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
    pub fn new(host: &str, m: Metrics, agents: u32, warm: bool) -> Probe {
        let score = m.load + 0.5 * agents as f64 - if warm { 0.25 } else { 0.0 }
            + if m.mem_gib < 4.0 { 1.0 } else { 0.0 };
        Probe { host: host.to_string(), score, load: m.load, mem_gib: m.mem_gib, agents, warm, cached: false }
    }
}

/// Measure one dev host now: load and memory over SSH, busy agents and an
/// existing project session over Rex. None when the host can't be reached.
fn probe(host: &str, project: &str) -> Option<Probe> {
    let m = metrics(host).ok()?;
    let rex = Command::new("rex")
        .args(["-S", host, "do", "-e", PROBE_SCRIPT, &format!("project={project}")])
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

/// What switchyard-server's cached probe (one entry of its `probes` reply,
/// see core's `server`) says about a host: Some(Some) a fresh measurement,
/// Some(None) fresh word that it can't be reached, None nothing to trust
/// (missing, stale, or no rows for its agents), so measure it now.
fn cached_probe(entry: Option<&serde_json::Value>) -> Option<Option<Probe>> {
    let e = entry?;
    if e["age_ms"].as_u64()? > PROBE_FRESH_MS {
        return None;
    }
    if !e["error"].is_null() {
        return Some(None);
    }
    let m = Metrics { load: e["load"].as_f64()?, mem_gib: e["mem_gib"].as_f64()? };
    let agents = u32::try_from(e["agents"].as_u64()?).ok()?;
    let mut p = Probe::new(e["host"].as_str()?, m, agents, e["warm"].as_bool()?);
    p.cached = true;
    Some(Some(p))
}

/// Probes for `hosts`: switchyard-server's where fresh (`use_cache`), the
/// rest measured now, in parallel. Unreachable hosts are left out.
fn probe_all(hosts: &[String], project: &str, use_cache: bool) -> Vec<Probe> {
    let cached = if use_cache { crate::server::probes(project).unwrap_or_default() } else { Vec::new() };
    let entry = |host: &str| cached.iter().find(|e| e["host"].as_str() == Some(host));
    let mut probes = Vec::new();
    let mut handles = Vec::new();
    for host in hosts {
        match cached_probe(entry(host)) {
            Some(p) => probes.extend(p),
            None => {
                let (host, project) = (host.clone(), project.to_string());
                handles.push(std::thread::spawn(move || probe(&host, &project)));
            }
        }
    }
    probes.extend(handles.into_iter().filter_map(|h| h.join().ok().flatten()));
    probes
}

/// The project's host, or the least busy reachable one when its pool has
/// several (probing is Linux only), with a one-line reason for the log.
pub fn choose_host(project: &Project) -> Result<(String, String), String> {
    choose(project, true)
}

/// `choose_host`, with or without switchyard-server's cached probes.
pub fn choose(project: &Project, use_cache: bool) -> Result<(String, String), String> {
    let hosts = pool(project);
    if let [only] = hosts.as_slice() {
        return Ok((resolve(only), format!("the only host for {}", project.name)));
    }
    let hosts: Vec<String> = hosts.iter().map(|h| resolve(h)).collect();
    let mut probes = probe_all(&hosts, &project.name, use_cache);
    probes.sort_by(|a, b| a.score.total_cmp(&b.score));
    let best = probes.first().ok_or_else(|| format!("no host for {} is reachable", project.name))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn metrics_are_per_cpu_load_and_free_gib() {
        let m = parse_metrics("3.20 2.10 1.00 2/900 12345\n16\nMemAvailable:   33554432 kB\n").unwrap();
        assert_eq!(m, Metrics { load: 0.2, mem_gib: 32.0 });
        assert_eq!(parse_metrics("3.20 2.10 1.00 2/900 12345\n"), None);
    }

    #[test]
    fn scores_prefer_idle_warm_hosts_with_memory() {
        let m = Metrics { load: 0.5, mem_gib: 32.0 };
        assert_eq!(Probe::new("a", m, 2, false).score, 1.5);
        assert_eq!(Probe::new("a", m, 0, true).score, 0.25);
        assert_eq!(Probe::new("a", Metrics { mem_gib: 2.0, ..m }, 0, false).score, 1.5);
    }

    #[test]
    fn cached_probes_count_only_when_fresh_and_whole() {
        let entry = json!({ "host": "miller", "load": 0.5, "mem_gib": 32.0, "agents": 1, "warm": true,
            "age_ms": 1000, "error": null });
        let p = cached_probe(Some(&entry)).unwrap().unwrap();
        assert_eq!((p.host.as_str(), p.score, p.cached), ("miller", 0.75, true));
        // Fresh word that the host is down: leave it out without asking again.
        let down = json!({ "host": "miller", "age_ms": 1000, "error": "couldn't reach miller" });
        assert_eq!(cached_probe(Some(&down)), Some(None));
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
                eprintln!("{} {} {ms} ms: {choice:?}", project.name, if use_cache { "cached" } else { "live" });
            }
        }
    }
}
