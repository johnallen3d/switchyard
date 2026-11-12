//! What the picker shows: Rex tabs grouped by session (from the config's
//! rows) and spawned tasks (from `switchyard task list` JSON), linked by
//! (session_id, window_id) because the two name hosts differently (`local`
//! against `hostname -s`).

use std::time::{SystemTime, UNIX_EPOCH};
use switchyard_ui::draw::str_width;

/// A tab's most urgent agent status; Idle means it reports none.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// No agent.
    Idle,
    /// An agent at its prompt.
    Ready,
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
            "idle" => State::Ready,
            _ => State::Idle,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            State::Idle => "",
            State::Ready => "idle",
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

    /// The ref a grouped badge shows after its kind's label: `#57` for
    /// `PR #57`, `1041` for `card 1041`.
    fn badge_ref(&self) -> String {
        let number = self.reference.rsplit_once('#').map_or(self.reference.as_str(), |(_, n)| n);
        match self.kind.as_str() {
            "pr" => format!("#{number}"),
            "issue" if self.system == "github" => format!("#{number}"),
            "card" => number.to_string(),
            "commit" => self.reference.chars().take(7).collect(),
            _ => self.reference.clone(),
        }
    }

    /// A PR's state or checks, as a badge shows them after its ref.
    pub fn badge_state(&self) -> Option<(&'static str, Tone)> {
        match (self.state.as_str(), self.checks.as_str()) {
            ("merged", _) => Some(("✓ merged", Tone::Merged)),
            ("closed", _) => Some(("closed", Tone::Muted)),
            ("draft", _) => Some(("draft", Tone::Muted)),
            (_, "pass") => Some(("✓", Tone::Pass)),
            (_, "fail") => Some(("✗", Tone::Fail)),
            (_, "pending") => Some(("●", Tone::Pending)),
            _ => None,
        }
    }
}

/// How a piece of a badge is coloured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Ref,
    Muted,
    Merged,
    Pass,
    Fail,
    Pending,
}

fn kind_label(kind: &str, plural: bool) -> String {
    let one = match kind {
        "pr" => "PR",
        other => other,
    };
    if plural { format!("{one}s") } else { one.to_string() }
}

/// A row's links as compact badges, grouped by kind with the label once:
/// `PR #57 ✓ merged · cards 1041 1042 1043`. Wider than `budget`, groups
/// show their first few refs and a count (`cards 1041 1042 +3`), the
/// biggest group giving way first; then whole groups go from the end,
/// counted at the end (`PR #57 ✓ merged +4`). The first ref always shows.
pub fn badges(links: &[Link], budget: usize) -> Vec<(String, Tone)> {
    let mut groups: Vec<(&str, Vec<&Link>)> = Vec::new();
    for l in links {
        match groups.iter_mut().find(|g| g.0 == l.kind) {
            Some(g) => g.1.push(l),
            None => groups.push((&l.kind, vec![l])),
        }
    }
    let render = |shown: &[usize]| {
        let mut out: Vec<(String, Tone)> = Vec::new();
        let mut hidden = 0;
        for ((kind, group), &n) in groups.iter().zip(shown) {
            if n == 0 {
                hidden += group.len();
                continue;
            }
            if !out.is_empty() {
                out.push((" · ".into(), Tone::Muted));
            }
            out.push((kind_label(kind, group.len() > 1), Tone::Ref));
            for l in &group[..n] {
                out.push((format!(" {}", l.badge_ref()), Tone::Ref));
                if let Some((text, tone)) = l.badge_state() {
                    out.push((format!(" {text}"), tone));
                }
            }
            if n < group.len() {
                out.push((format!(" +{}", group.len() - n), Tone::Muted));
            }
        }
        if hidden > 0 {
            out.push((format!(" +{hidden}"), Tone::Muted));
        }
        out
    };
    let width = |pieces: &[(String, Tone)]| pieces.iter().map(|(s, _)| str_width(s)).sum::<usize>();
    let mut shown: Vec<usize> = groups.iter().map(|g| g.1.len()).collect();
    loop {
        let pieces = render(&shown);
        if width(&pieces) <= budget {
            return pieces;
        }
        // The group showing the most refs gives one up (the later on a tie).
        if let Some(i) = (0..shown.len()).filter(|&i| shown[i] > 1).max_by_key(|&i| shown[i]) {
            shown[i] -= 1;
        } else if let Some(i) = shown.iter().rposition(|&n| n > 0).filter(|&i| i > 0) {
            shown[i] = 0;
        } else {
            return pieces;
        }
    }
}

/// The links the ctrl-b chooser offers, most likely first: those with a
/// URL, PRs, then cards, issues and commits.
pub fn chooser_links(links: &[Link]) -> Vec<Link> {
    let rank = |k: &str| ["pr", "card", "issue", "commit"].iter().position(|x| *x == k).unwrap_or(4);
    let mut out: Vec<Link> = links.iter().filter(|l| !l.url.is_empty()).cloned().collect();
    out.sort_by_key(|l| rank(&l.kind));
    out
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
    /// Pinned to the top of the inbox and the tab list.
    pub pinned: bool,
    /// Snoozed in the inbox until then (Unix seconds); 0 for not snoozed.
    pub snoozed_until: u64,
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

    /// Open and pinned.
    pub fn is_pinned(&self) -> bool {
        self.ended.is_none() && self.pinned
    }

    /// Open and snoozed at `now`.
    pub fn is_snoozed(&self, now: u64) -> bool {
        self.ended.is_none() && self.snoozed_until > now
    }

    /// The links as search text: their short forms and refs.
    pub fn links_text(&self) -> String {
        self.links.iter().map(|l| format!("{} {}", l.short(), l.reference)).collect::<Vec<_>>().join(" ")
    }
}

/// Every task from `switchyard task list` output, open and ended.
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
            pinned: t["pinned"].as_bool().unwrap_or(false),
            snoozed_until: t["snoozed_until"].as_u64().unwrap_or(0),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn link(kind: &str, reference: &str, state: &str) -> Link {
        let system = if kind == "card" { "fizzy" } else { "github" };
        Link {
            kind: kind.into(),
            system: system.into(),
            reference: reference.into(),
            url: format!("u/{reference}"),
            state: state.into(),
            checks: String::new(),
        }
    }

    fn text(pieces: &[(String, Tone)]) -> String {
        pieces.iter().map(|(s, _)| s.as_str()).collect()
    }

    fn cards(n: usize) -> Vec<Link> {
        (0..n).map(|i| link("card", &(1041 + i).to_string(), "")).collect()
    }

    #[test]
    fn badges_group_links_by_kind_with_the_label_once() {
        let mut links = vec![link("pr", "o/r#57", "merged")];
        links.extend(cards(3));
        links.push(link("issue", "o/r#42", ""));
        assert_eq!(text(&badges(&links, usize::MAX)), "PR #57 ✓ merged · cards 1041 1042 1043 · issue #42");
        assert_eq!(text(&badges(&cards(1), usize::MAX)), "card 1041");
        let prs = [link("pr", "o/r#57", "merged"), link("pr", "o/r#58", "draft")];
        assert_eq!(text(&badges(&prs, usize::MAX)), "PRs #57 ✓ merged #58 draft");
        assert!(badges(&[], 10).is_empty());
    }

    #[test]
    fn badges_truncate_to_the_first_few_and_a_count() {
        let links = cards(5);
        assert_eq!(text(&badges(&links, usize::MAX)), "cards 1041 1042 1043 1044 1045");
        assert_eq!(text(&badges(&links, 20)), "cards 1041 1042 +3");
        // The biggest group gives way first; the PR keeps its state.
        let mut mixed = vec![link("pr", "o/r#57", "merged")];
        mixed.extend(cards(5));
        assert_eq!(text(&badges(&mixed, 36)), "PR #57 ✓ merged · cards 1041 1042 +3");
        // Then whole groups go, counted at the end; the first ref stays.
        assert_eq!(text(&badges(&mixed, 20)), "PR #57 ✓ merged +5");
        assert_eq!(text(&badges(&mixed, 3)), "PR #57 ✓ merged +5");
    }

    #[test]
    fn chooser_lists_prs_then_cards_then_issues_with_urls() {
        let mut links = vec![link("issue", "o/r#42", ""), link("commit", "abcdef0123", "")];
        links.extend(cards(2));
        links.push(link("pr", "o/r#57", ""));
        links.push(Link { url: String::new(), ..link("card", "9", "") });
        let refs: Vec<String> = chooser_links(&links).iter().map(Link::short).collect();
        assert_eq!(refs, ["PR #57", "card 1041", "card 1042", "#42", "abcdef0"]);
    }
}
