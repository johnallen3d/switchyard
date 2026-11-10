//! The cmd+k picker for Rex: tabs, agents and the task archive in one float.
//!
//! The config's `tab_picker` action opens this float at once (`--wait`) and
//! a background job on the config host types the data into it as bracketed
//! pastes (see `protocol::DATA`), in any order:
//!
//! - `rows`: every tab across sessions and hosts (format in model.rs),
//!   with a note for each host that couldn't be listed, named in the
//!   header; `error` when no host could.
//! - `live`: `rex-spawn --tasks --live`, the task registry with each open
//!   task's live agent status; never replaces `tasks`.
//! - `tasks`: `rex-spawn --tasks`, the same after the worktree check.
//! - `tasks-error`: the registry couldn't be read. The tabs stay, and the
//!   header says "tabs only".
//!
//! The highlighted row and an armed archive survive each delivery. Tasks
//! link to tabs by (session_id, window_id). Three views, cycled with Tab:
//!
//! - All (default): tabs grouped by session; task tabs show their dirty
//!   count and a profile tint.
//! - Agents (`--view agents`, cmd+i): tabs with live agent status and open
//!   tasks, grouped by status in inbox urgency order.
//! - Archive (ctrl-a): ended tasks, most recent first; ctrl-f hides the
//!   archived ones, leaving those whose tab or worktree went without one.
//!
//! ctrl-o cycles a profile filter (all, personal, work): the registry's
//! profile, else work for a tab under ~/dev/src/amfaro. ctrl-x arms an
//! archive of the highlighted task (open, or ended with its worktree left);
//! with uncommitted changes the config's "ask" types another ctrl-x, which
//! moves on to "are you sure". Any other key disarms.
//!
//! The pick goes out in the terminal title as
//! `rex-tab-picker:ACTION:TARGET/SESSION/WINDOW`. ACTION is `go` (a tab),
//! `task` (a row with an open task, which ctrl-r can review), `resume` (an
//! ended task), or an armed `archive`, `ask` or `force-archive`. TARGET is
//! `-` for the config host, else the hex-encoded host label; task rows with
//! no listed tab use the registry host. The config's mode turns Enter,
//! ctrl-r and Escape into the actions, so in the float this program never
//! talks to Rex. ctrl-l (bound by the config too) opens the highlighted
//! task's main work item; task rows show their links as badges.
//!
//! Run bare from a shell (`rex-tab-picker`, or `rex-inbox` for the Agents
//! view), it works without the float: it fetches the data and acts on its
//! picks itself, staying open after each action (see standalone.rs).

mod model;
mod standalone;

use std::collections::HashMap;
use std::io::{self, IsTerminal, Read};
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use model::{Link, Session, State, Tab, Task, age, failed_hosts, link, now, parse_rows, parse_tasks, urgency, work_dir};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::{DefaultTerminal, Frame};
use switchyard_ui::Palette;
use switchyard_ui::draw::{self, delete_word, highlight, pill, str_width};
use switchyard_ui::fuzzy::score_fields;
use switchyard_ui::glyphs::{ICON_SEARCH, SPINNER, tick};
use switchyard_ui::protocol::{self, Title, hex};

const ICON_SESSION: &str = "\u{f0bdf}"; // nf-md-console_line
const ICON_INBOX: &str = "\u{f0e27}"; // nf-md-inbox_full

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    All,
    Agents,
    Archive,
}

impl View {
    fn next(self, back: bool) -> Self {
        match (self, back) {
            (View::All, false) | (View::Archive, true) => View::Agents,
            (View::Agents, false) | (View::All, true) => View::Archive,
            (View::Archive, false) | (View::Agents, true) => View::All,
        }
    }

    fn name(self) -> &'static str {
        match self {
            View::All => "all tabs",
            View::Agents => "agents",
            View::Archive => "archive",
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
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Item {
    Session(usize),
    /// An Agents status heading; the empty string means no known status.
    Status(&'static str),
    Tab(usize),
    /// An archive row, or an open task with no listed tab.
    Task(usize),
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
}

type Key = (String, String);

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
        link(&mut self.tabs, &self.sessions, &mut self.tasks);
        let want = if self.follow { Some(self.current.clone()) } else { keep.clone() };
        self.refilter_to(want.clone());
        // Following the current tab that isn't shown yet: keep the old row.
        if self.follow && self.selected_key() != want {
            self.select_key(keep.clone());
        }
        self.armed = if self.selected_key() == keep && self.armable() { armed } else { Armed::No };
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
            Item::Session(_) | Item::Status(_) => None,
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
            Item::Session(_) | Item::Status(_) => None,
            Item::Tab(t) => self.tabs[t].task.map(|k| &self.tasks[k]),
            Item::Task(k) => Some(&self.tasks[k]),
        }
    }

    fn selected_task(&self) -> Option<&Task> {
        self.selected_item().and_then(|i| self.task_of(i))
    }

    /// Whether ctrl-x can arm an archive for the highlighted row: an open
    /// task, or an ended one that left its worktree behind.
    fn armable(&self) -> bool {
        self.selected_task().is_some_and(|t| t.ended.is_none() || t.left)
    }

    fn is_current(&self, t: usize) -> bool {
        let tab = &self.tabs[t];
        self.sessions[tab.session].id == self.current.0 && tab.window_id == self.current.1
    }

    fn tab_work(&self, t: usize) -> bool {
        match self.tabs[t].task {
            Some(k) => self.tasks[k].work(),
            None => work_dir(&self.tabs[t].dir),
        }
    }

    fn profile_passes(&self, work: bool) -> bool {
        match self.profile {
            Profile::All => true,
            Profile::Personal => !work,
            Profile::Work => work,
        }
    }

    /// The Agents view's rows, before search: tabs with live agent status
    /// or an open task, and open tasks with no listed tab. This is the one
    /// place that says what an agent is.
    fn agents(&self) -> Vec<Item> {
        let tabs = (0..self.tabs.len())
            .filter(|&t| self.tabs[t].state != State::Idle || self.tabs[t].task.is_some())
            .map(Item::Tab);
        let orphans = (0..self.tasks.len())
            .filter(|&k| self.tasks[k].ended.is_none() && self.tasks[k].tab.is_none())
            .map(Item::Task);
        let mut rows: Vec<Item> = tabs.chain(orphans).collect();
        // Most urgent first and, within a state, the one waiting longest
        // first (unknown times last).
        rows.sort_by_key(|&i| {
            let f = self.flat(i);
            (std::cmp::Reverse(urgency(f.state)), if f.since == 0 { u64::MAX } else { f.since })
        });
        rows
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
                    },
                }
            }
            // Headers aren't flat rows.
            Item::Session(_) | Item::Status(_) => Flat {
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
            View::All => self.filter_grouped(&pattern, filtering),
            View::Agents => {
                let rows = self.agents();
                self.filter_flat(rows, &pattern, filtering)
            }
            View::Archive => {
                let rows = self.archive();
                self.filter_flat(rows, &pattern, filtering)
            }
        };
        let want = if filtering { best.and_then(|i| self.key_of(i)) } else { want };
        self.select_key(want);
        self.offset = 0;
    }

    /// All: tabs grouped under their sessions. Returns the best match.
    fn filter_grouped(&mut self, pattern: &Pattern, filtering: bool) -> Option<Item> {
        let mut best: Option<(u32, Item)> = None;
        for s in 0..self.sessions.len() {
            let label = self.sessions[s].label.clone();
            let host = self.sessions[s].host.clone();
            let shown: Vec<usize> =
                self.sessions[s].tabs.iter().copied().filter(|&t| self.profile_passes(self.tab_work(t))).collect();
            let mut matched = Vec::new();
            for &t in &shown {
                if !filtering {
                    matched.push(t);
                    continue;
                }
                let (name, dir) = (self.tabs[t].name.clone(), self.tabs[t].dir.clone());
                if let Some((score, f)) = score_fields(&mut self.matcher, pattern, &[&name, &dir, &label]) {
                    self.hits.insert(Item::Tab(t), Hits { name: f[0].clone(), second: f[1].clone() });
                    if best.is_none_or(|(b, _)| score > b) {
                        best = Some((score, Item::Tab(t)));
                    }
                    matched.push(t);
                }
            }
            // A search that matches a session's name or host shows its tabs.
            let session_hit =
                if filtering { score_fields(&mut self.matcher, pattern, &[&label, &host]) } else { None };
            if let Some((score, f)) = session_hit {
                self.hits.insert(Item::Session(s), Hits { name: f[0].clone(), second: Vec::new() });
                if matched.is_empty() {
                    matched = shown.clone();
                }
                if let Some(&first) = matched.first()
                    && best.is_none_or(|(b, _)| score > b)
                {
                    best = Some((score, Item::Tab(first)));
                }
            }
            if matched.is_empty() {
                continue;
            }
            self.rows.push(Item::Session(s));
            self.rows.extend(matched.into_iter().map(Item::Tab));
        }
        best.map(|(_, i)| i)
    }

    /// Agents and Archive: rows in the given order, with status headings
    /// for matching Agents only. Matches project (or session), name, host
    /// and the task's work items (`#42`, `card 1041`, `PR #57`), so an
    /// ended task can be found by its issue or card and resumed. Returns
    /// the best match.
    fn filter_flat(&mut self, rows: Vec<Item>, pattern: &Pattern, filtering: bool) -> Option<Item> {
        let mut best: Option<(u32, Item)> = None;
        let mut group = None;
        for item in rows {
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
            if self.view == View::Agents {
                let state = match self.flat(item).state {
                    "blocked" => "blocked",
                    "error" => "error",
                    "done" => "done",
                    "working" => "working",
                    "idle" => "idle",
                    _ => "",
                };
                if group != Some(state) {
                    self.rows.push(Item::Status(state));
                    group = Some(state);
                }
            }
            self.rows.push(item);
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
        self.view = view;
        self.refilter();
    }

    // ------------------------------------------------------------ the pick

    /// What Enter would do, as the config's title protocol expects.
    fn pick(&self) -> String {
        let Some(item) = self.selected_item() else { return String::new() };
        let task = self.task_of(item);
        let dirty = task.is_some_and(|t| t.dirty > 0);
        let action = match self.armed {
            Armed::Archive if dirty => "ask",
            Armed::Archive => "archive",
            Armed::Force => "force-archive",
            Armed::No => match task {
                Some(t) if t.ended.is_some() => "resume",
                Some(_) => "task",
                None => "go",
            },
        };
        match item {
            Item::Session(_) | Item::Status(_) => String::new(),
            Item::Tab(t) => {
                let tab = &self.tabs[t];
                let s = &self.sessions[tab.session];
                format!("{action}:{}/{}/{}", s.target, s.id, tab.window_id)
            }
            Item::Task(k) => {
                let t = &self.tasks[k];
                format!("{action}:{}/{}/{}", hex(&t.host), t.session_id, t.window_id)
            }
        }
    }

    fn report(&mut self) -> io::Result<()> {
        let pick = self.pick();
        self.title.report(pick)
    }

    // ------------------------------------------------------------ keys

    fn key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let page = (self.list_area.height as isize - 2).max(1);
        // ctrl-x arms an archive; on a worktree with uncommitted changes, the
        // next ctrl-x (typed by the config when Enter answers `ask`) moves on
        // to "are you sure". Enter and Escape never get here.
        if matches!(k.code, KeyCode::Char('x') if ctrl) {
            let dirty = self.selected_task().is_some_and(|t| t.dirty > 0);
            self.armed = match self.armed {
                _ if !self.armable() => Armed::No,
                Armed::No => Armed::Archive,
                Armed::Archive | Armed::Force if dirty => Armed::Force,
                armed => armed,
            };
            return true;
        }
        self.armed = Armed::No;
        match k.code {
            KeyCode::Char('c') if ctrl => return false,
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
            KeyCode::Char(c) if !ctrl && !alt => {
                self.query.push(c);
                self.moved();
                self.refilter();
            }
            KeyCode::Up if alt => self.jump_group(false),
            KeyCode::Down if alt => self.jump_group(true),
            KeyCode::Up => self.step(-1),
            KeyCode::Down => self.step(1),
            KeyCode::PageUp => self.step(-page),
            KeyCode::PageDown => self.step(page),
            KeyCode::Home => self.edge(false),
            KeyCode::End => self.edge(true),
            KeyCode::Tab => self.set_view(self.view.next(false)),
            KeyCode::BackTab => self.set_view(self.view.next(true)),
            _ => {}
        }
        true
    }

    fn delete_word(&mut self) {
        delete_word(&mut self.query);
        self.refilter();
    }

    // ------------------------------------------------------------ drawing

    fn draw(&mut self, f: &mut Frame) {
        let title = match self.view {
            View::All => "Go to tab",
            View::Agents => "Agents",
            View::Archive => "Archive",
        };
        let inner = draw::frame(f, &self.p, title, self.header_chips(), "⌘K", Margin::new(2, 1));
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
        self.draw_list(f, body);
        self.draw_footer(f, footer);
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
            View::All => "Search tabs, sessions, and folders",
            View::Agents => "Search agents",
            View::Archive => "Search ended tasks",
        };
        draw::search(f, p, area, &self.query, placeholder, typing, true);
        let shown = self.rows.iter().filter(|r| selectable(r)).count();
        let mut right = Vec::new();
        let spin = SPINNER[tick(self.started) % SPINNER.len()];
        match self.tasks_load {
            Tasks::Waiting => right.push(Span::styled(format!("{spin} loading tasks  "), Style::new().fg(p.muted))),
            Tasks::Live => right.push(Span::styled(format!("{spin} checking worktrees  "), Style::new().fg(p.muted))),
            _ => {}
        }
        if self.view == View::Agents {
            // Counts by what needs attention.
            let count = |st: &str| {
                self.rows.iter().filter(|&&i| selectable(&i) && self.flat(i).state == st).count()
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
        } else {
            let (total, kind) = match self.view {
                View::Archive => (self.tasks.iter().filter(|t| t.ended.is_some()).count(), "ended"),
                _ => (self.tabs.len(), "tabs"),
            };
            if typing || shown != total {
                right.push(Span::styled(shown.to_string(), Style::new().fg(p.text)));
                right.push(Span::styled(format!(" of {total} {kind} "), Style::new().fg(p.muted)));
            } else {
                right.push(Span::styled(format!("{total} {kind} "), Style::new().fg(p.muted)));
            }
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
        // Flat rows line up their project and name columns: size them for
        // the widest values, within what the busiest right side leaves.
        let cols = if self.view == View::All {
            (0, 0)
        } else {
            let shown = || self.rows.iter().filter(|i| selectable(i)).map(|&i| self.flat(i));
            let right_w = shown().map(|t| self.right_spans(&t, self.p.base, "").1).max().unwrap_or(0);
            let room = (list.width as usize).saturating_sub(3 + right_w + 1);
            let proj_w = shown().map(|t| str_width(t.project)).max().unwrap_or(0).min(room / 4);
            let name_w =
                shown().map(|t| str_width(t.name)).max().unwrap_or(0).min(room.saturating_sub(proj_w + 2) * 2 / 3);
            (proj_w, name_w)
        };
        for (i, row) in self.rows.iter().enumerate().skip(self.offset).take(h) {
            let r = Rect { y: area.y + (i - self.offset) as u16, height: 1, ..list };
            let selected = self.selected == Some(i);
            if let Item::Status(state) = row {
                self.draw_status_header(f, r, state);
            } else if self.view == View::All {
                let last = !self.rows.get(i + 1).is_some_and(selectable);
                self.draw_grouped_row(f, r, row, selected, last, tick);
            } else {
                self.draw_flat_row(f, r, *row, selected, tick, cols);
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
                View::Agents => {
                    let waiting = matches!(self.load, Load::Waiting) || matches!(self.tasks_load, Tasks::Waiting);
                    if waiting {
                        vec![spin(), Line::from(""), text("Loading agents…".into())]
                    } else if let Some(profile) = filtered {
                        vec![
                            icon(ICON_INBOX, p.hl_high),
                            Line::from(""),
                            text(format!("No {profile} agents")),
                            draw::key_hint(p, "⌃O", " changes profile"),
                        ]
                    } else {
                        vec![
                            icon(ICON_INBOX, p.hl_high),
                            Line::from(""),
                            text("No agents running".into()),
                            draw::key_hint(p, "⌃⌘N", " spawns one"),
                        ]
                    }
                }
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
                        draw::key_hint(p, "⇥", " shows all tabs"),
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
        let doomed = selected && self.armed != Armed::No;
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
            // Task and status rows only appear in the Agents/Archive views.
            Item::Task(_) | Item::Status(_) => {}
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
                let name_room = width.saturating_sub(lead + dir_w + dirty_w + 3 + here_w);
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

    /// A flat row's work items, such as `#42 · PR #57 ✓ merged`: at most
    /// two links (PRs first) and a count of the rest, or `merged` for a
    /// branch in the default branch with no PR. Empty for none.
    fn badge_spans(&self, t: &Flat, bg: Color) -> Vec<Span<'static>> {
        let p = &self.p;
        let mut spans: Vec<Span<'static>> = Vec::new();
        let dot = || Span::styled(" · ", Style::new().fg(p.muted).bg(bg));
        for link in t.links.iter().take(2) {
            if !spans.is_empty() {
                spans.push(dot());
            }
            spans.push(Span::styled(link.short(), Style::new().fg(p.subtle).bg(bg)));
            let state = match (link.state.as_str(), link.checks.as_str()) {
                ("merged", _) => Some(("✓ merged", p.iris)),
                ("closed", _) => Some(("closed", p.muted)),
                ("draft", _) => Some(("draft", p.muted)),
                (_, "pass") => Some(("✓", p.pine)),
                (_, "fail") => Some(("✗", p.love)),
                (_, "pending") => Some(("●", p.gold)),
                _ => None,
            };
            if let Some((text, color)) = state {
                spans.push(Span::styled(format!(" {text}"), Style::new().fg(color).bg(bg)));
            }
        }
        if t.links.len() > 2 {
            spans.push(Span::styled(format!(" +{}", t.links.len() - 2), Style::new().fg(p.muted).bg(bg)));
        }
        if t.merged == "merged" && !t.links.iter().any(|l| l.kind == "pr") {
            if !spans.is_empty() {
                spans.push(dot());
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
    fn right_spans(&self, t: &Flat, bg: Color, verb: &str) -> (Vec<Span<'static>>, usize) {
        let p = &self.p;
        let mut right: Vec<Span> = self.badge_spans(t, bg);
        if t.dirty > 0 {
            right.push(Span::styled(format!("✎{}  ", t.dirty), Style::new().fg(p.gold).bg(bg)));
        }
        let chip_fg = if t.work { p.gold } else { p.subtle };
        right.extend(pill(vec![Span::styled(t.host.to_string(), Style::new().fg(chip_fg))], p.surface, bg));
        let when = if t.since > 0 { format!(" {}", age(self.now.saturating_sub(t.since))) } else { String::new() };
        right.push(Span::styled(format!("  {verb:>9}{when:>4} "), Style::new().fg(p.muted).bg(bg)));
        let w = right.iter().map(|s| str_width(&s.content)).sum();
        (right, w)
    }

    fn draw_flat_row(&self, f: &mut Frame, r: Rect, item: Item, selected: bool, tick: usize, cols: (usize, usize)) {
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
        // Right side first, so the columns and message get what is left.
        let (right, right_w) = self.right_spans(&t, bg, verb);
        let mut left = vec![edge, Span::styled(format!("{glyph} "), Style::new().fg(color).bg(bg))];
        let room = (r.width as usize).saturating_sub(3 + right_w + 1);
        let (proj_w, name_w) = cols;
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
        column(t.project, &hits.second, proj_w, Style::new().fg(p.subtle).bg(bg));
        column(t.name, &hits.name, name_w, name_style);
        let msg_room = room.saturating_sub(proj_w + name_w + 4);
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
        if let (true, Some(t)) = (self.armed != Armed::No, self.selected_task()) {
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
        let task = self.selected_task();
        let mut keys: Vec<(&str, &str)> = match task {
            Some(t) if t.ended.is_some() && t.left && t.safe() => vec![("⏎", "resume"), ("⌃X", "remove worktree (merged)")],
            Some(t) if t.ended.is_some() && t.left => vec![("⏎", "resume"), ("⌃X", "remove worktree")],
            Some(t) if t.ended.is_some() => vec![("⏎", "resume")],
            Some(t) if t.safe() => vec![("⏎", "go"), ("⌃R", "review"), ("⌃X", "archive (merged)")],
            Some(_) => vec![("⏎", "go"), ("⌃R", "review"), ("⌃X", "archive")],
            None => vec![("⏎", "go")],
        };
        // ⌃L opens the task's main work item (core's `links::primary`: the
        // first kind with a URL, PRs first).
        if let Some(link) = task.and_then(|t| t.links.iter().find(|l| !l.url.is_empty())) {
            keys.push(("⌃L", if link.kind == "pr" { "open PR" } else { "open link" }));
        }
        keys.push(("⇥", self.view.next(false).name()));
        keys.push(("⌃O", self.profile.next().name()));
        match self.view {
            View::All => keys.push(("⌥↑↓", "session")),
            View::Archive => keys.push(("⌃F", if self.kept { "show archived" } else { "hide archived" })),
            View::Agents => {
                keys.push(("⌥↑↓", "status"));
                keys.push(("⌃A", "archive"));
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
        if !review && !open && !matches!(k.code, KeyCode::Enter) {
            return false;
        }
        self.note = None;
        let Some(pick) = standalone::parse_pick(&self.pick()) else { return true };
        let label = self.selected_label();
        // Open the task's main work item; nothing to refresh after.
        if open {
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
        let mut app = App::new((String::new(), String::new()), View::Agents);
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
    fn inbox_groups_by_status_and_preserves_urgency_and_age_order() {
        let app = inbox();
        assert_eq!(app.rows, vec![
            Item::Status("blocked"), Item::Task(1), Item::Task(2), Item::Tab(1),
            Item::Status("error"), Item::Tab(2),
            Item::Status("done"), Item::Tab(3),
            Item::Status("working"), Item::Tab(0),
            Item::Status("idle"), Item::Tab(4),
            Item::Status(""), Item::Task(3), Item::Task(4),
        ]);
        assert_eq!(app.selected_item(), Some(Item::Task(1)));
    }

    #[test]
    fn inbox_headings_follow_search_and_profile_filters() {
        let mut app = inbox();
        app.query = "older".into();
        app.refilter();
        assert_eq!(app.rows, vec![Item::Status("blocked"), Item::Task(1)]);
        assert_eq!(app.selected_item(), Some(Item::Task(1)));
        app.query.clear();
        app.profile = Profile::Work;
        app.refilter();
        assert_eq!(app.rows, vec![Item::Status("blocked"), Item::Task(2)]);
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
        app.selected = Some(4);
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
        assert_eq!(app.rows[at - 1], Item::Status("working"));
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
        let mut app = App::new((String::new(), String::new()), View::Agents);
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
        assert!(text.contains("PR #57 ✓ merged · #42"), "{text}");
        assert!(text.contains("merged"));
        let mut terminal = Terminal::new(TestBackend::new(160, 3)).unwrap();
        terminal.draw(|f| app.draw_footer(f, f.area())).unwrap();
        let text: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
        assert!(text.contains("open PR"), "{text}");
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
        assert_eq!(app.rows, vec![Item::Task(0)]);
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
        app.set_view(View::All);
        assert_eq!(app.rows, vec![
            Item::Session(0), Item::Tab(0), Item::Tab(1), Item::Tab(2), Item::Tab(3), Item::Tab(4),
        ]);
        app.set_view(View::Archive);
        assert_eq!(app.rows, vec![Item::Task(5)]);
    }
}

// ---------------------------------------------------------------- main

const USAGE: &str = "usage: rex-tab-picker [--current SESSION/WINDOW] [--view all|agents|archive] [ROWS | - | --wait]

Shows Rex tabs grouped by session, the agents among them, and ended tasks,
and reports the highlighted row in the terminal title as
rex-tab-picker:ACTION:TARGET/SESSION/WINDOW. ROWS are the tab separated rows
built by the Rex config; `-` or no argument reads stdin. --wait opens at
once and takes the data from bracketed pastes of rex-data:KIND, a newline
and the body: KIND rows (or error and a message), live and tasks
(rex-spawn --tasks JSON), or tasks-error.

Run bare from a terminal (or as rex-inbox, on the agents view), it gets the
tabs and tasks itself, from switchyard-server as they change when it runs,
else directly every few seconds, and acts on Enter, ctrl-r and ctrl-l (open
the task's work item), staying open until Escape or ctrl-c.";

fn run(term: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    loop {
        app.take_fetched();
        app.now = now();
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
                    if k.code == KeyCode::Esc {
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

fn main() -> io::Result<()> {
    let mut args = std::env::args();
    // rex-inbox is this binary under another name: standalone, on Agents.
    let inbox = args.next().is_some_and(|a| a.rsplit('/').next() == Some("rex-inbox"));
    let mut current = (String::new(), String::new());
    let mut view = if inbox { View::Agents } else { View::All };
    let mut rows: Option<String> = None;
    let mut stdin = false;
    let mut wait = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "--current" => {
                let v = args.next().unwrap_or_default();
                let (s, w) = v.split_once('/').unwrap_or((&v, ""));
                current = (s.to_string(), w.to_string());
            }
            "--view" => {
                view = match args.next().as_deref() {
                    Some("all") => View::All,
                    Some("agents") => View::Agents,
                    Some("archive") => View::Archive,
                    _ => {
                        eprintln!("{USAGE}");
                        std::process::exit(2);
                    }
                }
            }
            "--wait" => wait = true,
            "-" => {
                rows = None;
                stdin = true;
            }
            other => rows = Some(other.to_string()),
        }
    }
    // Nothing to read: the standalone picker.
    let standalone = !wait && rows.is_none() && !stdin && (inbox || io::stdin().is_terminal());
    let input = match rows {
        _ if wait || standalone => None,
        Some(r) => Some(r),
        None => {
            let mut s = String::new();
            io::stdin().read_to_string(&mut s)?;
            Some(s)
        }
    };
    if standalone {
        standalone::detach_env();
    }
    let mut app = App::new(current, view);
    if standalone {
        app.standalone = true;
        app.start_feed();
    }
    if let Some(input) = input {
        app.tasks_load = Tasks::Failed;
        app.paste(&format!("{}rows\n{input}", protocol::DATA));
    }
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
