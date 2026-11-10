//! What the picker shows: Rex tabs grouped by session (from the config's
//! rows) and spawned tasks (from `rex-spawn --tasks` JSON), linked by
//! (session_id, window_id) because the two name hosts differently (`local`
//! against `hostname -s`).

use std::time::{SystemTime, UNIX_EPOCH};

/// A tab's most urgent agent status; Idle means it reports none.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Done,
    Working,
    Error,
    Blocked,
}

impl State {
    pub fn parse(s: &str) -> Self {
        match s {
            "done" => State::Done,
            "working" => State::Working,
            "error" => State::Error,
            "blocked" => State::Blocked,
            _ => State::Idle,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            State::Idle => "",
            State::Done => "done",
            State::Working => "working",
            State::Error => "error",
            State::Blocked => "blocked",
        }
    }
}

/// Sort key for an agent status: what needs John first.
pub fn urgency(state: &str) -> u8 {
    match state {
        "blocked" => 5,
        "error" => 4,
        "done" => 3,
        "working" => 2,
        "idle" => 1,
        _ => 0,
    }
}

pub struct Session {
    /// `-` for the config host, else the hex-encoded host label.
    pub target: String,
    pub host: String,
    pub id: String,
    pub label: String,
    pub tabs: Vec<usize>,
}

pub struct Tab {
    pub session: usize,
    pub window_id: String,
    pub state: State,
    pub name: String,
    pub dir: String,
    /// The open task running in this tab, if any.
    pub task: Option<usize>,
}

/// Rows from the config, tab separated:
///   S  TARGET  HOST  SESSION_ID  SESSION_LABEL
///   T  WINDOW_ID  STATE  NAME  DIR
///   E  HOST  MESSAGE
/// A T row belongs to the S row before it. An E row says a host's tabs are
/// missing or stale (see `failed_hosts`); pickers without them skip it.
pub fn parse_rows(input: &str) -> (Vec<Session>, Vec<Tab>) {
    let mut sessions: Vec<Session> = Vec::new();
    let mut tabs = Vec::new();
    for line in input.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        match f.first() {
            Some(&"S") if f.len() >= 5 => sessions.push(Session {
                target: f[1].into(),
                host: f[2].into(),
                id: f[3].into(),
                label: f[4].into(),
                tabs: Vec::new(),
            }),
            Some(&"T") if f.len() >= 4 && !sessions.is_empty() => {
                let session = sessions.len() - 1;
                sessions[session].tabs.push(tabs.len());
                tabs.push(Tab {
                    session,
                    window_id: f[1].into(),
                    state: State::parse(f[2]),
                    name: f[3].into(),
                    dir: home_relative(f.get(4).copied().unwrap_or("")),
                    task: None,
                });
            }
            _ => {}
        }
    }
    (sessions, tabs)
}

/// The hosts the rows' E rows name, in order: their tabs couldn't be
/// listed, so what's shown of them (if anything) is from earlier.
pub fn failed_hosts(input: &str) -> Vec<String> {
    input
        .lines()
        .filter_map(|line| line.strip_prefix("E\t"))
        .map(|rest| rest.split('\t').next().unwrap_or("").to_string())
        .collect()
}

/// A work item linked to a task (see core's `links`), with a PR's live
/// state and checks when known.
#[derive(Clone, Default)]
pub struct Link {
    pub kind: String,
    pub system: String,
    pub reference: String,
    pub url: String,
    pub state: String,
    pub checks: String,
}

impl Link {
    /// The short form a badge shows: `#42`, `PR #57`, `card 123`, `a1b2c3d`.
    pub fn short(&self) -> String {
        let number = self.reference.rsplit_once('#').map_or(self.reference.as_str(), |(_, n)| n);
        match self.kind.as_str() {
            "pr" => format!("PR #{number}"),
            "issue" if self.system == "github" => format!("#{number}"),
            "card" => format!("card {number}"),
            "commit" => self.reference.chars().take(7).collect(),
            _ => self.reference.clone(),
        }
    }
}

pub struct Task {
    /// The registry's host label (`hostname -s`).
    pub host: String,
    pub session_id: String,
    pub window_id: String,
    pub project: String,
    pub name: String,
    pub profile: String,
    /// Uncommitted files in the worktree, and the first few of their paths.
    pub dirty: u64,
    pub dirty_files: Vec<String>,
    pub state: String,
    pub msg: String,
    /// When the status last changed, or when the task started if unknown;
    /// for an ended task, when it ended.
    pub since: u64,
    /// Why an ended task ended (`archived`, `tab closed`, ...); None while open.
    pub ended: Option<String>,
    /// An ended task whose worktree is still there, for ctrl-x to remove.
    pub left: bool,
    /// Linked work items, PRs first.
    pub links: Vec<Link>,
    /// Whether the branch is merged into the default branch (`merged`,
    /// `unmerged`, `new`), when its worktree was checked.
    pub merged: String,
    /// The listed tab it runs in, if open and listed.
    pub tab: Option<usize>,
}

impl Task {
    pub fn label(&self) -> String {
        format!("{}/{}", self.project, self.name)
    }

    pub fn work(&self) -> bool {
        self.profile == "work"
    }

    /// Ended by an archive (or a discard, its old name) rather than by its
    /// tab closing or worktree going away.
    pub fn archived(&self) -> bool {
        matches!(self.ended.as_deref(), Some("archived" | "force-archived" | "merged" | "discarded" | "force-discarded"))
    }

    /// Safe to archive: open (or ended with its worktree left), nothing
    /// uncommitted, and its work merged (a PR GitHub reports merged, or
    /// the branch in the default branch). Known only after the worktree
    /// check.
    pub fn safe(&self) -> bool {
        (self.ended.is_none() || self.left)
            && self.dirty == 0
            && (self.merged == "merged" || self.links.iter().any(|l| l.kind == "pr" && l.state == "merged"))
    }

    /// The links as search text: their short forms and refs.
    pub fn links_text(&self) -> String {
        self.links.iter().map(|l| format!("{} {}", l.short(), l.reference)).collect::<Vec<_>>().join(" ")
    }
}

/// Every task from `rex-spawn --tasks` output, open and ended.
pub fn parse_tasks(json: &str) -> Result<Vec<Task>, String> {
    let all: Vec<serde_json::Value> = serde_json::from_str(json).map_err(|e| format!("tasks: {e}"))?;
    let s = |t: &serde_json::Value, k: &str| t[k].as_str().unwrap_or("").to_string();
    Ok(all
        .iter()
        .map(|t| Task {
            host: s(t, "host"),
            session_id: s(t, "session_id"),
            window_id: s(t, "window_id"),
            project: s(t, "project"),
            name: s(t, "name"),
            profile: s(t, "profile"),
            dirty: t["dirty_count"].as_u64().unwrap_or(0),
            dirty_files: t["dirty_files"]
                .as_array()
                .map(|a| a.iter().filter_map(|f| f.as_str().map(String::from)).collect())
                .unwrap_or_default(),
            state: s(t, "status"),
            msg: s(t, "status_msg"),
            since: t["ended_at"]
                .as_u64()
                .or(t["status_at"].as_u64().filter(|&at| at > 0))
                .or(t["created_at"].as_u64())
                .unwrap_or(0),
            ended: (!t["ended_at"].is_null()).then(|| match s(t, "ended_reason") {
                r if r.is_empty() => "ended".to_string(),
                r => r,
            }),
            left: t["worktree_left"].as_bool().unwrap_or(false),
            links: parse_links(&t["links"]),
            merged: s(t, "branch_merged"),
            tab: None,
        })
        .collect())
}

/// A task's links, PRs first, then issues, cards and commits.
fn parse_links(v: &serde_json::Value) -> Vec<Link> {
    let s = |l: &serde_json::Value, k: &str| l[k].as_str().unwrap_or("").to_string();
    let mut links: Vec<Link> = v
        .as_array()
        .map(|a| {
            a.iter()
                .map(|l| Link {
                    kind: s(l, "kind"),
                    system: s(l, "system"),
                    reference: s(l, "ref"),
                    url: s(l, "url"),
                    state: s(l, "state"),
                    checks: s(l, "checks"),
                })
                .collect()
        })
        .unwrap_or_default();
    let rank = |k: &str| ["pr", "issue", "card", "commit"].iter().position(|x| *x == k).unwrap_or(4);
    links.sort_by_key(|l| rank(&l.kind));
    links
}

/// Link each open task to its listed tab, both ways.
pub fn link(tabs: &mut [Tab], sessions: &[Session], tasks: &mut [Task]) {
    for t in tabs.iter_mut() {
        t.task = None;
    }
    for (k, task) in tasks.iter_mut().enumerate() {
        task.tab = None;
        if task.ended.is_some() {
            continue;
        }
        let found = tabs
            .iter()
            .position(|t| t.window_id == task.window_id && sessions[t.session].id == task.session_id);
        if let Some(t) = found {
            tabs[t].task = Some(k);
            task.tab = Some(t);
        }
    }
}

/// Remote hosts report absolute paths; show them the way the local ones are.
fn home_relative(dir: &str) -> String {
    for root in ["/home/", "/Users/"] {
        if let Some(rest) = dir.strip_prefix(root) {
            let rest = rest.split_once('/').map(|(_, r)| format!("/{r}")).unwrap_or_default();
            return format!("~{rest}");
        }
    }
    dir.to_string()
}

/// Whether a home-relative folder is under the work profile's project root.
pub fn work_dir(dir: &str) -> bool {
    const ROOT: &str = "~/dev/src/amfaro";
    dir == ROOT || dir.strip_prefix(ROOT).is_some_and(|r| r.starts_with('/'))
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn age(secs: u64) -> String {
    match secs {
        0..60 => "now".into(),
        60..3600 => format!("{}m", secs / 60),
        3600..86400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}
