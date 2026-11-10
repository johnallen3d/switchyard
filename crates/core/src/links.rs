//! Work items linked to a task: the `links` array on each registry entry.
//!
//! Each link is `{kind, system, ref, url, source}`: kind `issue`, `card`,
//! `pr` or `commit`; system `github`, `fizzy`, ...; `ref` such as
//! `owner/repo#57`; source `spawn`, `agent`, `derived` or `manual`. Only
//! `derived` links are rewritten by derivation; the others are left as they
//! are. Live fields (a PR's `state` and `checks`) are added on read and
//! never saved, as a task's status and dirty count are.
//!
//! Where links come from:
//!
//! - spawn: refs in the agent's prompt (`#42`, `fizzy:123`, issue, PR and
//!   card URLs), recorded by `launch`.
//! - agent: the branch's git config, `branch.BRANCH.switchyard-link` (one
//!   ref per value; `switchyard link REF` adds one). It is read by the
//!   per-host worktree check, so links set on a remote host reach the
//!   registry; git config is the source of truth and the registry a cache.
//! - derived: the task's PR, found from its branch with
//!   `gh pr list --head BRANCH` on the config host under the project's
//!   profile (projects whose profile has no GitHub auth there are skipped);
//!   an issue number leading the branch's last part (`feature/42-foo`);
//!   `Fixes #42` (closes, resolves, ...) in the branch's commits or the PR
//!   body. A derived link never replaces an explicit one.
//!
//! A ref's meaning depends on the project's tracker, from the catalog's
//! `tracker` (see `Tracker`): a bare `#42` is a GitHub issue, or a Fizzy
//! card when the tracker is a Fizzy board.
//!
//! PR lookups are cached in `$XDG_STATE_HOME/rex/switchyard-links.json`
//! for `PR_TTL_SECS`, so the inbox's refreshes don't call GitHub every
//! time; `live_tasks` never calls GitHub, only `reconcile_tasks`.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::rex::{home, now};
use crate::sessions::work_env;

/// How long a PR lookup stays fresh.
const PR_TTL_SECS: u64 = 120;
/// How long a profile's GitHub auth check stays fresh.
const AUTH_TTL_SECS: u64 = 600;
/// How long one gh call may take before it is given up on.
const GH_TIMEOUT: Duration = Duration::from_secs(10);

/// `owner/repo` for a GitHub remote URL (https, ssh or scp form), or None.
pub fn github_repo(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = ["https://github.com/", "http://github.com/", "ssh://git@github.com/", "git@github.com:", "github:"]
        .iter()
        .find_map(|p| url.strip_prefix(p))?;
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let mut parts = rest.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

/// A PR as gh reports it, reduced to what the registry and inbox use.
#[derive(Clone, Debug, PartialEq)]
pub struct Pr {
    pub number: u64,
    pub url: String,
    /// `open`, `draft`, `merged` or `closed`.
    pub state: String,
    /// `pass`, `fail`, `pending`, or empty for no checks.
    pub checks: String,
    /// Issues the PR body says it fixes (`#42`, `owner/repo#42`).
    pub fixes: Vec<String>,
}

/// Summarize gh's statusCheckRollup: any failure fails, then anything not
/// finished is pending, else pass; empty for no checks.
fn checks_summary(rollup: &Value) -> String {
    let Some(items) = rollup.as_array().filter(|a| !a.is_empty()) else { return String::new() };
    let mut pending = false;
    for item in items {
        // CheckRun: status + conclusion; StatusContext: state.
        let conclusion = item["conclusion"].as_str().or(item["state"].as_str()).unwrap_or("");
        let status = item["status"].as_str().unwrap_or("COMPLETED");
        match conclusion {
            "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE" => {
                return "fail".into();
            }
            "SUCCESS" | "NEUTRAL" | "SKIPPED" => {}
            _ => pending = true,
        }
        if status != "COMPLETED" {
            pending = true;
        }
    }
    if pending { "pending" } else { "pass" }.into()
}

/// The newest PR in gh's `pr list --json` output, or None for no PR.
pub fn parse_pr_list(json: &str) -> Result<Option<Pr>, String> {
    let list: Value = serde_json::from_str(json).map_err(|e| format!("gh pr list: {e}"))?;
    let Some(pr) = list.as_array().and_then(|a| a.first()) else { return Ok(None) };
    let state = match pr["state"].as_str().unwrap_or("") {
        "MERGED" => "merged",
        "CLOSED" => "closed",
        _ if pr["isDraft"].as_bool() == Some(true) => "draft",
        _ => "open",
    };
    Ok(Some(Pr {
        number: pr["number"].as_u64().ok_or("gh pr list: no number")?,
        url: pr["url"].as_str().unwrap_or("").to_string(),
        state: state.into(),
        checks: checks_summary(&pr["statusCheckRollup"]),
        fixes: fixed_refs(pr["body"].as_str().unwrap_or("")),
    }))
}

fn cache_path() -> PathBuf {
    std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join(".local/state"))
        .join("rex/switchyard-links.json")
}

fn load_cache() -> Value {
    std::fs::read(cache_path())
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

fn save_cache(cache: &Value) {
    let path = cache_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension(format!("json.{}", std::process::id()));
    if std::fs::write(&tmp, cache.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn fresh(entry: &Value, ttl: u64, stamp: u64) -> bool {
    entry["at"].as_u64().is_some_and(|at| stamp.saturating_sub(at) < ttl)
}

/// Run gh under `profile` (work gets amfaro's mise env, personal the login
/// env, as the agents do), giving up after GH_TIMEOUT. None when gh can't
/// run, fails, or takes too long.
fn gh(profile: &str, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("gh");
    cmd.args(args).env("GH_PROMPT_DISABLED", "1").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    if profile == "work" {
        cmd.envs(work_env().ok()?);
    }
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < GH_TIMEOUT => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let out = reader.join().ok()?;
    status.success().then_some(out)
}

/// A PR lookup: which repo and branch, under which profile.
#[derive(Clone)]
pub struct Lookup {
    pub profile: String,
    pub repo: String,
    pub branch: String,
}

pub fn pr_key(repo: &str, branch: &str) -> String {
    format!("{repo}\t{branch}")
}

fn pr_to_json(pr: &Option<Pr>) -> Value {
    match pr {
        Some(p) => json!({"number": p.number, "url": p.url, "state": p.state, "checks": p.checks, "fixes": p.fixes}),
        None => Value::Null,
    }
}

fn pr_from_json(v: &Value) -> Option<Pr> {
    Some(Pr {
        number: v["number"].as_u64()?,
        url: v["url"].as_str().unwrap_or("").to_string(),
        state: v["state"].as_str().unwrap_or("").to_string(),
        checks: v["checks"].as_str().unwrap_or("").to_string(),
        fixes: v["fixes"]
            .as_array()
            .map(|a| a.iter().filter_map(|f| f.as_str().map(String::from)).collect())
            .unwrap_or_default(),
    })
}

/// The PR for each lookup, keyed by `pr_key`: Some(None) when GitHub says
/// the branch has none, absent when it couldn't be asked (no auth for the
/// profile, gh missing or failing). Fresh cache entries are used as they
/// are; the rest are asked of GitHub at once.
pub fn find_prs(lookups: &[Lookup]) -> HashMap<String, Option<Pr>> {
    let stamp = now();
    let mut cache = load_cache();
    let mut found = HashMap::new();
    let mut stale: Vec<Lookup> = Vec::new();
    for l in lookups {
        let key = pr_key(&l.repo, &l.branch);
        if found.contains_key(&key) || stale.iter().any(|s| pr_key(&s.repo, &s.branch) == key) {
            continue;
        }
        let entry = &cache["prs"][&key];
        if fresh(entry, PR_TTL_SECS, stamp) {
            found.insert(key, pr_from_json(&entry["pr"]));
        } else {
            stale.push(l.clone());
        }
    }
    if stale.is_empty() {
        return found;
    }
    // Which profiles may ask GitHub, checked once each.
    let mut profiles: Vec<String> = stale.iter().map(|l| l.profile.clone()).collect();
    profiles.sort();
    profiles.dedup();
    let mut authed: HashMap<String, bool> = HashMap::new();
    let mut checks = Vec::new();
    for p in profiles {
        let entry = &cache["auth"][&p];
        if fresh(entry, AUTH_TTL_SECS, stamp) {
            authed.insert(p, entry["ok"].as_bool().unwrap_or(false));
        } else {
            checks.push((p.clone(), std::thread::spawn(move || {
                gh(&p, &["auth", "status", "--hostname", "github.com"]).is_some()
            })));
        }
    }
    let mut changed = false;
    for (p, handle) in checks {
        let ok = handle.join().unwrap_or(false);
        if !cache["auth"].is_object() {
            cache["auth"] = json!({});
        }
        cache["auth"][&p] = json!({"at": stamp, "ok": ok});
        authed.insert(p, ok);
        changed = true;
    }
    let asks: Vec<_> = stale
        .into_iter()
        .filter(|l| authed.get(&l.profile).copied().unwrap_or(false))
        .map(|l| {
            std::thread::spawn(move || {
                let out = gh(&l.profile, &[
                    "pr", "list", "--repo", &l.repo, "--head", &l.branch, "--state", "all", "--limit", "1",
                    "--json", "number,url,state,isDraft,statusCheckRollup,body",
                ]);
                (l, out.and_then(|o| parse_pr_list(&o).ok()))
            })
        })
        .collect();
    for handle in asks {
        let Ok((l, Some(pr))) = handle.join() else { continue };
        let key = pr_key(&l.repo, &l.branch);
        if !cache["prs"].is_object() {
            cache["prs"] = json!({});
        }
        cache["prs"][&key] = json!({"at": stamp, "pr": pr_to_json(&pr)});
        found.insert(key, pr);
        changed = true;
    }
    if changed {
        // Drop entries no one has asked about in a day.
        if let Some(prs) = cache["prs"].as_object_mut() {
            prs.retain(|_, e| e["at"].as_u64().is_some_and(|at| stamp.saturating_sub(at) < 86400));
        }
        save_cache(&cache);
    }
    found
}

/// Apply a PR lookup's result to a task's saved links: replace its derived
/// PR link (or drop it when GitHub says the branch has none), leaving every
/// other link alone. True when the links changed.
pub fn set_derived_pr(task: &mut Value, repo: &str, pr: &Option<Pr>) -> bool {
    let before = task["links"].clone();
    let mut links: Vec<Value> = task["links"].as_array().cloned().unwrap_or_default();
    links.retain(|l| !(l["kind"] == "pr" && l["source"] == "derived"));
    if let Some(pr) = pr {
        let reference = format!("{repo}#{}", pr.number);
        // An explicit link to the same PR wins over a derived one.
        if !links.iter().any(|l| l["kind"] == "pr" && l["ref"] == reference.as_str()) {
            links.push(json!({
                "kind": "pr", "system": "github", "ref": reference, "url": pr.url, "source": "derived",
            }));
        }
    }
    let after = Value::from(links);
    if after == before || (before.is_null() && after.as_array().is_some_and(Vec::is_empty)) {
        return false;
    }
    task["links"] = after;
    true
}

/// Add a PR's live state and checks to the matching link (not saved).
pub fn add_live(task: &mut Value, repo: &str, pr: &Pr) {
    let reference = format!("{repo}#{}", pr.number);
    if let Some(links) = task["links"].as_array_mut() {
        for l in links.iter_mut().filter(|l| l["kind"] == "pr" && l["ref"] == reference.as_str()) {
            l["state"] = pr.state.clone().into();
            l["checks"] = pr.checks.clone().into();
        }
    }
}

/// What a project's refs mean, from the catalog's `tracker`: a comma or
/// space separated list of `github:OWNER/REPO` and `fizzy:ACCOUNT/BOARD`
/// (the board is optional). The first decides a bare `#42`; the GitHub repo
/// defaults to the one the origin or clone URL names.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tracker {
    pub github: Option<String>,
    pub fizzy_account: Option<String>,
    pub fizzy_board: Option<String>,
    /// A bare number is a Fizzy card rather than a GitHub issue.
    pub cards: bool,
}

impl Tracker {
    pub fn new(tracker: &str, remote: &str) -> Self {
        let mut t = Tracker::default();
        let mut first = true;
        for entry in tracker.split([',', ' ']).filter(|e| !e.is_empty()) {
            if let Some(repo) = entry.strip_prefix("github:") {
                t.github = Some(repo.to_string());
            } else if let Some(rest) = entry.strip_prefix("fizzy:") {
                let (account, board) = rest.split_once('/').unwrap_or((rest, ""));
                t.fizzy_account = Some(account.to_string()).filter(|a| !a.is_empty());
                t.fizzy_board = Some(board.to_string()).filter(|b| !b.is_empty());
                t.cards |= first;
            }
            first = false;
        }
        if t.github.is_none() {
            t.github = github_repo(remote);
        }
        t
    }

    fn card(&self, number: &str) -> Value {
        let url = self.fizzy_account.as_ref().map(|a| format!("https://app.fizzy.do/{a}/cards/{number}"));
        json!({"kind": "card", "system": "fizzy", "ref": number, "url": url.unwrap_or_default()})
    }

    fn github(&self, kind: &str, repo: &str, number: &str) -> Value {
        let path = if kind == "pr" { "pull" } else { "issues" };
        json!({"kind": kind, "system": "github", "ref": format!("{repo}#{number}"),
            "url": format!("https://github.com/{repo}/{path}/{number}")})
    }

    /// A bare number under this tracker: a card, or an issue in its repo.
    fn bare(&self, number: &str) -> Option<Value> {
        if self.cards {
            Some(self.card(number))
        } else {
            Some(self.github("issue", self.github.as_deref()?, number))
        }
    }
}

fn digits(s: &str) -> Option<&str> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then_some(s)
}

/// A ref as a link (without its source), or None when it isn't one or
/// can't be placed: `#42` or `42` (per the tracker), `OWNER/REPO#42`,
/// `fizzy:123`, `pr:57`, or a GitHub issue or PR URL or Fizzy card URL.
pub fn parse_ref(text: &str, t: &Tracker) -> Option<Value> {
    let text = text.trim().trim_end_matches(['.', ',', ')', ';', ':']);
    for scheme in ["https://", "http://"] {
        let Some(rest) = text.strip_prefix(scheme) else { continue };
        let rest = rest.split(['?', '#']).next().unwrap_or("");
        let parts: Vec<&str> = rest.split('/').collect();
        return match parts.as_slice() {
            ["github.com", owner, repo, kind @ ("issues" | "pull"), n, ..] => {
                let kind = if *kind == "pull" { "pr" } else { "issue" };
                Some(t.github(kind, &format!("{owner}/{repo}"), digits(n)?))
            }
            ["app.fizzy.do", account, "cards", n, ..] => {
                let mut link = Tracker { fizzy_account: Some(account.to_string()), ..Tracker::default() }.card(digits(n)?);
                link["url"] = format!("https://app.fizzy.do/{account}/cards/{n}").into();
                Some(link)
            }
            _ => None,
        };
    }
    if let Some(n) = text.strip_prefix("fizzy:") {
        return Some(t.card(digits(n)?));
    }
    if let Some(n) = text.strip_prefix("pr:") {
        return Some(t.github("pr", t.github.as_deref()?, digits(n)?));
    }
    if let Some((repo, n)) = text.split_once('#') {
        if repo.is_empty() {
            return t.bare(digits(n)?);
        }
        let (owner, name) = repo.split_once('/')?;
        let ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
        return (ok(owner) && ok(name)).then(|| t.github("issue", repo, n)).filter(|_| digits(n).is_some());
    }
    t.bare(digits(text)?)
}

/// Whether a ref is one `parse_ref` could place under some tracker, for
/// checking what `switchyard link` records before the tracker is known.
pub fn ref_shape_ok(text: &str) -> bool {
    let any = [
        Tracker { github: Some("o/r".into()), ..Tracker::default() },
        Tracker { cards: true, ..Tracker::default() },
    ];
    any.iter().any(|t| parse_ref(text, t).is_some())
}

/// The refs in free text such as a prompt: issue, PR and card URLs,
/// `fizzy:123`, `OWNER/REPO#42` and `#42`, as links under the tracker.
pub fn refs_in_text(text: &str, t: &Tracker) -> Vec<Value> {
    let mut links: Vec<Value> = Vec::new();
    for word in text.split(|c: char| c.is_whitespace() || "()[]<>\"'`".contains(c)) {
        let plausible = word.starts_with("http") || word.starts_with("fizzy:") || word.contains('#');
        if !plausible {
            continue;
        }
        if let Some(link) = parse_ref(word, t)
            && !links.iter().any(|l| same(l, &link))
        {
            links.push(link);
        }
    }
    links
}

/// Issue refs a text says it fixes: `Fixes #42`, `closes owner/repo#7`,
/// `Resolved: #3` (any case), as `#42` or `owner/repo#7`.
pub fn fixed_refs(text: &str) -> Vec<String> {
    const VERBS: [&str; 9] = ["fix", "fixes", "fixed", "close", "closes", "closed", "resolve", "resolves", "resolved"];
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut refs: Vec<String> = Vec::new();
    for pair in words.windows(2) {
        let verb = pair[0].trim_end_matches(':').to_ascii_lowercase();
        let verb = verb.trim_start_matches(|c: char| !c.is_ascii_alphabetic());
        if !VERBS.contains(&verb) {
            continue;
        }
        let r = pair[1].trim_end_matches(['.', ',', ')', ';']);
        if let Some((repo, n)) = r.split_once('#')
            && digits(n).is_some()
            && (repo.is_empty() || repo.contains('/'))
            && !refs.iter().any(|x| x == r)
        {
            refs.push(r.to_string());
        }
    }
    refs
}

/// The issue number leading a branch's last part: `feature/42-foo` → 42.
pub fn branch_number(branch: &str) -> Option<&str> {
    let last = branch.rsplit('/').next()?;
    let end = last.find(|c: char| !c.is_ascii_digit()).unwrap_or(last.len());
    let (n, rest) = last.split_at(end);
    (!n.is_empty() && (rest.is_empty() || rest.starts_with('-'))).then_some(n)
}

/// Two links name the same work item.
fn same(a: &Value, b: &Value) -> bool {
    a["kind"] == b["kind"] && a["system"] == b["system"] && a["ref"] == b["ref"]
}

/// Add a link with its source. An explicit link (spawn, agent, manual)
/// replaces a derived one for the same item; a derived one is skipped when
/// the item is already linked. True when the links changed.
pub fn add_link(task: &mut Value, mut link: Value, source: &str) -> bool {
    link["source"] = source.into();
    let mut links: Vec<Value> = task["links"].as_array().cloned().unwrap_or_default();
    if let Some(at) = links.iter().position(|l| same(l, &link)) {
        if source == "derived" || links[at]["source"] != "derived" {
            return false;
        }
        links.remove(at);
    }
    links.push(link);
    task["links"] = links.into();
    true
}

/// Make the task's `agent` links the refs in its branch's git config,
/// which is their source of truth. True when the links changed.
pub fn set_agent_links(task: &mut Value, refs: &[String], t: &Tracker) -> bool {
    let before = task["links"].clone();
    let mut links: Vec<Value> = before.as_array().cloned().unwrap_or_default();
    links.retain(|l| l["source"] != "agent");
    task["links"] = links.into();
    for r in refs {
        if let Some(link) = parse_ref(r, t) {
            add_link(task, link, "agent");
        }
    }
    // A task from before links stays without them.
    if before.is_null()
        && task["links"].as_array().is_some_and(Vec::is_empty)
        && let Some(o) = task.as_object_mut()
    {
        o.remove("links");
    }
    task["links"] != before
}

/// Whether GitHub last reported one of the task's PRs as merged (from the
/// lookup cache, however old: a merged PR stays merged).
pub fn pr_merged(task: &Value) -> bool {
    let branch = task["branch"].as_str().unwrap_or("");
    let cache = load_cache();
    task["links"].as_array().into_iter().flatten().filter(|l| l["kind"] == "pr").any(|l| {
        let Some((repo, n)) = l["ref"].as_str().and_then(|r| r.split_once('#')) else { return false };
        let pr = &cache["prs"][&pr_key(repo, branch)]["pr"];
        pr["state"] == "merged" && pr["number"].as_u64().map(|x| x.to_string()).as_deref() == Some(n)
    })
}

/// The link John most likely wants to open: a PR, then an issue, a card,
/// a commit; the first of a kind.
pub fn primary(task: &Value) -> Option<&Value> {
    let links = task["links"].as_array()?;
    ["pr", "issue", "card", "commit"]
        .iter()
        .find_map(|kind| links.iter().find(|l| l["kind"] == *kind && l["url"].as_str().is_some_and(|u| !u.is_empty())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_repo_parses_remote_forms() {
        for url in [
            "https://github.com/johnallen3d/switchyard.git",
            "https://github.com/johnallen3d/switchyard",
            "git@github.com:johnallen3d/switchyard.git",
            "ssh://git@github.com/johnallen3d/switchyard.git",
            "github:johnallen3d/switchyard",
        ] {
            assert_eq!(github_repo(url).as_deref(), Some("johnallen3d/switchyard"), "{url}");
        }
        assert_eq!(github_repo("https://gitlab.com/a/b.git"), None);
        assert_eq!(github_repo("~/dev/src/local"), None);
        assert_eq!(github_repo("https://github.com/a"), None);
    }

    #[test]
    fn pr_list_reduces_state_and_checks() {
        let pr = parse_pr_list(r#"[{"number":57,"url":"u","state":"OPEN","isDraft":false,
            "statusCheckRollup":[{"status":"COMPLETED","conclusion":"SUCCESS"},{"state":"SUCCESS"}]}]"#)
            .unwrap()
            .unwrap();
        assert_eq!(pr, Pr { number: 57, url: "u".into(), state: "open".into(), checks: "pass".into(), fixes: vec![] });
        let pr = parse_pr_list(r#"[{"number":1,"state":"OPEN","isDraft":true,
            "statusCheckRollup":[{"status":"IN_PROGRESS","conclusion":""}]}]"#).unwrap().unwrap();
        assert_eq!((pr.state.as_str(), pr.checks.as_str()), ("draft", "pending"));
        let pr = parse_pr_list(r#"[{"number":1,"state":"MERGED","statusCheckRollup":[
            {"status":"COMPLETED","conclusion":"SUCCESS"},{"status":"COMPLETED","conclusion":"FAILURE"}]}]"#)
            .unwrap()
            .unwrap();
        assert_eq!((pr.state.as_str(), pr.checks.as_str()), ("merged", "fail"));
        assert_eq!(parse_pr_list("[]").unwrap(), None);
    }

    #[test]
    fn derived_pr_replaces_only_derived_links() {
        let mut task = json!({"links": [
            {"kind":"card","system":"fizzy","ref":"123","url":"c","source":"spawn"},
            {"kind":"pr","system":"github","ref":"o/r#1","url":"old","source":"derived"},
        ]});
        let pr = Some(Pr { number: 2, url: "new".into(), state: "open".into(), checks: String::new(), fixes: vec![] });
        assert!(set_derived_pr(&mut task, "o/r", &pr));
        let links = task["links"].as_array().unwrap();
        assert_eq!(links.len(), 2);
        assert_eq!(links[0]["kind"], "card");
        assert_eq!(links[1]["ref"], "o/r#2");
        assert!(!set_derived_pr(&mut task, "o/r", &pr));
        assert!(set_derived_pr(&mut task, "o/r", &None));
        assert_eq!(task["links"].as_array().unwrap().len(), 1);
        // A task from before links stays as it was when there is no PR.
        let mut old = json!({});
        assert!(!set_derived_pr(&mut old, "o/r", &None));
        assert!(old["links"].is_null());
    }

    #[test]
    fn explicit_pr_link_wins_and_gets_live_state() {
        let mut task = json!({"links": [{"kind":"pr","system":"github","ref":"o/r#2","url":"x","source":"manual"}]});
        let pr = Pr { number: 2, url: "x".into(), state: "merged".into(), checks: "pass".into(), fixes: vec![] };
        assert!(!set_derived_pr(&mut task, "o/r", &Some(pr.clone())));
        add_live(&mut task, "o/r", &pr);
        assert_eq!(task["links"][0]["state"], "merged");
        assert_eq!(primary(&task).unwrap()["url"], "x");
    }

    fn github() -> Tracker {
        Tracker::new("", "git@github.com:o/r.git")
    }

    fn fizzy() -> Tracker {
        Tracker::new("fizzy:6284043/board", "https://github.com/o/r")
    }

    #[test]
    fn tracker_decides_bare_refs() {
        assert_eq!(fizzy(), Tracker {
            github: Some("o/r".into()), fizzy_account: Some("6284043".into()), fizzy_board: Some("board".into()), cards: true,
        });
        assert_eq!(Tracker::new("github:a/b, fizzy:1", "").github.as_deref(), Some("a/b"));
        assert!(!Tracker::new("github:a/b, fizzy:1", "").cards);
        let issue = parse_ref("#42", &github()).unwrap();
        assert_eq!((issue["kind"].as_str(), issue["ref"].as_str()), (Some("issue"), Some("o/r#42")));
        assert_eq!(issue["url"], "https://github.com/o/r/issues/42");
        let card = parse_ref("#42", &fizzy()).unwrap();
        assert_eq!((card["kind"].as_str(), card["ref"].as_str()), (Some("card"), Some("42")));
        assert_eq!(card["url"], "https://app.fizzy.do/6284043/cards/42");
        assert!(parse_ref("#42", &Tracker::default()).is_none());
    }

    #[test]
    fn parse_ref_reads_urls_and_prefixes() {
        let t = github();
        let pr = parse_ref("https://github.com/a/b/pull/57/files", &t).unwrap();
        assert_eq!((pr["kind"].as_str(), pr["ref"].as_str()), (Some("pr"), Some("a/b#57")));
        let card = parse_ref("https://app.fizzy.do/99/cards/1041.", &t).unwrap();
        assert_eq!((card["ref"].as_str(), card["url"].as_str()), (Some("1041"), Some("https://app.fizzy.do/99/cards/1041")));
        assert_eq!(parse_ref("fizzy:7", &t).unwrap()["url"], "");
        assert_eq!(parse_ref("a/b#3", &t).unwrap()["ref"], "a/b#3");
        assert_eq!(parse_ref("pr:9", &t).unwrap()["kind"], "pr");
        for bad in ["fizzy:x", "a b#3", "https://example.com/x", "#", "hello"] {
            assert!(parse_ref(bad, &t).is_none(), "{bad}");
        }
        assert!(ref_shape_ok("fizzy:12") && ref_shape_ok("#3") && !ref_shape_ok("nope"));
    }

    #[test]
    fn prompt_refs_branch_numbers_and_fixes() {
        let links = refs_in_text("Work on https://app.fizzy.do/1/cards/5 (see #42), then #42 again; email a#b", &github());
        let refs: Vec<_> = links.iter().map(|l| l["ref"].as_str().unwrap()).collect();
        assert_eq!(refs, ["5", "o/r#42"]);
        assert_eq!(branch_number("feature/42-foo"), Some("42"));
        assert_eq!(branch_number("42"), Some("42"));
        assert_eq!(branch_number("feature/v2-foo"), None);
        assert_eq!(branch_number("feature/42foo"), None);
        assert_eq!(fixed_refs("Fixes #1, closes a/b#2.\nResolved: #3 and fix #1 again; fixing #9"), ["#1", "a/b#2", "#3"]);
    }

    #[test]
    fn explicit_links_replace_derived_and_agent_links_follow_git_config() {
        let t = fizzy();
        let mut task = json!({"links": []});
        assert!(add_link(&mut task, parse_ref("#5", &t).unwrap(), "derived"));
        assert!(!add_link(&mut task, parse_ref("#5", &t).unwrap(), "derived"));
        assert!(add_link(&mut task, parse_ref("fizzy:5", &t).unwrap(), "spawn"));
        assert_eq!(task["links"].as_array().unwrap().len(), 1);
        assert_eq!(task["links"][0]["source"], "spawn");
        assert!(!add_link(&mut task, parse_ref("#5", &t).unwrap(), "derived"));
        assert!(set_agent_links(&mut task, &["fizzy:6".into(), "#7".into(), "junk".into()], &t));
        assert_eq!(task["links"].as_array().unwrap().len(), 3);
        assert!(!set_agent_links(&mut task, &["fizzy:6".into(), "#7".into()], &t));
        assert!(set_agent_links(&mut task, &[], &t));
        assert_eq!(task["links"].as_array().unwrap().len(), 1);
        let mut old = json!({});
        assert!(!set_agent_links(&mut old, &[], &t));
        assert_eq!(old, json!({}));
    }

    #[test]
    fn primary_prefers_pr_then_issue() {
        let task = json!({"links": [
            {"kind":"card","url":"c"}, {"kind":"issue","url":"i"}, {"kind":"pr","url":""},
        ]});
        assert_eq!(primary(&task).unwrap()["url"], "i");
        assert!(primary(&json!({})).is_none());
    }
}
