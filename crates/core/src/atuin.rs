//! Restoring, converting, and searching agent sessions through Atuin Threads.
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
//! - `search`: `atuin threads search --style json` searches captured threads'
//!   titles, messages, reasoning, changed file names, and tool-call inputs.
//!
//! The personal profile's threads live in the default store (`atuin`), the
//! work profile's in its own (`atuin-work`, which system-config installs with
//! its `ATUIN_HOME`). Other profiles (such as `notes` / `pi-notes`) and
//! Antigravity (`agy`) are neither captured nor valid `--in` targets.

use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::sessions::{session_exists_with_env, work_env};
use crate::tasks::rfc3339_secs;

/// How long an Atuin Threads call may take, sync included, before giving up.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Default maximum threads requested per store from `atuin threads search`.
pub const DEFAULT_SEARCH_LIMIT: usize = 30;

/// One of the Atuin stores Switchyard knows about.
///
/// Personal threads live in the default `atuin` store (synced through nas);
/// work threads live in `atuin-work` (`ATUIN_HOME=~/.atuin-work`, local only).
/// They are always queried read-only in their own processes and kept labelled
/// separately so work results are never copied into the personal store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Store {
    Personal,
    Work,
}

impl Store {
    /// Both Atuin stores, personal first.
    pub const ALL: [Store; 2] = [Store::Personal, Store::Work];

    /// Short profile label shown on search results.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Store::Personal => "personal",
            Store::Work => "work",
        }
    }

    /// The Atuin binary for this store.
    #[must_use]
    pub fn program(self) -> &'static str {
        store_program(self.label()).unwrap_or("atuin")
    }

    /// Whether a Switchyard task with `profile` belongs to this store.
    #[must_use]
    pub fn matches_profile(self, profile: &str) -> bool {
        match self {
            Store::Personal => matches!(profile, "" | "personal"),
            Store::Work => profile == "work",
        }
    }
}

/// Switchyard's harness name for an Atuin harness name (`claude-code` →
/// `claude`), falling back to the raw name for other harnesses.
#[must_use]
pub fn switchyard_harness(atuin_harness: &str) -> &str {
    match atuin_harness {
        "claude-code" => "claude",
        other => other,
    }
}

/// One matching thread returned by `atuin threads search --style json`.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    pub store: Store,
    /// Switchyard harness name (`claude`, `pi`, or Atuin's raw name).
    pub harness: String,
    /// Agent session ID, which maps back to a Switchyard task's `agent_session`.
    pub session_id: String,
    pub atuin_id: String,
    pub cwd: String,
    pub git_branch: String,
    pub model: String,
    /// Unix seconds (0 when absent or unparseable).
    pub started_at: u64,
    pub updated_at: u64,
    pub message_count: u64,
    /// Thread title (from the search hit's title block or session metadata).
    pub title: String,
    /// UTF-8 byte ranges `[start, end)` inside `title` that matched.
    pub title_matches: Vec<(usize, usize)>,
    /// Matched excerpt (`preview.text`), empty when the match was only in `title`.
    pub snippet: String,
    /// UTF-8 byte ranges `[start, end)` inside `snippet` that matched.
    pub snippet_matches: Vec<(usize, usize)>,
    /// First user prompt (`session.preview`).
    pub first_prompt: String,
    /// Last assistant reply (`session.last_reply`).
    pub last_reply: String,
    pub message_index: Option<u64>,
    pub score: f64,
}

/// Failure from searching a single Atuin store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreError {
    pub store: Store,
    pub message: String,
    /// True when the store's binary (`atuin` / `atuin-work`) is not installed.
    pub missing: bool,
}

/// Combined outcome of searching one or more Atuin stores.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchResult {
    pub hits: Vec<SearchHit>,
    pub errors: Vec<StoreError>,
}

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

/// Split a transcript search query into arguments for `atuin threads search`.
///
/// Unquoted whitespace-separated words become separate arguments (all must
/// appear). Quoted phrases (`"..."` or `'...'`) stay together in one argument
/// so Atuin searches for those words adjacent and in order. Unclosed trailing
/// quotes are tolerated so an incomplete query still searches sensibly.
#[must_use]
pub fn parse_search_terms(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut cur = String::new();
    let mut chars = query.chars();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                } else {
                    cur.push(c);
                }
            }
            Some('"') => match c {
                '"' => quote = None,
                '\\' => {
                    if let Some(next) = chars.next() {
                        cur.push(next);
                    }
                }
                _ => cur.push(c),
            },
            Some(_) => unreachable!(),
            None => match c {
                c if c.is_whitespace() => {
                    if !cur.is_empty() {
                        terms.push(std::mem::take(&mut cur));
                    }
                }
                '\'' | '"' => quote = Some(c),
                _ => cur.push(c),
            },
        }
    }
    if !cur.is_empty() {
        terms.push(cur);
    }
    terms
}

#[derive(Deserialize)]
struct RawSearchHit {
    #[serde(default)]
    session: RawSearchSession,
    #[serde(default)]
    score: f64,
    #[serde(default)]
    title: Option<RawHighlight>,
    #[serde(default)]
    preview: Option<RawHighlight>,
    #[serde(default)]
    message_index: Option<u64>,
}

#[derive(Default, Deserialize)]
struct RawSearchSession {
    #[serde(default)]
    harness: String,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    atuin_id: String,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    git_branch: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    message_count: u64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    preview: Option<String>,
    #[serde(default)]
    last_reply: Option<String>,
}

#[derive(Default, Deserialize)]
struct RawHighlight {
    #[serde(default)]
    text: String,
    #[serde(default)]
    matches: Vec<[usize; 2]>,
}

/// Keep only valid UTF-8 byte ranges inside `text`, sorted by start.
fn clean_matches(text: &str, raw: Vec<[usize; 2]>) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = raw
        .into_iter()
        .filter(|&[a, b]| {
            a < b && b <= text.len() && text.is_char_boundary(a) && text.is_char_boundary(b)
        })
        .map(|[a, b]| (a, b))
        .collect();
    ranges.sort_unstable();
    ranges
}

/// Parse the JSON array printed by `atuin threads search --style json` for
/// `store`, labelling every hit with `store`.
///
/// # Errors
///
/// Fails when `json` isn't a valid Atuin search JSON array.
pub fn parse_search_json(store: Store, json: &str) -> Result<Vec<SearchHit>, String> {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let raw: Vec<RawSearchHit> = serde_json::from_str(trimmed)
        .map_err(|e| format!("{}: bad search JSON: {e}", store.program()))?;
    Ok(raw
        .into_iter()
        .map(|item| {
            let (title, title_matches) = match item.title {
                Some(h) if !h.text.is_empty() => {
                    let m = clean_matches(&h.text, h.matches);
                    (h.text, m)
                }
                _ => (item.session.title.unwrap_or_default(), Vec::new()),
            };
            let (snippet, snippet_matches) = match item.preview {
                Some(h) if !h.text.is_empty() => {
                    let m = clean_matches(&h.text, h.matches);
                    (h.text, m)
                }
                _ => (String::new(), Vec::new()),
            };
            SearchHit {
                store,
                harness: switchyard_harness(&item.session.harness).to_string(),
                session_id: item.session.session_id,
                atuin_id: item.session.atuin_id,
                cwd: item.session.cwd,
                git_branch: item.session.git_branch.unwrap_or_default(),
                model: item.session.model.unwrap_or_default(),
                started_at: item
                    .session
                    .started_at
                    .as_deref()
                    .and_then(rfc3339_secs)
                    .unwrap_or(0),
                updated_at: item
                    .session
                    .updated_at
                    .as_deref()
                    .and_then(rfc3339_secs)
                    .unwrap_or(0),
                message_count: item.session.message_count,
                title,
                title_matches,
                snippet,
                snippet_matches,
                first_prompt: item.session.preview.unwrap_or_default(),
                last_reply: item.session.last_reply.unwrap_or_default(),
                message_index: item.message_index,
                score: item.score,
            }
        })
        .collect())
}

/// Search one Atuin `store` (`atuin` or `atuin-work`) read-only for `query`.
///
/// `harness`, when `Some`, is a Switchyard harness name (`claude` or `pi`)
/// translated through [`harness_name`] for `--harness`. `limit` caps hits per
/// store (0 for unbounded).
///
/// # Errors
///
/// Fails with a [`StoreError`] when `harness` isn't captured by Atuin, the
/// binary is missing, Atuin exits non-zero or times out, or its JSON can't be
/// parsed.
pub fn search_store(
    store: Store,
    query: &str,
    harness: Option<&str>,
    limit: usize,
) -> Result<Vec<SearchHit>, StoreError> {
    let terms = parse_search_terms(query);
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let program = store.program();
    let limit_str = limit.to_string();
    let mut args: Vec<&str> = vec![
        "threads", "search", "--style", "json", "--limit", &limit_str,
    ];
    if let Some(h) = harness {
        let Some(atuin_h) = harness_name(h) else {
            return Err(StoreError {
                store,
                message: format!("Atuin doesn't capture {h} sessions"),
                missing: false,
            });
        };
        args.push("--harness");
        args.push(atuin_h);
    }
    args.push("--");
    args.extend(terms.iter().map(String::as_str));
    let home = crate::rex::home();
    let stdout = run_atuin_detailed(program, &args, &[], &home, "search failed").map_err(
        |(message, missing)| StoreError {
            store,
            message,
            missing,
        },
    )?;
    parse_search_json(store, &stdout).map_err(|message| StoreError {
        store,
        message,
        missing: false,
    })
}

/// Search `stores` in parallel for `query`, keeping every hit labelled with
/// its store and sorting combined hits by relevance score (then recency).
///
/// Each store is queried read-only in its own process (`atuin` for personal,
/// `atuin-work` for work); results are never written or copied between stores.
#[must_use]
pub fn search(stores: &[Store], query: &str, harness: Option<&str>, limit: usize) -> SearchResult {
    if parse_search_terms(query).is_empty() || stores.is_empty() {
        return SearchResult::default();
    }
    let outcomes: Vec<Result<Vec<SearchHit>, StoreError>> = std::thread::scope(|scope| {
        let handles: Vec<_> = stores
            .iter()
            .copied()
            .map(|store| scope.spawn(move || search_store(store, query, harness, limit)))
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    let mut out = SearchResult::default();
    for outcome in outcomes {
        match outcome {
            Ok(hits) => out.hits.extend(hits),
            Err(err) => out.errors.push(err),
        }
    }
    sort_hits(&mut out.hits);
    out
}

/// Sort hits from one or more stores by score descending, then `updated_at`
/// descending, then session ID for stability.
fn sort_hits(hits: &mut [SearchHit]) {
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
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
    run_atuin_detailed(program, args, env, cwd, not_found).map_err(|(msg, _)| msg)
}

/// Like [`run_atuin`], also reporting whether `program` wasn't installed
/// (`NotFound`).
fn run_atuin_detailed(
    program: &str,
    args: &[&str],
    env: &[(String, String)],
    cwd: &Path,
    not_found: &str,
) -> Result<String, (String, bool)> {
    let mut child = Command::new(program)
        .args(args)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            let missing = e.kind() == std::io::ErrorKind::NotFound;
            let msg = if missing {
                format!("{program} isn't installed")
            } else {
                format!("{program}: {e}")
            };
            (msg, missing)
        })?;
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
                return Err((format!("{program} took over {}s", TIMEOUT.as_secs()), false));
            }
            Err(e) => return Err((format!("{program}: {e}"), false)),
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    if status.success() {
        return Ok(stdout);
    }
    Err((
        first_error(&stderr).map_or_else(|| not_found.to_string(), |e| format!("{program}: {e}")),
        false,
    ))
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

    #[test]
    fn store_labels_programs_and_profile_matching_stay_separate() {
        assert_eq!(Store::Personal.label(), "personal");
        assert_eq!(Store::Personal.program(), "atuin");
        assert!(Store::Personal.matches_profile("personal"));
        assert!(Store::Personal.matches_profile(""));
        assert!(!Store::Personal.matches_profile("work"));
        assert!(!Store::Personal.matches_profile("notes"));

        assert_eq!(Store::Work.label(), "work");
        assert_eq!(Store::Work.program(), "atuin-work");
        assert!(Store::Work.matches_profile("work"));
        assert!(!Store::Work.matches_profile("personal"));
        assert!(!Store::Work.matches_profile(""));
        assert!(!Store::Work.matches_profile("notes"));
    }

    #[test]
    fn parses_search_terms_with_phrases_and_escapes() {
        assert_eq!(parse_search_terms("   "), Vec::<String>::new());
        assert_eq!(
            parse_search_terms("fed.rs switchyard"),
            vec!["fed.rs", "switchyard"]
        );
        assert_eq!(
            parse_search_terms(r#"fed.rs "last week" 'exact phrase'"#),
            vec!["fed.rs", "last week", "exact phrase"]
        );
        assert_eq!(
            parse_search_terms(r#""say \"hi\"" --flag"#),
            vec![r#"say "hi""#, "--flag"]
        );
        assert_eq!(
            parse_search_terms(r#""unclosed phrase"#),
            vec!["unclosed phrase"]
        );
    }

    #[test]
    fn parses_search_json_for_claude_and_pi_and_validates_match_ranges() {
        let json = r#"[
          {
            "session": {
              "harness": "claude-code",
              "session_id": "fcc83b5d-647a-444e-98e0-5085a2ebf1d0",
              "atuin_id": "01a20464386f71c7b0a925a8713f68d3",
              "cwd": "/Users/john/dev/switchyard/.worktrees/demo",
              "git_branch": "feature/demo",
              "model": "claude-opus-5-5",
              "started_at": "2026-11-22T17:53:26.639+00:00",
              "updated_at": "2026-11-22T17:56:47.607+00:00",
              "message_count": 16,
              "title": "Fix fed.rs",
              "preview": "check fed.rs",
              "last_reply": "Done."
            },
            "score": 8.5,
            "title": {"text": "Fix fed.rs", "matches": [[4, 7], [8, 10]]},
            "preview": {"text": "…crates/fed.rs", "matches": [[10, 13], [14, 16], [1, 2], [99, 120]]},
            "message_index": 28
          },
          {
            "session": {
              "harness": "pi",
              "session_id": "e128b81d-e55c-42a0-a0c6-9c22263b62e3",
              "atuin_id": "01a1c742163c7e6881df20833b101cf6",
              "cwd": "/Users/john/dev/work-app",
              "model": "gpt-6.1-sol",
              "started_at": "2026-11-10T20:59:19.484+00:00",
              "updated_at": "2026-11-10T22:56:03.602+00:00",
              "message_count": 207,
              "preview": "first pi prompt",
              "last_reply": "wrap complete"
            },
            "score": 4.2
          }
        ]"#;
        let hits = parse_search_json(Store::Work, json).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].store, Store::Work);
        assert_eq!(hits[0].harness, "claude");
        assert_eq!(hits[0].session_id, "fcc83b5d-647a-444e-98e0-5085a2ebf1d0");
        assert_eq!(hits[0].title, "Fix fed.rs");
        assert_eq!(hits[0].title_matches, vec![(4, 7), (8, 10)]);
        // [1, 2] lands inside the 3-byte `…` ellipsis and [99, 120] is out of bounds: both dropped.
        assert_eq!(hits[0].snippet_matches, vec![(10, 13), (14, 16)]);
        assert_eq!(hits[0].message_index, Some(28));
        assert!(hits[0].updated_at > hits[0].started_at);

        assert_eq!(hits[1].store, Store::Work);
        assert_eq!(hits[1].harness, "pi");
        assert_eq!(hits[1].title, "");
        assert_eq!(hits[1].git_branch, "");
        assert_eq!(hits[1].first_prompt, "first pi prompt");

        assert_eq!(
            parse_search_json(Store::Personal, "[]").unwrap(),
            Vec::new()
        );
        assert!(parse_search_json(Store::Personal, "not json").is_err());
    }

    #[test]
    fn sort_hits_orders_by_score_then_recency() {
        let mk = |store, id: &str, score, updated_at| SearchHit {
            store,
            harness: "claude".into(),
            session_id: id.into(),
            atuin_id: String::new(),
            cwd: String::new(),
            git_branch: String::new(),
            model: String::new(),
            started_at: 0,
            updated_at,
            message_count: 1,
            title: String::new(),
            title_matches: Vec::new(),
            snippet: String::new(),
            snippet_matches: Vec::new(),
            first_prompt: String::new(),
            last_reply: String::new(),
            message_index: None,
            score,
        };
        let mut hits = vec![
            mk(Store::Personal, "a", 5.0, 100),
            mk(Store::Work, "b", 8.0, 50),
            mk(Store::Personal, "c", 5.0, 200),
        ];
        sort_hits(&mut hits);
        let ids: Vec<&str> = hits.iter().map(|h| h.session_id.as_str()).collect();
        assert_eq!(ids, vec!["b", "c", "a"]);
    }
}
