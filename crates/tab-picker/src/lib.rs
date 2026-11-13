//! The tab picker for Rex: tabs, agents and the task archive, run bare from
//! a shell (`switchyard`). cmd+i in the Rex config shows its tab in the
//! Mac's agents session, opening it there when there is none (see
//! lua/switchyard/tab-picker.lua). cmd+k opens it with `--quick` in a
//! float over the current tab instead: Inbox's list alone, no sidebar,
//! preview or reply box, where Enter jumps to the highlighted tab and
//! Escape closes it, quitting either way (see `quick`).
//!
//! It fetches its own data in the background (see standalone.rs), which
//! arrives as the data pastes `protocol::DATA` describes, in any order:
//!
//! - `rows`: every tab across sessions and hosts (format in model.rs),
//!   with a note for each host that couldn't be listed, named in the
//!   header; `error` when no host could.
//! - `live`: the task registry with each open task's live agent status;
//!   never replaces `tasks`.
//! - `tasks`: the same after the worktree check.
//! - `tasks-error`: the registry couldn't be read. The tabs stay, and the
//!   header says "tabs only".
//! - `usage` and `usage-error`: accounting for the Usage view.
//! - `preview`: the highlighted agent's transcript (see preview.rs).
//!
//! cmd+i also pastes `current`, SESSION/WINDOW, the tab it was pressed on,
//! into the terminal (or passes it as `--current` when opening the picker),
//! and the picker highlights that tab again.
//!
//! The highlighted row and an armed archive survive each delivery. Tasks
//! link to tabs by (session_id, window_id). Views, moved along with ctrl-h
//! and ctrl-l:
//!
//! - Inbox (default): one flat list across every session, each row naming
//!   its project. Agents (tabs with live agent status or an open task, and
//!   open tasks with no listed tab) go under status headings in urgency
//!   order (Waiting, Failed, Done, Working, Idle, No status). Plain tabs
//!   with no agent are left out, as is the picker's own `switchyard` tab;
//!   the quick float (cmd+k) lists them under Tabs.
//!   Pinned tasks lead the list and Snoozed ones trail it.
//!   With the width, the list is a sidebar on the left (search over it,
//!   compact rows), and the right side shows the highlighted row: its name,
//!   work items and host over its whole status message, the agent's
//!   transcript, live, and the reply box. ctrl-e collapses the sidebar, and
//!   typing to search brings it back. Narrower, a preview under the list
//!   shows the transcript, and ctrl-e expands it over the list.
//! - Usage: read-only accounting (see usage.rs).
//! - Archive (ctrl-a): one flat list of ended tasks, newest first, each
//!   row naming its project and how it ended; ctrl-f hides the archived
//!   ones, leaving those whose tab or worktree went without one.
//!
//! `?` (F1 where `?` is text) opens a float of every key, filtered with
//! `/` (see help.rs); the footer keeps outcomes and confirmations.
//!
//! ctrl-o cycles a profile filter (all, personal, work): the registry's
//! profile, else work for a tab under ~/dev/src/amfaro. ctrl-x arms an
//! archive of the highlighted task (open, or ended with its worktree left)
//! or of any agent tab, even one with no open task (an agent at its prompt
//! included); elsewhere it says in the footer why it can't. Enter runs it;
//! with uncommitted changes Enter first moves on to "are you sure". Any
//! other key disarms.
//!
//! Enter jumps to the highlighted tab (or resumes an ended task's agent in
//! a new tab), and ctrl-t pins or unpins the highlighted open task (`pinned` in the registry),
//! which puts it in a Pinned group at the top of Inbox. ctrl-s, in Inbox, opens
//! a prompt for when to wake the task (see snooze.rs); Enter snoozes it
//! into the Snoozed group until then (`snoozed_until`), and Enter on an
//! empty prompt wakes a snoozed one. ctrl-b opens the highlighted task's
//! work item; task rows show their links as badges grouped by kind. With
//! several it opens a chooser over them (PRs, cards, then issues).
//!
//! Inbox keeps a reply box at its bottom, under the preview (or the
//! transcript), expanded or not:
//! the new-session picker's prompt box (see the ui crate's prompt_box),
//! with its rounded corners. Tab moves into it to message the highlighted
//! agent; alt- or shift-Enter adds a line, Escape (or Tab, while it's
//! empty) leaves it, and shift with the arrows (or the page keys) scrolls
//! the preview. An agent waiting on a prompt is refused. Enter sends the
//! message (see core's actions).
//!
//! Each action goes through `pick`, the highlighted row as
//! `ACTION:TARGET/SESSION/WINDOW`: ACTION is `go` (a tab), `task` (a row
//! with an open task), `resume` (an ended task), or an armed `archive`,
//! `ask` or `force-archive`; TARGET is `-` for this host, else the
//! hex-encoded host label. The picker stays open after each action,
//! showing its outcome. No key quits it: it's a tab, closed like any
//! other (cmd+w).

mod help;
mod model;
mod preview;
mod scrollback;
mod snooze;
mod standalone;
mod usage;

use std::collections::HashMap;
use std::io;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use model::{
    Link, Session, State, Tab, Task, Tone, age, badges, chooser_links, failed_hosts, in_folders, link, now,
    parse_rows, parse_tasks, resolve_folders, urgency, work_dir,
};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use switchyard_ui::Palette;
use switchyard_ui::chrome;
use switchyard_ui::draw::{self, delete_word, highlight, pill, str_width};
use switchyard_ui::fuzzy::score_fields;
use switchyard_ui::glyphs::{ICON_SEARCH, SPINNER, tick};
use switchyard_ui::prompt_box::{self, PromptBox};
use switchyard_ui::protocol::{self, hex};

const ICON_SESSION: &str = "\u{f0bdf}"; // nf-md-console_line
const ICON_INBOX: &str = "\u{f0e27}"; // nf-md-inbox_full
const ICON_PIN: &str = "\u{f0403}"; // nf-md-pin
const ICON_SNOOZE: &str = "\u{f04b2}"; // nf-md-sleep

/// Inbox's sections, in order: Pinned, agents by status (the empty string is
/// no known status), the tabs with no agent (quick float only), then Snoozed.
const SECTIONS: [&str; 9] = ["pinned", "blocked", "error", "done", "working", "idle", "", "tabs", "snoozed"];

/// How long a pin or snooze made here holds over deliveries that don't
/// have it yet (the registry is saved, but the data was read before).
const PENDING_TTL: Duration = Duration::from_secs(15);

/// The label cmd+i gives the picker's own tab (lua/switchyard/tab-picker.lua).
const PICKER_TAB: &str = "switchyard";

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

/// The picker's views, cycled with ctrl-h and ctrl-l.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Inbox,
    Usage,
    Archive,
}

impl View {
    /// `inbox` (or the older `all`), `usage` or `archive`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "inbox" | "all" => Some(View::Inbox),
            "archive" => Some(View::Archive),
            "usage" => Some(View::Usage),
            _ => None,
        }
    }

    fn next(self, back: bool) -> Self {
        match (self, back) {
            (View::Inbox, false) | (View::Archive, true) => View::Usage,
            (View::Usage, false) | (View::Inbox, true) => View::Archive,
            (View::Archive, false) | (View::Usage, true) => View::Inbox,
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

impl Armed {
    fn archives(self) -> bool {
        matches!(self, Armed::Archive | Armed::Force)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Item {
    /// A heading over a section of Inbox: `pinned`, `snoozed`, an agent
    /// status (the empty string means no known status), or `tabs` for the
    /// tabs with no agent (quick float only).
    Status(&'static str),
    Tab(usize),
    /// An archive row, or an open task with no listed tab.
    Task(usize),
}

/// Highlighted character positions: the name, the project column, and a
/// tab's folder.
#[derive(Default, Clone)]
struct Hits {
    name: Vec<usize>,
    second: Vec<usize>,
    dir: Vec<usize>,
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

/// The ctrl-b chooser over a task's links (see model's `chooser_links`),
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

/// The reply box, while it has focus: a message for the highlighted agent, typed in the
/// prompt box the new-session picker uses and kept in a prompt file, which
/// `switchyard task act message` sends.
struct Reply {
    /// The agent's tab, as (session_id, window_id), which stays highlighted.
    key: Key,
    label: String,
    prompt: PromptBox,
}

/// Text rows the reply box grows between; longer messages scroll.
const REPLY_MIN_ROWS: u16 = 2;
const REPLY_MAX_ROWS: u16 = 6;

/// The narrowest content Inbox splits into the sidebar and the highlighted
/// row, and the sidebar's width range (three tenths of the content).
const SIDEBAR_MIN_WIDTH: u16 = 90;
const SIDEBAR_MIN: u16 = 30;
const SIDEBAR_MAX: u16 = 46;

/// Lines a tab or task takes in the sidebar.
const SIDE_ROW_LINES: usize = 2;

/// Lines of status message the right side's header shows at most.
const DETAIL_MSG_ROWS: usize = 3;

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
    /// The open ctrl-b link chooser; Enter opens its pick, Escape closes it.
    chooser: Option<Chooser>,
    /// The open ctrl-s snooze prompt; Enter snoozes, Escape closes it.
    snooze: Option<Snooze>,
    /// The reply box while Tab has moved into it; Enter sends, Escape
    /// leaves it. Unfocused, it is drawn from `idle`.
    reply: Option<Reply>,
    /// The reply box as drawn while it doesn't have focus.
    idle: PromptBox,
    /// Smooth prompt-box corners, when the terminal does kitty graphics, and
    /// the cell size they were drawn at, to draw them again after an editor.
    corners: Option<chrome::Corners>,
    cell: Option<chrome::CellSize>,
    /// Whether ctrl-g asked for the reply box's message in `$EDITOR`.
    edit_requested: bool,
    /// Pins and snoozes made here, by (host, window_id): (pinned,
    /// snoozed_until, when), kept over deliveries until they agree.
    pending: HashMap<Key, (bool, u64, Instant)>,
    list_area: Rect,
    started: Instant,
    now: u64,
    load: Load,
    /// Hosts whose tabs the last rows lack or have from earlier.
    failed: Vec<String>,
    tasks_load: Tasks,
    /// The last standalone action's outcome (and whether it failed), shown
    /// in the footer until the next key.
    note: Option<(String, bool)>,
    /// The picker's data, as pastes (see standalone::Feed).
    feed: Option<standalone::Feed>,
    /// A standalone archive running on its own thread, so the picker stays
    /// live meanwhile; its outcome becomes the note.
    archiving: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    usage: usage::Usage,
    /// The last pane capture (see preview.rs), shown while its row is
    /// highlighted.
    preview: Option<preview::Preview>,
    /// The pane a standalone picker last asked its feed to capture.
    asked: Option<preview::Pane>,
    /// The preview fills the picker (ctrl-e), for reading: with the
    /// sidebar, it is collapsed.
    expanded: bool,
    /// Inbox is drawn as the sidebar and the highlighted row (see
    /// draw_split), as of the last frame.
    sidebar: bool,
    /// How far the preview is scrolled back from its bottom, in lines, and
    /// the pane that is for.
    scroll: (usize, Option<preview::Pane>),
    /// Scrollback over the transcript (ctrl-v; see scrollback.rs), while
    /// it has the keys.
    scrollback: Option<scrollback::Scrollback>,
    /// The transcript's text area as last drawn, which scrollback moves in.
    transcript: Rect,
    /// Text scrollback copied, for the run loop to send to the clipboard.
    copied: Option<String>,
    /// The keys float (`?` or F1; see help.rs), over everything else.
    help: Option<help::Help>,
    /// The cmd+k quick jump float (`--quick`): Inbox's list alone, which
    /// Enter jumps from and Escape closes, quitting either way.
    quick: bool,
    /// Folders the Inbox leaves out (`[inbox] exclude`), resolved.
    excluded: Vec<std::path::PathBuf>,
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
            reply: None,
            idle: PromptBox::new(),
            corners: None,
            cell: None,
            edit_requested: false,
            excluded: Vec::new(),
            pending: HashMap::new(),
            list_area: Rect::default(),
            started: Instant::now(),
            now: now(),
            load: Load::Waiting,
            failed: Vec::new(),
            tasks_load: Tasks::Waiting,
            note: None,
            feed: None,
            archiving: None,
            usage: usage::Usage::default(),
            preview: None,
            asked: None,
            scroll: (0, None),
            scrollback: None,
            transcript: Rect::default(),
            copied: None,
            help: None,
            expanded: false,
            sidebar: false,
            quick: false,
        }
    }

    // ------------------------------------------------------------ data

    /// A bracketed paste: tabs, tasks, or a failure to get either.
    fn paste(&mut self, text: &str) {
        let Some((kind, body)) = protocol::data(text) else {
            // Anything else John pasted goes into the reply box, if open.
            if let Some(r) = &mut self.reply {
                r.prompt.paste(text);
            }
            return;
        };
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
            "current" => self.set_current(body),
            _ => {}
        }
    }

    /// cmd+i came from the tab `current` (SESSION/WINDOW): highlight it, as
    /// a fresh picker would, unless a prompt is open over another row.
    /// Without a tab, just clear the search, keeping the highlighted row.
    fn set_current(&mut self, current: &str) {
        let current = current.trim();
        if current.is_empty() {
            if self.chooser.is_none() && self.snooze.is_none() && self.reply.is_none() {
                self.clear_search();
            }
            return;
        }
        let (s, w) = current.split_once('/').unwrap_or((current, ""));
        self.current = (s.to_string(), w.to_string());
        if self.chooser.is_some() || self.snooze.is_some() || self.reply.is_some() {
            return;
        }
        self.follow = true;
        self.query.clear();
        // cmd+i shows Inbox, where the tab is a row: from Archive or Usage,
        // switch there first, or the highlight lands on nothing visible.
        self.set_view(View::Inbox);
        self.refilter_to(Some(self.current.clone()));
    }

    /// Empty the search, keeping the highlighted row.
    fn clear_search(&mut self) {
        if !self.query.is_empty() {
            self.query.clear();
            self.refilter();
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
        // The reply box stays on its agent, closing only when it's gone.
        if let Some(key) = self.reply.as_ref().map(|r| r.key.clone())
            && self.selected_key().as_ref() != Some(&key)
        {
            self.select_key(Some(key.clone()));
            if self.selected_key().as_ref() != Some(&key) {
                self.close_reply();
            }
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
            Item::Status(_) => None,
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
            Item::Status(_) => None,
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
            Item::Status(_) => None,
            Item::Tab(t) => self.tabs[t].task,
            Item::Task(k) => Some(k),
        }
    }

    /// The highlighted open task, which ctrl-t can pin (not in the archive)
    /// and ctrl-s snooze (in Inbox).
    fn selected_open_task(&self) -> Option<usize> {
        self.selected_item().and_then(|i| self.task_index(i)).filter(|&k| self.tasks[k].ended.is_none())
    }

    fn can_pin(&self) -> bool {
        self.view != View::Archive && self.selected_open_task().is_some()
    }

    fn can_snooze(&self) -> bool {
        self.view == View::Inbox && self.selected_open_task().is_some()
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

    /// Whether ctrl-x can arm an archive for the highlighted row: any agent
    /// tab, whether or not it has an open task (the archive adopts one
    /// without, or reopens an ended one whose tab is still open), an open
    /// task, or an ended one that left its worktree behind.
    fn armable(&self) -> bool {
        self.selected_agent_tab().is_some() || self.selected_task().is_some_and(|t| t.ended.is_none() || t.left)
    }

    /// The highlighted row's tab, when it is an agent with no open task.
    fn selected_agent_tab(&self) -> Option<usize> {
        match self.selected_item()? {
            item @ Item::Tab(t) if self.tabs[t].task.is_none() && self.is_agent(item) => Some(t),
            _ => None,
        }
    }

    /// Whether `armed` still applies to the highlighted row.
    fn can_arm(&self, armed: Armed) -> bool {
        match armed {
            Armed::No => true,
            Armed::Archive | Armed::Force => self.armable(),
        }
    }

    /// The tab whose agent to preview: the highlighted agent's, in Inbox.
    fn preview_want(&self) -> Option<preview::Pane> {
        if self.view != View::Inbox {
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

    /// Whether a row is the picker's own tab: the plain tab cmd+i opens on
    /// the config host, labelled `switchyard` (see
    /// lua/switchyard/tab-picker.lua). Inbox leaves it out, as it is always
    /// there and cmd+i already shows it.
    fn is_picker_tab(&self, item: Item) -> bool {
        let Item::Tab(t) = item else { return false };
        let tab = &self.tabs[t];
        tab.name == PICKER_TAB && self.sessions[tab.session].target == "-" && self.is_plain_tab(item)
    }

    /// Whether a row is a tab with no agent.
    fn is_plain_tab(&self, item: Item) -> bool {
        matches!(item, Item::Tab(_)) && !self.is_agent(item)
    }

    /// Whether Inbox lists a row: never the picker's own tab (cmd+i shows
    /// it already), and a plain tab only in the quick float, which jumps to
    /// any tab.
    fn listed(&self, item: Item) -> bool {
        !self.is_picker_tab(item) && (self.quick || !self.is_plain_tab(item)) && !self.left_out(item)
    }

    /// Whether a tab on the config host sits in a folder `[inbox] exclude`
    /// names, or under one.
    fn left_out(&self, item: Item) -> bool {
        let Item::Tab(t) = item else { return false };
        let tab = &self.tabs[t];
        self.sessions[tab.session].target == "-" && in_folders(&tab.dir, &self.excluded)
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
            Item::Status(_) => Flat {
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
            View::Inbox => self.filter_merged(&pattern, filtering),
            View::Archive => self.filter_archive(&pattern, filtering),
            View::Usage => None,
        };
        let want = if filtering { best.and_then(|i| self.key_of(i)) } else { want };
        self.select_key(want);
        self.offset = 0;
    }

    /// Score an Inbox row against the search, recording its highlights;
    /// None when it doesn't match. Matches name, folder, project, session,
    /// host and work items.
    fn score_row(&mut self, item: Item, pattern: &Pattern) -> Option<u32> {
        let (name, project, host) = {
            let f = self.flat(item);
            (f.name.to_string(), f.project.to_string(), f.host.to_string())
        };
        let dir = match item {
            Item::Tab(t) => self.tabs[t].dir.clone(),
            _ => String::new(),
        };
        let label = self.session_of(item).map(|s| self.sessions[s].label.clone()).unwrap_or_default();
        let links = self.task_of(item).map(Task::links_text).unwrap_or_default();
        let (score, f) = score_fields(&mut self.matcher, pattern, &[&name, &dir, &project, &label, &host, &links])?;
        self.hits.insert(item, Hits { name: f[0].clone(), second: f[2].clone(), dir: f[1].clone() });
        Some(score)
    }

    /// The Inbox section a row goes in (see `SECTIONS`): an agent's group,
    /// or `tabs` for a tab with no agent.
    fn section(&self, item: Item) -> &'static str {
        if self.is_agent(item) { self.agent_group(item) } else { "tabs" }
    }

    /// Inbox: one flat list across every session, in sections under
    /// headings (see `SECTIONS`). Agents go by urgency and age within
    /// theirs (see `agent_key`); tabs with no agent keep Rex's order. Each
    /// row carries its project, so no session headings are needed. Returns
    /// the best match.
    fn filter_merged(&mut self, pattern: &Pattern, filtering: bool) -> Option<Item> {
        let mut best: Option<(u32, Item)> = None;
        let mut sections: Vec<Vec<Item>> = vec![Vec::new(); SECTIONS.len()];
        let items: Vec<Item> = (0..self.tabs.len()).map(Item::Tab).chain(self.orphans()).collect();
        for item in items {
            if !self.listed(item) || !self.profile_passes(self.flat(item).work) {
                continue;
            }
            if filtering {
                let Some(score) = self.score_row(item, pattern) else {
                    continue;
                };
                if best.is_none_or(|(b, _)| score > b) {
                    best = Some((score, item));
                }
            }
            let section = self.section(item);
            let at = SECTIONS.iter().position(|&s| s == section).unwrap_or(SECTIONS.len() - 1);
            sections[at].push(item);
        }
        for (&heading, mut rows) in SECTIONS.iter().zip(sections) {
            if rows.is_empty() {
                continue;
            }
            if heading != "tabs" {
                rows.sort_by_cached_key(|&i| self.agent_key(i));
            }
            self.rows.push(Item::Status(heading));
            self.rows.extend(rows);
        }
        best.map(|(_, i)| i)
    }

    /// Archive: one flat list of ended tasks, each row naming its project,
    /// newest first, with no headings. Matches project, name, host and the task's work items
    /// (`#42`, `card 1041`, `PR #57`), so an ended task can be found by its
    /// issue or card and resumed. Returns the best match.
    fn filter_archive(&mut self, pattern: &Pattern, filtering: bool) -> Option<Item> {
        let mut best: Option<(u32, Item)> = None;
        // Newest first, as archive() sorts.
        for item in self.archive() {
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
                self.hits.insert(item, Hits { name: f[1].clone(), second: f[0].clone(), dir: Vec::new() });
                if best.is_none_or(|(b, _)| score > b) {
                    best = Some((score, item));
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
        self.armed = Armed::No;
        self.chooser = None;
        self.snooze = None;
        self.close_reply();
        self.view = view;
        self.usage.offset = 0;
        self.refilter();
    }

    // ------------------------------------------------------------ the pick

    /// What Enter would do with the highlighted row, as standalone.rs's
    /// parse_pick reads it.
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
        // Several links: ctrl-b opens the chooser rather than the main one.
        let links = task.map_or(0, |t| chooser_links(&t.links).len());
        let more = if links > 1 { format!("/{links}") } else { String::new() };
        match item {
            Item::Status(_) => String::new(),
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

    /// Open the snooze prompt on the highlighted open task in Inbox.
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

    /// The prompt's Enter, once the snooze is saved: show it here, moving
    /// on from a task just snoozed to the next.
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
    /// finish_snooze), or Escape or ctrl-c to close it.
    fn snooze_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let now = self.now;
        let Some(z) = &mut self.snooze else { return };
        match k.code {
            KeyCode::Esc => self.snooze = None,
            KeyCode::Char('c') if ctrl => self.snooze = None,
            KeyCode::Enter => {
                self.finish_snooze();
                return;
            }
            KeyCode::Char('u') if ctrl => z.text.clear(),
            KeyCode::Char('w') if ctrl => delete_word(&mut z.text),
            KeyCode::Backspace if alt || ctrl => delete_word(&mut z.text),
            KeyCode::Backspace => {
                z.text.pop();
            }
            KeyCode::Char(c) if !ctrl && !alt => z.text.push(c),
            _ => return,
        }
        if let Some(z) = &mut self.snooze {
            z.update(now);
        }
    }

    /// Keys while the chooser is open: move, or Escape or ctrl-c to close
    /// it. Enter never gets here (standalone_key opens the pick).
    fn chooser_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some(c) = &mut self.chooser else { return };
        let last = c.links.len() - 1;
        match k.code {
            KeyCode::Esc => self.chooser = None,
            KeyCode::Char('c') if ctrl => self.chooser = None,
            KeyCode::Char('?') => self.help = Some(help::Help::default()),
            KeyCode::Up | KeyCode::Char('k') => c.selected = c.selected.saturating_sub(1),
            KeyCode::Char('p') if ctrl => c.selected = c.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => c.selected = (c.selected + 1).min(last),
            KeyCode::Char('n') if ctrl => c.selected = (c.selected + 1).min(last),
            KeyCode::Home => c.selected = 0,
            KeyCode::End => c.selected = last,
            _ => {}
        }
    }

    /// Open the reply box on the highlighted agent in Inbox. An agent waiting
    /// on a prompt is refused: the message would answer it.
    fn open_reply(&mut self) {
        let Some(item) = self.selected_item() else { return };
        if self.preview_want().is_none() {
            self.note = Some(("highlight an agent to message it".into(), true));
            return;
        }
        let Some(key) = self.selected_key() else { return };
        let label = self.selected_label();
        if self.flat(item).state == "blocked" {
            self.note = Some((format!("{label} is waiting on a prompt; answer it in its tab"), true));
            return;
        }
        self.moved();
        self.reply = Some(Reply { key, label, prompt: PromptBox::new() });
    }

    /// Close the reply box, throwing its message away.
    fn close_reply(&mut self) {
        if let Some(r) = self.reply.take() {
            r.prompt.discard();
        }
    }

    /// The reply box's Enter once the message is sent (`switchyard task act
    /// message` also removes the prompt file).
    fn finish_reply(&mut self) {
        if let Some(r) = self.reply.take() {
            self.note = Some((format!("sent to {}", r.label), false));
        }
    }

    /// Edit the reply box's message in `$VISUAL` or `$EDITOR` (ctrl-g), then
    /// put the saved text back; ctrl-z undoes that.
    fn edit_reply(&mut self, term: &mut DefaultTerminal) -> io::Result<()> {
        let Some(r) = &mut self.reply else { return Ok(()) };
        let before = r.prompt.field.lines().join("\n");
        let fills = [self.p.overlay, self.p.surface];
        let edited = prompt_box::edit_in_editor(term, &r.prompt, &mut self.corners, self.cell, fills, || {})?;
        if let Some(text) = edited.filter(|text| *text != before) {
            r.prompt.clear();
            r.prompt.paste(&text);
        }
        Ok(())
    }

    /// Keys while the reply box has focus: edit it (alt- or shift-Enter for
    /// a newline), scroll the preview with the page keys or shift and the
    /// arrows, Enter (see finish_reply), or Escape to leave it (Tab too,
    /// while it's empty). ctrl-c clears the message, then leaves the box;
    /// ctrl-g edits it in the editor (see edit_reply).
    fn reply_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let newline = k.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT);
        let page = self.page();
        let Some(r) = &mut self.reply else { return };
        match k.code {
            KeyCode::Esc => self.close_reply(),
            KeyCode::Tab | KeyCode::BackTab if r.prompt.is_empty() => self.close_reply(),
            KeyCode::Tab | KeyCode::BackTab => {}
            KeyCode::Enter if newline => r.prompt.field.insert_newline(),
            KeyCode::Enter => self.finish_reply(),
            KeyCode::Char('c') if ctrl && !r.prompt.is_empty() => r.prompt.clear(),
            KeyCode::Char('c') if ctrl => self.close_reply(),
            KeyCode::Char('v') if ctrl => self.enter_scrollback(None),
            KeyCode::Char('g') if ctrl => self.edit_requested = true,
            KeyCode::PageUp => self.scroll_preview(page),
            KeyCode::PageDown => self.scroll_preview(-page),
            KeyCode::Up if shift => self.scroll_preview(3),
            KeyCode::Down if shift => self.scroll_preview(-3),
            _ => r.prompt.key(k),
        }
    }

    /// Keep the reply box's message in its prompt file as it is typed.
    fn save_reply(&mut self) {
        if let Some(r) = &mut self.reply {
            r.prompt.save();
        }
    }

    // ------------------------------------------------------------ keys

    fn key(&mut self, k: KeyEvent) {
        if let Some(h) = &mut self.help {
            if h.key(k) == help::Outcome::Close {
                self.help = None;
            }
            return;
        }
        if k.code == KeyCode::F(1) {
            self.help = Some(help::Help::default());
            return;
        }
        if self.chooser.is_some() {
            return self.chooser_key(k);
        }
        if self.snooze.is_some() {
            return self.snooze_key(k);
        }
        // Scrollback has the keys over the reply box it was entered from,
        // which gets them back when it ends.
        if self.scrollback.is_some() {
            return self.scrollback_key(k);
        }
        if self.reply.is_some() {
            return self.reply_key(k);
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let page = self.page();
        // ctrl-x arms an archive; on a worktree with uncommitted changes, the
        // next ctrl-x (typed by the config when Enter answers `ask`) moves on
        // to "are you sure". Enter and Escape never get here.
        if matches!(k.code, KeyCode::Char('x') if ctrl) {
            let dirty = self.selected_task().is_some_and(|t| t.dirty > 0);
            if !self.armable() {
                let why = if self.selected_task().is_some() {
                    "its worktree is already gone: nothing to archive"
                } else {
                    "no agent here: ctrl-x archives agent tabs"
                };
                self.note = Some((why.into(), true));
            }
            self.armed = match self.armed {
                _ if !self.armable() => Armed::No,
                Armed::Archive | Armed::Force if dirty => Armed::Force,
                armed if armed.archives() => armed,
                _ => Armed::Archive,
            };
            return;
        }
        // standalone_key acts on Enter before it gets here.
        if k.code == KeyCode::Enter {
            self.armed = Armed::No;
            return;
        }
        self.armed = Armed::No;
        if self.view == View::Usage {
            let delta = match k.code {
                KeyCode::Up | KeyCode::Char('p' | 'k') if k.code == KeyCode::Up || ctrl => Some(-1),
                KeyCode::Down | KeyCode::Char('n' | 'j') if k.code == KeyCode::Down || ctrl => Some(1),
                KeyCode::PageUp => Some(-page),
                KeyCode::PageDown => Some(page),
                KeyCode::Home => { self.usage.offset = 0; return; }
                KeyCode::End => { self.usage.offset = usize::MAX; return; }
                _ => None,
            };
            if let Some(delta) = delta {
                self.usage.offset = self.usage.offset.saturating_add_signed(delta);
                return;
            }
        }
        match k.code {
            // standalone_key acts on ctrl-b and ctrl-t.
            KeyCode::Char('s') if ctrl => self.open_snooze(),
            KeyCode::Tab | KeyCode::BackTab if self.view == View::Inbox => self.open_reply(),
            KeyCode::Char('e') if ctrl && self.view == View::Inbox => self.expanded = !self.expanded,
            KeyCode::Char('v') if ctrl && self.view == View::Inbox => self.enter_scrollback(None),
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
                self.set_view(if self.view == View::Archive { View::Inbox } else { View::Archive })
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
            // ctrl-h and ctrl-l cycle the views, so every letter
            // stays free for the search.
            KeyCode::Char('h') if ctrl => self.set_view(self.view.next(true)),
            KeyCode::Char('l') if ctrl => self.set_view(self.view.next(false)),
            KeyCode::Char('?') if !ctrl && !alt => self.help = Some(help::Help::default()),
            KeyCode::Char(c) if !ctrl && !alt => {
                // Searching brings back the list it searches.
                self.expanded = false;
                self.query.push(c);
                self.moved();
                self.refilter();
            }
            // Shift scrolling back through the transcript, as the arrows
            // do with it expanded, goes into scrollback; it is already at
            // its end.
            KeyCode::Up | KeyCode::PageUp if shift || self.expanded => self.enter_scrollback(Some(k)),
            KeyCode::Home if self.expanded => self.enter_scrollback(Some(k)),
            KeyCode::Down | KeyCode::PageDown if shift || self.expanded => {}
            KeyCode::End if self.expanded => {}
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
    }

    /// How far the page keys move: a list's height in rows, less two for
    /// context (two-line rows in the sidebar).
    fn page(&self) -> isize {
        let lines = (self.list_area.height as isize - 2).max(1);
        if self.sidebar && !self.expanded { (lines / SIDE_ROW_LINES as isize).max(1) } else { lines }
    }

    /// Scroll the preview back (positive) or forward; draw_preview keeps it
    /// within the capture.
    fn scroll_preview(&mut self, delta: isize) {
        self.scroll.0 = self.scroll.0.saturating_add_signed(delta);
    }

    fn delete_word(&mut self) {
        delete_word(&mut self.query);
        self.refilter();
    }

    /// The highlighted agent's transcript as text, wrapped as last drawn.
    fn transcript_text(&mut self) -> Option<Vec<String>> {
        let p = self.p;
        let pane = self.preview_want()?;
        let width = self.transcript.width;
        if width == 0 {
            return None;
        }
        let pv = self.preview.as_mut().filter(|pv| pv.pane == pane)?;
        let lines = pv.lines(width, &p).ok()?;
        Some(lines.iter().map(scrollback::line_text).collect())
    }

    /// Scrollback over the transcript, from the bottom of what the preview
    /// shows, then `k` in it.
    fn enter_scrollback(&mut self, k: Option<KeyEvent>) {
        let (Some(pane), Some(lines)) = (self.preview_want(), self.transcript_text()) else {
            self.note = Some(("no transcript here to scroll back through".into(), true));
            return;
        };
        if lines.is_empty() {
            return;
        }
        let h = self.transcript.height as usize;
        let back = if self.scroll.1.as_ref() == Some(&pane) { self.scroll.0 } else { 0 };
        let mut sb = scrollback::Scrollback::new(pane, lines.len(), h);
        sb.cursor = sb.cursor.saturating_sub(back);
        sb.sync(lines.len(), h);
        if let Some(k) = k {
            sb.key(k, &lines, h);
            sb.sync(lines.len(), h);
        }
        self.scrollback = Some(sb);
    }

    fn scrollback_key(&mut self, k: KeyEvent) {
        let Some(lines) = self.transcript_text() else { return self.leave_scrollback() };
        let h = self.transcript.height as usize;
        let Some(sb) = &mut self.scrollback else { return };
        match sb.key(k, &lines, h) {
            scrollback::Outcome::Stay => sb.sync(lines.len(), h),
            scrollback::Outcome::Exit => self.leave_scrollback(),
            scrollback::Outcome::Copy(a, b) => {
                let Some(text) = self.preview.as_ref().and_then(|pv| pv.source(a, b)) else {
                    return self.leave_scrollback();
                };
                let n = text.lines().count();
                self.note = Some((format!("copied {n} line{}", if n == 1 { "" } else { "s" }), false));
                self.copied = Some(text);
                self.leave_scrollback();
            }
        }
    }

    /// Back to the live end of the transcript.
    fn leave_scrollback(&mut self) {
        self.scrollback = None;
        self.scroll.0 = 0;
    }

    // ------------------------------------------------------------ drawing

    fn draw(&mut self, f: &mut Frame) {
        let mut top = self.header_chips();
        if !top.is_empty() {
            top.insert(0, Span::raw(" "));
        }
        let inner = draw::frame_spans(f, &self.p, top, "", Margin::new(2, 1));
        // Down to the bottom border, which holds the footer.
        let content = Rect { height: inner.height + 1, ..inner };
        self.sidebar = !self.quick && self.view == View::Inbox && content.width >= SIDEBAR_MIN_WIDTH && content.height >= 10;
        if self.sidebar {
            self.draw_split(f, content);
        } else {
            self.draw_stacked(f, content);
        }
        self.draw_footer_in_border(f);
        if self.help.is_some() {
            let sections = self.help_sections();
            let p = self.p;
            if let Some(h) = &mut self.help {
                h.draw(f, &p, inner, &sections);
            }
        }
    }

    /// Inbox, with room: the sidebar of every open tab and agent on the left
    /// (search over its list), and the highlighted one on the right: its
    /// name and status message, its transcript, and the reply box under it.
    /// ctrl-e collapses the sidebar, giving the right side the whole width.
    fn draw_split(&mut self, f: &mut Frame, area: Rect) {
        let p = self.p;
        let main = if self.expanded {
            area
        } else {
            let w = (area.width * 3 / 10).clamp(SIDEBAR_MIN, SIDEBAR_MAX);
            let [side, gap, main] =
                Layout::horizontal([Constraint::Length(w), Constraint::Length(3), Constraint::Min(0)]).areas(area);
            let [search, rule, list] =
                Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)]).areas(side);
            self.draw_search(f, search);
            draw::rule(f, &p, rule);
            self.draw_list(f, list);
            let buf = f.buffer_mut();
            for y in gap.y..gap.y + gap.height {
                buf[(gap.x + 1, y)].set_symbol("│").set_style(Style::new().fg(p.hl_med));
            }
            main
        };
        let head_h = self.detail_rows(main.width);
        let rows = match &self.reply {
            Some(r) => r.prompt.rows(main.width, REPLY_MIN_ROWS, REPLY_MAX_ROWS),
            None => 1,
        };
        let card_h = (rows + prompt_box::CHROME_ROWS).min(main.height.saturating_sub(head_h + 4));
        let [head, rule, pane, _, card] = Layout::vertical([
            Constraint::Length(head_h),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(card_h),
        ])
        .areas(main);
        if self.expanded {
            // Page keys page through the transcript.
            self.list_area = pane;
        }
        self.draw_detail(f, head);
        draw::rule(f, &p, rule);
        self.draw_transcript(f, pane);
        if card_h > prompt_box::CHROME_ROWS {
            self.draw_reply(f, card, card_h - prompt_box::CHROME_ROWS);
        }
        self.draw_chooser(f, main);
        self.draw_snooze(f, main);
    }

    /// Rows the right side's header takes in `width`: the name line, then
    /// the status message wrapped to at most DETAIL_MSG_ROWS.
    fn detail_rows(&self, width: u16) -> u16 {
        let Some(item) = self.selected_item() else { return 1 };
        let msg = self.flat(item).msg;
        if msg.is_empty() {
            return 1;
        }
        1 + preview::wrap(msg, (width as usize).saturating_sub(2).max(1)).len().min(DETAIL_MSG_ROWS) as u16
    }

    /// The right side's header: the highlighted row's status, project and
    /// name, with its work items, host and age on the right (a plain tab's
    /// folder), then its whole status message.
    fn draw_detail(&self, f: &mut Frame, area: Rect) {
        let p = &self.p;
        let [line, msg_area] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
        let Some(item) = self.selected_item() else {
            f.render_widget(Line::styled(" Nothing highlighted", Style::new().fg(p.muted)), line);
            return;
        };
        let t = self.flat(item);
        let plain = self.is_plain_tab(item);
        let (glyph, color, verb) = self.status_of(&t, item, tick(self.started));
        let (right, right_w) = match item {
            Item::Tab(tab) if plain => {
                let room = (area.width as usize).saturating_sub(str_width(t.name) + 6);
                let right = self.tab_right(tab, &[], p.base, true, room);
                let w = right.iter().map(|s| str_width(&s.content)).sum();
                (right, w)
            }
            _ => self.right_spans(&t, p.base, verb, area.width),
        };
        let mut left = vec![Span::styled(format!("{glyph} "), Style::new().fg(color))];
        let mut room = (area.width as usize).saturating_sub(2 + right_w + 1);
        let (project, _) = self.project_of(item);
        if !project.is_empty() && str_width(&project) + 4 < room / 2 {
            room -= str_width(&project) + 3;
            left.push(Span::styled(project, Style::new().fg(p.iris)));
            left.push(Span::styled(" / ", Style::new().fg(p.muted)));
        }
        let bold = Style::new().fg(p.text).add_modifier(Modifier::BOLD);
        left.extend(highlight(t.name, &[], room, false, bold, bold));
        f.render_widget(Line::from(left), line);
        f.render_widget(Line::from(right).right_aligned(), line);
        if t.msg.is_empty() || msg_area.height == 0 {
            return;
        }
        let style = Style::new().fg(if t.state == "blocked" { p.gold } else { p.subtle });
        let width = (msg_area.width as usize).saturating_sub(2).max(1);
        let mut wrapped = preview::wrap(t.msg, width);
        let max = msg_area.height as usize;
        if wrapped.len() > max {
            wrapped.truncate(max);
            if let Some(last) = wrapped.last_mut() {
                let keep: String = last.chars().take(width.saturating_sub(1)).collect();
                *last = format!("{keep}…");
            }
        }
        let lines: Vec<Line> = wrapped.into_iter().map(|l| Line::styled(format!("  {l}"), style)).collect();
        f.render_widget(Paragraph::new(lines), msg_area);
    }

    /// The other views, and Inbox without room for the sidebar: the search
    /// over the list, with Inbox's preview and reply box under it.
    fn draw_stacked(&mut self, f: &mut Frame, area: Rect) {
        let [search, rule, _, body] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(3),
        ])
        .areas(area);
        self.draw_search(f, search);
        draw::rule(f, &self.p, rule);
        // The reply box keeps the bottom of Inbox, under the preview, with
        // one text row until Tab moves into it.
        let mut reply_card = None;
        let body = if !self.quick && self.view == View::Inbox && body.height >= prompt_box::CHROME_ROWS + 4 {
            let rows = match &self.reply {
                Some(r) => r.prompt.rows(body.width, REPLY_MIN_ROWS, REPLY_MAX_ROWS),
                None => 1,
            };
            let h = (rows + prompt_box::CHROME_ROWS).min(body.height.saturating_sub(3));
            let [rest, card] = Layout::vertical([Constraint::Min(0), Constraint::Length(h)]).areas(body);
            reply_card = Some((card, h.saturating_sub(prompt_box::CHROME_ROWS)));
            rest
        } else {
            body
        };
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
        if let Some((card, rows)) = reply_card {
            self.draw_reply(f, card, rows);
        }
    }

    /// Inbox's list and the preview under it, when there is room for both:
    /// a third of the height (8 to 14 lines) for the preview, the rest
    /// for the list; expanded (ctrl-e), the preview alone. The
    /// preview stays while John moves, whatever the row, so the list
    /// doesn't change height under him. With focus in the reply box, the
    /// preview gets half of what is left, as what John is replying to.
    fn preview_split(&self, body: Rect) -> Option<(Rect, Rect)> {
        if self.view != View::Inbox || self.quick {
            return None;
        }
        if self.expanded {
            return Some((Rect { height: 0, ..body }, body));
        }
        let replying = self.reply.is_some();
        if body.height < if replying { 12 } else { 15 } {
            return None;
        }
        let pane = if replying { (body.height / 2).clamp(6, 14) } else { (body.height / 3).clamp(8, 14) };
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
        self.draw_transcript(f, body);
    }

    /// The highlighted agent's transcript, scrolled to its end until John
    /// scrolls back.
    fn draw_transcript(&mut self, f: &mut Frame, screen: Rect) {
        let p = self.p;
        let p = &p;
        let muted = |text: &str| vec![Line::styled(text.to_string(), Style::new().fg(p.muted))];
        let want = self.preview_want();
        // Scrollback is for one agent's transcript.
        if self.scrollback.as_ref().is_some_and(|sb| Some(&sb.pane) != want.as_ref()) {
            self.leave_scrollback();
        }
        let Some(pane) = want else {
            let why = match self.selected_item() {
                Some(Item::Task(_)) => "This task has no tab listed",
                _ => "Highlight an agent to see what it has been saying",
            };
            return draw::centered(f, screen, muted(why));
        };
        // A new agent starts at the end of its transcript.
        if self.scroll.1.as_ref() != Some(&pane) {
            self.scroll = (0, Some(pane.clone()));
        }
        // Leave a column for the scrollbar.
        let text = Rect { width: screen.width.saturating_sub(2), ..screen };
        self.transcript = text;
        let Some(pv) = self.preview.as_mut().filter(|pv| pv.pane == pane) else {
            return draw::centered(f, screen, muted("Loading the transcript…"));
        };
        match pv.lines(text.width, p) {
            Err(e) => draw::centered(f, screen, muted(e)),
            Ok(lines) => {
                let h = text.height as usize;
                if let Some(sb) = &mut self.scrollback {
                    sb.sync(lines.len(), h);
                    let sel = sb.selection();
                    let query = sb.query();
                    let lit = Style::new().fg(p.base).bg(p.gold);
                    let end = lines.len().min(sb.top + h);
                    let shown: Vec<Line> = (sb.top..end)
                        .map(|i| {
                            let mut line = lines[i].clone();
                            if let Some(q) = query {
                                let at = scrollback::matches(&scrollback::line_text(&line), q);
                                line = scrollback::mark(line, &at, lit);
                            }
                            let bg = if sel.is_some_and(|(a, b)| (a..=b).contains(&i)) {
                                Some(p.hl_med)
                            } else if i == sb.cursor {
                                Some(p.surface)
                            } else {
                                None
                            };
                            match bg {
                                Some(bg) => line.style(Style::new().bg(bg)),
                                None => line,
                            }
                        })
                        .collect();
                    f.render_widget(Paragraph::new(shown), text);
                    if lines.len() > h {
                        draw::scrollbar(f, p, screen, sb.top, lines.len());
                    }
                    return;
                }
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

    /// The reply box: the prompt box with a chip row naming the agent it
    /// goes to, filled brighter while it has focus. Unfocused, it says
    /// what Tab would do.
    fn draw_reply(&mut self, f: &mut Frame, card: Rect, rows: u16) {
        let p = self.p;
        let unfocused = match self.reply {
            Some(_) => None,
            None => {
                let label = self.preview_want().map(|_| self.selected_label());
                let blocked = self.selected_item().is_some_and(|i| self.flat(i).state == "blocked");
                Some(match label {
                    Some(l) if blocked => (format!("{l} is waiting on a prompt; answer it in its tab"), Some(l)),
                    Some(l) => ("Tab to type a message".to_string(), Some(l)),
                    None => ("Highlight an agent to message it".to_string(), None),
                })
            }
        };
        let (prompt, label, focused) = match (&mut self.reply, unfocused) {
            (Some(r), _) => {
                r.prompt.set_placeholder(&format!("Message {}", r.label));
                (&mut r.prompt, Some(r.label.clone()), true)
            }
            (None, Some((placeholder, label))) => {
                self.idle.set_placeholder(&placeholder);
                (&mut self.idle, label, false)
            }
            (None, None) => return,
        };
        let Some((chips, skip)) = prompt.draw(f, &p, card, rows, focused, self.corners.as_ref()) else { return };
        let fill = if focused { p.overlay } else { p.surface };
        let mut left = Vec::new();
        if let Some(label) = label {
            left.push(Span::styled("to ", Style::new().fg(p.muted).bg(fill)));
            let style = if focused { Style::new().fg(p.text).add_modifier(Modifier::BOLD) } else { Style::new().fg(p.subtle) };
            left.push(Span::styled(label, style.bg(fill)));
        }
        let mut right = Vec::new();
        if skip > 0 {
            right.push(Span::styled(format!("+{skip} lines above  "), Style::new().fg(p.muted).bg(fill)));
        }
        if !prompt.saved_ok {
            right.push(Span::styled("not saved ", Style::new().fg(p.love).bg(fill)));
        }
        let left_w: usize = left.iter().map(|s| str_width(&s.content)).sum();
        let right_w: usize = right.iter().map(|s| str_width(&s.content)).sum();
        if left_w + right_w < chips.width as usize {
            f.render_widget(Line::from(right).right_aligned(), chips);
        }
        f.render_widget(Line::from(left), chips);
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

    /// The ctrl-b chooser, when open: a small box over the list, one link a
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
            View::Inbox if self.sidebar => "Search",
            View::Inbox if self.quick => "Jump to a tab or agent",
            View::Inbox => "Search tabs, agents, and work items",
            View::Archive => "Search ended tasks",
            View::Usage => "Search accounts, harnesses, models, and hosts",
        };
        draw::search(f, p, area, &self.query, placeholder, typing, true);
        if self.view == View::Usage { return; }
        let shown = self.rows.iter().filter(|r| selectable(r)).count();
        let mut right = Vec::new();
        let spin = SPINNER[tick(self.started) % SPINNER.len()];
        match self.tasks_load {
            Tasks::Waiting | Tasks::Live if self.sidebar => {
                right.push(Span::styled(format!("{spin}  "), Style::new().fg(p.muted)))
            }
            Tasks::Waiting => right.push(Span::styled(format!("{spin} loading tasks  "), Style::new().fg(p.muted))),
            Tasks::Live => right.push(Span::styled(format!("{spin} checking worktrees  "), Style::new().fg(p.muted))),
            _ => {}
        }
        // The sidebar is too narrow for the counts by status; its
        // headings say them.
        if self.view == View::Inbox && !self.sidebar {
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
            _ => {
                let listed = (0..self.tabs.len()).filter(|&t| self.listed(Item::Tab(t))).count();
                (listed + self.orphans().count(), "tabs")
            }
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
        if self.sidebar {
            return self.draw_side_list(f, area);
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
        // the widest values, within what the busiest agent's right side
        // leaves (a plain tab's folder takes what its row has left).
        let flat = || self.rows.iter().copied().filter(selectable);
        let right_w = flat()
            .filter(|&i| !self.is_plain_tab(i))
            .map(|i| self.right_spans(&self.flat(i), self.p.base, "", list.width).1)
            .max()
            .unwrap_or(0);
        let room = (list.width as usize).saturating_sub(3 + right_w + 1);
        let proj_w = flat()
            .map(|i| str_width(self.flat(i).project))
            .max()
            .unwrap_or(0)
            .min(room / 4);
        // A plain tab's long name (a title a program set, say) stops at a
        // third of the row, so it can't crowd out the folder and the rows
        // around it.
        let name_cap = (list.width as usize / 3).max(24);
        let name_w = flat()
            .map(|i| {
                let w = str_width(self.flat(i).name);
                if self.is_plain_tab(i) { w.min(name_cap) } else { w }
            })
            .max()
            .unwrap_or(0)
            .min(room.saturating_sub(proj_w + 2) * 2 / 3);
        let cols = (proj_w, name_w);
        for (i, row) in self.rows.iter().enumerate().skip(self.offset).take(h) {
            let r = Rect { y: area.y + (i - self.offset) as u16, height: 1, ..list };
            let selected = self.selected == Some(i);
            match row {
                Item::Status(state) => self.draw_status_header(f, r, state),
                _ => self.draw_flat_row(f, r, *row, selected, tick, cols),
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
            "tabs" => ("·", "Tabs", p.subtle),
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
                View::Usage => Vec::new(),
                View::Inbox => match &self.load {
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
                        draw::key_hint(p, "⌃A", " shows all tabs"),
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

    /// A tab with no agent's right side: whether it is the current tab,
    /// and its folder within `room` columns.
    fn tab_right(&self, t: usize, hits: &[usize], bg: Color, selected: bool, room: usize) -> Vec<Span<'static>> {
        let p = &self.p;
        let tab = &self.tabs[t];
        let mut right = Vec::new();
        if self.is_current(t) {
            right.push(Span::styled("current  ", Style::new().fg(p.iris).bg(bg).add_modifier(Modifier::ITALIC)));
        }
        let used: usize = right.iter().map(|s| str_width(&s.content)).sum();
        let dir_w = str_width(&tab.dir).min(room.saturating_sub(used + 1)).min(48);
        let hl = Style::new().fg(p.rose).bg(bg).add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        let dir_style = Style::new().fg(if selected { p.subtle } else { p.muted }).bg(bg);
        right.extend(highlight(&tab.dir, hits, dir_w, true, dir_style, hl));
        right.push(Span::styled(" ", Style::new().bg(bg)));
        right
    }

    /// A row's status glyph, its colour, and the word for it: how an ended
    /// task ended, else its agent's state; a plain tab's dot.
    fn status_of(&self, t: &Flat, item: Item, tick: usize) -> (&'static str, Color, &'static str) {
        let p = &self.p;
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
        let (glyph, color) = if self.is_plain_tab(item) { ("·", p.muted) } else { (glyph, color) };
        let verb = if t.wake > 0 { "snoozed" } else { verb };
        (glyph, color, verb)
    }

    /// The project a row is in, named the same way for every row: its
    /// task's project, else its tab's folder (the repository a worktree
    /// under `.worktrees` belongs to), and whether that is the task's (which
    /// search highlights).
    fn project_of(&self, item: Item) -> (String, bool) {
        let tab = match item {
            Item::Tab(t) => &self.tabs[t],
            Item::Task(k) => return (self.tasks[k].project.clone(), true),
            _ => return (String::new(), false),
        };
        if let Some(k) = tab.task {
            return (self.tasks[k].project.clone(), true);
        }
        (folder_project(&tab.dir), false)
    }

    /// Lines a sidebar row takes: two for a tab or task, one for a heading.
    fn side_height(item: &Item) -> usize {
        if selectable(item) { SIDE_ROW_LINES } else { 1 }
    }

    /// Inbox's list as the sidebar: each tab or task on two lines (see
    /// draw_side_row) under the one-line status headings, scrolled to keep
    /// the highlighted row, and the heading over it, in view.
    fn draw_side_list(&mut self, f: &mut Frame, area: Rect) {
        let h = area.height as usize;
        let heights: Vec<usize> = self.rows.iter().map(Self::side_height).collect();
        if let Some(sel) = self.selected {
            let header_above = sel > 0 && !selectable(&self.rows[sel - 1]);
            let first = if header_above { sel - 1 } else { sel };
            if first < self.offset {
                self.offset = first;
            }
            while self.offset < sel && heights[self.offset..=sel].iter().sum::<usize>() > h {
                self.offset += 1;
            }
        }
        self.offset = self.offset.min(self.rows.len().saturating_sub(1));
        let total: usize = heights.iter().sum();
        let overflow = total > h && h > 0;
        let list = Rect { width: area.width.saturating_sub(if overflow { 2 } else { 0 }), ..area };
        let tick = tick(self.started);
        let mut y = 0;
        for (i, row) in self.rows.iter().enumerate().skip(self.offset) {
            if y >= h {
                break;
            }
            let rows = heights[i].min(h - y) as u16;
            let r = Rect { y: area.y + y as u16, height: rows, ..list };
            match row {
                Item::Status(state) => self.draw_status_header(f, r, state),
                _ => self.draw_side_row(f, r, *row, self.selected == Some(i), tick),
            }
            y += heights[i];
        }
        if overflow {
            let above: usize = heights[..self.offset].iter().sum();
            draw::scrollbar(f, &self.p, area, above, total);
        }
    }

    /// A sidebar row on two lines: status, name, and how long it has been
    /// in that state (until it wakes, snoozed); then, under the name, its
    /// project and status message, with its uncommitted files on the right.
    /// A plain tab shows its folder there. The right side shows the rest.
    fn draw_side_row(&self, f: &mut Frame, r: Rect, item: Item, selected: bool, tick: usize) {
        let p = &self.p;
        let t = self.flat(item);
        let (bg, edge) = self.row_style(selected);
        f.buffer_mut().set_style(r, Style::new().bg(bg));
        let (glyph, color, _) = self.status_of(&t, item, tick);
        let hits = self.hits.get(&item).cloned().unwrap_or_default();
        let hl = Style::new().fg(p.rose).bg(bg).add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        let muted = Style::new().fg(p.muted).bg(bg);
        let width = r.width as usize;
        // The first line: status, name, age.
        let when = match (t.wake, t.since) {
            (wake, _) if wake > 0 => format!("{} ", age(wake.saturating_sub(self.now))),
            (_, since) if since > 0 => format!("{} ", age(self.now.saturating_sub(since))),
            _ => String::new(),
        };
        let room = width.saturating_sub(3 + str_width(&when) + 1);
        let name_style = Style::new().fg(p.text).bg(bg);
        let name_style = if selected { name_style.add_modifier(Modifier::BOLD) } else { name_style };
        let mut left = vec![edge.clone(), Span::styled(format!("{glyph} "), Style::new().fg(color).bg(bg))];
        left.extend(highlight(t.name, &hits.name, room, false, name_style, hl));
        let line = Rect { height: 1, ..r };
        f.render_widget(Line::from(left), line);
        // A span, not a styled Line: a Line's style colours its whole row.
        f.render_widget(Line::from(Span::styled(when, muted)).right_aligned(), line);
        if r.height < 2 {
            return;
        }
        // The second line, under the name: project · message, or a plain
        // tab's folder.
        let line = Rect { y: r.y + 1, height: 1, ..r };
        let dirty = if t.dirty > 0 { format!("✎{} ", t.dirty) } else { String::new() };
        let mut room = width.saturating_sub(3 + str_width(&dirty) + 1);
        let mut left = vec![edge, Span::styled("  ", Style::new().bg(bg))];
        let push = |left: &mut Vec<Span<'static>>, spans: Vec<Span<'static>>, room: &mut usize| {
            *room = room.saturating_sub(spans.iter().map(|s| str_width(&s.content)).sum());
            left.extend(spans);
        };
        let (project, own) = self.project_of(item);
        if !project.is_empty() {
            let style = Style::new().fg(p.iris).bg(bg);
            let hits = if own { hits.second.as_slice() } else { &[] };
            push(&mut left, highlight(&project, hits, room, false, style, hl), &mut room);
            if !t.msg.is_empty() && room > 6 {
                push(&mut left, vec![Span::styled(" · ", muted)], &mut room);
            }
        }
        if !t.msg.is_empty() && room > 3 {
            let style = Style::new().fg(if t.state == "blocked" { p.gold } else { p.muted }).bg(bg);
            let msg = t.msg.lines().next().unwrap_or("");
            push(&mut left, highlight(msg, &[], room, false, style, style), &mut room);
        }
        f.render_widget(Line::from(left), line);
        f.render_widget(Line::from(Span::styled(dirty, Style::new().fg(p.gold).bg(bg))).right_aligned(), line);
    }

    /// A flat row. A tab with no agent shows its folder on the right rather
    /// than a host and status.
    fn draw_flat_row(
        &self,
        f: &mut Frame,
        r: Rect,
        item: Item,
        selected: bool,
        tick: usize,
        cols: (usize, usize),
    ) {
        let p = &self.p;
        let t = self.flat(item);
        let (bg, edge) = self.row_style(selected);
        f.buffer_mut().set_style(r, Style::new().bg(bg));
        let (glyph, color, verb) = self.status_of(&t, item, tick);
        let plain = self.is_plain_tab(item);
        // Right side first, so the columns and message get what is left; a
        // plain tab's folder gets what its columns leave.
        let (right, right_w) = if plain { (Vec::new(), 0) } else { self.right_spans(&t, bg, verb, r.width) };
        let mut left = vec![edge];
        left.push(Span::styled(format!("{glyph} "), Style::new().fg(color).bg(bg)));
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
        if let Item::Tab(tab) = item
            && plain
        {
            let used = 3 + proj_w + 2 + str_width(t.name).min(name_w) + 2;
            let right = self.tab_right(tab, &hits.dir, bg, selected, (r.width as usize).saturating_sub(used));
            f.render_widget(Line::from(left), r);
            f.render_widget(Line::from(right).right_aligned(), r);
            return;
        }
        let msg_room = room.saturating_sub(proj_w + 2 + name_w + 2);
        if !t.msg.is_empty() && msg_room > 4 {
            let msg_style = Style::new().fg(if t.state == "blocked" { p.gold } else { p.subtle }).bg(bg);
            left.extend(highlight(t.msg, &[], msg_room, false, msg_style, msg_style));
        }
        f.render_widget(Line::from(left), r);
        f.render_widget(Line::from(right).right_aligned(), r);
    }

    /// The keys float's bindings, the group for where John is first: the
    /// highlighted row's actions as the footer used to name them.
    fn help_sections(&self) -> Vec<help::Section> {
        use help::Section;
        let own = |keys: Vec<(&str, &str)>| keys.into_iter().map(|(k, d)| (k.to_string(), d.to_string())).collect();
        let mut sections = Vec::new();
        if self.view != View::Usage {
            let task = self.selected_task();
            let mut row: Vec<(&str, &str)> = match task {
                Some(t) if t.ended.is_some() && t.left && t.safe() => vec![("⏎", "resume"), ("⌃X", "remove worktree (merged)")],
                Some(t) if t.ended.is_some() && t.left => vec![("⏎", "resume"), ("⌃X", "remove worktree")],
                Some(t) if t.ended.is_some() => vec![("⏎", "resume")],
                Some(t) if t.safe() => vec![("⏎", "go"), ("⌃X", "archive (merged)")],
                Some(_) => vec![("⏎", "go"), ("⌃X", "archive")],
                None if self.selected_agent_tab().is_some() => vec![("⏎", "go"), ("⌃X", "archive")],
                None if self.selected_item().is_some() => vec![("⏎", "go")],
                None => vec![],
            };
            // ⌃B opens the task's only work item, or a chooser over several.
            let links = task.map(|t| chooser_links(&t.links)).unwrap_or_default();
            match links.as_slice() {
                [] => {}
                [link] => row.push(("⌃B", if link.kind == "pr" { "open PR" } else { "open link" })),
                _ => row.push(("⌃B", "choose a link to open")),
            }
            if self.can_pin() {
                row.push(("⌃T", if task.is_some_and(|t| t.pinned) { "unpin" } else { "pin" }));
            }
            if self.can_snooze() {
                row.push(("⌃S", if task.is_some_and(|t| t.is_snoozed(self.now)) { "wake" } else { "snooze" }));
            }
            if self.preview_want().is_some() {
                row.push(("⇥", "message the agent"));
            }
            if !row.is_empty() {
                sections.push(Section { title: "Highlighted row", keys: own(row) });
            }
            let mut moving = vec![("↑↓ ⌃P ⌃N ⌃K ⌃J", "move"), ("PgUp PgDn", "page"), ("Home End", "first, last")];
            if self.view == View::Inbox {
                moving.push(("⌥↑↓", "previous, next group"));
            }
            moving.extend([("type", "search"), ("⌃W ⌥⌫", "delete a word"), ("⌃U", "clear the search")]);
            sections.push(Section { title: "Moving and searching", keys: own(moving) });
        } else {
            sections.push(Section::new("Usage", &[("↑↓ ⌃P ⌃N", "scroll"), ("PgUp PgDn", "page"), ("Home End", "top, bottom")]));
        }
        let profile = format!("show {}", self.profile.next().name());
        let mut views = vec![("⌃H ⌃L", "previous, next view"), ("⌃A", "archive, and back"), ("⌃O", profile.as_str())];
        match self.view {
            View::Inbox if self.sidebar => views.push(("⌃E", if self.expanded { "show the sidebar" } else { "hide the sidebar" })),
            View::Inbox => views.push(("⌃E", if self.expanded { "back to the list" } else { "expand the preview" })),
            View::Archive => views.push(("⌃F", if self.kept { "show archived tasks" } else { "hide archived tasks" })),
            View::Usage => {}
        }
        views.push(("? F1", "these keys"));
        sections.push(Section { title: "Views", keys: own(views) });
        sections.push(Section::new(
            "Transcript",
            &[("⌃V", "scroll back"), ("⇧↑ ⇧PgUp", "scroll back from here"), ("⇥", "message the agent")],
        ));
        sections.push(Section::new(
            "Scrollback",
            &[
                ("j k ↑↓", "move a line"),
                ("⌃D ⌃U", "half a page"),
                ("⌃F ⌃B PgDn PgUp", "a page"),
                ("{ }", "previous, next blank line"),
                ("g G Home End", "top, bottom"),
                ("/ ?", "search forward, back"),
                ("n N", "next, previous match"),
                ("v V Space", "start or drop a selection"),
                ("y ⏎", "copy and leave"),
                ("esc", "drop selection, search, then leave"),
                ("q ⌃C ⌃V", "leave"),
            ],
        ));
        sections.push(Section::new(
            "Reply box",
            &[
                ("⏎", "send"),
                ("⌥⏎ ⇧⏎", "new line"),
                ("⇧↑↓ PgUp PgDn", "scroll the transcript"),
                ("⌃V", "scroll back, then back here"),
                ("⌃G", "edit in the editor"),
                ("⌃C", "clear, then leave"),
                ("esc", "leave"),
            ],
        ));
        sections.push(Section::new("Snooze prompt", &[("⏎", "snooze (empty: wake)"), ("⌃U", "clear"), ("esc", "cancel")]));
        sections.push(Section::new("Link chooser", &[("⏎", "open"), ("↑↓ j k", "move"), ("esc", "cancel")]));
        sections.push(Section::new(
            "This float",
            &[("/", "filter"), ("⌃U", "clear the filter"), ("j k ↑↓", "scroll"), ("esc q", "close")],
        ));
        // Where John is goes first.
        let here = if self.scrollback.is_some() {
            Some("Scrollback")
        } else if self.reply.is_some() {
            Some("Reply box")
        } else if self.snooze.is_some() {
            Some("Snooze prompt")
        } else if self.chooser.is_some() {
            Some("Link chooser")
        } else {
            None
        };
        if let Some(i) = here.and_then(|t| sections.iter().position(|s| s.title == t)) {
            let s = sections.remove(i);
            sections.insert(0, s);
        }
        sections
    }

    /// Scrollback's footer: the search being typed, else its keys, with
    /// where the cursor is on the right.
    fn draw_scrollback_footer(&self, f: &mut Frame, area: Rect, sb: &scrollback::Scrollback) {
        let p = &self.p;
        if let Some(t) = &sb.typing {
            let line = Line::from(vec![
                Span::styled(if t.forward { " /" } else { " ?" }, Style::new().fg(p.gold).add_modifier(Modifier::BOLD)),
                Span::styled(t.query.clone(), Style::new().fg(p.text)),
                Span::styled("▏", Style::new().fg(p.subtle)),
            ]);
            return f.render_widget(line, area);
        }
        let right = Span::styled(format!("SCROLLBACK {}/{} ", sb.cursor + 1, sb.len()), Style::new().fg(p.iris));
        let right_w = str_width(&right.content);
        let room = (area.width as usize).saturating_sub(right_w + 1);
        let mut spans = match &sb.note {
            Some(note) => vec![Span::raw(" "), Span::styled(note.clone(), Style::new().fg(p.love))],
            None => {
                let select = if sb.anchor.is_some() { "copy selection" } else { "copy line" };
                draw::hints(p, &[("y", select, p.subtle), ("q", "done", p.subtle), ("F1", "keys", p.subtle)], room).0
            }
        };
        let used: usize = spans.iter().map(|s| str_width(&s.content)).sum();
        spans.push(Span::raw(" ".repeat((area.width as usize).saturating_sub(used + right_w))));
        spans.push(right);
        f.render_widget(Line::from(spans), area);
    }

    /// The footer, in the frame's bottom border as the shortcut is in its
    /// top one: the border runs on through its gaps, a space either side
    /// of what it shows.
    fn draw_footer_in_border(&self, f: &mut Frame) {
        let outer = f.area();
        if outer.width < 6 || outer.height < 2 {
            return;
        }
        let row = Rect { x: outer.x + 2, y: outer.bottom() - 1, width: outer.width - 4, height: 1 };
        let border = Style::new().fg(self.p.hl_med).bg(self.p.base);
        let buf = f.buffer_mut();
        for x in row.x..row.right() {
            buf[(x, row.y)].set_symbol(" ");
        }
        self.draw_footer(f, row);
        let buf = f.buffer_mut();
        let blank = |buf: &ratatui::buffer::Buffer, x: u16| buf[(x, row.y)].symbol() == " ";
        let mut x = row.x;
        while x < row.right() {
            if !blank(buf, x) {
                x += 1;
                continue;
            }
            let start = x;
            while x < row.right() && blank(buf, x) {
                x += 1;
            }
            // Gaps between hints are three spaces; longer ones are border,
            // as is all of an empty footer.
            let (from, to) = (if start == row.x { start } else { start + 1 }, if x == row.right() { x } else { x - 1 });
            if x - start > 3 && from < to {
                for bx in from..to {
                    buf[(bx, row.y)].set_symbol("─").set_style(border);
                }
            }
        }
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        let p = &self.p;
        if let Some((msg, failed)) = &self.note {
            let style = Style::new().fg(if *failed { p.love } else { p.pine });
            f.render_widget(Line::from(vec![Span::raw(" "), Span::styled(msg.clone(), style)]), area);
            return;
        }
        if let Some(sb) = &self.scrollback {
            return self.draw_scrollback_footer(f, area, sb);
        }
        if let Some(r) = &self.reply {
            let ready = !r.prompt.text().is_empty() && r.prompt.saved_ok;
            let hints = [
                ("⏎", "send", if ready { p.pine } else { p.muted }),
                ("⌃G", "editor", p.subtle),
                ("F1", "keys", p.subtle),
            ];
            let (spans, _) = draw::hints(p, &hints, area.width as usize);
            f.render_widget(Line::from(spans), area);
            return;
        }
        if let Some(z) = &self.snooze {
            let enter = match z.until {
                Ok(0) => "wake",
                _ => "snooze",
            };
            let (spans, _) = draw::hints(p, &[("⏎", enter, p.subtle), ("F1", "keys", p.subtle)], area.width as usize);
            f.render_widget(Line::from(spans), area);
            return;
        }
        if self.chooser.is_some() {
            let (spans, _) = draw::hints(p, &[("⏎", "open", p.subtle), ("?", "keys", p.subtle)], area.width as usize);
            f.render_widget(Line::from(spans), area);
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
        if let (true, Some(t)) = (self.armed.archives(), self.selected_agent_tab()) {
            let key = Style::new().fg(p.love).add_modifier(Modifier::BOLD);
            let line = Line::from(vec![
                Span::raw(" "),
                Span::styled("⏎", key),
                Span::styled(format!(" archive {}", self.tabs[t].name), Style::new().fg(p.love)),
                Span::styled(": closes tab, removes its worktree · other keys cancel", Style::new().fg(p.muted)),
            ]);
            f.render_widget(line, area);
            return;
        }
        let hints: &[(&str, &str, Color)] = if self.quick {
            &[("⏎", "jump", p.subtle), ("esc", "close", p.subtle)]
        } else {
            &[("?", "keys", p.subtle)]
        };
        let (spans, used) = draw::hints(p, hints, area.width as usize);
        f.render_widget(Line::from(spans), area);
        // The summary only when it still fits beside the hints.
        if self.view != View::Inbox {
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
        self.feed = Some(standalone::Feed::start(self.quick));
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

    /// A finished archive's outcome, as the note; then fetch again.
    fn take_archived(&mut self) {
        let Some(rx) = &self.archiving else { return };
        match rx.try_recv() {
            Ok(r) => {
                self.archiving = None;
                self.note = Some(match r {
                    Ok(msg) => (msg, false),
                    Err(e) => (e, true),
                });
                self.refetch();
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.archiving = None,
        }
    }

    /// Data that has arrived from the feed.
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

    /// The keys that act: Enter, ctrl-b, ctrl-t, and Enter in the
    /// snooze prompt and reply box; false for other keys.
    fn standalone_key(&mut self, k: KeyEvent, term: &mut DefaultTerminal) -> bool {
        // The keys float and scrollback have every key while they last.
        if self.help.is_some() || self.scrollback.is_some() {
            return false;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let open = matches!(k.code, KeyCode::Char('b') if ctrl);
        // The reply box's Enter sends the message; other keys edit it. A
        // failed send keeps the box, so the message isn't lost.
        if let Some(r) = &self.reply {
            let newline = k.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT);
            if k.code != KeyCode::Enter || newline {
                return false;
            }
            let text = r.prompt.text();
            if text.is_empty() {
                return true;
            }
            let Some(pick) = standalone::parse_pick(&self.pick()) else { return true };
            let label = r.label.clone();
            self.busy(term, format!("sending to {label}…"));
            self.note = Some(match standalone::message(&pick, &text) {
                Ok(msg) => {
                    self.close_reply();
                    (msg, false)
                }
                Err(e) => (format!("{label}: {e}"), true),
            });
            return true;
        }
        // The snooze prompt's Enter saves the snooze; other keys edit it.
        if let Some(z) = &self.snooze {
            // Unreadable text: Enter does nothing.
            let Ok(until) = z.until else { return matches!(k.code, KeyCode::Enter) };
            if !matches!(k.code, KeyCode::Enter) {
                return false;
            }
            let Some(pick) = standalone::parse_pick(&self.pick()) else { return true };
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
            let Some(pick) = standalone::parse_pick(&self.pick()) else { return true };
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
        // The chooser's pick opens on Enter (or ctrl-b again); other keys
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
        if !open && !matches!(k.code, KeyCode::Enter) {
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
            // As the config types ctrl-x for `ask`: on to "are you sure".
            "ask" => {
                self.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
                return true;
            }
            "archive" | "force-archive" => {
                self.armed = Armed::No;
                if self.archiving.is_some() {
                    self.note = Some(("one archive at a time: wait for this one to finish".into(), true));
                    return true;
                }
                let note = (format!("Archiving {label} on {}…", pick.host), false);
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(standalone::archive(&pick.action, &pick));
                });
                self.archiving = Some(rx);
                note
            }
            "resume" => {
                self.busy(term, format!("Resuming {label}…"));
                done(standalone::resume(&pick), format!("resumed {label}"))
            }
            _ => {
                let r = standalone::go(&pick);
                // Jumped away: come back to an empty search.
                if r.is_ok() {
                    self.clear_search();
                }
                done(r, format!("showed {label}"))
            }
        };
        self.note = Some(note);
        self.refetch();
        true
    }

    /// The quick float's keys: typing searches and the arrows (or ctrl-n,
    /// ctrl-p) move; Enter jumps to the highlighted tab and Escape (or
    /// ctrl-c, ctrl-g) closes the float. Whether to quit.
    fn quick_key(&mut self, k: KeyEvent, term: &mut DefaultTerminal) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        match k.code {
            KeyCode::Esc => return true,
            KeyCode::Char('c' | 'g') if ctrl => return true,
            KeyCode::Enter => {
                let Some(pick) = standalone::parse_pick(&self.pick()) else { return false };
                let label = self.selected_label();
                let r = if pick.action == "resume" {
                    self.busy(term, format!("Resuming {label}…"));
                    standalone::resume(&pick)
                } else {
                    standalone::go(&pick)
                };
                // A failure stays, so it can be read.
                match r {
                    Ok(()) => return true,
                    Err(e) => self.note = Some((e, true)),
                }
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                self.note = None;
                self.query.push(c);
                self.moved();
                self.refilter();
            }
            KeyCode::Char('p' | 'k' | 'n' | 'j' | 'u' | 'w' | 'o') if ctrl => self.key(k),
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End
            | KeyCode::Backspace
                if !shift =>
            {
                self.note = None;
                self.key(k)
            }
            _ => {}
        }
        false
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
        let mut app = App::new((String::new(), String::new()), View::Inbox);
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
    fn ctrl_h_and_l_cycle_the_views_while_letters_search() {
        let mut app = inbox();
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let plain = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.key(plain(KeyCode::Char('h')));
        app.key(plain(KeyCode::Char('l')));
        assert!(app.view == View::Inbox && app.query == "hl");
        app.key(ctrl('l'));
        assert!(app.view == View::Usage);
        app.key(ctrl('l'));
        assert!(app.view == View::Archive);
        app.key(ctrl('h'));
        assert!(app.view == View::Usage);
        app.key(ctrl('h'));
        assert!(app.view == View::Inbox);
    }

    #[test]
    fn cmd_k_without_a_tab_clears_the_search_and_keeps_the_row() {
        let mut app = inbox();
        app.key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
        let picked = app.selected_key();
        assert!(picked.is_some());
        app.paste("rex-data:current\n");
        assert!(app.query.is_empty());
        assert_eq!(app.selected_key(), picked);
    }

    #[test]
    fn cmd_i_from_the_archive_shows_inbox_with_the_current_tab_highlighted() {
        let mut app = inbox();
        app.set_view(View::Archive);
        app.paste("rex-data:current\ns/wait");
        assert!(app.view == View::Inbox);
        assert_eq!(
            app.selected_key(),
            Some(("s".to_string(), "wait".to_string()))
        );
    }

    #[test]
    fn agents_group_by_status_by_urgency_and_age() {
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
    fn working_agents_keep_their_order_as_they_report() {
        let order = |a: u64, b: u64| {
            let mut app = App::new((String::new(), String::new()), View::Inbox);
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
        let expected = vec![Item::Status("working"), Item::Task(1), Item::Task(0)];
        assert_eq!(order(10, 20), expected);
        assert_eq!(order(20, 10), expected);
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

    fn inbox_with_lifecycle() -> App {
        let mut app = App::new((String::new(), String::new()), View::Inbox);
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
            Item::Status("done"), Item::Tab(2),
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
    fn all_leaves_out_the_pickers_own_tab() {
        let mut app = App::new((String::new(), String::new()), View::Inbox);
        app.follow = false;
        app.deliver(|app| {
            (app.sessions, app.tabs) = parse_rows(concat!(
                "S\t-\tlocal\ta\tagents\n",
                "T\tpicker\t\tswitchyard\t~\n",
                "T\tshell\t\tshell\t~\n",
                "T\tagent\tworking\tswitchyard\t~\n",
                "S\t6d696c6c6572\tmiller\tb\tagents\n",
                "T\tremote\t\tswitchyard\t~\n",
            ));
            app.load = Load::Ready;
            app.tasks_load = Tasks::Ready;
        });
        // Inbox leaves out plain tabs: the config host's switchyard tab, and
        // the other plain tabs too. The agent stays.
        assert_eq!(app.rows, vec![Item::Status("working"), Item::Tab(2)]);
        // The quick float (cmd+k) keeps the plain tabs, the picker's own
        // tab still out.
        app.quick = true;
        app.refilter();
        assert_eq!(app.rows, vec![
            Item::Status("working"), Item::Tab(2),
            Item::Status("tabs"), Item::Tab(1), Item::Tab(3),
        ]);
    }

    #[test]
    fn every_session_shares_the_status_sections_and_plain_tabs_stay_out() {
        let mut app = App::new((String::new(), String::new()), View::Inbox);
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
        // Agents from every session share the status sections, and the open
        // task with no listed session among them. The plain tabs are left
        // out of Inbox.
        assert_eq!(app.rows, vec![
            Item::Status("blocked"), Item::Tab(3),
            Item::Status("done"), Item::Task(0),
            Item::Status("working"), Item::Tab(1),
        ]);
        let mut terminal = Terminal::new(TestBackend::new(100, 10)).unwrap();
        terminal.draw(|f| app.draw_list(f, f.area())).unwrap();
        let line = |y: u16| -> String {
            let buf = terminal.backend().buffer();
            (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
        };
        for label in ["Waiting", "Done", "Working", "approval", "stray"] {
            assert!((0..10).any(|y| line(y).contains(label)), "{label}");
        }
        assert!((0..10).all(|y| !line(y).contains("Tabs")), "plain tabs in Inbox");
        // Each row names its session: no session headings or tree guides.
        assert!(line(1).contains("busy") && line(5).contains("quiet"), "{}", line(5));
        assert!((0..10).all(|y| !line(y).contains('├') && !line(y).contains('╰')));
    }

    #[test]
    fn ctrl_t_pins_and_unpins_and_holds_over_stale_data() {
        let mut app = inbox_with_lifecycle();
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(2));
        // ctrl-t saves the pin in the registry (standalone_key), then shows it.
        assert!(app.can_pin());
        app.toggle_pin();
        assert!(app.tasks[2].pinned);
        assert_eq!(app.selected_item(), Some(Item::Tab(2)));
        // Done ranks above working, pinned or not.
        assert_eq!(&app.rows[..3], &[Item::Status("pinned"), Item::Tab(2), Item::Tab(0)]);
        // Data read before the pin was saved doesn't undo it ...
        app.deliver(|app| app.tasks[2].pinned = false);
        assert!(app.tasks[2].pinned);
        // ... and once it agrees, it is the data's again.
        app.deliver(|app| app.tasks[2].pinned = true);
        assert!(app.pending.is_empty());
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(2));
        app.toggle_pin();
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
        let target = app.pick().split_once(':').unwrap().1.to_string();
        app.key(ctrl('s'));
        assert!(app.snooze.as_ref().is_some_and(|z| z.until.is_err()));
        for c in "2x".chars() {
            app.key(plain(KeyCode::Char(c)));
        }
        assert!(app.snooze.as_ref().is_some_and(|z| z.until.is_err()));
        // Enter on unreadable text does nothing; the search is untouched.
        app.key(plain(KeyCode::Enter));
        assert!(app.snooze.is_some() && app.query.is_empty());
        app.key(plain(KeyCode::Backspace));
        app.key(plain(KeyCode::Char('h')));
        assert_eq!(app.snooze.as_ref().unwrap().until, Ok(1000 + 7200));
        assert_eq!(app.pick().split_once(':').unwrap().1, target);
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
        assert_eq!(app.snooze.as_ref().unwrap().until, Ok(0));
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
    fn tab_messages_the_highlighted_agent() {
        let mut app = inbox_with_lifecycle();
        let plain = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(2));
        let target = app.pick().split_once(':').unwrap().1.to_string();
        app.key(plain(KeyCode::Tab));
        assert!(app.reply.as_ref().is_some_and(|r| r.prompt.text().is_empty()));
        for c in "run the tests".chars() {
            app.key(plain(KeyCode::Char(c)));
        }
        // alt-Enter and pastes add lines; data pastes still deliver.
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        app.paste("then\r\nreport");
        app.paste("rex-data:usage-error\nx");
        let r = app.reply.as_ref().unwrap();
        assert_eq!(r.prompt.text(), "run the tests\nthen\nreport");
        // The message goes to the highlighted agent's tab.
        assert_eq!(app.pick().split_once(':').unwrap().1, target);
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let text: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
        assert!(text.contains("to /woke") && text.contains("report") && text.contains("F1 keys"), "{text}");
        assert!(help_text(&app).starts_with("Reply box\n⏎ send\n⌥⏎ ⇧⏎ new line"), "{}", help_text(&app));
        // Enter, typed back by the config once the message is sent, closes
        // the box with a note.
        app.key(plain(KeyCode::Enter));
        assert!(app.reply.is_none());
        assert_eq!(app.note, Some(("sent to /woke".into(), false)));
        // Tab leaves an empty box, but not one with a message.
        app.key(plain(KeyCode::Tab));
        app.key(plain(KeyCode::Char('x')));
        app.key(plain(KeyCode::Tab));
        assert!(app.reply.is_some());
        app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        app.key(plain(KeyCode::Tab));
        assert!(app.reply.is_none());
        // Escape cancels, keeping the highlight; an agent waiting on a
        // prompt is refused.
        app.key(plain(KeyCode::Tab));
        app.key(plain(KeyCode::Esc));
        assert!(app.reply.is_none() && app.selected_item() == Some(Item::Tab(2)));
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(1));
        app.key(plain(KeyCode::Tab));
        assert!(app.reply.is_none());
        assert!(app.note.as_ref().is_some_and(|(n, failed)| *failed && n.contains("waiting on a prompt")));
    }

    #[test]
    fn the_reply_box_stays_under_the_preview_expanded_or_not() {
        let mut app = inbox_with_lifecycle();
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(2));
        let screen = |app: &mut App| {
            let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect::<String>()
        };
        let text = screen(&mut app);
        assert!(text.contains("Tab to type a message") && text.contains("to /woke"), "{text}");
        app.key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert!(app.expanded);
        let text = screen(&mut app);
        assert!(text.contains("Tab to type a message"), "{text}");
        // ctrl-y no longer opens it.
        app.key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert!(app.reply.is_none());
    }

    #[test]
    fn failed_hosts_keep_the_other_rows_and_show_in_the_header() {
        let mut app = App::new((String::new(), String::new()), View::Inbox);
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
        let mut app = App::new((String::new(), String::new()), View::Inbox);
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
        assert!(text.contains("? keys"), "{text}");
        assert!(help_text(&app).contains("⌃B choose a link to open"));
    }

    #[test]
    fn ctrl_l_chooses_among_several_links_prs_first() {
        let mut app = App::new((String::new(), String::new()), View::Inbox);
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
        let plain = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let url = |app: &App| app.chooser.as_ref().map(|c| c.url().to_string());
        app.selected = app.rows.iter().position(|&i| i == Item::Task(1));
        // One link: no chooser (standalone_key opens it), and the pick
        // carries no count.
        assert_eq!(app.pick(), format!("task:{}/s/b", hex("")));
        assert!(!app.open_chooser());
        app.selected = app.rows.iter().position(|&i| i == Item::Task(0));
        assert_eq!(app.pick(), format!("task:{}/s/a/3", hex("")));
        assert_eq!(standalone::parse_pick(&app.pick()).unwrap().window, "a");
        assert!(app.open_chooser());
        // The PR first and selected, so ctrl-b then Enter opens it.
        assert_eq!(url(&app).as_deref(), Some("https://p"));
        app.key(plain(KeyCode::Char('j')));
        assert_eq!(url(&app).as_deref(), Some("https://c"));
        app.key(plain(KeyCode::Down));
        app.key(plain(KeyCode::Down));
        assert_eq!(url(&app).as_deref(), Some("https://i"));
        app.key(plain(KeyCode::Up));
        app.key(plain(KeyCode::Char('k')));
        app.key(plain(KeyCode::Char('k')));
        assert_eq!(url(&app).as_deref(), Some("https://p"));
        // Other keys don't type into the search.
        app.key(plain(KeyCode::Char('x')));
        assert!(app.query.is_empty());
        let mut terminal = Terminal::new(TestBackend::new(100, 16)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let text: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Open link") && text.contains("card 1041") && text.contains("? keys"), "{text}");
        assert!(help_text(&app).starts_with("Link chooser"));
        app.key(plain(KeyCode::Esc));
        assert!(app.chooser.is_none());
        assert_eq!(app.pick(), format!("task:{}/s/a/3", hex("")));
    }

    /// The keys float's bindings as text: each group's title, then a line
    /// per binding.
    fn help_text(app: &App) -> String {
        let mut out = Vec::new();
        for s in app.help_sections() {
            out.push(s.title.to_string());
            out.extend(s.keys.iter().map(|(k, d)| format!("{k} {d}")));
        }
        out.join("\n")
    }

    #[test]
    fn question_mark_opens_a_filterable_keys_float_over_the_picker() {
        let mut app = App::new((String::new(), String::new()), View::Inbox);
        app.follow = false;
        app.paste("rex-data:rows\nS\t-\tlocal\ts\tdemo\nT\tagent\tdone\tclaude\t~/r\n");
        app.paste("rex-data:tasks\n[]");
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(0));
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        let mut screen = |app: &mut App| {
            terminal.draw(|f| app.draw(f)).unwrap();
            terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect::<String>()
        };
        let plain = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        app.key(plain('?'));
        assert!(app.query.is_empty());
        let text = screen(&mut app);
        assert!(text.contains(" Keys ") && text.contains("Highlighted row") && text.contains("archive"), "{text}");
        for c in "/copy".chars() {
            app.key(plain(c));
        }
        let text = screen(&mut app);
        assert!(text.contains("copy and leave") && !text.contains("Highlighted row"), "{text}");
        // Escape stops typing, clears the filter, then closes.
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.help.is_some());
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.help.is_none() && app.query.is_empty());
        // F1 opens it from the reply box, which takes `?` as text.
        app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        app.key(plain('?'));
        assert!(app.help.is_none());
        app.key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
        assert!(app.help.is_some() && help_text(&app).starts_with("Reply box"));
    }

    #[test]
    fn scrollback_moves_searches_and_copies_the_transcript() {
        let mut app = App::new((String::new(), String::new()), View::Inbox);
        app.follow = false;
        app.paste("rex-data:rows\nS\t-\tlocal\ts\tdemo\nT\tagent\tdone\tclaude\t~/r\n");
        app.paste("rex-data:tasks\n[]");
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(0));
        let entries: Vec<_> = (0..40).map(|i| serde_json::json!({"kind": "assistant", "text": format!("answer {i}")})).collect();
        app.paste(&format!("rex-data:preview\n-/s/agent\n{}", serde_json::json!({"entries": entries})));
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        let mut screen = |app: &mut App| {
            terminal.draw(|f| app.draw(f)).unwrap();
            terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect::<String>()
        };
        let text = screen(&mut app);
        assert!(app.sidebar && text.contains("answer 39") && !text.contains("answer 30 "), "{text}");
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let plain = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        app.key(ctrl('v'));
        let text = screen(&mut app);
        assert!(text.contains("SCROLLBACK 79/79"), "{text}");
        // Search back for an early answer: the view moves to it.
        for c in "?answer 3\n".chars() {
            app.key(if c == '\n' { KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE) } else { plain(c) });
        }
        app.key(plain('N'));
        app.key(plain('N'));
        let text = screen(&mut app);
        assert!(text.contains("answer 3 ") && !text.contains("answer 39"), "{text}");
        // New output arriving leaves the view where it is.
        let more: Vec<_> = (0..45).map(|i| serde_json::json!({"kind": "assistant", "text": format!("answer {i}")})).collect();
        app.paste(&format!("rex-data:preview\n-/s/agent\n{}", serde_json::json!({"entries": more})));
        let text = screen(&mut app);
        assert!(text.contains("answer 3 ") && !text.contains("answer 44"), "{text}");
        app.key(plain('v'));
        app.key(plain('j'));
        app.key(plain('j'));
        app.key(plain('y'));
        assert_eq!(app.copied.as_deref(), Some("answer 3\n\nanswer 4"));
        assert!(app.scrollback.is_none());
        let text = screen(&mut app);
        assert!(text.contains("copied 3 lines") && text.contains("answer 44"), "{text}");
        // From the reply box, ctrl-v toggles scrollback and leaves the box
        // as it was.
        app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        app.key(plain('h'));
        app.key(ctrl('v'));
        assert!(app.scrollback.is_some() && app.reply.is_some());
        app.key(plain('k'));
        app.key(ctrl('v'));
        assert!(app.scrollback.is_none());
        assert_eq!(app.reply.as_ref().map(|r| r.prompt.text()).as_deref(), Some("h"));
    }

    #[test]
    fn ctrl_x_arms_on_any_agent_tab_and_says_why_elsewhere() {
        let mut app = App::new((String::new(), String::new()), View::Inbox);
        app.follow = false;
        // An agent with no task (started by hand, or its task ended by
        // mistake) and a plain shell.
        app.paste("rex-data:rows\nS\t-\tlocal\ts\tdemo\nT\tagent\tdone\tclaude\t~/r\nT\tshell\t\tzsh\t~\nT\tready\tidle\tpi\t~/r\n");
        app.paste("rex-data:tasks\n[]");
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let at = |app: &App, t: usize| app.rows.iter().position(|&i| i == Item::Tab(t)).unwrap();
        app.selected = Some(at(&app, 0));
        app.key(ctrl('x'));
        assert_eq!(app.pick(), "archive:-/s/agent");
        let mut terminal = Terminal::new(TestBackend::new(100, 16)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let text: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
        assert!(text.contains("archive claude") && text.contains("removes its worktree"), "{text}");
        // An agent waiting at its prompt is one too. (The plain shell tab
        // isn't listed in Inbox, so there is no row to arm there.)
        app.selected = Some(at(&app, 2));
        app.key(ctrl('x'));
        assert_eq!(app.pick(), "archive:-/s/ready");
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
        app.set_view(View::Archive);
        assert_eq!(app.rows, vec![Item::Task(5)]);
    }

    #[test]
    fn archive_is_one_flat_list_newest_first() {
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
        // One flat list, newest first, whatever the project or ending.
        assert_eq!(app.rows, vec![Item::Task(4), Item::Task(0), Item::Task(3), Item::Task(1), Item::Task(2)]);
        assert_eq!(app.selected, Some(0));
        // ctrl-f hides the archived (and merged) ones.
        app.key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert_eq!(app.rows, vec![Item::Task(1), Item::Task(2)]);
        let mut terminal = Terminal::new(TestBackend::new(100, 10)).unwrap();
        terminal.draw(|f| app.draw_list(f, f.area())).unwrap();
        let text: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
        // Each row names its project and how it ended; no headings.
        assert!(text.contains("beta") && text.contains("closed"), "{text}");
        assert!(!text.contains("Left behind") && !text.contains("─"), "{text}");
    }

    #[test]
    fn usage_is_integrated_read_only_and_handles_narrow_frames() {
        let mut app = inbox();
        let mut view = View::Inbox;
        for expected in [View::Usage, View::Archive, View::Inbox] {
            view = view.next(false);
            assert!(view == expected);
            assert!(view.next(true).next(false) == view);
        }
        app.set_view(View::Usage);
        assert!(app.rows.is_empty());
        assert_eq!(app.pick(), "");
        assert!(!app.can_pin() && !app.can_snooze() && !app.armable());
        app.paste("rex-data:usage\n{\"fetched\":1,\"totals\":[],\"quotas\":[],\"warnings\":[\"remote host unavailable\"]}");
        app.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        for (width, height) in [(120, 35), (32, 10), (12, 5)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            assert_eq!(app.pick(), "");
        }
        app.set_view(View::Inbox);
        assert!(!app.rows.is_empty());
    }

    #[test]
    fn long_tab_names_stop_at_a_third_of_the_row() {
        // Plain tabs are listed only in the quick float.
        let mut app = App::new((String::new(), String::new()), View::Inbox);
        app.quick = true;
        let long = "x".repeat(200);
        app.paste(&format!("rex-data:rows\nS\t-\tlocal\ts\tdemo\nT\tw\t\t{long}\t~/dev/src/project\n"));
        let mut terminal = Terminal::new(TestBackend::new(100, 3)).unwrap();
        terminal.draw(|f| app.draw_list(f, f.area())).unwrap();
        let buf = terminal.backend().buffer();
        let row: String = (0..100).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        let shown = row.matches('x').count();
        assert!(row.contains('…') && row.contains("~/dev/src/project") && (24..=34).contains(&shown), "{row}");
    }
    #[test]
    fn wide_all_shows_a_collapsible_sidebar_beside_the_highlighted_agent() {
        let mut app = inbox_with_lifecycle();
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(2));
        let screen = |app: &mut App, w: u16| {
            let mut terminal = Terminal::new(TestBackend::new(w, 30)).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            let buf = terminal.backend().buffer();
            (0..30).map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>()
        };
        // The list on the left, the agent's name, transcript and reply box
        // on the right, on the same rows.
        let rows = screen(&mut app, 140);
        assert!(app.sidebar);
        let row = rows.iter().find(|r| r.contains("Search")).unwrap();
        assert!(row.contains('│') && row.contains("woke") && row.contains("done"), "{row}");
        assert!(rows.iter().any(|r| r.contains("pinned") && r.contains('│')), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains("Tab to type a message") && r.contains('│')), "{rows:#?}");
        // ctrl-e collapses the sidebar; typing to search brings it back.
        app.key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        let rows = screen(&mut app, 140);
        assert!(!rows.iter().any(|r| r.contains("Search") || r.contains("pinned")), "{rows:#?}");
        assert!(help_text(&app).contains("⌃E show the sidebar"));
        app.key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
        assert!(!app.expanded && app.query == "w");
        // Too narrow, Inbox stacks the list over the reply box as before.
        app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        let rows = screen(&mut app, 80);
        assert!(!app.sidebar);
        assert!(rows.iter().any(|r| r.contains("Search tabs, agents, and work items")), "{rows:#?}");
    }

    #[test]
    fn sidebar_rows_take_two_lines_and_scroll_to_the_highlight() {
        let mut app = App::new((String::new(), String::new()), View::Inbox);
        app.follow = false;
        app.now = 1000;
        let mut rows = String::from("S\t-\tlocal\ts\tdemo\n");
        let mut tasks = Vec::new();
        for i in 0..12 {
            rows.push_str(&format!("T\tw{i}\tworking\tt{i}\n"));
            tasks.push(serde_json::json!({"session_id":"s", "window_id":format!("w{i}"), "name":format!("task-{i:02}"),
                "project":"proj", "status":"working", "status_msg":format!("step {i}"), "status_at":900}));
        }
        app.deliver(|app| {
            (app.sessions, app.tabs) = parse_rows(&rows);
            app.tasks = parse_tasks(&serde_json::Value::from(tasks).to_string()).unwrap();
            app.load = Load::Ready;
            app.tasks_load = Tasks::Ready;
        });
        let screen = |app: &mut App| {
            let mut terminal = Terminal::new(TestBackend::new(140, 20)).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            let buf = terminal.backend().buffer();
            (0..20).map(|y| (0..50).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>()
        };
        // Every project is the same colour, edits or not; only the pencil
        // is gold, and the name keeps the text colour beside the age.
        app.tasks[1].dirty = 3;
        let mut terminal = Terminal::new(TestBackend::new(140, 20)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let buf = terminal.backend().buffer();
        let find = |text: &str| {
            (0..20u16).find_map(|y| {
                let row: String = (0..50).map(|x| buf[(x, y)].symbol().to_string()).collect();
                row.find(text).map(|i| (row[..i].chars().count() as u16, y))
            }).unwrap()
        };
        for (x, y) in [find("proj · step 0"), find("proj · step 1")] {
            assert_eq!(buf[(x, y)].fg, app.p.iris);
        }
        let (x, y) = find("✎3");
        assert_eq!(buf[(x, y)].fg, app.p.gold);
        let (x, y) = find("task-01");
        assert_eq!(buf[(x, y)].fg, app.p.text);
        let lines = screen(&mut app);
        assert!(app.sidebar);
        let at = lines.iter().position(|l| l.contains("task-00")).unwrap();
        assert!(lines[at].contains("1m") && lines[at + 1].contains("proj · step 0"), "{lines:#?}");
        // The last row scrolls into view with both its lines.
        app.edge(true);
        let lines = screen(&mut app);
        let at = lines.iter().position(|l| l.contains("task-11")).unwrap();
        assert!(lines[at + 1].contains("step 11") && !lines.iter().any(|l| l.contains("task-00")), "{lines:#?}");
    }

    #[test]
    fn plain_tabs_name_their_project_like_tasks_do() {
        assert_eq!(folder_project("~/dev/src/amfaro"), "amfaro");
        assert_eq!(folder_project("~/dev/src/playground/switchyard/.worktrees/more-t3-code-like/crates"), "switchyard");
        assert_eq!(folder_project("~"), "~");
        assert_eq!(folder_project("/"), "/");
    }
}

// ---------------------------------------------------------------- main

/// The project a folder is in: the repository a `.worktrees` worktree
/// belongs to, else the folder's own name (`~` for home).
fn folder_project(dir: &str) -> String {
    let dir = match dir.trim_end_matches('/') {
        "" => return dir.to_string(),
        d => d,
    };
    let dir = dir.split_once("/.worktrees/").map_or(dir, |(repo, _)| repo);
    dir.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(dir).to_string()
}

fn run(term: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    loop {
        app.take_fetched();
        app.take_archived();
        app.ask_preview();
        let before = app.now;
        app.now = now();
        // A snooze ran out: back to its group.
        if app.tasks.iter().any(|t| t.snoozed_until > before && t.snoozed_until <= app.now) {
            app.refilter();
        }
        term.draw(|f| app.draw(f))?;
        app.save_reply();
        let animate = app.archiving.is_some()
            || matches!(app.load, Load::Waiting)
            || matches!(app.tasks_load, Tasks::Waiting | Tasks::Live)
            || app.tabs.iter().any(|t| t.state == State::Working);
        let wait = if animate { Duration::from_millis(90) } else { Duration::from_millis(250) };
        if !event::poll(wait)? {
            continue;
        }
        match event::read()? {
            Event::Key(k) if k.kind != KeyEventKind::Release && app.quick => {
                if app.quick_key(k, term) {
                    return Ok(());
                }
            }
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                if app.standalone_key(k, term) {
                    continue;
                }
                app.note = None;
                app.key(k);
                if std::mem::take(&mut app.edit_requested) {
                    app.edit_reply(term)?;
                }
                if let Some(text) = app.copied.take() {
                    use std::io::Write;
                    let mut out = io::stdout();
                    let _ = out.write_all(scrollback::osc52(&text).as_bytes()).and_then(|()| out.flush());
                }
            }
            // cmd+k's `current`, and John's own pastes, for the reply box.
            Event::Paste(text) if app.reply.is_some() || protocol::data(&text).is_some_and(|(k, _)| k == "current") => {
                app.paste(&text)
            }
            _ => {}
        }
    }
}

/// The picker from a terminal: it fetches its own tabs and tasks and acts on
/// Enter itself. `current` is the SESSION/WINDOW to highlight, if any.
pub fn standalone(view: View, current: Option<&str>) -> io::Result<()> {
    start(view, current, false)
}

/// The cmd+k quick jump float (see lua/switchyard/tab-picker.lua): Inbox's
/// list alone over the current tab, `current` highlighted. Enter shows the
/// highlighted tab and quits, closing the float; Escape just quits.
pub fn quick(current: Option<&str>) -> io::Result<()> {
    start(View::Inbox, current, true)
}

fn start(view: View, current: Option<&str>, quick: bool) -> io::Result<()> {
    standalone::detach_env();
    let mut app = App::new((String::new(), String::new()), view);
    app.quick = quick;
    app.excluded = resolve_folders(&switchyard_core::config::read().map(|c| c.inbox.exclude).unwrap_or_default());
    if let Some(c) = current {
        app.set_current(c);
    }
    app.start_feed();
    picker(app)
}

/// Sanitized usage accounting as JSON: per-model totals across configured
/// hosts (what the Usage view renders), or with `local` this host's raw
/// samples (the remote collector protocol, deduplicated by the caller).
pub fn usage_json(local: bool) -> io::Result<()> {
    let mut collector = switchyard_core::usage::Collector::default();
    let json = if local {
        let settings = switchyard_core::config::read().map(|c| c.usage).unwrap_or_default();
        serde_json::to_string(&collector.local(&settings, &switchyard_core::rex::local_label(), now()))
    } else {
        serde_json::to_string(&collector.collect().summary())
    };
    println!("{}", json.map_err(io::Error::other)?);
    Ok(())
}

fn picker(mut app: App) -> io::Result<()> {
    let mut term = ratatui::init();
    // Raw mode is on and nothing has read stdin yet: ask about graphics now,
    // for the reply box's rounded corners.
    if let Some(cell) = chrome::detect(Duration::from_millis(300)) {
        app.cell = Some(cell);
        app.corners = chrome::Corners::new(cell, &[app.p.overlay, app.p.surface], &mut io::stdout()).ok();
    }
    // No mouse capture, so the terminal's own selection and copy work. Kitty
    // key reporting makes the reply box's shift-Enter arrive as one
    // crossterm can decode, as in the new-session picker.
    let _ = execute!(
        io::stdout(),
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );
    let result = run(&mut term, &mut app);
    // Never leave an archive half done: wait for one still running.
    if let Some(rx) = app.archiving.take() {
        app.note = Some(("Finishing the archive…".into(), false));
        let _ = term.draw(|f| app.draw(f));
        let _ = rx.recv();
    }
    let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags, DisableBracketedPaste);
    if let Some(corners) = &app.corners {
        let _ = corners.delete(&mut io::stdout());
    }
    ratatui::restore();
    // A message typed but never sent.
    app.close_reply();
    result
}
