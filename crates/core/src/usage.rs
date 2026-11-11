//! Read-only subscription and session accounting. No credentials or prompts are
//! exported: hosts return only quota fields and usage counters. Quota is an
//! account snapshot (never summed); session events are deduplicated across
//! copied logs/hosts. Prices are optional runtime settings, not billing claims.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{config, rex};

pub const PERIOD: u64 = 7 * 86400;
pub const STALE: u64 = 300;
pub const REFRESH: Duration = Duration::from_secs(60);

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// None uses only this process's active profiles. An empty list disables it.
    pub sessions: Option<Vec<SessionSource>>,
    pub quotas: Option<Vec<QuotaSource>>,
    pub rates: Vec<Rate>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionSource {
    /// claude, pi, or codex. Path is the sessions/projects directory.
    pub harness: String,
    pub path: String,
    pub account: String,
    #[serde(default)]
    pub profile: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QuotaSource {
    pub path: String,
    pub account: String,
    #[serde(default)]
    pub profile: String,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct Rate {
    pub model: String,
    /// Empty matches any provider. All four prices must be supplied.
    pub provider: String,
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Tokens {
    pub fn total(&self) -> u64 {
        self.input.saturating_add(self.output).saturating_add(self.cache_read).saturating_add(self.cache_write)
    }

    fn add(&mut self, t: &Self) {
        self.input = self.input.saturating_add(t.input);
        self.output = self.output.saturating_add(t.output);
        self.cache_read = self.cache_read.saturating_add(t.cache_read);
        self.cache_write = self.cache_write.saturating_add(t.cache_write);
    }

    fn delta(&self, old: &Self) -> Self {
        // A context reset starts a new cumulative counter.
        if self.total() < old.total() {
            return self.clone();
        }
        Self {
            input: self.input.saturating_sub(old.input),
            output: self.output.saturating_sub(old.output),
            cache_read: self.cache_read.saturating_sub(old.cache_read),
            cache_write: self.cache_write.saturating_sub(old.cache_write),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sample {
    pub id: String,
    pub harness: String,
    pub account: String,
    pub profile: String,
    pub model: String,
    pub provider: String,
    pub at: u64,
    pub tokens: Tokens,
    pub cost: Option<f64>,
    pub hosts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Window {
    pub label: String,
    pub used: Option<f64>,
    pub reset: Option<u64>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Quota {
    pub account: String,
    pub profile: String,
    pub provider: String,
    pub plan: String,
    pub status: String,
    pub fetched: u64,
    pub windows: Vec<Window>,
    pub credits: Option<String>,
    pub hosts: Vec<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub fetched: u64,
    pub samples: Vec<Sample>,
    pub quotas: Vec<Quota>,
    pub warnings: Vec<String>,
}

fn string(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_default().to_string()
}

fn number(v: &Value, key: &str) -> u64 {
    v[key].as_u64().unwrap_or(0)
}

fn finite(v: &Value) -> Option<f64> {
    v.as_f64().filter(|n| n.is_finite() && *n >= 0.0)
}

fn timestamp(v: &Value) -> u64 {
    if let Some(n) = v.as_u64() {
        return if n > 10_000_000_000 { n / 1000 } else { n };
    }
    v.as_str()
        .and_then(|s| time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok())
        .and_then(|t| u64::try_from(t.unix_timestamp()).ok())
        .unwrap_or(0)
}

fn expand(path: &str) -> PathBuf {
    if path == "~" {
        rex::home()
    } else if let Some(p) = path.strip_prefix("~/") {
        rex::home().join(p)
    } else {
        PathBuf::from(path)
    }
}

fn active(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var).filter(|p| !p.is_empty()).map(PathBuf::from).unwrap_or_else(|| expand(fallback))
}

fn defaults() -> Vec<SessionSource> {
    [
        ("claude", active("CLAUDE_CONFIG_DIR", "~/.claude"), "projects"),
        ("pi", active("PI_CODING_AGENT_DIR", "~/.config/pi"), "sessions"),
        ("codex", active("CODEX_HOME", "~/.codex"), "sessions"),
    ]
    .into_iter()
    .map(|(harness, dir, sub)| SessionSource {
        harness: harness.into(),
        account: format!("{} (unmapped)", dir.file_name().unwrap_or_default().to_string_lossy()),
        profile: String::new(),
        path: dir.join(sub).display().to_string(),
    })
    .collect()
}

fn default_quota() -> QuotaSource {
    let dir = active("XDG_CACHE_HOME", "~/.cache");
    QuotaSource {
        path: dir.join("llm-usage/usage.json").display().to_string(),
        account: "unmapped account".into(),
        profile: String::new(),
    }
}

/// Cache parsed files between refreshes. No cache or source file is written.
#[derive(Default)]
pub struct Collector {
    files: HashMap<PathBuf, (u64, SystemTime, Vec<Sample>, Vec<Quota>, usize)>,
}

impl Collector {
    pub fn local(&mut self, settings: &Settings, host: &str, now: u64) -> Snapshot {
        let mut snap = Snapshot { fetched: now, ..Snapshot::default() };
        let mut seen = Vec::new();
        if settings.sessions.is_none() || settings.quotas.is_none() {
            snap.warnings.push(format!("{host}: account mapping not configured; defaults cover active profiles only"));
        }
        for mut source in settings.quotas.clone().unwrap_or_else(|| vec![default_quota()]) {
            if settings.quotas.is_none() {
                source.account = format!("unmapped account on {host}");
            }
            match fs::read_to_string(expand(&source.path)) {
                Ok(text) => match parse_quota(&text, &source, host) {
                    Ok(quotas) => snap.quotas.extend(quotas),
                    Err(e) => snap.warnings.push(format!("{host}: quota {}: {e}", source.account)),
                },
                Err(e) => snap.warnings.push(format!("{host}: quota {}: {e}", source.account)),
            }
        }
        for mut source in settings.sessions.clone().unwrap_or_else(defaults) {
            if settings.sessions.is_none() {
                source.account = format!("{} on {host}", source.account);
            }
            if !matches!(source.harness.as_str(), "claude" | "pi" | "codex") {
                snap.warnings.push(format!("{host}: unsupported harness {}", source.harness));
                continue;
            }
            let mut paths = Vec::new();
            // A profile that has never run on this host has no log directory yet.
            match files(&expand(&source.path), &mut paths) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    snap.warnings.push(format!("{host}: {} {} logs: {e}", source.account, source.harness));
                }
                _ => {}
            }
            let mut malformed = 0;
            for path in paths {
                let Ok(meta) = fs::metadata(&path) else { continue };
                let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                if modified.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs()
                    < now.saturating_sub(PERIOD)
                {
                    continue;
                }
                seen.push(path.clone());
                let fresh =
                    self.files.get(&path).is_some_and(|(len, at, _, _, _)| *len == meta.len() && *at == modified);
                if !fresh {
                    match parse_log(&path, &source, host) {
                        Ok((samples, quotas, bad)) => {
                            self.files.insert(path.clone(), (meta.len(), modified, samples, quotas, bad));
                        }
                        Err(e) => {
                            snap.warnings.push(format!("{host}: {} log: {e}", source.harness));
                            continue;
                        }
                    }
                }
                if let Some((_, _, samples, quotas, bad)) = self.files.get(&path) {
                    malformed += bad;
                    snap.quotas.extend(quotas.iter().cloned().map(|mut q| {
                        q.account = source.account.clone();
                        q.profile = source.profile.clone();
                        q
                    }));
                    // Remap cached samples if the declarative account labels changed.
                    snap.samples.extend(
                        samples.iter().filter(|s| s.at >= now.saturating_sub(PERIOD) && s.at <= now).cloned().map(
                            |mut s| {
                                s.account = source.account.clone();
                                s.profile = source.profile.clone();
                                s
                            },
                        ),
                    );
                }
            }
            if malformed > 0 {
                snap.warnings
                    .push(format!("{host}: {}: {malformed} malformed/incomplete log lines skipped", source.account));
            }
        }
        self.files.retain(|p, _| seen.contains(p));
        for sample in &mut snap.samples {
            if sample.cost.is_none() {
                sample.cost = estimate(&sample.tokens, &sample.model, &sample.provider, &settings.rates);
            }
        }
        snap.dedup();
        snap
    }

    /// Hosts are runtime settings, never a compiled-in pool. Missing binaries,
    /// old protocols and unreachable hosts remain visible as partial coverage.
    pub fn collect(&mut self) -> Snapshot {
        let now = rex::now();
        let host = rex::local_label();
        let config = config::read();
        let mut snap = self.local(&config.as_ref().map(|c| c.usage.clone()).unwrap_or_default(), &host, now);
        match config {
            Ok(config) => {
                let mut hosts = config.hosts;
                hosts.sort();
                hosts.dedup();
                let remote = thread::scope(|scope| {
                    let jobs: Vec<_> = hosts
                        .iter()
                        .filter(|h| *h != &host && h.as_str() != "config")
                        .map(|h| (h, scope.spawn(move || remote(h))))
                        .collect();
                    jobs.into_iter()
                        .map(|(h, j)| (h.clone(), j.join().unwrap_or_else(|_| Err("collector failed".into()))))
                        .collect::<Vec<_>>()
                });
                for (host, result) in remote {
                    match result {
                        Ok(other) => {
                            snap.samples.extend(other.samples);
                            snap.quotas.extend(other.quotas);
                            snap.warnings.extend(other.warnings);
                        }
                        Err(e) => snap.warnings.push(format!("{host}: {e}")),
                    }
                }
                for sample in &mut snap.samples {
                    if sample.cost.is_none() {
                        sample.cost = estimate(&sample.tokens, &sample.model, &sample.provider, &config.usage.rates);
                    }
                }
            }
            Err(e) => snap.warnings.push(e),
        }
        snap.dedup();
        snap
    }
}

fn files(root: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    // Do not follow symlinks: profile boundaries must stay explicit.
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            files(&entry.path(), out)?;
        } else if kind.is_file() && entry.path().extension().is_some_and(|e| e == "jsonl") {
            out.push(entry.path());
        }
    }
    Ok(())
}

pub fn parse_quota(text: &str, source: &QuotaSource, host: &str) -> Result<Vec<Quota>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if v["schema_version"] != 2 {
        return Err("unsupported llm-usage schema (expected 2)".into());
    }
    let providers = v["providers"].as_array().ok_or("missing providers")?;
    Ok(providers
        .iter()
        .map(|p| Quota {
            account: source.account.clone(),
            profile: source.profile.clone(),
            provider: string(p, "provider"),
            plan: string(p, "plan"),
            status: if p["available"] == false {
                "unavailable".into()
            } else if p["display"]["exhausted"] == true {
                "exhausted".into()
            } else {
                string(p, "status")
            },
            fetched: p["fetched_at"].as_u64().unwrap_or_else(|| number(&v, "fetched_at")),
            windows: p["windows"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|w| Window {
                    label: string(w, "label"),
                    used: finite(&w["used_percent"]),
                    reset: w["reset_at"].as_u64(),
                    status: if w["limit_reached"] == true { "limit reached".into() } else { string(w, "status") },
                })
                .collect(),
            credits: p
                .get("credits")
                .filter(|c| !c.is_null())
                .map(|c| c.as_str().map(str::to_string).unwrap_or_else(|| c.to_string())),
            hosts: vec![host.into()],
        })
        .collect())
}

fn parse_log(path: &Path, source: &SessionSource, host: &str) -> std::io::Result<(Vec<Sample>, Vec<Quota>, usize)> {
    let file = fs::File::open(path)?;
    let mut parser = LogParser::new(source, host, path.file_stem().unwrap_or_default().to_string_lossy().to_string());
    let mut bad = 0;
    for line in BufReader::new(file).lines() {
        let line = line?;
        match serde_json::from_str::<Value>(&line) {
            Ok(v) => parser.push(&v),
            Err(_) => bad += 1,
        }
    }
    Ok((parser.samples, parser.quota.into_iter().collect(), bad))
}

struct LogParser<'a> {
    source: &'a SessionSource,
    host: &'a str,
    session: String,
    model: String,
    previous: Tokens,
    samples: Vec<Sample>,
    quota: Option<Quota>,
}

impl<'a> LogParser<'a> {
    fn new(source: &'a SessionSource, host: &'a str, session: String) -> Self {
        Self {
            source,
            host,
            session,
            model: "unknown".into(),
            previous: Tokens::default(),
            samples: Vec::new(),
            quota: None,
        }
    }

    fn push(&mut self, v: &Value) {
        let harness = self.source.harness.as_str();
        if harness == "pi" && v["type"] == "session" {
            if let Some(id) = v["id"].as_str() {
                self.session = id.into()
            }
        }
        if harness == "codex" {
            let p = &v["payload"];
            if v["type"] == "session_meta" {
                if let Some(id) = p["id"].as_str() {
                    self.session = id.into()
                }
            }
            if v["type"] == "turn_context" {
                self.model = p["model"].as_str().unwrap_or("unknown").into();
            }
            if v["type"] != "event_msg" || p["type"] != "token_count" {
                return;
            }
            let limits = &p["rate_limits"];
            if limits.is_object() {
                let windows = [("primary", "primary"), ("secondary", "secondary")]
                    .into_iter()
                    .filter_map(|(key, label)| {
                        let w = &limits[key];
                        w.is_object().then(|| Window {
                            label: w["window_minutes"]
                                .as_u64()
                                .map(|m| format!("{}m", m))
                                .unwrap_or_else(|| label.into()),
                            used: finite(&w["used_percent"]),
                            reset: w["resets_at"].as_u64(),
                            status: "ok".into(),
                        })
                    })
                    .collect();
                let credits = &limits["credits"];
                let balance = credits["balance"]
                    .as_str()
                    .and_then(|s| s.parse::<f64>().ok())
                    .filter(|n| n.is_finite() && *n >= 0.0)
                    .or_else(|| finite(&credits["balance"]));
                self.quota = Some(Quota {
                    account: self.source.account.clone(),
                    profile: self.source.profile.clone(),
                    provider: limits["limit_id"].as_str().filter(|s| !s.is_empty()).unwrap_or("codex").into(),
                    plan: string(limits, "plan_type"),
                    status: "ok".into(),
                    fetched: timestamp(&v["timestamp"]),
                    windows,
                    credits: if credits["unlimited"] == true {
                        Some("unlimited".into())
                    } else {
                        balance.map(|n| format!("{n:.4}"))
                    },
                    hosts: vec![self.host.into()],
                });
            }
            let u = &p["info"]["total_token_usage"];
            if !u.is_object() {
                return;
            }
            let cumulative = Tokens {
                input: number(u, "input_tokens").saturating_sub(number(u, "cached_input_tokens")),
                output: number(u, "output_tokens"),
                cache_read: number(u, "cached_input_tokens"),
                cache_write: number(u, "cache_write_input_tokens"),
            };
            let tokens = cumulative.delta(&self.previous);
            let id = format!(
                "{}:{}:{}/{}/{}/{}",
                self.session,
                v["timestamp"],
                cumulative.input,
                cumulative.output,
                cumulative.cache_read,
                cumulative.cache_write
            );
            self.previous = cumulative;
            if tokens.total() > 0 {
                self.sample(id, self.model.clone(), "openai".into(), timestamp(&v["timestamp"]), tokens, None);
            }
            return;
        }
        let m = &v["message"];
        let u = &m["usage"];
        if m["role"] != "assistant" || !u.is_object() || m["model"] == "<synthetic>" {
            return;
        }
        let (tokens, id, provider, cost) = if harness == "pi" {
            let cost = finite(&u["cost"]["total"]).filter(|c| *c > 0.0 || u["cost"]["totalFromProvider"] == true);
            (
                Tokens {
                    input: number(u, "input"),
                    output: number(u, "output"),
                    cache_read: number(u, "cacheRead"),
                    cache_write: number(u, "cacheWrite"),
                },
                string(v, "id"),
                string(m, "provider"),
                cost,
            )
        } else {
            self.session = v["sessionId"].as_str().unwrap_or(&self.session).to_string();
            (
                Tokens {
                    input: number(u, "input_tokens"),
                    output: number(u, "output_tokens"),
                    cache_read: number(u, "cache_read_input_tokens"),
                    cache_write: number(u, "cache_creation_input_tokens"),
                },
                string(m, "id"),
                "anthropic".into(),
                finite(&v["costUSD"]),
            )
        };
        if id.is_empty() || tokens.total() == 0 {
            return;
        }
        self.sample(
            format!("{}:{id}", self.session),
            string(m, "model"),
            provider,
            timestamp(&v["timestamp"]),
            tokens,
            cost,
        );
    }

    fn sample(&mut self, id: String, model: String, provider: String, at: u64, tokens: Tokens, cost: Option<f64>) {
        self.samples.push(Sample {
            id,
            harness: self.source.harness.clone(),
            account: self.source.account.clone(),
            profile: self.source.profile.clone(),
            model,
            provider,
            at,
            tokens,
            cost,
            hosts: vec![self.host.into()],
        });
    }
}

fn estimate(tokens: &Tokens, model: &str, provider: &str, rates: &[Rate]) -> Option<f64> {
    let r = rates
        .iter()
        .filter(|r| r.model == model && (r.provider.is_empty() || r.provider == provider))
        .max_by_key(|r| !r.provider.is_empty())?;
    let prices = [r.input?, r.output?, r.cache_read?, r.cache_write?];
    if prices.iter().any(|p| !p.is_finite() || *p < 0.0) {
        return None;
    }
    Some(
        (tokens.input as f64 * prices[0]
            + tokens.output as f64 * prices[1]
            + tokens.cache_read as f64 * prices[2]
            + tokens.cache_write as f64 * prices[3])
            / 1_000_000.0,
    )
}

fn union(a: &mut Vec<String>, b: &[String]) {
    a.extend_from_slice(b);
    a.sort();
    a.dedup();
}

impl Snapshot {
    pub fn dedup(&mut self) {
        let mut samples: BTreeMap<(String, String, String), Sample> = BTreeMap::new();
        for s in self.samples.drain(..) {
            let key = (s.harness.clone(), s.account.clone(), s.id.clone());
            if let Some(old) = samples.get_mut(&key) {
                let mut hosts = old.hosts.clone();
                union(&mut hosts, &s.hosts);
                // Claude emits the same request with multiple content blocks.
                // The largest counter is the completed request, not new spend.
                if s.tokens.total() > old.tokens.total()
                    || (s.tokens.total() == old.tokens.total() && old.cost.is_none() && s.cost.is_some())
                {
                    *old = s
                }
                old.hosts = hosts;
            } else {
                samples.insert(key, s);
            }
        }
        self.samples = samples.into_values().collect();
        let mut quotas: BTreeMap<(String, String), Quota> = BTreeMap::new();
        for q in self.quotas.drain(..) {
            let key = (q.account.clone(), q.provider.clone());
            if let Some(old) = quotas.get_mut(&key) {
                let mut hosts = old.hosts.clone();
                union(&mut hosts, &q.hosts);
                if q.fetched > old.fetched {
                    *old = q
                }
                old.hosts = hosts;
            } else {
                quotas.insert(key, q);
            }
        }
        self.quotas = quotas.into_values().collect();
        self.warnings.sort();
        self.warnings.dedup();
    }

    pub fn totals(&self) -> Vec<Total> {
        let mut totals: BTreeMap<(String, String, String, String, String), Total> = BTreeMap::new();
        for s in &self.samples {
            let t = totals
                .entry((s.profile.clone(), s.account.clone(), s.harness.clone(), s.provider.clone(), s.model.clone()))
                .or_insert_with(|| Total {
                    profile: s.profile.clone(),
                    account: s.account.clone(),
                    harness: s.harness.clone(),
                    provider: s.provider.clone(),
                    model: s.model.clone(),
                    ..Total::default()
                });
            t.tokens.add(&s.tokens);
            t.requests += 1;
            union(&mut t.hosts, &s.hosts);
            if let Some(cost) = s.cost { t.cost += cost } else { t.unpriced += 1 }
        }
        totals.into_values().collect()
    }
}

#[derive(Default)]
pub struct Total {
    pub profile: String,
    pub account: String,
    pub harness: String,
    pub provider: String,
    pub model: String,
    pub tokens: Tokens,
    pub requests: usize,
    pub cost: f64,
    pub unpriced: usize,
    pub hosts: Vec<String>,
}

/// Bound the entire remote command, not only SSH's connection handshake.
/// Only sanitized accounting JSON crosses the connection, never raw logs.
fn remote(host: &str) -> Result<Snapshot, String> {
    let mut child = rex::ssh(host)
        .arg("PATH=\"$HOME/.local/bin:$HOME/.nix-profile/bin:/etc/profiles/per-user/$(id -un)/bin:$PATH\" sh -c 'exec switchyard usage --local'")
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null())
        .spawn().map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().ok_or("no collector output")?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(32 * 1024 * 1024 + 1).read_to_end(&mut bytes).map(|_| bytes)
    });
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let bytes = reader.join().map_err(|_| "collector reader failed")?.map_err(|e| e.to_string())?;
                if !status.success() {
                    return Err("usage collector unavailable (install matching Switchyard on this host)".into());
                }
                if bytes.len() > 32 * 1024 * 1024 {
                    return Err("usage collector response too large".into());
                }
                return serde_json::from_slice(&bytes).map_err(|_| "incompatible usage collector response".into());
            }
            Ok(None) if started.elapsed() < Duration::from_secs(30) => thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("usage collector timed out/unreachable".into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn source(harness: &str) -> SessionSource {
        SessionSource { harness: harness.into(), account: "work".into(), profile: "work".into(), path: String::new() }
    }

    #[test]
    fn timestamps_accept_cli_and_pi_formats() {
        assert_eq!(timestamp(&json!("2026-11-10T19:14:46.503Z")), 1794338086);
        assert_eq!(timestamp(&json!(1794338086503_u64)), 1794338086);
        assert_eq!(timestamp(&json!("not a date")), 0);
    }

    #[test]
    fn claude_blocks_and_copied_host_logs_are_not_added_twice() {
        let src = source("claude");
        let mut parser = LogParser::new(&src, "mac", "file".into());
        for output in [10, 30] {
            parser.push(&json!({"type":"assistant", "sessionId":"session", "timestamp":"2026-11-10T19:00:00Z",
                "message":{"id":"request", "role":"assistant", "model":"opus", "content":"PRIVATE PROMPT MUST NOT LEAVE HOST", "usage":{
                    "input_tokens":100,"output_tokens":output,"cache_read_input_tokens":200,"cache_creation_input_tokens":40}}}));
        }
        let mut copy = parser.samples[1].clone();
        copy.hosts = vec!["linux".into()];
        let mut snap = Snapshot { samples: parser.samples, ..Snapshot::default() };
        snap.samples.push(copy);
        snap.dedup();
        let totals = snap.totals();
        assert_eq!(snap.samples.len(), 1);
        assert_eq!(totals[0].tokens.total(), 370);
        assert_eq!(totals[0].hosts, ["linux", "mac"]);
        assert_eq!(totals[0].unpriced, 1);
        assert!(!serde_json::to_string(&snap).unwrap().contains("PRIVATE PROMPT"));
    }

    #[test]
    fn pi_zero_placeholder_is_not_a_free_request() {
        let src = source("pi");
        let mut parser = LogParser::new(&src, "mac", "file".into());
        parser.push(&json!({"type":"session", "id":"session"}));
        for (id, cost, reported) in [("a", 0.0, false), ("b", 0.0, true), ("c", 0.25, false)] {
            parser.push(&json!({"type":"message", "id":id,"timestamp":"2026-11-10T19:00:00Z", "message":{
                "role":"assistant","model":"model","provider":"provider", "usage":{"input":10,"output":20,
                "cacheRead":30,"cacheWrite":40,"reasoning":10,"totalTokens":100,
                "cost":{"total":cost,"totalFromProvider":reported}}}}));
        }
        assert_eq!(parser.samples[0].tokens.total(), 100); // reasoning is already in output
        assert!(parser.samples[0].cost.is_none());
        assert_eq!(parser.samples[1].cost, Some(0.0));
        assert_eq!(parser.samples[2].cost, Some(0.25));
    }

    #[test]
    fn codex_uses_cumulative_deltas_not_repeated_last_turns() {
        let src = source("codex");
        let mut parser = LogParser::new(&src, "linux", "file".into());
        parser.push(&json!({"type":"session_meta", "payload":{"id":"session"}}));
        parser.push(&json!({"type":"turn_context", "payload":{"model":"gpt"}}));
        for (input, cached, output) in [(100, 80, 10), (100, 80, 10), (150, 100, 20)] {
            parser.push(&json!({"type":"event_msg", "timestamp":"2026-11-10T19:00:00Z", "payload":{
                "type":"token_count", "info":{"total_token_usage":{"input_tokens":input,"cached_input_tokens":cached,
                "output_tokens":output,"reasoning_output_tokens":5}}, "rate_limits":{"limit_id":"codex", "plan_type":"team",
                "primary":{"used_percent":50,"window_minutes":300,"resets_at":1794350000},
                "credits":{"unlimited":false,"balance":"1324.062595"}}}}));
        }
        assert_eq!(parser.samples.len(), 2);
        let snap = Snapshot { samples: parser.samples, ..Snapshot::default() };
        let total = &snap.totals()[0];
        assert_eq!(total.tokens.total(), 170);
        assert_eq!(total.tokens.input, 50); // cached input is a subset, not extra tokens
        assert_eq!(total.model, "gpt");
        let quota = parser.quota.unwrap();
        assert_eq!(quota.credits.as_deref(), Some("1324.0626"));
        assert_eq!(quota.windows[0].used, Some(50.0));
    }

    #[test]
    fn quota_uses_latest_account_snapshot_not_sum_or_first_host() {
        let src = QuotaSource { path: String::new(), account: "work".into(), profile: "work".into() };
        let text = json!({"schema_version":2,"fetched_at":100,"providers":[{"provider":"codex","status":"ok",
            "windows":[{"label":"5h","used_percent":50,"reset_at":200,"status":"ok"}]}]})
        .to_string();
        let mut quotas = parse_quota(&text, &src, "mac").unwrap();
        let mut newer = quotas[0].clone();
        newer.fetched = 150;
        newer.windows[0].used = Some(60.0);
        newer.hosts = vec!["linux".into()];
        quotas.push(newer);
        let mut other = quotas[0].clone();
        other.account = "personal".into();
        quotas.push(other);
        let mut snap = Snapshot { quotas, ..Snapshot::default() };
        snap.dedup();
        assert_eq!(snap.quotas.len(), 2);
        let work = snap.quotas.iter().find(|q| q.account == "work").unwrap();
        assert_eq!(work.windows[0].used, Some(60.0));
        assert_eq!(work.hosts, ["linux", "mac"]);
        assert!(parse_quota(r#"{"schema_version":3}"#, &src, "mac").is_err());
    }

    #[test]
    fn rates_require_exact_models_and_all_token_categories() {
        let tokens = Tokens { input: 1_000_000, output: 100_000, cache_read: 1_000_000, cache_write: 100_000 };
        let rate = Rate {
            model: "model".into(),
            input: Some(1.0),
            output: Some(10.0),
            cache_read: Some(0.1),
            cache_write: Some(2.0),
            ..Rate::default()
        };
        assert_eq!(estimate(&tokens, "model", "provider", &[rate.clone()]), Some(2.3));
        assert!(estimate(&tokens, "new-model", "provider", &[rate.clone()]).is_none());
        assert!(estimate(&tokens, "model", "provider", &[Rate { cache_read: None, ..rate }]).is_none());
    }

    #[test]
    fn counter_reset_does_not_underflow() {
        let before = Tokens { input: 100, ..Tokens::default() };
        let after = Tokens { input: 20, ..Tokens::default() };
        assert_eq!(after.delta(&before).input, 20);
    }

    #[test]
    fn settings_accept_explicit_empty_sources_and_do_not_select_other_profiles() {
        let c: config::Config = toml::from_str("[usage]\nsessions=[]\nquotas=[]").unwrap();
        let snap = Collector::default().local(&c.usage, "test-host", 1794338086);
        assert!(snap.samples.is_empty() && snap.quotas.is_empty() && snap.warnings.is_empty());
    }

    #[test]
    fn missing_log_directory_is_not_a_warning() {
        let source = SessionSource {
            harness: "claude".into(),
            path: "/nonexistent/switchyard-test/projects".into(),
            account: "personal".into(),
            profile: "personal".into(),
        };
        let settings = Settings { sessions: Some(vec![source]), quotas: Some(vec![]), rates: vec![] };
        let snap = Collector::default().local(&settings, "test-host", 1794338086);
        assert!(snap.samples.is_empty() && snap.warnings.is_empty());
    }
}
