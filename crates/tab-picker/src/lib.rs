//! The cmd+k picker for Rex: tabs, agents and the task archive in one float.
//!
//! The config's `tab_picker` action opens this float at once (`--wait`) and
//! a background job on the config host types the data into it as bracketed
//! pastes (see `protocol::DATA`), in any order:
//!
//! - `rows`: every tab across sessions and hosts (format in model.rs),
//!   with a note for each host that couldn't be listed, named in the
//!   header; `error` when no host could.
//! - `live`: `switchyard task list --live`, the task registry with each open
//!   task's live agent status; never replaces `tasks`.
//! - `tasks`: `switchyard task list`, the same after the worktree check.
//! - `tasks-error`: the registry couldn't be read. The tabs stay, and the
//!   header says "tabs only".
//! - `preview`: a capture of the highlighted agent's pane (see preview.rs),
//!   which the config takes from the title's pick.
//!
//! The highlighted row and an armed archive survive each delivery. Tasks
//! link to tabs by (session_id, window_id). Views, cycled with Tab:
//!
//! - All (default): tabs grouped by session, sessions with an agent that
//!   wants John (waiting, failed, done) first. In a session, its agents (tabs
//!   with live agent status or an open task) go under status headings in
//!   urgency order, then its other tabs. Pinned tasks lead the list,
//!   Snoozed ones trail it, and open tasks whose session isn't listed come
//!   just before Snoozed, under No tab. With room, a preview under the list
//!   shows the highlighted agent's pane, live, below its whole status
//!   message.
//! - Usage: read-only accounting (see usage.rs).
//! - Archive (ctrl-a): ended tasks grouped by project (those with a
//!   worktree left behind first), under headings for how they ended
//!   (Left behind, Merged, Archived, Closed), newest first; ctrl-f hides the
//!   archived ones, leaving those whose tab or worktree went without one.
//!
//! ctrl-o cycles a profile filter (all, personal, work): the registry's
//! profile, else work for a tab under ~/dev/src/amfaro. ctrl-x arms an
//! archive of the highlighted task (open, or ended with its worktree left);
//! with uncommitted changes the config's "ask" types another ctrl-x, which
//! moves on to "are you sure". Any other key disarms.
//!
//! ctrl-t pins or unpins the highlighted open task (`pinned` in the
//! registry), which puts it in a Pinned group at the top of All.
//! ctrl-s, in All, opens a prompt for when to wake the task (see
//! snooze.rs); Enter snoozes it into the Snoozed group until then
//! (`snoozed_until`), and Enter on an empty prompt wakes a snoozed one. In
//! the float the config saves both and types Enter back for the picker to
//! show them.
//!
//! The pick goes out in the terminal title as
//! `rex-tab-picker:ACTION:TARGET/SESSION/WINDOW`. ACTION is `go` (a tab),
//! `task` (a row with an open task), `resume` (an ended task), or an armed
//! `archive`, `ask`, `force-archive`, `review` or `pin`. TARGET is
//! `-` for the config host, else the hex-encoded host label; task rows with
//! no listed tab use the registry host. The config's mode binds only Enter
//! and Escape, so Rex's key-mode popup stays small, and in the float this
//! program never talks to Rex. ctrl-r and ctrl-t arm Enter instead, as
//! ctrl-x does: the pick becomes `review` or `pin` until another key
//! cancels it. ctrl-l opens the highlighted task's work item; task rows
//! show their links as badges grouped by kind. With one, ctrl-l arms Enter
//! to open it; with several it opens a chooser over them (PRs, cards, then
//! issues). Either way the pick is `link:URL` (URL hex-encoded), which the
//! config opens on Enter; with the chooser open it types Escape back to
//! close it. A task with several links also adds their count
//! (`.../WINDOW/N`).
//! While the snooze prompt is open it is `snooze:UNTIL:TARGET/SESSION/WINDOW`
//! (UNTIL in Unix seconds, 0 to wake), or `snooze-invalid` until the text
//! reads as a time.
//!
//! Run bare from a shell (`switchyard`), it works without the float: it
//! fetches the data and acts on its picks itself, staying open after each
//! action (see standalone.rs).

mod model;
mod preview;
mod snooze;
mod standalone;
mod usage;

use std::collections::HashMap;
use std::io;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use model::{
    Link, Session, State, Tab, Task, Tone, age, badges, chooser_links, failed_hosts, link, now, parse_rows, parse_tasks,
    urgency, work_dir,
};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use switchyard_ui::Palette;
use switchyard_ui::draw::{self, delete_word, highlight, pill, str_width};
use switchyard_ui::fuzzy::score_fields;
use switchyard_ui::glyphs::{ICON_SEARCH, SPINNER, tick};
use switchyard_ui::protocol::{self, Title, hex};

const ICON_SESSION: &str = "\u{f0bdf}"; // nf-md-console_line
const ICON_INBOX: &str = "\u{f0e27}"; // nf-md-inbox_full
const ICON_PIN: &str = "\u{f0403}"; // nf-md-pin
const ICON_SNOOZE: &str = "\u{f04b2}"; // nf-md-sleep
const ICON_PROJECT: &str = "\u{f024b}"; // nf-md-folder

/// How long a pin or snooze made here holds over deliveries that don't
/// have it yet (the registry is saved, but the data was read before).
const PENDING_TTL: Duration = Duration::from_secs(15);

/// Whether the tabs have arrived.
enum Load {
    Waiting,
    Ready,
    Failed(String),
}

/// Whether the tasks have arrived.
enum Tasks {
    Waiting,
    /// With live status; their worktrees are still being checked.
    Live,
    Ready,
    /// The registry couldn't be read: tabs only.
    Failed,
}

/// The picker's views, along its top bar.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    All,
    Usage,
    Archive,
}

impl View {
    /// `all`, `usage` or `archive`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "all" => Some(View::All),
            "archive" => Some(View::Archive),
            "usage" => Some(View::Usage),
            _ => None,
        }
    }

    fn next(self, back: bool) -> Self {
        match (self, back) {
            (View::All, false) | (View::Archive, true) => View::Usage,
            (View::Usage, false) | (View::All, true) => View::Archive,
            (View::Archive, false) | (View::Usage, true) => View::All,
        }
    }

    const ALL: [View; 3] = [View::All, View::Usage, View::Archive];

    /// Its label in the top bar.
    fn label(self) -> &'static str {
        match self {
            View::All => "All tabs",
            View::Archive => "Archive",
            View::Usage => "Usage",
        }
    }
}

/// Which profile's rows to show; ctrl-o cycles through them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Profile {
    All,
    Personal,
    Work,
}

impl Profile {
    fn next(self) -> Self {
        match self {
            Profile::All => Profile::Personal,
            Profile::Personal => Profile::Work,
            Profile::Work => Profile::All,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Profile::All => "all",
            Profile::Personal => "personal",
            Profile::Work => "work",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Armed {
    No,
    Archive,
    Force,
    /// In the float, ctrl-r, ctrl-l (one work item) and ctrl-t arm these
    /// for Enter, so the config's mode binds only Enter and Escape.
    Review,
    Link,
    Pin,
}

impl Armed {
    fn archives(self) -> bool {
        matches!(self, Armed::Archive | Armed::Force)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Item {
    Session(usize),
    /// An Archive project heading, by the index of its first task.
    Project(usize),
    /// A heading over the whole list: `pinned`, `snoozed`, or `notab` (open
    /// tasks whose session isn't listed).
    Status(&'static str),
    /// A status heading within a session: an agent status (the empty
    /// string means no known status), or `tabs` for its other tabs; within
    /// an Archive project, how its tasks ended (see `ending`).
    Sub(&'static str),
    Tab(usize),
    /// An archive row, or an open task with no listed tab.
    Task(usize),
}

/// How draw_list draws a row.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Draw {
    Header,
    /// A row with a project column.
    Flat,
    /// An agent under its session.
    Nested,
    /// A tab under its session.
    Tab,
}

/// Highlighted character positions: the name, and the folder (grouped
/// rows) or project column (flat rows).
#[derive(Default, Clone)]
struct Hits {
    name: Vec<usize>,
    second: Vec<usize>,
}

/// Session and status rows are headers, not picks.
fn selectable(r: &Item) -> bool {
    matches!(r, Item::Tab(_) | Item::Task(_))
}

/// A row as the flat views draw it.
struct Flat<'a> {
    project: &'a str,
    name: &'a str,
    msg: &'a str,
    host: &'a str,
    state: &'a str,
    ended: Option<&'a str>,
    left: bool,
    work: bool,
    dirty: u64,
    since: u64,
    /// Linked work items, PRs first, and the branch's merge state.
    links: &'a [Link],
    merged: &'a str,
    /// When a snoozed task wakes; 0 when not snoozed.
    wake: u64,
}

type Key = (String, String);

/// The ctrl-l chooser over a task's links (see model's `chooser_links`),
/// open while the task has several.
struct Chooser {
    links: Vec<Link>,
    selected: usize,
}

impl Chooser {
    fn url(&self) -> &str {
        &self.links[self.selected].url
    }
}

/// The ctrl-s prompt: when to wake the highlighted task, typed (see
/// snooze.rs). Empty, it wakes a snoozed task.
struct Snooze {
    /// The task's tab, as (session_id, window_id), which stays highlighted.
    key: Key,
    label: String,
    /// The task's current snooze, 0 for none.
    current: u64,
    text: String,
    /// When the text says, or why it can't; Ok(0) wakes.
    until: Result<u64, String>,
}

impl Snooze {
    fn update(&mut self, now: u64) {
        self.until = match self.text.trim() {
            "" if self.current > now => Ok(0),
            text => snooze::parse(text, now),
        };
    }
}

/// How wide a row's work-item badges may be in a list `width` wide.
fn badge_budget(width: u16) -> usize {
    (width as usize / 3).clamp(16, 48)
}

// ---------------------------------------------------------------- app

struct App {
    p: Palette,
    sessions: Vec<Session>,
    tabs: Vec<Tab>,
    tasks: Vec<Task>,
    /// The tab the picker opened from, as (session_id, window_id); the
    /// selection follows it until John moves.
    current: Key,
    follow: bool,
    view: View,
    profile: Profile,
    /// In the archive, hide the tasks an archive ended (ctrl-f).
    kept: bool,
    query: String,
    matcher: Matcher,
    rows: Vec<Item>,
    hits: HashMap<Item, Hits>,
    selected: Option<usize>,
    offset: usize,
    /// What ctrl-x has armed for the selected task; Enter confirms it.
    armed: Armed,
    /// The open ctrl-l link chooser; Enter opens its pick, Escape closes it.
    chooser: Option<Chooser>,
    /// The open ctrl-s snooze prompt; Enter snoozes, Escape closes it.
    snooze: Option<Snooze>,
    /// Pins and snoozes made here, by (host, window_id): (pinned,
    /// snoozed_until, when), kept over deliveries until they agree.
    pending: HashMap<Key, (bool, u64, Instant)>,
    list_area: Rect,
    title: Title,
    started: Instant,
    now: u64,
    load: Load,
    /// Hosts whose tabs the last rows lack or have from earlier.
    failed: Vec<String>,
    tasks_load: Tasks,
    /// Run from a shell rather than in the config's float: Enter, ctrl-r
    /// and Escape reach the picker, which acts on them itself.
    standalone: bool,
    /// The last standalone action's outcome (and whether it failed), shown
    /// in the footer until the next key.
    note: Option<(String, bool)>,
    /// A standalone picker's data, as the float's pastes would carry it.
    feed: Option<standalone::Feed>,
    usage: usage::Usage,
    /// The last pane capture (see preview.rs), shown while its row is
    /// highlighted.
    preview: Option<preview::Preview>,
    /// The pane a standalone picker last asked its feed to capture.
    asked: Option<preview::Pane>,
    /// The preview fills the picker (ctrl-e), for reading.
    expanded: bool,
    /// How far the preview is scrolled back from its bottom, in lines, and
    /// the pane that is for.
    scroll: (usize, Option<preview::Pane>),
}

impl App {
    fn new(current: Key, view: View) -> Self {
        App {
            p: Palette::load(),
            sessions: Vec::new(),
            tabs: Vec::new(),
            tasks: Vec::new(),
            current,
            follow: true,
            view,
            profile: Profile::All,
            kept: false,
            query: String::new(),
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            rows: Vec::new(),
            hits: HashMap::new(),
            selected: None,
            offset: 0,
            armed: Armed::No,
            chooser: None,
            snooze: None,
            pending: HashMap::new(),
            list_area: Rect::default(),
            title: Title::new("rex-tab-picker"),
            started: Instant::now(),
            now: now(),
            load: Load::Waiting,
            failed: Vec::new(),
            tasks_load: Tasks::Waiting,
            standalone: false,
            note: None,
            feed: None,
            usage: usage::Usage::default(),
            preview: None,
            asked: None,
            scroll: (0, None),
            expanded: false,
        }
    }

    // ------------------------------------------------------------ data

    /// A bracketed paste: tabs, tasks, or a failure to get either.
    fn paste(&mut self, text: &str) {
        let Some((kind, body)) = protocol::data(text) else { return };
        match kind {
            "rows" => {
                let (sessions, tabs) = parse_rows(body);
                let failed = failed_hosts(body);
                self.deliver(|app| {
                    app.sessions = sessions;
                    app.tabs = tabs;
                    app.failed = failed;
                    app.load = Load::Ready;
                });
            }
            "error" => self.load = Load::Failed(body.trim().to_string()),
            // The worktree check can't come before the live tasks it improves on.
            "live" if matches!(self.tasks_load, Tasks::Ready) => {}
            "live" | "tasks" => match parse_tasks(body) {
                Ok(tasks) => {
                    let next = if kind == "live" { Tasks::Live } else { Tasks::Ready };
                    self.deliver(|app| {
                        app.tasks = tasks;
                        app.tasks_load = next;
                    });
                }
                Err(_) => self.tasks_failed(),
            },
            "tasks-error" => self.tasks_failed(),
            "usage" => self.usage.paste(body),
            "usage-error" => self.usage.error = Some(body.trim().to_string()),
            "preview" => self.preview = preview::parse(body).or(self.preview.take()),
            _ => {}
        }
    }

    /// Before any tasks, show tabs only; after, keep them (only their
    /// worktree check failed).
    fn tasks_failed(&mut self) {
        self.tasks_load = match self.tasks_load {
            Tasks::Waiting | Tasks::Failed => Tasks::Failed,
            _ => Tasks::Ready,
        };
    }

    /// Take new data, keeping the highlighted row and what ctrl-x armed for it.
    fn deliver(&mut self, take: impl FnOnce(&mut App)) {
        let keep = self.selected_key();
        let armed = self.armed;
        take(self);
        self.apply_pending();
        link(&mut self.tabs, &self.sessions, &mut self.tasks);
        let want = if self.follow { Some(self.current.clone()) } else { keep.clone() };
        self.refilter_to(want.clone());
        // Following the current tab that isn't shown yet: keep the old row.
        if self.follow && self.selected_key() != want {
            self.select_key(keep.clone());
        }
        self.armed = if self.selected_key() == keep && self.can_arm(armed) { armed } else { Armed::No };
        // The snooze prompt goes with its task.
        if self.snooze.as_ref().is_some_and(|z| self.selected_key().as_ref() != Some(&z.key)) {
            self.snooze = None;
        }
    }

    /// Hold the pins and snoozes made here over data from before them.
    fn apply_pending(&mut self) {
        let tasks = &mut self.tasks;
        self.pending.retain(|(host, window), (pinned, until, at)| {
            if at.elapsed() > PENDING_TTL {
                return false;
            }
            match tasks.iter_mut().find(|t| t.ended.is_none() && &t.host == host && &t.window_id == window) {
                Some(t) if t.pinned == *pinned && t.snoozed_until == *until => false,
                Some(t) => {
                    t.pinned = *pinned;
                    t.snoozed_until = *until;
                    true
                }
                None => true,
            }
        });
    }

    // ------------------------------------------------------------ rows

    fn item(&self, row: usize) -> Option<Item> {
        self.rows.get(row).copied()
    }

    fn selected_item(&self) -> Option<Item> {
        self.selected.and_then(|r| self.item(r))
    }

    /// The (session_id, window_id) a row stands for.
    fn key_of(&self, item: Item) -> Option<Key> {
        match item {
            Item::Session(_) | Item::Project(_) | Item::Status(_) | Item::Sub(_) => None,
            Item::Tab(t) => {
                let tab = &self.tabs[t];
                Some((self.sessions[tab.session].id.clone(), tab.window_id.clone()))
            }
            Item::Task(k) => Some((self.tasks[k].session_id.clone(), self.tasks[k].window_id.clone())),
        }
    }

    fn selected_key(&self) -> Option<Key> {
        self.selected_item().and_then(|i| self.key_of(i))
    }

    fn select_key(&mut self, want: Option<Key>) {
        let at = want.and_then(|w| self.rows.iter().position(|&i| self.key_of(i).as_ref() == Some(&w)));
        self.selected = at.or_else(|| self.rows.iter().position(selectable));
    }

    /// The task a row stands for: a tab's open task, or the row's own.
    fn task_of(&self, item: Item) -> Option<&Task> {
        match item {
            Item::Session(_) | Item::Project(_) | Item::Status(_) | Item::Sub(_) => None,
            Item::Tab(t) => self.tabs[t].task.map(|k| &self.tasks[k]),
            Item::Task(k) => Some(&self.tasks[k]),
        }
    }

    fn selected_task(&self) -> Option<&Task> {
        self.selected_item().and_then(|i| self.task_of(i))
    }

    /// The index of the task a row stands for.
    fn task_index(&self, item: Item) -> Option<usize> {
        match item {
            Item::Session(_) | Item::Project(_) | Item::Status(_) | Item::Sub(_) => None,
            Item::Tab(t) => self.tabs[t].task,
            Item::Task(k) => Some(k),
        }
    }

    /// The highlighted open task, which ctrl-t can pin (not in the archive)
    /// and ctrl-s snooze (in All).
    fn selected_open_task(&self) -> Option<usize> {
        self.selected_item().and_then(|i| self.task_index(i)).filter(|&k| self.tasks[k].ended.is_none())
    }

    fn can_pin(&self) -> bool {
        self.view != View::Archive && self.selected_open_task().is_some()
    }

    fn can_snooze(&self) -> bool {
        self.view == View::All && self.selected_open_task().is_some()
    }

    /// The Agents group a row goes in: `pinned`, `snoozed` (which wins), or
    /// its status (empty when unknown).
    fn agent_group(&self, item: Item) -> &'static str {
        let task = self.task_of(item);
        if task.is_some_and(|t| t.is_snoozed(self.now)) {
            return "snoozed";
        }
        if task.is_some_and(Task::is_pinned) {
            return "pinned";
        }
        match self.flat(item).state {
            "blocked" => "blocked",
            "error" => "error",
            "done" => "done",
            "working" => "working",
            "idle" => "idle",
            _ => "",
        }
    }

    /// Whether ctrl-x can arm an archive for the highlighted row: an open
    /// task, or an ended one that left its worktree behind.
    fn armable(&self) -> bool {
        self.selected_task().is_some_and(|t| t.ended.is_none() || t.left)
    }

    /// Whether `armed` still applies to the highlighted row.
    fn can_arm(&self, armed: Armed) -> bool {
        match armed {
            Armed::No => true,
            Armed::Archive | Armed::Force => self.armable(),
            Armed::Review => self.view != View::Usage && self.selected_open_task().is_some(),
            Armed::Link => self.view != View::Usage && self.selected_link().is_some(),
            Armed::Pin => self.view != View::Usage && self.can_pin(),
        }
    }

    /// The highlighted task's only work item, which ctrl-l opens.
    fn selected_link(&self) -> Option<Link> {
        let mut links = chooser_links(&self.selected_task()?.links);
        if links.len() == 1 { links.pop() } else { None }
    }

    /// The tab whose agent to preview: the highlighted agent's, in All.
    fn preview_want(&self) -> Option<preview::Pane> {
        if self.view != View::All {
            return None;
        }
        let item = self.selected_item()?;
        let Item::Tab(t) = item else { return None };
        if !self.is_agent(item) {
            return None;
        }
        let tab = &self.tabs[t];
        let s = &self.sessions[tab.session];
        Some(preview::Pane { target: s.target.clone(), session: s.id.clone(), window: tab.window_id.clone() })
    }

    fn is_current(&self, t: usize) -> bool {
        let tab = &self.tabs[t];
        self.sessions[tab.session].id == self.current.0 && tab.window_id == self.current.1
    }

    fn profile_passes(&self, work: bool) -> bool {
        match self.profile {
            Profile::All => true,
            Profile::Personal => !work,
            Profile::Work => work,
        }
    }

    /// Whether a row is an agent: a tab with live agent status or an open
    /// task, or an open task with no listed tab. This is the one place that
    /// says what an agent is.
    fn is_agent(&self, item: Item) -> bool {
        match item {
            Item::Tab(t) => self.tabs[t].state != State::Idle || self.tabs[t].task.is_some(),
            Item::Task(k) => self.tasks[k].ended.is_none(),
            _ => false,
        }
    }

    /// Open tasks with no listed tab.
    fn orphans(&self) -> impl Iterator<Item = Item> + '_ {
        (0..self.tasks.len()).filter(|&k| self.tasks[k].ended.is_none() && self.tasks[k].tab.is_none()).map(Item::Task)
    }

    /// The listed session a row belongs to: a tab's, or for a task with
    /// no listed tab, the session it ran in if that is listed.
    fn session_of(&self, item: Item) -> Option<usize> {
        match item {
            Item::Tab(t) => Some(self.tabs[t].session),
            Item::Task(k) => self.sessions.iter().position(|s| s.id == self.tasks[k].session_id),
            _ => None,
        }
    }

    /// Where an agent goes among others in its group: most urgent first
    /// and, within a state that wants John, the one waiting longest first
    /// (unknown times last); the snoozed by when they wake. Working and idle
    /// agents go by name: their status time moves with every status
    /// message, and rows that swap as agents report are hard to pick. Ties
    /// go by name too, so the order never depends on which host answered
    /// first.
    fn agent_key(&self, i: Item) -> (std::cmp::Reverse<u8>, u64, (String, String, String)) {
        let f = self.flat(i);
        let name = (f.project.to_string(), f.name.to_string(), f.host.to_string());
        if self.agent_group(i) == "snoozed" {
            return (std::cmp::Reverse(0), f.wake, name);
        }
        let at = match f.state {
            "working" | "idle" => 0,
            _ if f.since == 0 => u64::MAX,
            _ => f.since,
        };
        (std::cmp::Reverse(urgency(f.state)), at, name)
    }

    /// The Archive view's rows, before search: ended tasks, most recent first.
    fn archive(&self) -> Vec<Item> {
        let mut rows: Vec<usize> = (0..self.tasks.len())
            .filter(|&k| self.tasks[k].ended.is_some())
            .filter(|&k| !(self.kept && self.tasks[k].archived()))
            .collect();
        rows.sort_by_key(|&k| std::cmp::Reverse(self.tasks[k].since));
        rows.into_iter().map(Item::Task).collect()
    }

    /// A tab or task row as the flat views show it.
    fn flat(&self, item: Item) -> Flat<'_> {
        match item {
            Item::Tab(i) => {
                let tab = &self.tabs[i];
                let session = &self.sessions[tab.session];
                match tab.task.map(|k| &self.tasks[k]) {
                    Some(t) => Flat {
                        project: &t.project,
                        name: &t.name,
                        msg: &t.msg,
                        host: &t.host,
                        state: if tab.state != State::Idle { tab.state.name() } else { &t.state },
                        ended: None,
                        left: false,
                        work: t.work(),
                        dirty: t.dirty,
                        since: t.since,
                        links: &t.links,
                        merged: &t.merged,
                        wake: if t.is_snoozed(self.now) { t.snoozed_until } else { 0 },
                    },
                    None => Flat {
                        project: &session.label,
                        name: &tab.name,
                        msg: "",
                        host: &session.host,
                        state: tab.state.name(),
                        ended: None,
                        left: false,
                        work: work_dir(&tab.dir),
                        dirty: 0,
                        since: 0,
                        links: &[],
                        merged: "",
                        wake: 0,
                    },
                }
            }
            // Headers aren't flat rows.
            Item::Session(_) | Item::Project(_) | Item::Status(_) | Item::Sub(_) => Flat {
                project: "",
                name: "",
                msg: "",
                host: "",
                state: "",
                ended: None,
                left: false,
                work: false,
                dirty: 0,
                since: 0,
                links: &[],
                merged: "",
                wake: 0,
            },
            Item::Task(k) => {
                let t = &self.tasks[k];
                Flat {
                    project: &t.project,
                    name: &t.name,
                    msg: &t.msg,
                    host: &t.host,
                    state: &t.state,
                    ended: t.ended.as_deref(),
                    left: t.left,
                    work: t.work(),
                    dirty: t.dirty,
                    since: t.since,
                    links: &t.links,
                    merged: &t.merged,
                    wake: if t.is_snoozed(self.now) { t.snoozed_until } else { 0 },
                }
            }
        }
    }

    fn refilter(&mut self) {
        let keep = self.selected_key();
        self.refilter_to(keep);
    }

    /// Rebuild the rows for the view, profile and search, then highlight
    /// `want` (or, while searching, the best match).
    fn refilter_to(&mut self, want: Option<Key>) {
        self.rows.clear();
        self.hits.clear();
        let pattern = Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart);
        let filtering = !self.query.trim().is_empty();
        let best = match self.view {
            View::All => self.filter_merged(&pattern, filtering),
            View::Archive => self.filter_archive(&pattern, filtering),
            View::Usage => None,
        };
        let want = if filtering { best.and_then(|i| self.key_of(i)) } else { want };
        self.select_key(want);
        self.offset = 0;
    }

    /// Score a row against the search, recording its highlights; None
    /// when it doesn't match. `flat` is for rows drawn with a project
    /// column, whose second highlight is the project rather than the folder.
    fn score_row(&mut self, item: Item, pattern: &Pattern, flat: bool) -> Option<u32> {
        let (name, project, host) = {
            let f = self.flat(item);
            (f.name.to_string(), f.project.to_string(), f.host.to_string())
        };
        let (dir, label) = match item {
            Item::Tab(t) => (self.tabs[t].dir.clone(), self.sessions[self.tabs[t].session].label.clone()),
            _ => (String::new(), String::new()),
        };
        let links = self.task_of(item).map(Task::links_text).unwrap_or_default();
        let (score, f) = score_fields(&mut self.matcher, pattern, &[&name, &dir, &project, &label, &host, &links])?;
        let second = if flat { f[2].clone() } else { f[1].clone() };
        self.hits.insert(item, Hits { name: f[0].clone(), second });
        Some(score)
    }

    /// All: Pinned, then each session (most urgent first) with its agents
    /// under status headings and its other tabs after them, then open tasks
    /// whose session isn't listed, then Snoozed. A session with no agents
    /// shows its tabs without headings. Matches name, folder, project,
    /// session, host and work items. Returns the best match.
    fn filter_merged(&mut self, pattern: &Pattern, filtering: bool) -> Option<Item> {
        let mut best: Option<(u32, Item)> = None;
        let take = |best: &mut Option<(u32, Item)>, score: u32, item: Item| {
            if best.is_none_or(|(b, _)| score > b) {
                *best = Some((score, item));
            }
        };
        let (mut pinned, mut snoozed, mut notab) = (Vec::new(), Vec::new(), Vec::new());
        let mut by_session: Vec<Vec<Item>> = vec![Vec::new(); self.sessions.len()];
        let items: Vec<Item> = (0..self.tabs.len()).map(Item::Tab).chain(self.orphans()).collect();
        for item in items {
            if !self.profile_passes(self.flat(item).work) {
                continue;
            }
            match (self.agent_group(item), self.session_of(item)) {
                ("pinned", _) => pinned.push(item),
                ("snoozed", _) => snoozed.push(item),
                (_, Some(s)) => by_session[s].push(item),
                (_, None) => notab.push(item),
            }
        }
        // A group over the whole list: its rows by urgency, with a heading.
        let global = |app: &mut Self, best: &mut Option<(u32, Item)>, heading: &'static str, mut rows: Vec<Item>| {
            rows.sort_by_cached_key(|&i| app.agent_key(i));
            let mut shown = Vec::new();
            for item in rows {
                if filtering {
                    let Some(score) = app.score_row(item, pattern, true) else { continue };
                    take(best, score, item);
                }
                shown.push(item);
            }
            if !shown.is_empty() {
                app.rows.push(Item::Status(heading));
                app.rows.extend(shown);
            }
        };
        global(self, &mut best, "pinned", pinned);
        // Each session's rows, ranked by the most urgent agent that wants
        // John (waiting, failed, done); the rest keep Rex's order.
        let mut groups: Vec<(u8, usize, Vec<Item>)> = Vec::new();
        for (s, shown) in by_session.into_iter().enumerate() {
            let mut matched = Vec::new();
            for &item in &shown {
                if !filtering {
                    matched.push(item);
                    continue;
                }
                if let Some(score) = self.score_row(item, pattern, false) {
                    take(&mut best, score, item);
                    matched.push(item);
                }
            }
            // A search that matches a session's name or host shows its rows.
            if filtering {
                let (label, host) = (self.sessions[s].label.clone(), self.sessions[s].host.clone());
                if let Some((score, f)) = score_fields(&mut self.matcher, pattern, &[&label, &host]) {
                    self.hits.insert(Item::Session(s), Hits { name: f[0].clone(), second: Vec::new() });
                    if matched.is_empty() {
                        matched = shown.clone();
                    }
                    if let Some(&first) = matched.first() {
                        take(&mut best, score, first);
                    }
                }
            }
            if matched.is_empty() {
                continue;
            }
            let (mut agents, plain): (Vec<Item>, Vec<Item>) = matched.into_iter().partition(|&i| self.is_agent(i));
            agents.sort_by_cached_key(|&i| self.agent_key(i));
            let rank = agents.iter().map(|&i| urgency(self.flat(i).state)).filter(|&u| u >= urgency("done")).max();
            let mut rows = Vec::new();
            let mut group = None;
            for item in agents {
                let state = self.agent_group(item);
                if group != Some(state) {
                    rows.push(Item::Sub(state));
                    group = Some(state);
                }
                rows.push(item);
            }
            if group.is_some() && !plain.is_empty() {
                rows.push(Item::Sub("tabs"));
            }
            rows.extend(plain);
            groups.push((rank.unwrap_or(0), s, rows));
        }
        groups.sort_by_key(|&(rank, s, _)| (std::cmp::Reverse(rank), s));
        for (_, s, rows) in groups {
            self.rows.push(Item::Session(s));
            self.rows.extend(rows);
        }
        global(self, &mut best, "notab", notab);
        global(self, &mut best, "snoozed", snoozed);
        best.map(|(_, i)| i)
    }

    /// Archive: ended tasks by project, under headings for how they
    /// ended (see `ending`), newest first within each. Projects with a
    /// worktree left behind come first, then the most recently ended.
    /// Matches project, name, host and the task's work items (`#42`,
    /// `card 1041`, `PR #57`), so an ended task can be found by its issue or
    /// card and resumed. Returns the best match.
    fn filter_archive(&mut self, pattern: &Pattern, filtering: bool) -> Option<Item> {
        let mut best: Option<(u32, Item)> = None;
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        for item in self.archive() {
            let Item::Task(k) = item else { continue };
            let (project, name, host, work) = {
                let f = self.flat(item);
                (f.project.to_string(), f.name.to_string(), f.host.to_string(), f.work)
            };
            if !self.profile_passes(work) {
                continue;
            }
            if filtering {
                let links = self.task_of(item).map(Task::links_text).unwrap_or_default();
                let Some((score, f)) = score_fields(&mut self.matcher, pattern, &[&project, &name, &host, &links])
                else {
                    continue;
                };
                self.hits.insert(item, Hits { name: f[1].clone(), second: f[0].clone() });
                if best.is_none_or(|(b, _)| score > b) {
                    best = Some((score, item));
                }
            }
            match groups.iter_mut().find(|(p, _)| *p == project) {
                Some((_, ks)) => ks.push(k),
                None => groups.push((project, vec![k])),
            }
        }
        let tasks = &self.tasks;
        for (_, ks) in &mut groups {
            // Stable: newest first within each heading, as archive() sorts.
            ks.sort_by_key(|&k| ENDINGS.iter().position(|&e| e == ending(&tasks[k])));
        }
        groups.sort_by_cached_key(|(project, ks)| {
            let left = ks.iter().any(|&k| tasks[k].left);
            let latest = ks.iter().map(|&k| tasks[k].since).max().unwrap_or(0);
            (!left, std::cmp::Reverse(latest), project.clone())
        });
        for (project, ks) in groups {
            let head = Item::Project(ks[0]);
            if filtering && let Some((_, f)) = score_fields(&mut self.matcher, pattern, &[&project]) {
                self.hits.insert(head, Hits { name: f[0].clone(), second: Vec::new() });
            }
            self.rows.push(head);
            let mut group = None;
            for k in ks {
                let state = ending(&self.tasks[k]);
                if group != Some(state) {
                    self.rows.push(Item::Sub(state));
                    group = Some(state);
                }
                self.rows.push(Item::Task(k));
            }
        }
        best.map(|(_, i)| i)
    }

    // ------------------------------------------------------------ moving

    fn moved(&mut self) {
        self.follow = false;
    }

    fn step(&mut self, delta: isize) {
        let Some(at) = self.selected else { return };
        self.moved();
        self.selected = Some(draw::step(&self.rows, at, delta, selectable));
    }

    /// Move to the first pick of the next group, or back to the first pick
    /// of this group (then the one before).
    fn jump_group(&mut self, forward: bool) {
        let Some(at) = self.selected else { return };
        let first_pick = |i: usize| selectable(&self.rows[i]) && i > 0 && !selectable(&self.rows[i - 1]);
        let found = if forward {
            (at + 1..self.rows.len()).find(|&i| first_pick(i))
        } else {
            (0..at).rev().find(|&i| first_pick(i))
        };
        if let Some(i) = found {
            self.moved();
            self.selected = Some(i);
        }
    }

    fn edge(&mut self, end: bool) {
        self.moved();
        self.selected = if end { self.rows.iter().rposition(selectable) } else { self.rows.iter().position(selectable) };
    }

    fn set_view(&mut self, view: View) {
        self.armed = Armed::No;
        self.chooser = None;
        self.snooze = None;
        self.view = view;
        self.usage.offset = 0;
        self.refilter();
    }

    // ------------------------------------------------------------ the pick

    /// What Enter would do, as the config's title protocol expects. With
    /// the snooze prompt open, `snooze:UNTIL:` and the row's target, or
    /// `snooze-invalid` while the text can't be read.
    fn pick(&self) -> String {
        if let Some(c) = &self.chooser {
            return format!("link:{}", hex(c.url()));
        }
        // ctrl-l on a task with one work item: Enter opens it, as from the
        // chooser.
        if let (Armed::Link, Some(l)) = (self.armed, self.selected_link()) {
            return format!("link:{}", hex(&l.url));
        }
        if let Some(z) = &self.snooze {
            let row = self.row_pick();
            return match (&z.until, row.split_once(':')) {
                (Ok(until), Some((_, target))) => format!("snooze:{until}:{target}"),
                _ => "snooze-invalid".into(),
            };
        }
        self.row_pick()
    }

    /// The highlighted row's pick, whatever is open over it.
    fn row_pick(&self) -> String {
        let Some(item) = self.selected_item() else { return String::new() };
        let task = self.task_of(item);
        let dirty = task.is_some_and(|t| t.dirty > 0);
        let action = match self.armed {
            Armed::Archive if dirty => "ask",
            Armed::Archive => "archive",
            Armed::Force => "force-archive",
            Armed::Review => "review",
            Armed::Pin => "pin",
            Armed::No | Armed::Link => match task {
                Some(t) if t.ended.is_some() => "resume",
                Some(_) => "task",
                None => "go",
            },
        };
        // Several links: ctrl-l opens the chooser rather than the main one.
        let links = task.map_or(0, |t| chooser_links(&t.links).len());
        let more = if links > 1 { format!("/{links}") } else { String::new() };
        match item {
            Item::Session(_) | Item::Project(_) | Item::Status(_) | Item::Sub(_) => String::new(),
            Item::Tab(t) => {
                let tab = &self.tabs[t];
                let s = &self.sessions[tab.session];
                format!("{action}:{}/{}/{}{more}", s.target, s.id, tab.window_id)
            }
            Item::Task(k) => {
                let t = &self.tasks[k];
                format!("{action}:{}/{}/{}{more}", hex(&t.host), t.session_id, t.window_id)
            }
        }
    }

    /// Open the chooser on the selected task's links when it has several;
    /// whether it opened.
    fn open_chooser(&mut self) -> bool {
        let links = self.selected_task().map(|t| chooser_links(&t.links)).unwrap_or_default();
        if links.len() < 2 {
            return false;
        }
        self.armed = Armed::No;
        self.chooser = Some(Chooser { links, selected: 0 });
        true
    }

    /// Pin or unpin the highlighted open task here; whether it could.
    fn toggle_pin(&mut self) -> bool {
        if !self.can_pin() {
            return false;
        }
        let Some(k) = self.selected_open_task() else { return false };
        let (pinned, until) = (!self.tasks[k].pinned, self.tasks[k].snoozed_until);
        self.moved();
        self.set_inbox(k, pinned, until);
        self.refilter();
        true
    }

    /// Record a pin or snooze made here, over the data until it agrees.
    fn set_inbox(&mut self, k: usize, pinned: bool, until: u64) {
        let t = &mut self.tasks[k];
        t.pinned = pinned;
        t.snoozed_until = until;
        self.pending.insert((t.host.clone(), t.window_id.clone()), (pinned, until, Instant::now()));
    }

    /// Open the snooze prompt on the highlighted open task in All.
    fn open_snooze(&mut self) {
        if !self.can_snooze() {
            return;
        }
        let (Some(k), Some(key)) = (self.selected_open_task(), self.selected_key()) else { return };
        let t = &self.tasks[k];
        let current = if t.is_snoozed(self.now) { t.snoozed_until } else { 0 };
        let mut z = Snooze { key, label: t.label(), current, text: String::new(), until: Ok(0) };
        z.update(self.now);
        self.moved();
        self.snooze = Some(z);
    }

    /// The prompt's Enter, once the snooze is saved (in the float, the
    /// config types Enter back after `switchyard task act snooze`): show it
    /// here, moving on from a task just snoozed to the next.
    fn finish_snooze(&mut self) {
        let Some(z) = self.snooze.take() else { return };
        let Ok(until) = z.until else {
            self.snooze = Some(z);
            return;
        };
        let Some(k) = self.selected_open_task() else { return };
        let next = match (until, self.selected) {
            (0, _) | (_, None) => self.selected_key(),
            (_, Some(at)) => {
                let after = (at + 1..self.rows.len()).find(|&i| selectable(&self.rows[i]));
                let before = (0..at).rev().find(|&i| selectable(&self.rows[i]));
                after.or(before).and_then(|i| self.key_of(self.rows[i]))
            }
        };
        self.set_inbox(k, self.tasks[k].pinned, until);
        self.refilter_to(next);
    }

    /// Keys while the snooze prompt is open: type, Enter (see
    /// finish_snooze) or Escape to close it. False to quit.
    fn snooze_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let now = self.now;
        let Some(z) = &mut self.snooze else { return true };
        match k.code {
            KeyCode::Char('c') if ctrl => return false,
            KeyCode::Esc => self.snooze = None,
            KeyCode::Enter => {
                self.finish_snooze();
                return true;
            }
            KeyCode::Char('u') if ctrl => z.text.clear(),
            KeyCode::Char('w') if ctrl => delete_word(&mut z.text),
            KeyCode::Backspace if alt || ctrl => delete_word(&mut z.text),
            KeyCode::Backspace => {
                z.text.pop();
            }
            KeyCode::Char(c) if !ctrl && !alt => z.text.push(c),
            _ => return true,
        }
        if let Some(z) = &mut self.snooze {
            z.update(now);
        }
        true
    }

    /// Keys while the chooser is open: move, or Escape to close it. Enter
    /// never gets here (the config's mode, or standalone_key, opens the
    /// pick). False to quit.
    fn chooser_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some(c) = &mut self.chooser else { return true };
        let last = c.links.len() - 1;
        match k.code {
            KeyCode::Char('c') if ctrl => return false,
            KeyCode::Esc => self.chooser = None,
            KeyCode::Up | KeyCode::Char('k') => c.selected = c.selected.saturating_sub(1),
            KeyCode::Char('p') if ctrl => c.selected = c.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => c.selected = (c.selected + 1).min(last),
            KeyCode::Char('n') if ctrl => c.selected = (c.selected + 1).min(last),
            KeyCode::Home => c.selected = 0,
            KeyCode::End => c.selected = last,
            _ => {}
        }
        true
    }

    fn report(&mut self) -> io::Result<()> {
        let pick = self.pick();
        self.title.report(pick)
    }

    // ------------------------------------------------------------ keys

    fn key(&mut self, k: KeyEvent) -> bool {
        if self.chooser.is_some() {
            return self.chooser_key(k);
        }
        if self.snooze.is_some() {
            return self.snooze_key(k);
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let page = (self.list_area.height as isize - 2).max(1);
        // ctrl-x arms an archive; on a worktree with uncommitted changes, the
        // next ctrl-x (typed by the config when Enter answers `ask`) moves on
        // to "are you sure". Enter and Escape never get here.
        if matches!(k.code, KeyCode::Char('x') if ctrl) {
            let dirty = self.selected_task().is_some_and(|t| t.dirty > 0);
            self.armed = match self.armed {
                _ if !self.armable() => Armed::No,
                Armed::Archive | Armed::Force if dirty => Armed::Force,
                armed if armed.archives() => armed,
                _ => Armed::Archive,
            };
            return true;
        }
        // In the float, Enter reaches here only when the config types it
        // back after saving an armed pin: show it.
        if k.code == KeyCode::Enter {
            if self.armed == Armed::Pin {
                self.toggle_pin();
            }
            self.armed = Armed::No;
            return true;
        }
        self.armed = Armed::No;
        if self.view == View::Usage {
            let delta = match k.code {
                KeyCode::Up | KeyCode::Char('p' | 'k') if k.code == KeyCode::Up || ctrl => Some(-1),
                KeyCode::Down | KeyCode::Char('n' | 'j') if k.code == KeyCode::Down || ctrl => Some(1),
                KeyCode::PageUp => Some(-page),
                KeyCode::PageDown => Some(page),
                KeyCode::Home => { self.usage.offset = 0; return true; }
                KeyCode::End => { self.usage.offset = usize::MAX; return true; }
                _ => None,
            };
            if let Some(delta) = delta {
                self.usage.offset = self.usage.offset.saturating_add_signed(delta);
                return true;
            }
        }
        match k.code {
            KeyCode::Char('c') if ctrl => return false,
            // The rest reach here only in the float (standalone_key acts on
            // them): ctrl-l opens the chooser over several work items, and
            // ctrl-l with one, ctrl-r and ctrl-t arm Enter.
            KeyCode::Char('l') if ctrl => {
                if !self.open_chooser() {
                    self.arm(Armed::Link);
                }
            }
            KeyCode::Char('r') if ctrl => self.arm(Armed::Review),
            KeyCode::Char('t') if ctrl => self.arm(Armed::Pin),
            KeyCode::Char('s') if ctrl => self.open_snooze(),
            KeyCode::Char('e') if ctrl && self.view == View::All => self.expanded = !self.expanded,
            KeyCode::Char('p') | KeyCode::Char('k') if ctrl => self.step(-1),
            KeyCode::Char('n') | KeyCode::Char('j') if ctrl => self.step(1),
            KeyCode::Char('u') if ctrl => {
                self.query.clear();
                self.refilter();
            }
            KeyCode::Char('w') if ctrl => self.delete_word(),
            KeyCode::Char('o') if ctrl => {
                self.profile = self.profile.next();
                self.refilter();
            }
            KeyCode::Char('a') if ctrl => {
                self.set_view(if self.view == View::Archive { View::All } else { View::Archive })
            }
            KeyCode::Char('f') if ctrl && self.view == View::Archive => {
                self.kept = !self.kept;
                self.refilter();
            }
            KeyCode::Backspace if alt || ctrl => self.delete_word(),
            KeyCode::Backspace => {
                self.query.pop();
                self.refilter();
            }
            // With nothing typed, h and l move along the top bar.
            KeyCode::Char('h') if !ctrl && !alt && self.query.is_empty() => self.set_view(self.view.next(true)),
            KeyCode::Char('l') if !ctrl && !alt && self.query.is_empty() => self.set_view(self.view.next(false)),
            KeyCode::Char(c) if !ctrl && !alt => {
                self.query.push(c);
                self.moved();
                self.refilter();
            }
            // Shift scrolls the preview back through the transcript, as the
            // arrows do with it expanded.
            KeyCode::Up if shift || self.expanded => self.scroll_preview(3),
            KeyCode::Down if shift || self.expanded => self.scroll_preview(-3),
            KeyCode::PageUp if shift || self.expanded => self.scroll_preview(page),
            KeyCode::PageDown if shift || self.expanded => self.scroll_preview(-page),
            KeyCode::Home if self.expanded => self.scroll_preview(isize::MAX),
            KeyCode::End if self.expanded => self.scroll.0 = 0,
            KeyCode::Up if alt => self.jump_group(false),
            KeyCode::Down if alt => self.jump_group(true),
            KeyCode::Up => self.step(-1),
            KeyCode::Down => self.step(1),
            KeyCode::PageUp => self.step(-page),
            KeyCode::PageDown => self.step(page),
            KeyCode::Home => self.edge(false),
            KeyCode::End => self.edge(true),
            _ => {}
        }
        true
    }

    /// Scroll the preview back (positive) or forward; draw_preview keeps it
    /// within the capture.
    fn scroll_preview(&mut self, delta: isize) {
        self.scroll.0 = self.scroll.0.saturating_add_signed(delta);
    }

    fn arm(&mut self, armed: Armed) {
        self.armed = if self.can_arm(armed) { armed } else { Armed::No };
    }

    fn delete_word(&mut self) {
        delete_word(&mut self.query);
        self.refilter();
    }

    // ------------------------------------------------------------ drawing

    fn draw(&mut self, f: &mut Frame) {
        let labels = View::ALL.map(View::label);
        let active = View::ALL.iter().position(|&v| v == self.view).unwrap_or(0);
        let mut top = draw::tabs(&self.p, &labels, active);
        top.extend(self.header_chips());
        let inner = draw::frame_spans(f, &self.p, top, "⌘K", Margin::new(2, 1));
        let [search, rule, _, body, _, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(inner);
        self.draw_search(f, search);
        draw::rule(f, &self.p, rule);
        if self.view == View::Usage {
            self.list_area = body;
            let profile = match self.profile { Profile::All => None, p => Some(p.name()) };
            self.usage.draw(f, body, &self.p, self.now, &self.query, profile);
        } else if let Some((list, pane)) = self.preview_split(body) {
            if self.expanded {
                // Page keys page through the preview.
                self.list_area = pane;
            } else {
                self.draw_list(f, list);
            }
            self.draw_preview(f, pane);
        } else {
            self.draw_list(f, body);
        }
        self.draw_chooser(f, body);
        self.draw_snooze(f, body);
        self.draw_footer(f, footer);
    }

    /// All's list and the preview under it, when there is room for both:
    /// a third of the height (8 to 14 lines) for the preview, the rest
    /// for the list; expanded (ctrl-e), the preview alone. The
    /// preview stays while John moves, whatever the row, so the list
    /// doesn't change height under him.
    fn preview_split(&self, body: Rect) -> Option<(Rect, Rect)> {
        if self.view != View::All {
            return None;
        }
        if self.expanded {
            return Some((Rect { height: 0, ..body }, body));
        }
        if body.height < 20 {
            return None;
        }
        let pane = (body.height / 3).clamp(8, 14);
        let [list, _, preview] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1), Constraint::Length(pane)]).areas(body);
        Some((list, preview))
    }

    /// The highlighted agent's transcript (see preview.rs) under a rule
    /// naming it, scrolled to its end until John scrolls back.
    fn draw_preview(&mut self, f: &mut Frame, area: Rect) {
        let p = self.p;
        let p = &p;
        let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
        let want = self.preview_want();
        let flat = want.as_ref().and(self.selected_item()).map(|i| self.flat(i));
        let mut spans = vec![Span::styled("── ", Style::new().fg(p.overlay))];
        match &flat {
            Some(t) => {
                if !t.project.is_empty() && t.project != t.name {
                    spans.push(Span::styled(format!("{} ", t.project), Style::new().fg(p.subtle)));
                }
                spans.push(Span::styled(format!("{} ", t.name), Style::new().fg(p.text).add_modifier(Modifier::BOLD)));
            }
            None => spans.push(Span::styled("Preview ", Style::new().fg(p.muted))),
        }
        let used: usize = spans.iter().map(|s| str_width(&s.content)).sum();
        spans.push(Span::styled("─".repeat((head.width as usize).saturating_sub(used)), Style::new().fg(p.overlay)));
        f.render_widget(Line::from(spans), head);
        let muted = |text: &str| vec![Line::styled(text.to_string(), Style::new().fg(p.muted))];
        let Some(pane) = want else {
            return draw::centered(f, body, muted("Highlight an agent to see what it has been saying"));
        };
        let screen = body;
        // A new agent starts at the end of its transcript.
        if self.scroll.1.as_ref() != Some(&pane) {
            self.scroll = (0, Some(pane.clone()));
        }
        // Leave a column for the scrollbar.
        let text = Rect { width: screen.width.saturating_sub(2), ..screen };
        let Some(pv) = self.preview.as_mut().filter(|pv| pv.pane == pane) else {
            return draw::centered(f, screen, muted("Loading the transcript…"));
        };
        match pv.lines(text.width, p) {
            Err(e) => draw::centered(f, screen, muted(e)),
            Ok(lines) => {
                let h = text.height as usize;
                let top = lines.len().saturating_sub(h);
                self.scroll.0 = self.scroll.0.min(top);
                let skip = top - self.scroll.0;
                f.render_widget(Paragraph::new(lines[skip..lines.len().min(skip + h)].to_vec()), text);
                if lines.len() > h {
                    draw::scrollbar(f, p, screen, skip, lines.len());
                }
            }
        }
    }

    /// The ctrl-s prompt, when open: a small box over the list with the
    /// typed text and when it means, or why it can't be read.
    fn draw_snooze(&self, f: &mut Frame, area: Rect) {
        let Some(z) = &self.snooze else { return };
        let p = &self.p;
        let w = 60.min(area.width);
        let h = 4.min(area.height);
        let r = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
        f.render_widget(Clear, r);
        let title = format!(" Snooze {} ", z.label);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(p.iris))
            .style(Style::new().bg(p.base))
            .title(Span::styled(title, Style::new().fg(p.text)));
        let inner = block.inner(r);
        f.render_widget(block, r);
        if inner.height == 0 {
            return;
        }
        let input = Line::from(vec![
            Span::styled(" until ", Style::new().fg(p.muted)),
            Span::styled(z.text.clone(), Style::new().fg(p.text).add_modifier(Modifier::BOLD)),
            Span::styled("▏", Style::new().fg(p.iris)),
        ]);
        f.render_widget(input, Rect { height: 1, ..inner });
        if inner.height < 2 {
            return;
        }
        let row = Rect { y: inner.y + 1, height: 1, ..inner };
        let line = match (&z.until, z.text.trim().is_empty()) {
            (Ok(0), _) => Line::from(vec![
                Span::styled(format!(" Snoozed until {} · ", snooze::describe(z.current, self.now)), Style::new().fg(p.muted)),
                Span::styled("⏎", Style::new().fg(p.text).add_modifier(Modifier::BOLD)),
                Span::styled(" wakes it", Style::new().fg(p.muted)),
            ]),
            (Ok(until), _) => Line::styled(format!(" {}", snooze::describe(*until, self.now)), Style::new().fg(p.pine)),
            (Err(_), true) => Line::styled(format!(" Try {}", snooze::EXAMPLES), Style::new().fg(p.muted)),
            (Err(e), false) => Line::styled(format!(" {e}"), Style::new().fg(p.love)),
        };
        f.render_widget(line, row);
    }

    /// The ctrl-l chooser, when open: a small box over the list, one link a
    /// line with its state and URL.
    fn draw_chooser(&self, f: &mut Frame, area: Rect) {
        let Some(c) = &self.chooser else { return };
        let p = &self.p;
        let want = c
            .links
            .iter()
            .map(|l| str_width(&l.short()) + l.badge_state().map_or(0, |(s, _)| str_width(s) + 1) + str_width(&l.url) + 6)
            .max()
            .unwrap_or(0);
        let w = (want as u16 + 2).min(area.width);
        let h = (c.links.len() as u16 + 2).min(area.height);
        let r = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
        f.render_widget(Clear, r);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(p.iris))
            .style(Style::new().bg(p.base))
            .title(Span::styled(" Open link ", Style::new().fg(p.text)));
        let inner = block.inner(r);
        f.render_widget(block, r);
        for (i, l) in c.links.iter().enumerate().take(inner.height as usize) {
            let (short, state, url) = (l.short(), l.badge_state(), &l.url);
            let selected = i == c.selected;
            let bg = if selected { p.overlay } else { p.base };
            let row = Rect { y: inner.y + i as u16, height: 1, ..inner };
            f.buffer_mut().set_style(row, Style::new().bg(bg));
            let edge = if selected { Span::styled("▎", Style::new().fg(p.iris).bg(bg)) } else { Span::styled(" ", Style::new().bg(bg)) };
            let mut style = Style::new().fg(p.text).bg(bg);
            if selected {
                style = style.add_modifier(Modifier::BOLD);
            }
            let mut spans = vec![edge, Span::styled(short, style)];
            if let Some((text, tone)) = state {
                let fg = match tone {
                    Tone::Merged => p.iris,
                    Tone::Pass => p.pine,
                    Tone::Fail => p.love,
                    Tone::Pending => p.gold,
                    Tone::Ref | Tone::Muted => p.muted,
                };
                spans.push(Span::styled(format!(" {text}"), Style::new().fg(fg).bg(bg)));
            }
            spans.push(Span::styled(format!("  {url}"), Style::new().fg(p.muted).bg(bg)));
            f.render_widget(Line::from(spans), row);
        }
    }

    /// The active filters, for the frame's title.
    fn header_chips(&self) -> Vec<Span<'static>> {
        let p = &self.p;
        let mut spans = Vec::new();
        let mut chip = |text: &str, fg: Color| {
            spans.extend(pill(vec![Span::styled(text.to_string(), Style::new().fg(fg))], p.surface, p.base));
            spans.push(Span::raw(" "));
        };
        match self.profile {
            Profile::All => {}
            Profile::Work => chip("work", p.gold),
            Profile::Personal => chip("personal", p.subtle),
        }
        if self.view == View::Archive && self.kept {
            chip("not archived", p.foam);
        }
        if self.view == View::Usage { return spans; }
        if let Tasks::Failed = self.tasks_load {
            chip("tabs only", p.muted);
        }
        match self.failed.as_slice() {
            [] => {}
            [host] => chip(&format!("{host} failed"), p.love),
            hosts => chip(&format!("{} hosts failed", hosts.len()), p.love),
        }
        spans
    }

    fn draw_search(&self, f: &mut Frame, area: Rect) {
        let p = &self.p;
        let typing = !self.query.is_empty();
        let placeholder = match self.view {
            View::All => "Search tabs, agents, and work items",
            View::Archive => "Search ended tasks",
            View::Usage => "Search accounts, harnesses, models, and hosts",
        };
        draw::search(f, p, area, &self.query, placeholder, typing, true);
        if self.view == View::Usage { return; }
        let shown = self.rows.iter().filter(|r| selectable(r)).count();
        let mut right = Vec::new();
        let spin = SPINNER[tick(self.started) % SPINNER.len()];
        match self.tasks_load {
            Tasks::Waiting => right.push(Span::styled(format!("{spin} loading tasks  "), Style::new().fg(p.muted))),
            Tasks::Live => right.push(Span::styled(format!("{spin} checking worktrees  "), Style::new().fg(p.muted))),
            _ => {}
        }
        if self.view == View::All {
            // Counts by what needs attention, the snoozed aside.
            let count = |st: &str| {
                self.rows
                    .iter()
                    .filter(|&&i| selectable(&i) && self.is_agent(i) && self.flat(i).state == st)
                    .filter(|&&i| self.agent_group(i) != "snoozed")
                    .count()
            };
            for (n, label, color) in [
                (count("blocked"), "waiting", p.gold),
                (count("error"), "failed", p.love),
                (count("done"), "done", p.pine),
                (count("working"), "working", p.foam),
            ] {
                if n > 0 {
                    right.push(Span::styled(format!("{n} {label}  "), Style::new().fg(color)));
                }
            }
        }
        let (total, kind) = match self.view {
            View::Archive => (self.tasks.iter().filter(|t| t.ended.is_some()).count(), "ended"),
            _ => (self.tabs.len() + self.orphans().count(), "tabs"),
        };
        if typing || shown != total {
            right.push(Span::styled(shown.to_string(), Style::new().fg(p.text)));
            right.push(Span::styled(format!(" of {total} {kind} "), Style::new().fg(p.muted)));
        } else {
            right.push(Span::styled(format!("{total} {kind} "), Style::new().fg(p.muted)));
        }
        f.render_widget(Line::from(right).right_aligned(), area);
    }

    fn draw_list(&mut self, f: &mut Frame, area: Rect) {
        self.list_area = area;
        let h = area.height as usize;
        if self.rows.is_empty() {
            self.draw_empty(f, area);
            return;
        }
        if let Some(sel) = self.selected {
            // Keep the group header in view when scrolling back to its
            // first pick.
            let header_above = sel > 0 && !selectable(&self.rows[sel - 1]);
            self.offset = draw::keep_in_view(self.offset, sel, h, header_above);
        }
        let tick = tick(self.started);
        let overflow = self.rows.len() > h && h > 0;
        // Leave a column for the scrollbar only when there is one.
        let list = Rect { width: area.width.saturating_sub(if overflow { 2 } else { 0 }), ..area };
        // How each row is drawn: under a session, agents as flat rows on
        // the tree (no project column) and other tabs as tab rows; in the
        // groups over the whole list and the archive, flat rows.
        let mut style = Vec::with_capacity(self.rows.len());
        let mut nested = false;
        for &row in &self.rows {
            style.push(match row {
                Item::Session(_) | Item::Project(_) | Item::Sub(_) => {
                    nested = true;
                    Draw::Header
                }
                Item::Status(_) => {
                    nested = false;
                    Draw::Header
                }
                _ if !nested => Draw::Flat,
                // Agents, and ended tasks under their Archive project.
                Item::Task(_) => Draw::Nested,
                _ if self.is_agent(row) => Draw::Nested,
                _ => Draw::Tab,
            });
        }
        // Flat rows line up their project and name columns: size them for
        // the widest values, within what the busiest right side leaves.
        let flat = || {
            self.rows.iter().zip(&style).filter(|(_, d)| matches!(d, Draw::Flat | Draw::Nested)).map(|(&i, d)| (self.flat(i), *d))
        };
        let right_w = flat().map(|(t, _)| self.right_spans(&t, self.p.base, "", list.width).1).max().unwrap_or(0);
        let room = (list.width as usize).saturating_sub(3 + right_w + 1);
        let proj_w =
            flat().filter(|(_, d)| *d == Draw::Flat).map(|(t, _)| str_width(t.project)).max().unwrap_or(0).min(room / 4);
        let name_w =
            flat().map(|(t, _)| str_width(t.name)).max().unwrap_or(0).min(room.saturating_sub(proj_w + 2) * 2 / 3);
        let cols = (proj_w, name_w);
        for (i, row) in self.rows.iter().enumerate().skip(self.offset).take(h) {
            let r = Rect { y: area.y + (i - self.offset) as u16, height: 1, ..list };
            let selected = self.selected == Some(i);
            let last = !self.rows.get(i + 1).is_some_and(selectable);
            match (row, style[i]) {
                (Item::Status(state), _) => self.draw_status_header(f, r, state),
                (Item::Sub(state), _) => self.draw_sub_header(f, r, state),
                (_, Draw::Flat) => self.draw_flat_row(f, r, *row, selected, tick, cols, None),
                (_, Draw::Nested) => {
                    let guide = if last { "╰─ " } else { "├─ " };
                    self.draw_flat_row(f, r, *row, selected, tick, cols, Some(guide))
                }
                _ => self.draw_grouped_row(f, r, row, selected, last, tick),
            }
        }
        if overflow {
            draw::scrollbar(f, &self.p, area, self.offset, self.rows.len());
        }
    }

    fn draw_status_header(&self, f: &mut Frame, area: Rect, state: &str) {
        let p = &self.p;
        let (glyph, label, color) = match state {
            "blocked" => ("●", "Waiting", p.gold),
            "error" => ("✗", "Failed", p.love),
            "done" => ("✓", "Done", p.pine),
            "working" => ("●", "Working", p.foam),
            "idle" => ("·", "Idle", p.muted),
            "pinned" => (ICON_PIN, "Pinned", p.iris),
            "snoozed" => (ICON_SNOOZE, "Snoozed", p.muted),
            "notab" => (ICON_SESSION, "No tab", p.muted),
            _ => ("?", "No status", p.muted),
        };
        let heading = format!(" {glyph} {label} ");
        let rule = "─".repeat((area.width as usize).saturating_sub(str_width(&heading)));
        f.render_widget(
            Line::from(vec![
                Span::styled(heading, Style::new().fg(color).add_modifier(Modifier::BOLD)),
                Span::styled(rule, Style::new().fg(p.hl_med)),
            ]),
            area,
        );
    }

    /// A status heading within a session, indented under it.
    fn draw_sub_header(&self, f: &mut Frame, area: Rect, state: &str) {
        let p = &self.p;
        let (glyph, label, color) = match state {
            "blocked" => ("●", "Waiting", p.gold),
            "error" => ("✗", "Failed", p.love),
            "done" => ("✓", "Done", p.pine),
            "working" => ("●", "Working", p.foam),
            "idle" => ("·", "Idle", p.muted),
            "tabs" => ("·", "Tabs", p.subtle),
            "left" => ("▢", "Left behind", p.gold),
            "merged" => ("▢", "Merged", p.iris),
            "archived" => ("▢", "Archived", p.iris),
            "closed" => ("▢", "Closed", p.muted),
            _ => ("?", "No status", p.muted),
        };
        f.render_widget(
            Line::from(vec![
                Span::raw("   "),
                Span::styled(format!("{glyph} {label}"), Style::new().fg(color).add_modifier(Modifier::BOLD)),
            ]),
            area,
        );
    }

    fn draw_empty(&self, f: &mut Frame, area: Rect) {
        let p = &self.p;
        let spin = || Line::styled(SPINNER[tick(self.started) % SPINNER.len()], Style::new().fg(p.foam));
        let icon = |glyph: &'static str, color: Color| Line::styled(glyph, Style::new().fg(color));
        let text = |s: String| Line::styled(s, Style::new().fg(p.subtle));
        let muted = |s: String| Line::styled(s, Style::new().fg(p.muted));
        let filtered = match self.profile {
            Profile::All => None,
            profile => Some(profile.name()),
        };
        let lines = if !self.query.trim().is_empty() {
            vec![
                icon(ICON_SEARCH, p.hl_high),
                Line::from(""),
                Line::from(vec![
                    Span::styled("Nothing matches ", Style::new().fg(p.subtle)),
                    Span::styled(format!("“{}”", self.query), Style::new().fg(p.text)),
                ]),
                draw::key_hint(p, "⌃U", " clears the search"),
            ]
        } else {
            match self.view {
                View::Usage => Vec::new(),
                View::All => match &self.load {
                    Load::Waiting => vec![spin(), Line::from(""), text("Loading tabs…".into())],
                    Load::Failed(err) => vec![
                        icon(ICON_SESSION, p.love),
                        Line::from(""),
                        text("Couldn't list tabs".into()),
                        muted(err.clone()),
                    ],
                    Load::Ready if self.tabs.is_empty() => vec![
                        icon(ICON_SESSION, p.hl_high),
                        Line::from(""),
                        text("No tabs to show".into()),
                        muted("Rex didn't report any sessions".into()),
                    ],
                    Load::Ready => vec![
                        icon(ICON_SESSION, p.hl_high),
                        Line::from(""),
                        text(format!("No {} tabs", filtered.unwrap_or("matching"))),
                        draw::key_hint(p, "⌃O", " changes profile"),
                    ],
                },
                View::Archive => match self.tasks_load {
                    Tasks::Waiting => vec![spin(), Line::from(""), text("Loading tasks…".into())],
                    Tasks::Failed => vec![
                        icon(ICON_INBOX, p.hl_high),
                        Line::from(""),
                        text("No task registry".into()),
                        muted("Showing tabs only".into()),
                    ],
                    _ if self.kept => vec![
                        icon(ICON_INBOX, p.hl_high),
                        Line::from(""),
                        text(match filtered {
                            Some(profile) => format!("No {profile} tasks ended without an archive"),
                            None => "No tasks ended without an archive".into(),
                        }),
                        draw::key_hint(p, "⌃F", " shows archived"),
                    ],
                    _ => vec![
                        icon(ICON_INBOX, p.hl_high),
                        Line::from(""),
                        text(match filtered {
                            Some(profile) => format!("No ended {profile} tasks"),
                            None => "No ended tasks".into(),
                        }),
                        draw::key_hint(p, "h", " shows all tabs"),
                    ],
                },
            }
        };
        draw::centered(f, area, lines);
    }

    /// The row's background and left edge: iris when highlighted, love when
    /// an archive is armed for it.
    fn row_style(&self, selected: bool) -> (Color, Span<'static>) {
        let p = &self.p;
        let doomed = selected && self.armed.archives();
        let bg = if doomed { p.hl_med } else if selected { p.overlay } else { p.base };
        let edge = match (doomed, selected) {
            (true, _) => Span::styled("▎", Style::new().fg(p.love).bg(bg)),
            (false, true) => Span::styled("▎", Style::new().fg(p.iris).bg(bg)),
            _ => Span::styled(" ", Style::new().bg(bg)),
        };
        (bg, edge)
    }

    fn draw_grouped_row(&self, f: &mut Frame, r: Rect, item: &Item, selected: bool, last: bool, tick: usize) {
        let p = &self.p;
        let width = r.width as usize;
        let (bg, edge) = self.row_style(selected);
        let chip_bg = if selected { p.hl_med } else { p.overlay };
        f.buffer_mut().set_style(r, Style::new().bg(bg));
        let hits = self.hits.get(item).cloned().unwrap_or_default();
        let hl = Style::new().fg(p.rose).bg(bg).add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        match item {
            Item::Session(s) => {
                let s = &self.sessions[*s];
                let count = |st: State| s.tabs.iter().filter(|&&t| self.tabs[t].state == st).count();
                let mut right: Vec<Span> = Vec::new();
                let mut chip = |text: String, fg: Color| {
                    right.extend(pill(vec![Span::styled(text, Style::new().fg(fg))], chip_bg, bg));
                    right.push(Span::styled(" ", Style::new().bg(bg)));
                };
                let (blocked, errors) = (count(State::Blocked), count(State::Error));
                if blocked > 0 {
                    chip(format!("● {blocked} waiting"), p.gold);
                }
                if errors > 0 {
                    chip(format!("✗ {errors} failed"), p.love);
                }
                let done = count(State::Done);
                if done > 0 {
                    chip(format!("✓ {done} done"), p.pine);
                }
                let n = s.tabs.len();
                chip(format!("{n} tab{}", if n == 1 { "" } else { "s" }), p.subtle);
                let right_w: usize = right.iter().map(|s| str_width(&s.content)).sum();
                let avail = width.saturating_sub(4 + right_w + 2);
                let label = Style::new().fg(p.text).bg(bg).add_modifier(Modifier::BOLD);
                let mut spans = vec![
                    edge,
                    Span::styled(ICON_SESSION, Style::new().fg(if selected { p.iris } else { p.subtle }).bg(bg)),
                    Span::styled("  ", Style::new().bg(bg)),
                ];
                spans.extend(highlight(&s.label, &hits.name, avail, false, label, hl));
                f.render_widget(Line::from(spans), r);
                f.render_widget(Line::from(right).right_aligned(), r);
            }
            Item::Project(k) => {
                let project = &self.tasks[*k].project;
                // This project's rows, up to the next heading over a group.
                let rows = self.rows.iter().skip_while(|&&i| i != *item).skip(1);
                let tasks: Vec<&Task> = rows
                    .take_while(|i| !matches!(i, Item::Project(_)))
                    .filter_map(|&i| self.task_of(i))
                    .collect();
                let mut right: Vec<Span> = Vec::new();
                let mut chip = |text: String, fg: Color| {
                    right.extend(pill(vec![Span::styled(text, Style::new().fg(fg))], chip_bg, bg));
                    right.push(Span::styled(" ", Style::new().bg(bg)));
                };
                let left = tasks.iter().filter(|t| t.left).count();
                if left > 0 {
                    chip(format!("▢ {left} left"), p.gold);
                }
                let n = tasks.len();
                chip(format!("{n} task{}", if n == 1 { "" } else { "s" }), p.subtle);
                let right_w: usize = right.iter().map(|s| str_width(&s.content)).sum();
                let avail = width.saturating_sub(4 + right_w + 2);
                let label = Style::new().fg(p.text).bg(bg).add_modifier(Modifier::BOLD);
                let mut spans = vec![
                    edge,
                    Span::styled(ICON_PROJECT, Style::new().fg(if selected { p.iris } else { p.subtle }).bg(bg)),
                    Span::styled("  ", Style::new().bg(bg)),
                ];
                spans.extend(highlight(project, &hits.name, avail, false, label, hl));
                f.render_widget(Line::from(spans), r);
                f.render_widget(Line::from(right).right_aligned(), r);
            }
            // Headings and tasks with no tab are drawn elsewhere.
            Item::Task(_) | Item::Status(_) | Item::Sub(_) => {}
            Item::Tab(t) => {
                let tab = &self.tabs[*t];
                let task = tab.task.map(|k| &self.tasks[k]);
                let (glyph, color) = match tab.state {
                    State::Working => (SPINNER[tick % SPINNER.len()], p.foam),
                    State::Blocked => ("●", p.gold),
                    State::Done => ("✓", p.pine),
                    State::Error => ("✗", p.love),
                    State::Idle => ("·", p.muted),
                };
                let here = self.is_current(*t);
                let here_label = "current";
                let guide = if last { "╰─ " } else { "├─ " };
                let lead = 1 + 1 + 3 + 2; // edge, indent, guide, glyph + space
                // A task tab: its uncommitted files, and its folder tinted by profile.
                let dirty = task.filter(|t| t.dirty > 0).map(|t| format!("✎{}  ", t.dirty)).unwrap_or_default();
                let dirty_w = str_width(&dirty);
                let dir_room = (width.saturating_sub(lead + dirty_w) * 2 / 5).min(48);
                let dir_w = str_width(&tab.dir).min(dir_room);
                let here_w = if here { 2 + here_label.len() } else { 0 };
                // A long name (a title a program set, say) stops at a third
                // of the row, so it can't crowd out the folder and the rows
                // around it.
                let name_cap = (width.saturating_sub(lead) / 3).max(24);
                let name_room = width.saturating_sub(lead + dir_w + dirty_w + 3 + here_w).min(name_cap);
                let name_style = if selected {
                    Style::new().fg(p.text).bg(bg).add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(p.text).bg(bg)
                };
                let mut spans = vec![
                    edge,
                    Span::styled(" ", Style::new().bg(bg)),
                    Span::styled(guide, Style::new().fg(p.hl_med).bg(bg)),
                    Span::styled(glyph, Style::new().fg(color).bg(bg)),
                    Span::styled(" ", Style::new().bg(bg)),
                ];
                spans.extend(highlight(&tab.name, &hits.name, name_room, false, name_style, hl));
                if here {
                    spans.push(Span::styled(
                        format!("  {here_label}"),
                        Style::new().fg(p.iris).bg(bg).add_modifier(Modifier::ITALIC),
                    ));
                }
                f.render_widget(Line::from(spans), r);
                let dir_fg = match task {
                    Some(t) if t.work() => p.gold,
                    Some(_) => p.foam,
                    None if selected => p.subtle,
                    None => p.muted,
                };
                let mut right = vec![Span::styled(dirty, Style::new().fg(p.gold).bg(bg))];
                right.extend(highlight(&tab.dir, &hits.second, dir_w, true, Style::new().fg(dir_fg).bg(bg), hl));
                right.push(Span::styled(" ", Style::new().bg(bg)));
                f.render_widget(Line::from(right).right_aligned(), r);
            }
        }
    }

    /// A flat row's work items, grouped by kind within `budget` columns
    /// (see model's `badges`), such as `PR #57 ✓ merged · cards 1041 1042`,
    /// or `merged` for a branch in the default branch with no PR. Empty for
    /// none.
    fn badge_spans(&self, t: &Flat, bg: Color, budget: usize) -> Vec<Span<'static>> {
        let p = &self.p;
        let mut spans: Vec<Span<'static>> = badges(t.links, budget)
            .into_iter()
            .map(|(text, tone)| {
                let fg = match tone {
                    Tone::Ref => p.subtle,
                    Tone::Muted => p.muted,
                    Tone::Merged => p.iris,
                    Tone::Pass => p.pine,
                    Tone::Fail => p.love,
                    Tone::Pending => p.gold,
                };
                Span::styled(text, Style::new().fg(fg).bg(bg))
            })
            .collect();
        if t.merged == "merged" && !t.links.iter().any(|l| l.kind == "pr") {
            if !spans.is_empty() {
                spans.push(Span::styled(" · ", Style::new().fg(p.muted).bg(bg)));
            }
            spans.push(Span::styled("merged", Style::new().fg(p.iris).bg(bg)));
        }
        if !spans.is_empty() {
            spans.push(Span::styled("  ", Style::new().bg(bg)));
        }
        spans
    }

    /// The right side of a flat row, [work items] [dirty] [host chip]
    /// [state age], and its width.
    fn right_spans(&self, t: &Flat, bg: Color, verb: &str, width: u16) -> (Vec<Span<'static>>, usize) {
        let p = &self.p;
        let mut right: Vec<Span> = self.badge_spans(t, bg, badge_budget(width));
        if t.dirty > 0 {
            right.push(Span::styled(format!("✎{}  ", t.dirty), Style::new().fg(p.gold).bg(bg)));
        }
        let chip_fg = if t.work { p.gold } else { p.subtle };
        right.extend(pill(vec![Span::styled(t.host.to_string(), Style::new().fg(chip_fg))], p.surface, bg));
        // A snoozed task: how long until it wakes.
        let when = match (t.wake, t.since) {
            (wake, _) if wake > 0 => format!(" {}", age(wake.saturating_sub(self.now))),
            (_, since) if since > 0 => format!(" {}", age(self.now.saturating_sub(since))),
            _ => String::new(),
        };
        right.push(Span::styled(format!("  {verb:>9}{when:>4} "), Style::new().fg(p.muted).bg(bg)));
        let w = right.iter().map(|s| str_width(&s.content)).sum();
        (right, w)
    }

    /// A flat row; with a guide, on a session's tree, without the project
    /// column (the session heading says it).
    #[allow(clippy::too_many_arguments)]
    fn draw_flat_row(
        &self,
        f: &mut Frame,
        r: Rect,
        item: Item,
        selected: bool,
        tick: usize,
        cols: (usize, usize),
        guide: Option<&str>,
    ) {
        let p = &self.p;
        let t = self.flat(item);
        let (bg, edge) = self.row_style(selected);
        f.buffer_mut().set_style(r, Style::new().bg(bg));
        let (glyph, color, verb) = match (t.ended, t.state) {
            // Closed without an archive: the worktree is still there.
            (Some(_), _) if t.left => ("▢", p.gold, "closed"),
            (Some("merged"), _) => ("▢", p.iris, "merged"),
            (Some("archived" | "force-archived" | "discarded" | "force-discarded"), _) => ("▢", p.iris, "archived"),
            (Some("tab closed"), _) => ("▢", p.muted, "closed"),
            (Some("worktree removed"), _) => ("▢", p.muted, "removed"),
            (Some(_), _) => ("▢", p.muted, "ended"),
            (None, "working") => (SPINNER[tick % SPINNER.len()], p.foam, "working"),
            (None, "blocked") => ("●", p.gold, "waiting"),
            (None, "done") => ("✓", p.pine, "done"),
            (None, "error") => ("✗", p.love, "failed"),
            (None, "idle") => ("·", p.muted, "idle"),
            (None, _) => ("?", p.muted, "no status"),
        };
        let verb = if t.wake > 0 { "snoozed" } else { verb };
        // Right side first, so the columns and message get what is left.
        let (right, right_w) = self.right_spans(&t, bg, verb, r.width);
        let mut left = vec![edge];
        if let Some(guide) = guide {
            left.push(Span::styled(" ", Style::new().bg(bg)));
            left.push(Span::styled(guide.to_string(), Style::new().fg(p.hl_med).bg(bg)));
        }
        left.push(Span::styled(format!("{glyph} "), Style::new().fg(color).bg(bg)));
        let lead = if guide.is_some() { 4 } else { 0 };
        let room = (r.width as usize).saturating_sub(3 + lead + right_w + 1);
        let (proj_w, name_w) = cols;
        let proj_w = if guide.is_some() { 0 } else { proj_w };
        let name_style = if selected {
            Style::new().fg(p.text).bg(bg).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(p.text).bg(bg)
        };
        let hl = Style::new().fg(p.rose).bg(bg).add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        let hits = self.hits.get(&item).cloned().unwrap_or_default();
        let mut column = |text: &str, hits: &[usize], w: usize, style: Style| {
            let spans = highlight(text, hits, w, false, style, hl);
            let used: usize = spans.iter().map(|s| str_width(&s.content)).sum();
            left.extend(spans);
            left.push(Span::styled(" ".repeat(w.saturating_sub(used) + 2), Style::new().bg(bg)));
        };
        if guide.is_none() {
            column(t.project, &hits.second, proj_w, Style::new().fg(p.subtle).bg(bg));
        }
        column(t.name, &hits.name, name_w, name_style);
        let msg_room = room.saturating_sub(if guide.is_none() { proj_w + 2 } else { 0 } + name_w + 2);
        if !t.msg.is_empty() && msg_room > 4 {
            let msg_style = Style::new().fg(if t.state == "blocked" { p.gold } else { p.subtle }).bg(bg);
            left.extend(highlight(t.msg, &[], msg_room, false, msg_style, msg_style));
        }
        f.render_widget(Line::from(left), r);
        f.render_widget(Line::from(right).right_aligned(), r);
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        let p = &self.p;
        if let Some((msg, failed)) = &self.note {
            let style = Style::new().fg(if *failed { p.love } else { p.pine });
            f.render_widget(Line::from(vec![Span::raw(" "), Span::styled(msg.clone(), style)]), area);
            return;
        }
        if let Some(z) = &self.snooze {
            let enter = match z.until {
                Ok(0) => "wake",
                _ => "snooze",
            };
            let keys = [("⏎", enter, p.subtle), ("⌃U", "clear", p.subtle), ("esc", "cancel", p.subtle)];
            let (spans, _) = draw::hints(p, &keys, area.width as usize);
            f.render_widget(Line::from(spans), area);
            return;
        }
        if self.chooser.is_some() {
            let keys = [("⏎", "open", p.subtle), ("↑↓", "move", p.subtle), ("esc", "cancel", p.subtle)];
            let (spans, _) = draw::hints(p, &keys, area.width as usize);
            f.render_widget(Line::from(spans), area);
            return;
        }
        if let (Armed::Review | Armed::Link | Armed::Pin, Some(t)) = (self.armed, self.selected_task()) {
            let what = match self.armed {
                Armed::Review => format!("review {}", t.label()),
                Armed::Pin if t.pinned => format!("unpin {}", t.label()),
                Armed::Pin => format!("pin {}", t.label()),
                _ => match self.selected_link() {
                    Some(l) if l.kind == "pr" => "open PR".into(),
                    _ => "open link".into(),
                },
            };
            let line = Line::from(vec![
                Span::raw(" "),
                Span::styled("⏎", Style::new().fg(p.iris).add_modifier(Modifier::BOLD)),
                Span::styled(format!(" {what}"), Style::new().fg(p.text)),
                Span::styled(" · other keys cancel", Style::new().fg(p.muted)),
            ]);
            f.render_widget(line, area);
            return;
        }
        if let (true, Some(t)) = (self.armed.archives(), self.selected_task()) {
            let mut spans = vec![Span::raw(" ")];
            let key = Style::new().fg(p.love).add_modifier(Modifier::BOLD);
            let files = || {
                let mut list = t.dirty_files.join(", ");
                if t.dirty > t.dirty_files.len() as u64 {
                    list.push_str(&format!(" +{}", t.dirty - t.dirty_files.len() as u64));
                }
                list
            };
            match self.armed {
                Armed::Archive if t.dirty > 0 => {
                    spans.push(Span::styled("⏎", key));
                    spans.push(Span::styled(format!(" archive {}", t.label()), Style::new().fg(p.love)));
                    spans.push(Span::styled(format!(" (uncommitted: {})", files()), Style::new().fg(p.gold)));
                    spans.push(Span::styled(" · other keys cancel", Style::new().fg(p.muted)));
                }
                Armed::Force => {
                    spans.push(Span::styled("Are you sure? ", Style::new().fg(p.love).add_modifier(Modifier::BOLD)));
                    spans.push(Span::styled("⏎", key));
                    spans.push(Span::styled(format!(" throws away {}", files()), Style::new().fg(p.love)));
                    spans.push(Span::styled(" · other keys cancel", Style::new().fg(p.muted)));
                }
                _ => {
                    let what = if t.ended.is_some() { "removes worktree" } else { "closes tab, removes worktree" };
                    spans.push(Span::styled("⏎", key));
                    spans.push(Span::styled(format!(" archive {}", t.label()), Style::new().fg(p.love)));
                    spans.push(Span::styled(format!(": {what} · other keys cancel"), Style::new().fg(p.muted)));
                }
            }
            f.render_widget(Line::from(spans), area);
            return;
        }
        if self.view == View::Usage {
            let keys = [("↑↓", "scroll", p.subtle), ("h/l", "views", p.subtle), ("⌃O", self.profile.next().name(), p.subtle), ("esc", "close", p.subtle)];
            let (spans, _) = draw::hints(p, &keys, area.width as usize);
            f.render_widget(Line::from(spans), area);
            return;
        }
        let task = self.selected_task();
        let mut keys: Vec<(&str, &str)> = match task {
            Some(t) if t.ended.is_some() && t.left && t.safe() => vec![("⏎", "resume"), ("⌃X", "remove worktree (merged)")],
            Some(t) if t.ended.is_some() && t.left => vec![("⏎", "resume"), ("⌃X", "remove worktree")],
            Some(t) if t.ended.is_some() => vec![("⏎", "resume")],
            Some(t) if t.safe() => vec![("⏎", "go"), ("⌃R", "review"), ("⌃X", "archive (merged)")],
            Some(_) => vec![("⏎", "go"), ("⌃R", "review"), ("⌃X", "archive")],
            None => vec![("⏎", "go")],
        };
        // ⌃L opens the task's only work item, or a chooser over several.
        let links = task.map(|t| chooser_links(&t.links)).unwrap_or_default();
        match links.as_slice() {
            [] => {}
            [link] => keys.push(("⌃L", if link.kind == "pr" { "open PR" } else { "open link" })),
            _ => keys.push(("⌃L", "links")),
        }
        if self.can_pin() {
            keys.push(("⌃T", if task.is_some_and(|t| t.pinned) { "unpin" } else { "pin" }));
        }
        if self.can_snooze() {
            keys.push(("⌃S", if task.is_some_and(|t| t.is_snoozed(self.now)) { "wake" } else { "snooze" }));
        }
        if self.query.is_empty() {
            keys.push(("h/l", "views"));
        }
        keys.push(("⌃O", self.profile.next().name()));
        match self.view {
            View::Usage => {}
            View::All => {
                keys.push(("⌥↑↓", "group"));
                if self.expanded {
                    keys.push(("⌃E", "list"));
                    keys.push(("↑↓", "scroll"));
                } else if self.preview_want().is_some() {
                    keys.push(("⌃E", "expand"));
                    keys.push(("⇧↑↓", "scroll"));
                }
            }
            View::Archive => {
                keys.push(("⌥↑↓", "group"));
                keys.push(("⌃F", if self.kept { "show archived" } else { "hide archived" }));
            }
        }
        keys.push(("esc", "close"));
        let keys: Vec<_> = keys.into_iter().map(|(k, label)| (k, label, p.subtle)).collect();
        let (spans, used) = draw::hints(p, &keys, area.width as usize);
        f.render_widget(Line::from(spans), area);
        // The summary only when it still fits beside the hints.
        if self.view != View::All {
            return;
        }
        let n = self.sessions.len();
        let summary = format!("{n} session{}", if n == 1 { "" } else { "s" });
        if used + str_width(&summary) + 1 > area.width as usize {
            return;
        }
        f.render_widget(Line::styled(format!("{summary} "), Style::new().fg(p.muted)).right_aligned(), area);
    }
}

// ---------------------------------------------------------------- standalone

impl App {
    /// Start a standalone picker's data (see standalone::Feed).
    fn start_feed(&mut self) {
        self.feed = Some(standalone::Feed::start());
    }

    /// Ask for the data again after an action, keeping what is shown until
    /// it comes.
    fn refetch(&mut self) {
        if !matches!(self.tasks_load, Tasks::Failed) {
            self.tasks_load = Tasks::Waiting;
        }
        if let Some(feed) = &self.feed {
            feed.poke();
        }
    }

    /// Tell a standalone picker's feed which pane to capture, when it changes.
    fn ask_preview(&mut self) {
        let Some(feed) = &self.feed else { return };
        let want = self.preview_want();
        if want != self.asked {
            feed.preview(want.clone());
            self.asked = want;
        }
    }

    /// Data that has arrived, taken as the float takes pastes.
    fn take_fetched(&mut self) {
        let Some(feed) = &self.feed else { return };
        let mut got = Vec::new();
        loop {
            match feed.rx.try_recv() {
                Ok(text) => got.push(text),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        for text in got {
            self.paste(&text);
        }
    }

    /// Enter, ctrl-r and Escape in a standalone picker, as the config's
    /// tab_picker mode handles them; false for other keys.
    fn standalone_key(&mut self, k: KeyEvent, term: &mut DefaultTerminal) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let review = matches!(k.code, KeyCode::Char('r') if ctrl);
        let open = matches!(k.code, KeyCode::Char('l') if ctrl);
        // The snooze prompt's Enter saves the snooze; other keys edit it.
        if let Some(z) = &self.snooze {
            // Unreadable text: Enter does nothing.
            let Ok(until) = z.until else { return matches!(k.code, KeyCode::Enter) };
            if !matches!(k.code, KeyCode::Enter) {
                return false;
            }
            let Some(pick) = standalone::parse_pick(&self.row_pick()) else { return true };
            let label = z.label.clone();
            self.note = Some(match standalone::snooze(&pick, until) {
                Ok(msg) => {
                    self.finish_snooze();
                    let msg = if until > 0 { format!("{msg} until {}", snooze::describe(until, self.now)) } else { msg };
                    (msg, false)
                }
                Err(e) => {
                    self.snooze = None;
                    (format!("{label}: {e}"), true)
                }
            });
            self.refetch();
            return true;
        }
        // Pin or unpin in the registry, then here.
        if matches!(k.code, KeyCode::Char('t') if ctrl) {
            if !self.can_pin() {
                return true;
            }
            let Some(pick) = standalone::parse_pick(&self.row_pick()) else { return true };
            self.note = Some(match standalone::pin(&pick) {
                Ok(msg) => {
                    self.toggle_pin();
                    (msg, false)
                }
                Err(e) => (e, true),
            });
            self.refetch();
            return true;
        }
        // The chooser's pick opens on Enter (or ctrl-l again); other keys
        // move in it.
        if let Some(c) = &self.chooser {
            if !open && !matches!(k.code, KeyCode::Enter) {
                return false;
            }
            let url = c.url().to_string();
            self.chooser = None;
            self.note = Some(match standalone::open_url(&url) {
                Ok(()) => (format!("opened {url}"), false),
                Err(e) => (e, true),
            });
            return true;
        }
        if !review && !open && !matches!(k.code, KeyCode::Enter) {
            return false;
        }
        self.note = None;
        let Some(pick) = standalone::parse_pick(&self.pick()) else { return true };
        let label = self.selected_label();
        // Open the task's only work item, or the chooser over several;
        // nothing to refresh after.
        if open {
            if self.open_chooser() {
                return true;
            }
            if self.selected_task().is_some() {
                self.note = Some(match standalone::open_link(&pick) {
                    Ok(url) => (format!("opened {url}"), false),
                    Err(e) => (e, true),
                });
            }
            return true;
        }
        let done = |r: Result<(), String>, msg: String| match r {
            Ok(()) => (msg, false),
            Err(e) => (e, true),
        };
        let note = match pick.action.as_str() {
            // Only open tasks have something to review.
            _ if review && pick.action != "task" => return true,
            _ if review => {
                self.busy(term, format!("Opening review of {label}…"));
                done(standalone::review(&pick), format!("reviewing {label}"))
            }
            // As the config types ctrl-x for `ask`: on to "are you sure".
            "ask" => {
                self.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
                return true;
            }
            "archive" | "force-archive" => {
                self.armed = Armed::No;
                self.busy(term, format!("Archiving {label} on {}…", pick.host));
                match standalone::archive(&pick.action, &pick) {
                    Ok(msg) => (msg, false),
                    Err(e) => (e, true),
                }
            }
            "resume" => {
                self.busy(term, format!("Resuming {label}…"));
                done(standalone::resume(&pick), format!("resumed {label}"))
            }
            _ => done(standalone::go(&pick), format!("showed {label}")),
        };
        self.note = Some(note);
        self.refetch();
        true
    }

    /// The highlighted row's name for an outcome: the task, or the tab.
    fn selected_label(&self) -> String {
        match self.selected_item() {
            Some(Item::Tab(t)) if self.tabs[t].task.is_none() => self.tabs[t].name.clone(),
            Some(item) => self.task_of(item).map(Task::label).unwrap_or_default(),
            None => String::new(),
        }
    }

    /// Show `msg` in the footer while a slow action runs.
    fn busy(&mut self, term: &mut DefaultTerminal, msg: String) {
        self.note = Some((msg, false));
        let _ = term.draw(|f| self.draw(f));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn inbox() -> App {
        let mut app = App::new((String::new(), String::new()), View::All);
        app.follow = false;
        app.deliver(|app| {
            (app.sessions, app.tabs) = parse_rows(concat!(
                "S\t-\tlocal\ts\tdemo\n",
                "T\tbuild\tworking\tbuilding\n",
                "T\twait\tblocked\tapproval\n",
                "T\tfail\terror\tbroken\n",
                "T\tdone\tdone\tready\n",
                "T\tidle\t\tregistered\n",
            ));
            app.tasks = parse_tasks(&serde_json::json!([
                {"session_id":"s", "window_id":"idle", "status":"idle"},
                {"session_id":"s", "window_id":"old", "name":"older", "status":"blocked", "status_at":10},
                {"session_id":"s", "window_id":"new", "name":"newer", "status":"blocked", "status_at":20, "profile":"work"},
                {"session_id":"s", "window_id":"unknown"},
                {"session_id":"s", "window_id":"future", "status":"future"},
                {"session_id":"s", "window_id":"ended", "ended_at":30}
            ]).to_string()).unwrap();
            app.load = Load::Ready;
            app.tasks_load = Tasks::Ready;
        });
        app
    }

    #[test]
    fn agents_group_by_status_in_their_session_by_urgency_and_age() {
        let app = inbox();
        assert_eq!(app.rows, vec![
            Item::Session(0),
            Item::Sub("blocked"), Item::Task(1), Item::Task(2), Item::Tab(1),
            Item::Sub("error"), Item::Tab(2),
            Item::Sub("done"), Item::Tab(3),
            Item::Sub("working"), Item::Tab(0),
            Item::Sub("idle"), Item::Tab(4),
            Item::Sub(""), Item::Task(3), Item::Task(4),
        ]);
        assert_eq!(app.selected_item(), Some(Item::Task(1)));
    }

    #[test]
    fn working_agents_keep_their_order_as_they_report() {
        let order = |a: u64, b: u64| {
            let mut app = App::new((String::new(), String::new()), View::All);
            app.follow = false;
            app.deliver(|app| {
                app.tasks = parse_tasks(&serde_json::json!([
                    {"session_id":"s", "window_id":"a", "project":"p", "name":"zeta", "status":"working", "status_at":a},
                    {"session_id":"s", "window_id":"b", "project":"p", "name":"alpha", "status":"working", "status_at":b},
                ]).to_string()).unwrap();
                app.load = Load::Ready;
                app.tasks_load = Tasks::Ready;
            });
            app.rows
        };
        let expected = vec![Item::Status("notab"), Item::Task(1), Item::Task(0)];
        assert_eq!(order(10, 20), expected);
        assert_eq!(order(20, 10), expected);
    }

    #[test]
    fn inbox_headings_follow_search_and_profile_filters() {
        let mut app = inbox();
        app.query = "older".into();
        app.refilter();
        assert_eq!(app.rows, vec![Item::Session(0), Item::Sub("blocked"), Item::Task(1)]);
        assert_eq!(app.selected_item(), Some(Item::Task(1)));
        app.query.clear();
        app.profile = Profile::Work;
        app.refilter();
        assert_eq!(app.rows, vec![Item::Session(0), Item::Sub("blocked"), Item::Task(2)]);
        app.query = "no-match".into();
        app.refilter();
        assert!(app.rows.is_empty());
        assert!(app.selected.is_none());
    }

    #[test]
    fn inbox_navigation_skips_headings_and_jumps_status_groups() {
        let mut app = inbox();
        app.jump_group(true);
        assert_eq!(app.selected_item(), Some(Item::Tab(2)));
        app.step(-1);
        assert_eq!(app.selected_item(), Some(Item::Tab(1)));
        app.jump_group(false);
        assert_eq!(app.selected_item(), Some(Item::Task(1)));
        app.edge(true);
        assert_eq!(app.selected_item(), Some(Item::Task(4)));
        app.edge(false);
        assert_eq!(app.selected_item(), Some(Item::Task(1)));
        app.selected = Some(5);
        assert!(app.pick().is_empty());
        assert!(app.selected_key().is_none());
        assert!(app.selected_task().is_none());
    }

    #[test]
    fn status_updates_keep_the_selected_task_and_armed_action() {
        let mut app = inbox();
        let selected = app.selected_key();
        app.armed = Armed::Archive;
        app.deliver(|app| app.tasks[1].state = "working".into());
        assert_eq!(app.selected_key(), selected);
        assert_eq!(app.selected_item(), Some(Item::Task(1)));
        assert!(app.armed == Armed::Archive);
        let at = app.selected.unwrap();
        assert_eq!(app.rows[at - 1], Item::Sub("working"));
    }

    fn inbox_with_lifecycle() -> App {
        let mut app = App::new((String::new(), String::new()), View::All);
        app.follow = false;
        app.now = 1000;
        app.deliver(|app| {
            (app.sessions, app.tabs) = parse_rows(concat!(
                "S\t-\tlocal\ts\tdemo\n",
                "T\ta\tworking\tbusy\n",
                "T\tb\tblocked\tasks\n",
                "T\tc\tdone\tdone\n",
                "T\td\tidle\tdone\n",
            ));
            app.tasks = parse_tasks(&serde_json::json!([
                {"session_id":"s", "window_id":"a", "name":"pinned", "pinned":true},
                {"session_id":"s", "window_id":"b", "name":"later", "snoozed_until":5000},
                {"session_id":"s", "window_id":"c", "name":"woke", "snoozed_until":500},
                {"session_id":"s", "window_id":"d", "name":"both", "pinned":true, "snoozed_until":4000}
            ]).to_string()).unwrap();
            app.load = Load::Ready;
            app.tasks_load = Tasks::Ready;
        });
        app
    }

    #[test]
    fn pinned_tasks_lead_and_snoozed_ones_trail_the_list() {
        let mut app = inbox_with_lifecycle();
        assert_eq!(app.rows, vec![
            Item::Status("pinned"), Item::Tab(0),
            Item::Session(0), Item::Sub("done"), Item::Tab(2),
            Item::Status("snoozed"), Item::Tab(3), Item::Tab(1),
        ]);
        // A snoozed blocked task isn't counted as waiting.
        let mut terminal = Terminal::new(TestBackend::new(100, 1)).unwrap();
        terminal.draw(|f| app.draw_search(f, f.area())).unwrap();
        let text: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
        assert!(!text.contains("waiting") && text.contains("1 done"), "{text}");
        let mut terminal = Terminal::new(TestBackend::new(100, 8)).unwrap();
        terminal.draw(|f| app.draw_list(f, f.area())).unwrap();
        let text: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Pinned") && text.contains("Snoozed") && text.contains("snoozed  1h"), "{text}");
    }

    #[test]
    fn searching_a_session_finds_its_rows_in_every_group() {
        let mut app = inbox_with_lifecycle();
        app.query = "demo".into();
        app.refilter();
        for tab in 0..4 {
            assert!(app.rows.contains(&Item::Tab(tab)), "missing tab {tab}");
        }
    }

    #[test]
    fn sessions_wanting_john_lead_and_keep_their_other_tabs_last() {
        let mut app = App::new((String::new(), String::new()), View::All);
        app.follow = false;
        app.deliver(|app| {
            (app.sessions, app.tabs) = parse_rows(concat!(
                "S\t-\tlocal\ta\tquiet\n",
                "T\tshell\t\tshell\t~\n",
                "T\tbusy\tworking\tbuilding\t~\n",
                "S\t-\tlocal\tb\tbusy\n",
                "T\tnotes\t\tnotes\t~\n",
                "T\tasks\tblocked\tapproval\t~\n",
                "S\t-\tlocal\tc\tplain\n",
                "T\tsh\t\tsh\t~\n",
            ));
            app.tasks = parse_tasks(&serde_json::json!([
                {"session_id":"gone", "window_id":"w", "name":"stray", "status":"done"}
            ]).to_string()).unwrap();
            app.load = Load::Ready;
            app.tasks_load = Tasks::Ready;
        });
        assert_eq!(app.rows, vec![
            Item::Session(1), Item::Sub("blocked"), Item::Tab(3), Item::Sub("tabs"), Item::Tab(2),
            Item::Session(0), Item::Sub("working"), Item::Tab(1), Item::Sub("tabs"), Item::Tab(0),
            Item::Session(2), Item::Tab(4),
            Item::Status("notab"), Item::Task(0),
        ]);
        let mut terminal = Terminal::new(TestBackend::new(100, 16)).unwrap();
        terminal.draw(|f| app.draw_list(f, f.area())).unwrap();
        let text: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
        for label in ["Waiting", "Working", "Tabs", "No tab", "approval", "stray"] {
            assert!(text.contains(label), "{label}: {text}");
        }
    }

    #[test]
    fn ctrl_t_pins_and_unpins_and_holds_over_stale_data() {
        let mut app = inbox_with_lifecycle();
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(2));
        // ctrl-t arms Enter; the config saves the pin and types Enter back.
        app.key(ctrl('t'));
        assert!(!app.tasks[2].pinned);
        assert!(app.pick().starts_with("pin:"));
        app.key(enter);
        assert!(app.tasks[2].pinned);
        assert!(app.armed == Armed::No);
        assert_eq!(app.selected_item(), Some(Item::Tab(2)));
        // Done ranks above working, pinned or not.
        assert_eq!(&app.rows[..3], &[Item::Status("pinned"), Item::Tab(2), Item::Tab(0)]);
        // Data read before the pin was saved doesn't undo it ...
        app.deliver(|app| app.tasks[2].pinned = false);
        assert!(app.tasks[2].pinned);
        // ... and once it agrees, it is the data's again.
        app.deliver(|app| app.tasks[2].pinned = true);
        assert!(app.pending.is_empty());
        // Another key cancels an armed pin.
        app.key(ctrl('t'));
        app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.key(enter);
        assert!(app.tasks[2].pinned);
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(2));
        app.key(ctrl('t'));
        app.key(enter);
        assert!(!app.tasks[2].pinned);
        // Not in the archive.
        app.set_view(View::Archive);
        assert!(!app.can_pin());
    }

    #[test]
    fn ctrl_s_snoozes_until_a_typed_time_and_wakes() {
        let mut app = inbox_with_lifecycle();
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let plain = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(2));
        let target = app.row_pick().split_once(':').unwrap().1.to_string();
        app.key(ctrl('s'));
        assert!(app.snooze.is_some());
        assert_eq!(app.pick(), "snooze-invalid");
        for c in "2x".chars() {
            app.key(plain(KeyCode::Char(c)));
        }
        assert_eq!(app.pick(), "snooze-invalid");
        // Enter on unreadable text does nothing; the search is untouched.
        app.key(plain(KeyCode::Enter));
        assert!(app.snooze.is_some() && app.query.is_empty());
        app.key(plain(KeyCode::Backspace));
        app.key(plain(KeyCode::Char('h')));
        assert_eq!(app.pick(), format!("snooze:{}:{target}", 1000 + 7200));
        let mut terminal = Terminal::new(TestBackend::new(100, 16)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let text: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Snooze /woke") && text.contains("until 2h"), "{text}");
        app.key(plain(KeyCode::Enter));
        assert!(app.snooze.is_none());
        assert_eq!(app.tasks[2].snoozed_until, 8200);
        assert_eq!(app.rows.last(), Some(&Item::Tab(2)));
        // The highlight moved on to the row that followed.
        assert_eq!(app.selected_item(), Some(Item::Tab(3)));
        // On a snoozed task, an empty prompt wakes it.
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(2));
        app.key(ctrl('s'));
        assert_eq!(app.pick(), format!("snooze:0:{target}"));
        app.key(plain(KeyCode::Enter));
        assert_eq!(app.tasks[2].snoozed_until, 0);
        assert_eq!(app.selected_item(), Some(Item::Tab(2)));
        // Escape closes the prompt; the archive doesn't snooze.
        app.key(ctrl('s'));
        app.key(plain(KeyCode::Esc));
        assert!(app.snooze.is_none());
        app.set_view(View::Archive);
        app.key(ctrl('s'));
        assert!(app.snooze.is_none());
    }

    #[test]
    fn failed_hosts_keep_the_other_rows_and_show_in_the_header() {
        let mut app = App::new((String::new(), String::new()), View::All);
        let chips = |app: &App| app.header_chips().iter().map(|s| s.content.to_string()).collect::<String>();
        app.paste("rex-data:rows\nS\t-\tlocal\ts\tdemo\nT\tw\t\tshell\t~\nE\tmiller\tmiller: timed out\n");
        assert!(matches!(app.load, Load::Ready));
        assert_eq!((app.tabs.len(), app.failed.as_slice()), (1, ["miller".to_string()].as_slice()));
        assert!(chips(&app).contains("miller failed"));
        app.paste("rex-data:rows\nE\tmiller\tx\nE\tdrummer\tnot listed yet\n");
        assert!(chips(&app).contains("2 hosts failed"));
        app.paste("rex-data:rows\nS\t-\tlocal\ts\tdemo\n");
        assert!(app.failed.is_empty() && !chips(&app).contains("failed"));
    }

    #[test]
    fn task_rows_show_work_item_badges_and_open_hint() {
        let mut app = App::new((String::new(), String::new()), View::All);
        app.follow = false;
        app.deliver(|app| {
            app.tasks = parse_tasks(&serde_json::json!([
                {"session_id":"s", "window_id":"a", "name":"linked", "status":"done", "links":[
                    {"kind":"issue","system":"github","ref":"o/r#42","url":"i"},
                    {"kind":"pr","system":"github","ref":"o/r#57","url":"p","state":"merged","checks":"pass"}]},
                {"session_id":"s", "window_id":"b", "name":"local", "status":"working", "branch_merged":"merged"}
            ]).to_string()).unwrap();
            app.tasks_load = Tasks::Ready;
        });
        assert_eq!(app.tasks[0].links[0].kind, "pr");
        let mut terminal = Terminal::new(TestBackend::new(120, 6)).unwrap();
        terminal.draw(|f| app.draw_list(f, f.area())).unwrap();
        let text: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
        assert!(text.contains("PR #57 ✓ merged · issue #42"), "{text}");
        assert!(text.contains("merged"));
        let mut terminal = Terminal::new(TestBackend::new(160, 3)).unwrap();
        terminal.draw(|f| app.draw_footer(f, f.area())).unwrap();
        let text: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
        assert!(text.contains("⌃L links"), "{text}");
    }

    #[test]
    fn ctrl_l_chooses_among_several_links_prs_first() {
        let mut app = App::new((String::new(), String::new()), View::All);
        app.follow = false;
        app.deliver(|app| {
            app.tasks = parse_tasks(&serde_json::json!([
                {"session_id":"s", "window_id":"a", "name":"many", "status":"done", "links":[
                    {"kind":"issue","system":"github","ref":"o/r#42","url":"https://i"},
                    {"kind":"card","system":"fizzy","ref":"1041","url":"https://c"},
                    {"kind":"pr","system":"github","ref":"o/r#57","url":"https://p"}]},
                {"session_id":"s", "window_id":"b", "name":"one", "status":"idle", "links":[
                    {"kind":"card","system":"fizzy","ref":"1042","url":"https://c2"}]}
            ]).to_string()).unwrap();
            app.tasks_load = Tasks::Ready;
        });
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let plain = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.selected = app.rows.iter().position(|&i| i == Item::Task(1));
        // One link: no chooser, and the pick carries no count.
        assert_eq!(app.pick(), format!("task:{}/s/b", hex("")));
        app.key(ctrl('l'));
        assert!(app.chooser.is_none());
        app.selected = app.rows.iter().position(|&i| i == Item::Task(0));
        assert_eq!(app.pick(), format!("task:{}/s/a/3", hex("")));
        assert_eq!(standalone::parse_pick(&app.pick()).unwrap().window, "a");
        app.key(ctrl('l'));
        // The PR first and selected, so ctrl-l then Enter opens it.
        assert_eq!(app.pick(), format!("link:{}", hex("https://p")));
        app.key(plain(KeyCode::Char('j')));
        assert_eq!(app.pick(), format!("link:{}", hex("https://c")));
        app.key(plain(KeyCode::Down));
        app.key(plain(KeyCode::Down));
        assert_eq!(app.pick(), format!("link:{}", hex("https://i")));
        app.key(plain(KeyCode::Up));
        app.key(plain(KeyCode::Char('k')));
        app.key(plain(KeyCode::Char('k')));
        assert_eq!(app.pick(), format!("link:{}", hex("https://p")));
        // Other keys don't type into the search.
        app.key(plain(KeyCode::Char('x')));
        assert!(app.query.is_empty());
        let mut terminal = Terminal::new(TestBackend::new(100, 16)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let text: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Open link") && text.contains("card 1041") && text.contains("cancel"), "{text}");
        app.key(plain(KeyCode::Esc));
        assert!(app.chooser.is_none());
        assert_eq!(app.pick(), format!("task:{}/s/a/3", hex("")));
    }

    #[test]
    fn archive_search_finds_tasks_by_work_item_and_merged_work_is_safe() {
        let mut app = App::new((String::new(), String::new()), View::Archive);
        app.follow = false;
        app.deliver(|app| {
            app.tasks = parse_tasks(&serde_json::json!([
                {"session_id":"s", "window_id":"a", "name":"carded", "ended_at":5, "ended_reason":"merged",
                 "links":[{"kind":"card","system":"fizzy","ref":"1041","url":"c"}]},
                {"session_id":"s", "window_id":"b", "name":"other", "ended_at":6, "ended_reason":"tab closed",
                 "worktree_left":true, "dirty_count":0, "branch_merged":"merged"},
                {"session_id":"s", "window_id":"c", "name":"dirty", "dirty_count":2,
                 "links":[{"kind":"pr","system":"github","ref":"o/r#1","url":"p","state":"merged"}]},
                {"session_id":"s", "window_id":"d", "name":"open",
                 "links":[{"kind":"pr","system":"github","ref":"o/r#2","url":"p","state":"merged"}]}
            ]).to_string()).unwrap();
            app.tasks_load = Tasks::Ready;
        });
        app.query = "card 1041".into();
        app.refilter();
        assert_eq!(app.rows, vec![Item::Project(0), Item::Sub("merged"), Item::Task(0)]);
        assert_eq!(app.pick(), format!("resume:{}/s/a", hex("")));
        assert!(app.tasks[0].archived());
        assert!(!app.tasks[0].safe() && app.tasks[1].safe() && !app.tasks[2].safe() && app.tasks[3].safe());
    }

    #[test]
    fn status_headings_render_without_changing_other_views() {
        let mut app = inbox();
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal.draw(|f| app.draw_list(f, f.area())).unwrap();
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        for label in ["Waiting", "Failed", "Done", "Working", "Idle", "No status"] {
            assert!(text.contains(label), "missing status heading: {label}");
        }
        app.set_view(View::Archive);
        assert_eq!(app.rows, vec![Item::Project(5), Item::Sub("closed"), Item::Task(5)]);
    }

    #[test]
    fn archive_groups_by_project_left_behind_first() {
        let mut app = App::new((String::new(), String::new()), View::Archive);
        app.follow = false;
        app.deliver(|app| {
            app.tasks = parse_tasks(&serde_json::json!([
                {"session_id":"s", "window_id":"a", "project":"alpha", "name":"merged", "ended_at":50, "ended_reason":"merged"},
                {"session_id":"s", "window_id":"b", "project":"beta", "name":"closed", "ended_at":10, "ended_reason":"tab closed"},
                {"session_id":"s", "window_id":"c", "project":"beta", "name":"left", "ended_at":5, "ended_reason":"tab closed",
                 "worktree_left":true},
                {"session_id":"s", "window_id":"d", "project":"alpha", "name":"archived", "ended_at":40, "ended_reason":"archived"},
                {"session_id":"s", "window_id":"e", "project":"alpha", "name":"newer", "ended_at":60, "ended_reason":"archived"},
                {"session_id":"s", "window_id":"f", "project":"alpha", "name":"open"}
            ]).to_string()).unwrap();
            app.tasks_load = Tasks::Ready;
        });
        // beta has a worktree left behind, so it leads though alpha ended
        // more recently; within a project, actionable headings first, then
        // newest first.
        #[rustfmt::skip]
        assert_eq!(app.rows, vec![
            Item::Project(2), Item::Sub("left"), Item::Task(2), Item::Sub("closed"), Item::Task(1),
            Item::Project(0), Item::Sub("merged"), Item::Task(0), Item::Sub("archived"), Item::Task(4), Item::Task(3),
        ]);
        assert_eq!(app.selected, Some(2));
        app.jump_group(true);
        assert_eq!(app.rows[app.selected.unwrap()], Item::Task(1));
        // ctrl-f hides the archived ones, and with them alpha.
        app.key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert_eq!(app.rows, vec![Item::Project(2), Item::Sub("left"), Item::Task(2), Item::Sub("closed"), Item::Task(1)]);
        let mut terminal = Terminal::new(TestBackend::new(100, 10)).unwrap();
        terminal.draw(|f| app.draw_list(f, f.area())).unwrap();
        let text: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
        for want in ["beta", "1 left", "2 tasks", "Left behind", "Closed", "╰─"] {
            assert!(text.contains(want), "missing {want}");
        }
    }

    #[test]
    fn usage_is_integrated_read_only_and_handles_narrow_frames() {
        let mut app = inbox();
        let mut view = View::All;
        for expected in [View::Usage, View::Archive, View::All] {
            view = view.next(false);
            assert!(view == expected);
            assert!(view.next(true).next(false) == view);
        }
        app.set_view(View::Usage);
        assert!(app.rows.is_empty());
        assert_eq!(app.pick(), "");
        assert!(!app.can_pin() && !app.can_snooze() && !app.armable());
        app.paste("rex-data:usage\n{\"fetched\":1,\"samples\":[],\"quotas\":[],\"warnings\":[\"remote host unavailable\"]}");
        app.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        for (width, height) in [(120, 35), (32, 10), (12, 5)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            assert_eq!(app.pick(), "");
        }
        app.set_view(View::All);
        assert!(!app.rows.is_empty());
    }

    #[test]
    fn long_tab_names_stop_at_a_third_of_the_row() {
        let mut app = App::new((String::new(), String::new()), View::All);
        let long = "x".repeat(200);
        app.paste(&format!("rex-data:rows\nS\t-\tlocal\ts\tdemo\nT\tw\t\t{long}\t~/dev/src/project\n"));
        let mut terminal = Terminal::new(TestBackend::new(100, 3)).unwrap();
        terminal.draw(|f| app.draw_list(f, f.area())).unwrap();
        let buf = terminal.backend().buffer();
        let row: String = (0..100).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        let shown = row.matches('x').count();
        assert!(row.contains('…') && row.contains("~/dev/src/project") && (24..=34).contains(&shown), "{row}");
    }

    /// Set in the child run of `split_paste_reader`.
    const PASTE_CHILD: &str = "SWITCHYARD_PASTE_CHILD";

    /// The float's side of `split_tasks_paste_over_64k_arrives_whole`: read
    /// one paste from the terminal on stdin, as `run` does, and report what
    /// the picker made of it.
    #[test]
    fn split_paste_reader() {
        if std::env::var_os(PASTE_CHILD).is_none() {
            return;
        }
        ratatui::crossterm::terminal::enable_raw_mode().unwrap();
        println!("ready");
        loop {
            if let Event::Paste(text) = event::read().unwrap() {
                let mut app = App::new((String::new(), String::new()), View::All);
                app.paste(&text);
                println!("result bytes={} tasks={} ready={}", text.len(), app.tasks.len(), matches!(app.tasks_load, Tasks::Ready));
                break;
            }
        }
        ratatui::crossterm::terminal::disable_raw_mode().unwrap();
    }

    /// Rex drops a terminal write over about 64 KiB, so float.lua's deliver
    /// types a big paste in several writes; the reader must join them.
    #[test]
    fn split_tasks_paste_over_64k_arrives_whole() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::fd::{FromRawFd, OwnedFd};
        use std::process::{Command, Stdio};

        let tasks: Vec<_> = (0..400)
            .map(|i| serde_json::json!({"session_id":"s", "window_id":format!("w{i}"), "name":format!("task {i} ✓"),
                "status":"idle", "status_msg":"x".repeat(150)}))
            .collect();
        let body = serde_json::Value::Array(tasks).to_string();
        assert!(body.len() > 64 * 1024);

        let (mut master, mut slave) = (0, 0);
        let r = unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) };
        assert_eq!(r, 0, "openpty");
        let (mut master, slave) = unsafe { (std::fs::File::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        // Kill the child even when an assertion fails, or it waits forever.
        struct Kill(std::process::Child);
        impl Drop for Kill {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::split_paste_reader", "--nocapture", "--test-threads=1"])
            .env(PASTE_CHILD, "1")
            .stdin(Stdio::from(slave))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map(Kill)
            .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let out = BufReader::new(child.0.stdout.take().unwrap());
        std::thread::spawn(move || {
            for line in out.lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
        let line = |want: &str| loop {
            let l = rx.recv_timeout(Duration::from_secs(10)).expect("child output");
            // The harness may print its own text ahead on the same line.
            if let Some(at) = l.find(want) {
                break l[at..].to_string();
            }
        };
        line("ready");

        // As deliver writes it: marker and header, 8 KiB chunks, end marker.
        let mut writes = vec![b"\x1b[200~rex-data:tasks\n".to_vec()];
        writes.extend(body.as_bytes().chunks(8 * 1024).map(<[u8]>::to_vec));
        writes.push(b"\x1b[201~".to_vec());
        for w in writes {
            master.write_all(&w).unwrap();
            std::thread::sleep(Duration::from_millis(10));
        }
        let result = line("result");
        assert_eq!(result, format!("result bytes={} tasks=400 ready=true", "rex-data:tasks\n".len() + body.len()));
    }
}

// ---------------------------------------------------------------- main

/// The Archive headings within a project, most actionable first.
const ENDINGS: [&str; 4] = ["left", "merged", "archived", "closed"];

/// How an ended task ended, as an Archive heading: `left` (its worktree is
/// still there, for ctrl-x), `merged`, `archived` (or discarded), or
/// `closed` (its tab closed or worktree went away).
fn ending(t: &Task) -> &'static str {
    match t.ended.as_deref() {
        _ if t.left => "left",
        Some("merged") => "merged",
        _ if t.archived() => "archived",
        _ => "closed",
    }
}

fn run(term: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    loop {
        app.take_fetched();
        app.ask_preview();
        let before = app.now;
        app.now = now();
        // A snooze ran out: back to its group.
        if app.tasks.iter().any(|t| t.snoozed_until > before && t.snoozed_until <= app.now) {
            app.refilter();
        }
        term.draw(|f| app.draw(f))?;
        app.report()?;
        let animate = matches!(app.load, Load::Waiting)
            || matches!(app.tasks_load, Tasks::Waiting | Tasks::Live)
            || app.tabs.iter().any(|t| t.state == State::Working);
        let wait = if animate { Duration::from_millis(90) } else { Duration::from_millis(250) };
        if !event::poll(wait)? {
            continue;
        }
        match event::read()? {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                if app.standalone {
                    if k.code == KeyCode::Esc && app.chooser.is_none() && app.snooze.is_none() {
                        return Ok(());
                    }
                    if app.standalone_key(k, term) {
                        continue;
                    }
                    app.note = None;
                }
                if !app.key(k) {
                    return Ok(());
                }
            }
            Event::Paste(text) if !app.standalone => app.paste(&text),
            _ => {}
        }
    }
}

/// The picker in a float: it opens at once and takes its data from the
/// config's bracketed pastes (see the crate docs). `current` is the
/// SESSION/WINDOW the float was opened from.
pub fn float(current: &str, view: View) -> io::Result<()> {
    let (s, w) = current.split_once('/').unwrap_or((current, ""));
    picker(App::new((s.to_string(), w.to_string()), view))
}

/// The picker from a terminal: it fetches its own tabs and tasks and acts on
/// Enter itself.
pub fn standalone(view: View) -> io::Result<()> {
    standalone::detach_env();
    let mut app = App::new((String::new(), String::new()), view);
    app.standalone = true;
    app.start_feed();
    picker(app)
}

/// Sanitized usage accounting as JSON: across configured hosts, or with
/// `local` this host only (the remote collector protocol).
pub fn usage_json(local: bool) -> io::Result<()> {
    let mut collector = switchyard_core::usage::Collector::default();
    let snapshot = if local {
        let settings = switchyard_core::config::read().map(|c| c.usage).unwrap_or_default();
        collector.local(&settings, &switchyard_core::rex::local_label(), now())
    } else {
        collector.collect()
    };
    println!("{}", serde_json::to_string(&snapshot).map_err(io::Error::other)?);
    Ok(())
}

fn picker(mut app: App) -> io::Result<()> {
    let mut term = ratatui::init();
    // No mouse capture, so the terminal's own selection and copy work.
    let result = run(&mut term, &mut app);
    ratatui::restore();
    // Leave the shell's title as it was before the picks.
    if app.standalone {
        protocol::clear_title();
    }
    result
}
