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
//! - `changes`: the highlighted task's diff and commits (see changes.rs).
//!
//! cmd+i also pastes `current`, SESSION/WINDOW, the tab it was pressed on,
//! into the terminal (or passes it as `--current` when opening the picker),
//! and the picker highlights that tab again.
//!
//! The highlighted row and an armed archive survive each delivery. Tasks
//! link to tabs by (`session_id`, `window_id`). Views, picked with cmd+1-4
//! on the picker's tab:
//!
//! - Inbox (default): one flat list across every session, each row naming
//!   its project. Agents (tabs with live agent status or an open task, and
//!   open tasks with no listed tab) go under status headings in urgency
//!   order (Waiting, Failed, Done, Working, Idle, No status). Plain tabs
//!   with no agent are left out, as is the picker's own `switchyard` tab;
//!   the quick float (cmd+k) lists them under Tabs.
//!   Pinned tasks lead the list and Snoozed ones trail it, with the agents
//!   scheduled to resume themselves (a timer or background job). Orphans
//!   come last: worktrees no task owns (see core's `orphans`), whether
//!   each is clean and merged, which ctrl-x removes; not in the quick
//!   float. A task an
//!   agent spawned (through the MCP endpoint's `spawn_task`) hangs off its
//!   spawner's row as a small tree when both are listed (see `nest`),
//!   folded away by default so ctrl-j and ctrl-k pass over it: the
//!   spawner's name carries `▸N` (gold while one of them waits on a
//!   prompt), Right lists them under it, and Left folds them again. A
//!   search lists every match.
//!   With the width, the list is a sidebar on the left (search over it,
//!   compact rows), and the right side shows the highlighted row: its name,
//!   work items and host over its whole status message, the agent's
//!   transcript, live, and the reply box. ctrl-s collapses the sidebar, and
//!   `/` to search brings it back. Narrower, a preview under the list
//!   shows the transcript, and ctrl-s expands it over the list. ctrl-l
//!   turns the transcript into the task's diff against its base branch,
//!   then its commits, then back (see changes.rs). ctrl-u and ctrl-d go
//!   into scrollback on it, half a page up or down, rather than paging the
//!   list, which search moves through faster. ctrl-y copies the
//!   highlighted agent's host, harness session ID and session log path,
//!   a line each.
//! - Work: one catalog project's open cards or issues, or with ctrl-f its
//!   open pull requests, the ones an open task links to dimmed; Enter starts a session from the highlighted
//!   item, switching to Inbox and highlighting the session once it lists,
//!   or goes to its task (see work.rs). With the width, the highlighted
//!   item shows on the right: its description and comments, and with
//!   ctrl-l a pull request's diff and commits (see detail.rs); ctrl-u and
//!   ctrl-d scroll it, in scrollback, as in Inbox.
//! - Usage: read-only accounting (see usage.rs).
//! - Archive (ctrl-a): one flat list of ended tasks, newest first, each
//!   row naming its project and how it ended; ctrl-f hides the archived
//!   ones, leaving those whose tab or worktree went without one. Tab
//!   opens a chooser over the listed projects, as in Work, to show one
//!   project's tasks or all of them (see projects.rs). ctrl-u and ctrl-d
//!   page by half a page here instead of clearing the search.
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
//! other key disarms. ctrl-c, in Inbox, likewise arms an interrupt of the
//! highlighted agent while it's working, and Enter presses Escape in its
//! tab (see core's actions); one waiting on a prompt or idle is refused.
//!
//! Enter jumps to the highlighted tab (or resumes an ended task's agent in
//! a new tab, staying in the picker: Inbox highlights it once it lists);
//! in Inbox that's cmd+Enter, and Enter instead clears the
//! search, keeping the row it found highlighted. ctrl-r pins or unpins
//! the highlighted open task (`pinned` in the registry), which puts it
//! in a Pinned group at the top of Inbox. ctrl-z, in Inbox, opens a prompt
//! for when to wake the task (see snooze.rs); Enter snoozes it
//! into the Snoozed group until then (`snoozed_until`), and Enter on an
//! empty prompt wakes a snoozed one. Task rows show their links as badges
//! grouped by kind. ctrl-b opens the work items float on the highlighted
//! task's links (PRs, cards, then issues), even one: show each in the Work view or open it, read its description and comments in place, comment
//! on a card, issue or PR, close a card or issue, move a card to Not Now,
//! mark a draft PR ready, each
//! mutation behind its own key and a confirmation naming the item (see
//! items.rs).
//!
//! Inbox keeps a reply box at its bottom, under the preview (or the
//! transcript), expanded or not:
//! the new-session picker's prompt box (see the ui crate's `prompt_box`),
//! with its rounded corners. Tab moves into it to message the highlighted
//! agent; alt- or shift-Enter adds a line, Escape (or Tab, while it's
//! empty) leaves it, and shift with the arrows (or the page keys) scrolls
//! the preview. ctrl-j and ctrl-k leave it and move through the list;
//! ctrl-u and ctrl-d leave it for scrollback, half a page up or down;
//! ctrl-x leaves it and arms an archive, as in the list. An
//! agent waiting on a prompt is refused. Enter sends the message (see
//! core's actions).
//!
//! Each action goes through `pick`, the highlighted row as
//! `ACTION:TARGET/SESSION/WINDOW`: ACTION is `go` (a tab), `task` (a row
//! with an open task), `resume` (an ended task), or an armed `archive`,
//! `ask` or `force-archive`; TARGET is `-` for this host, else the
//! hex-encoded host label. The picker stays open after each action,
//! showing its outcome. No key quits it: it's a tab, closed like any
//! other (cmd+w).

mod changes;
mod detail;
mod fed;
mod help;
mod history;
mod items;
mod model;
mod preview;
mod projects;
mod scrollback;
mod snooze;
mod standalone;
mod usage;
mod work;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::io;
use std::sync::mpsc::{self as chan, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use model::{
    Link, Session, State, Tab, Task, Tone, age, badges, chooser_links, failed_hosts, home_repo,
    in_folders, in_raw_folders, link, now, parse_rows, parse_tasks, resolve_folders, urgency,
    work_dir,
};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{DefaultTerminal, Frame};
use switchyard_core::commands::{self, SlashCommand};
use switchyard_core::item_actions::Action;
use switchyard_core::rex::log;
use switchyard_ui::Palette;
use switchyard_ui::chrome;
use switchyard_ui::draw::{self, delete_word, highlight, pill, str_width};
use switchyard_ui::fuzzy::score_fields;
use switchyard_ui::glyphs::{ICON_SEARCH, SPINNER, tick};
use switchyard_ui::popup;
use switchyard_ui::prompt_box::{self, PromptBox};
use switchyard_ui::protocol::{self, hex};

const ICON_SESSION: &str = "\u{f0bdf}"; // nf-md-console_line
const ICON_INBOX: &str = "\u{f0e27}"; // nf-md-inbox_full
const ICON_PIN: &str = "\u{f0403}"; // nf-md-pin
const ICON_SNOOZE: &str = "\u{f04b2}"; // nf-md-sleep

pub(crate) use switchyard_ui::draw::cells;

/// A count as a float, for display and proportions: rounding only past
/// 2^53, which no token count, byte count or row count comes near.
// There is no lossless conversion from u64 to f64; this is the one place
// that accepts the rounding, rather than a cast at every use.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn approx(n: u64) -> f64 {
    n as f64
}

/// A non-negative cell count from a float, rounded to the nearest.
// Floats have no `TryFrom` into integers; `as` saturates (NaN and
// negatives become 0), which is the clamp wanted here.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn round_cells(x: f64) -> usize {
    x.round() as usize
}

/// A `width` by `height` box in the middle of `area`, which must hold it.
pub(crate) fn centered(area: Rect, width: u16, height: u16) -> Rect {
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// Dim the float behind a popup, all but the bottom border, which keeps
/// the popup's keys.
pub(crate) fn backdrop(f: &mut Frame, p: &Palette) {
    let mut area = f.area();
    area.height = area.height.saturating_sub(1);
    popup::backdrop(f, p, area);
}

/// Inbox's sections, in order: Pinned, agents by status (the empty string is
/// no known status), the tabs with no agent (quick float only), Snoozed
/// (which takes the scheduled agents too), then Orphans: worktrees no task
/// owns (see core's `orphans`), not in the quick float.
const SECTIONS: [&str; 10] = [
    "pinned", "blocked", "error", "done", "working", "idle", "", "tabs", "snoozed", "orphaned",
];

/// How long a pin or snooze made here holds over deliveries that don't
/// have it yet (the registry is saved, but the data was read before).
const PENDING_TTL: Duration = Duration::from_secs(15);
/// How long after a Work view launch Inbox waits for its row to highlight.
const LAUNCHED_TTL: Duration = Duration::from_secs(30);

/// How long a fed quick float waits for the config host to act on its pick
/// (see fed.rs) before saying it didn't.
const FED_ANSWER: Duration = Duration::from_secs(5);

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

/// The picker's views, picked with cmd+1-4 on the picker's tab.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Inbox,
    /// One project's open work items, to start a session from (see work.rs).
    Work,
    Usage,
    Archive,
}

impl View {
    /// Every view, in the frame's view strip's order.
    const ALL: [View; 4] = [View::Inbox, View::Work, View::Usage, View::Archive];

    /// Its name in the frame's view strip.
    fn name(self) -> &'static str {
        match self {
            View::Inbox => "Inbox",
            View::Work => "Work",
            View::Usage => "Usage",
            View::Archive => "Archive",
        }
    }

    /// `inbox` (or the older `all`), `work`, `usage` or `archive`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "inbox" | "all" => Some(View::Inbox),
            "archive" => Some(View::Archive),
            "work" => Some(View::Work),
            "usage" => Some(View::Usage),
            _ => None,
        }
    }
}

/// The footer's hint for moving along the frame's view strip.
const VIEW_HINT: (&str, &str) = ("⌘1-4", "views");

/// Columns `spans` take.
fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| str_width(&s.content)).sum()
}

/// A session the Work view is starting (see `App::launch`).
struct Launching {
    url: String,
    name: String,
    rx: Receiver<Result<(String, String, String), String>>,
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
    /// ctrl-c on a working agent: Enter presses Escape in its tab.
    Interrupt,
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

/// Whether a row heads Inbox's bottom groups, Snoozed and Orphans, which
/// wrapping passes over and the free space goes above.
fn bottom_heading(r: &Item) -> bool {
    matches!(r, Item::Status("snoozed" | "orphaned"))
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
    /// When a scheduled agent resumes; 0 when unknown (a background job).
    alarm: u64,
    /// Shown as `working` because a task it spawned, or one of theirs, is
    /// working while it waits at its prompt (see `kids_working`).
    waiting: bool,
}

type Key = (String, String);

/// A nested row's tree guides: the columns its ancestors' lines take
/// (`│ ` while an ancestor has siblings still to come, else blanks), and
/// whether it is the last of its siblings.
#[derive(Clone, Default, Debug, PartialEq, Eq)]
struct Guide {
    lead: String,
    last: bool,
}

impl Guide {
    /// The guide on the row's own line: `├ ` or `└ ` after its ancestors'.
    fn branch(&self) -> String {
        format!("{}{}", self.lead, if self.last { "└ " } else { "├ " })
    }

    /// The guide on the lines under it: its siblings' line carries on.
    fn stem(&self) -> String {
        format!("{}{}", self.lead, if self.last { "  " } else { "│ " })
    }
}

/// Push `item`'s children, and theirs in turn, each with its guide, when
/// `item` is `open`.
fn descend(
    item: Item,
    lead: &str,
    children: &HashMap<Item, Vec<Item>>,
    open: &HashSet<Item>,
    rows: &mut Vec<Item>,
    tree: &mut HashMap<Item, Guide>,
) {
    let Some(kids) = children.get(&item).filter(|_| open.contains(&item)) else {
        return;
    };
    for (n, &kid) in kids.iter().enumerate() {
        let guide = Guide {
            lead: lead.to_string(),
            last: n + 1 == kids.len(),
        };
        let next = guide.stem();
        rows.push(kid);
        tree.insert(kid, guide);
        descend(kid, &next, children, open, rows, tree);
    }
}

/// Every row nested under `item`, at any depth.
fn descendants(item: Item, children: &HashMap<Item, Vec<Item>>) -> Vec<Item> {
    let mut found = Vec::new();
    let mut todo = vec![item];
    while let Some(at) = todo.pop() {
        for &kid in children.get(&at).into_iter().flatten() {
            found.push(kid);
            todo.push(kid);
        }
    }
    found
}

/// A spawner's rows nested under it: folded away (the default) or listed
/// under it (Right unfolds, Left folds; a search lists them all).
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct Fold {
    /// Its descendants, at every depth.
    rows: usize,
    /// Whether they are listed under it.
    open: bool,
    /// Whether one of them, folded away, is waiting on a prompt.
    blocked: bool,
}

impl Fold {
    /// The badge after its name: `▸3` folded, `▾` unfolded.
    fn badge(self) -> String {
        if self.open {
            " ▾".to_string()
        } else {
            format!(" ▸{}", self.rows)
        }
    }
}

/// The ctrl-z prompt: when to wake the highlighted task, typed (see
/// snooze.rs). Empty, it wakes a snoozed task.
struct Snooze {
    /// The task's tab, as (`session_id`, `window_id`), which stays highlighted.
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
    /// The agent's tab, as (`session_id`, `window_id`), which stays highlighted.
    key: Key,
    label: String,
    prompt: PromptBox,
    /// Where Up and Down have stepped to in the session's earlier messages.
    history: history::History,
    /// The suggestion Tab or Right took into the box, which Tab may leave
    /// while the box still holds it unedited: it shows again as ghost text.
    taken: Option<String>,
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

/// Work item titles the right side's header shows at most, one a line.
const DETAIL_LINK_ROWS: usize = 3;

/// A task's links whose titles the right side's header shows.
fn titled(links: &[Link]) -> impl Iterator<Item = &Link> {
    links
        .iter()
        .filter(|l| !l.title.is_empty())
        .take(DETAIL_LINK_ROWS)
}

/// The Archive's fixed right-side columns, so every row's work items, host
/// and outcome line up under the headers: the work items column (badges
/// and any dirty count, with their gap), the widest host name, and the age
/// column (with its leading space).
#[derive(Clone, Copy)]
struct RightCols {
    links: usize,
    host: usize,
    when: usize,
}

/// The Archive's column headers over the right side `cols` lays out, as
/// wide as each row's.
fn right_header(cols: RightCols) -> String {
    let RightCols { links, host, when } = cols;
    let links = if links == 0 {
        String::new()
    } else {
        format!("{:<links$}", "work items")
    };
    format!(
        "{links} {:<host$}   {:>9}{:>when$} ",
        "host", "outcome", "age"
    )
}

/// Lines drawn under scrollback's cursor, selection and search in `text`
/// (within `screen`, which holds the scrollbar), moving it to suit them.
pub(crate) fn draw_scrollback(
    f: &mut Frame,
    p: &Palette,
    screen: Rect,
    text: Rect,
    lines: &[Line<'static>],
    sb: &mut scrollback::Scrollback,
) {
    let h = text.height as usize;
    sb.sync(lines.len(), h);
    let sel = sb.selection().filter(|_| sb.linewise());
    let width = |i: usize| {
        lines
            .get(i)
            .map_or(0, |l| scrollback::line_text(l).chars().count())
    };
    let chars = sb.text_selection(width(sb.cursor));
    let query = sb.query();
    let lit = Style::new().fg(p.base).bg(p.gold);
    let picked = Style::new().bg(p.hl_med);
    let block = Style::new().fg(p.base).bg(p.text);
    let end = lines.len().min(sb.top + h);
    let shown: Vec<Line> = (sb.top..end)
        .map(|i| {
            let mut line = lines[i].clone();
            let text = scrollback::line_text(&line);
            if let Some(q) = query {
                let at = scrollback::matches(&text, q);
                line = scrollback::mark(line, &at, lit);
            }
            if let Some(((l0, c0), (l1, c1))) = chars
                && (l0..=l1).contains(&i)
            {
                let len = text.chars().count();
                let from = if i == l0 { c0 } else { 0 };
                let to = if i == l1 { (c1 + 1).min(len) } else { len };
                line = scrollback::mark(line, &[(from, to)], picked);
            }
            if i == sb.cursor {
                let c = sb.col_on(&text);
                line = scrollback::mark(line, &[(c, c + 1)], block);
            }
            let bg = if sel.is_some_and(|(a, b)| (a..=b).contains(&i)) {
                Some(p.hl_med)
            } else if i == sb.cursor {
                Some(p.overlay)
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
    // A bar in the margin marks the cursor's line, which a
    // fill alone barely sets apart.
    if let Some(row) = sb
        .cursor
        .checked_sub(sb.top)
        .and_then(|r| u16::try_from(r).ok())
        && row < text.height
        && text.width > 0
    {
        let cell = &mut f.buffer_mut()[(text.x, text.y + row)];
        if cell.symbol() == " " {
            cell.set_symbol("▌").set_fg(p.iris);
        }
    }
    if lines.len() > h {
        draw::scrollbar(f, p, screen, sb.top, lines.len());
    }
}

/// How wide a row's work-item badges may be in a list `width` wide.
fn badge_budget(width: u16) -> usize {
    (width as usize / 3).clamp(16, 48)
}

/// The harness session ID a log is named for: Claude names its logs
/// `ID.jsonl`, Pi `TIMESTAMP_ID.jsonl`.
fn log_session_id(log: &str) -> &str {
    let name = log.rsplit('/').next().unwrap_or(log);
    let stem = name.strip_suffix(".jsonl").unwrap_or(name);
    stem.rsplit_once('_').map_or(stem, |(_, id)| id)
}

/// The help float's sections for the transcript, scrollback and the
/// floats, which say the same whatever is highlighted.
fn help_float_sections() -> Vec<help::Section> {
    use help::Section;
    vec![
        Section::new(
            "Transcript",
            &[
                ("⌃V", "scroll back"),
                ("⇧↑ ⇧PgUp", "scroll back from here"),
                ("⌃L", "the task's diff, then commits, then back"),
                ("⌃U ⌃D", "scroll back, half a page"),
                ("⌃Y", "host, session ID and log path to the clipboard"),
                ("⇥", "message the agent"),
            ],
        ),
        Section::new(
            "Scrollback",
            &[
                ("j k ↑↓", "move a line"),
                ("⌃D ⌃U", "half a page"),
                ("⌃F ⌃B PgDn PgUp", "a page"),
                ("{ }", "previous, next blank line"),
                ("[ ]", "previous, next file in a diff, or comment"),
                ("⏎ on a listed file", "go to its patch"),
                ("g G Home End", "top, bottom"),
                ("/ ?", "search forward, back"),
                ("n N", "next, previous match"),
                ("h l ← →", "move a char"),
                ("w b e", "next word, word start, word end"),
                ("W B E", "the same by blank-separated WORDs"),
                ("0 ^ $", "line start, first non-blank, line end"),
                ("f F t T", "to, or just before, a char on the line"),
                ("; ,", "repeat that find, either way"),
                ("count", "repeat a motion, as 3w (NG: line N)"),
                ("v", "select text (again: drop it)"),
                ("V Space", "select lines (again: drop them)"),
                ("iw aw i\" i( …", "select a word, quote, brackets"),
                ("y", "copy the selection, then reply"),
                ("y + motion", "copy what it covers: yw ye y$ yiw yt)"),
                ("yy Y ⏎", "copy the line, then reply"),
                ("esc", "drop y, selection, search, then leave"),
                ("q ⌃C ⌃V", "leave"),
                ("⇥", "leave for the reply box"),
                ("⌃S", "collapse or expand the sidebar"),
            ],
        ),
        Section::new(
            "Reply box",
            &[
                ("⏎", "send (empty: Claude's suggestion)"),
                ("⇥ →", "take Claude's suggestion"),
                ("⌥⏎ ⇧⏎", "new line"),
                ("↑↓", "earlier messages, at the first or last line"),
                ("⇧↑↓ PgUp PgDn", "scroll the transcript"),
                ("⌃V", "scroll back, then back here"),
                ("⌃J ⌃K", "leave for the list, next or previous"),
                ("⌃U ⌃D", "leave for scrollback, half a page"),
                ("⌃X", "leave and arm an archive"),
                ("⌃G", "edit in the editor"),
                ("⌃S", "collapse or expand the sidebar"),
                ("⌃C", "clear, then leave"),
                ("esc", "leave"),
            ],
        ),
        Section::new(
            "Snooze prompt",
            &[
                ("⏎", "snooze (empty: wake)"),
                ("⌃U", "clear"),
                ("esc", "cancel"),
            ],
        ),
        Section::new(
            "Work items",
            &[
                ("⏎ o", "open in the browser"),
                ("v", "read description and comments here"),
                ("j k ␣ b g G", "scroll the reader; esc q back"),
                ("c", "comment on a card, issue or PR (asks first)"),
                ("⌥⏎", "new line in a comment"),
                ("x", "close a card or issue (asks first)"),
                ("n", "move a card to Not Now (asks first)"),
                ("r", "mark a draft PR ready (asks first)"),
                ("⏎ y", "confirm; other keys go back"),
                ("↑↓ j k", "move"),
                ("esc ⌃B", "dismiss"),
            ],
        ),
        Section::new(
            "This float",
            &[
                ("/", "filter"),
                ("⌃U", "clear the filter"),
                ("j k ↑↓", "scroll"),
                ("esc q", "close"),
            ],
        ),
    ]
}

// ---------------------------------------------------------------- app

// The picker's whole state: its flags (follow, kept, expanded, sidebar,
// quick, ...) are independent toggles of separate features, not states of
// one machine, so folding them into enums would only obscure them.
#[allow(clippy::struct_excessive_bools)]
struct App {
    p: Palette,
    sessions: Vec<Session>,
    tabs: Vec<Tab>,
    tasks: Vec<Task>,
    /// The tab the picker opened from, as (`session_id`, `window_id`); the
    /// selection follows it until John moves.
    current: Key,
    follow: bool,
    view: View,
    profile: Profile,
    /// In the archive, hide the tasks an archive ended (ctrl-f).
    kept: bool,
    /// In the archive, a `g` was typed outside search: another jumps to
    /// the first row, as vim's `gg` does.
    g_typed: bool,
    /// In the archive, only this project's tasks (chosen with Tab); None
    /// lists all.
    project: Option<String>,
    /// The Archive's open project chooser; Enter picks, Escape closes it.
    projects: Option<projects::Chooser>,
    query: String,
    /// Whether `/` has focused the search box, so letters type into it;
    /// Escape or Enter leaves it. The quick float is always searching.
    searching: bool,
    matcher: Matcher,
    rows: Vec<Item>,
    hits: HashMap<Item, Hits>,
    /// Inbox rows nested under the agent that spawned them (see `nest`),
    /// with the tree guides they are drawn with.
    tree: HashMap<Item, Guide>,
    /// Each Inbox row with rows nested under it: how many, folded or not
    /// (see `Fold`).
    folds: HashMap<Item, Fold>,
    /// The spawners whose children John unfolded (Right); the rest fold
    /// theirs away, so ctrl-j and ctrl-k pass over them.
    unfolded: HashSet<Key>,
    selected: Option<usize>,
    offset: usize,
    /// What ctrl-x has armed for the selected task; Enter confirms it.
    armed: Armed,
    /// The open ctrl-b work items float (see items.rs).
    items: Option<items::Items>,
    /// The open ctrl-z snooze prompt; Enter snoozes, Escape closes it.
    snooze: Option<Snooze>,
    /// The reply box while Tab has moved into it; Enter sends, Escape
    /// leaves it. Unfocused, it is drawn from `idle`.
    reply: Option<Reply>,
    /// The reply box as drawn while it doesn't have focus.
    idle: PromptBox,
    /// Messages typed into the reply box but neither sent nor cleared, by
    /// agent: leaving the box (Escape, scrollback, the list) keeps them, and
    /// it opens on them again.
    drafts: HashMap<Key, String>,
    /// Each agent's slash commands, by where it runs, once the reply box
    /// has been on it (None while they're being fetched), and the
    /// fetches' results.
    commands: HashMap<preview::Agent, Option<Vec<SlashCommand>>>,
    commands_tx: Sender<(preview::Agent, Result<Vec<SlashCommand>, String>)>,
    commands_rx: Receiver<(preview::Agent, Result<Vec<SlashCommand>, String>)>,
    /// Where the focused reply box was last drawn, and within what, for its
    /// slash-command menu.
    reply_card: Option<(Rect, Rect)>,
    /// Smooth prompt-box corners, when the terminal does kitty graphics, and
    /// the cell size they were drawn at, to draw them again after an editor.
    corners: Option<chrome::Corners>,
    cell: Option<chrome::CellSize>,
    /// Whether ctrl-g asked for the reply box's message in `$EDITOR`.
    edit_requested: bool,
    /// Pins and snoozes made here, by (host, `window_id)`: (pinned,
    /// `snoozed_until`, when), kept over deliveries until they agree.
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
    /// The picker's data, as pastes (see `standalone::Feed`).
    feed: Option<standalone::Feed>,
    /// A standalone archive running on its own thread, so the picker stays
    /// live meanwhile; its outcome becomes the note.
    archiving: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    /// The row a standalone archive is taking away, by its (session, window)
    /// key: left out of the lists from the moment John asks, so it goes at
    /// once rather than after the archive. Cleared by the first data that
    /// arrives once the archive has ended (see `deliver`).
    gone: Option<Key>,
    usage: usage::Usage,
    /// The Work view (see work.rs).
    work: work::Work,
    /// Sessions the Work view is starting: the item's URL and name, and
    /// the launch's outcome to come.
    launching: Vec<Launching>,
    /// A session the Work view started, to highlight in Inbox once its
    /// row is listed, and when it started.
    launched: Option<(Key, Instant)>,
    /// The last pane capture (see preview.rs), shown while its row is
    /// highlighted.
    preview: Option<preview::Preview>,
    /// The pane a standalone picker last asked its feed to capture.
    asked: Option<preview::Pane>,
    /// What the preview shows of the highlighted agent (ctrl-l).
    lens: changes::Lens,
    /// The last changes fetched (see changes.rs), shown while their row is
    /// highlighted and a lens other than the transcript is on.
    changes: Option<changes::Changes>,
    /// The tab a standalone picker last asked its feed for changes of.
    asked_changes: Option<preview::Pane>,
    /// The first line of the changes shown, from the top, and the tab and
    /// lens that is for.
    changes_top: (usize, Option<(preview::Pane, changes::Lens)>),
    /// The preview fills the picker (ctrl-s), for reading: with the
    /// sidebar, it is collapsed.
    expanded: bool,
    /// Inbox is drawn as the sidebar and the highlighted row (see
    /// `draw_split`), as of the last frame.
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
    /// A quick float on a host other than the config host, which feeds it
    /// and acts on its picks (see fed.rs): it fetches nothing itself.
    fed: bool,
    /// When a fed float reported its pick, until the config host answers.
    fed_pick: Option<Instant>,
    /// The config host answered a fed float's pick: quit.
    quit: bool,
    /// Folders the Inbox leaves out (`[inbox] exclude`), resolved.
    excluded: Vec<std::path::PathBuf>,
    /// The same folders as written, for tabs on other hosts.
    excluded_raw: Vec<String>,
}

impl App {
    fn new(current: Key, view: View) -> Self {
        let (commands_tx, commands_rx) = chan::channel();
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
            g_typed: false,
            project: None,
            projects: None,
            query: String::new(),
            searching: false,
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            rows: Vec::new(),
            hits: HashMap::new(),
            tree: HashMap::new(),
            folds: HashMap::new(),
            unfolded: HashSet::new(),
            selected: None,
            offset: 0,
            armed: Armed::No,
            items: None,
            snooze: None,
            reply: None,
            idle: PromptBox::new(),
            drafts: HashMap::new(),
            commands: HashMap::new(),
            commands_tx,
            commands_rx,
            reply_card: None,
            corners: None,
            cell: None,
            edit_requested: false,
            excluded: Vec::new(),
            excluded_raw: Vec::new(),
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
            gone: None,
            usage: usage::Usage::default(),
            work: work::Work::default(),
            launching: Vec::new(),
            launched: None,
            preview: None,
            asked: None,
            lens: changes::Lens::Transcript,
            changes: None,
            asked_changes: None,
            changes_top: (0, None),
            scroll: (0, None),
            scrollback: None,
            transcript: Rect::default(),
            copied: None,
            help: None,
            expanded: false,
            sidebar: false,
            quick: false,
            fed: false,
            fed_pick: None,
            quit: false,
        }
    }

    // ------------------------------------------------------------ data

    /// A bracketed paste: tabs, tasks, or a failure to get either.
    fn paste(&mut self, text: &str) {
        let Some((kind, body)) = protocol::data(text) else {
            // Anything else John pasted goes into a comment being typed, or
            // the reply box, if open.
            if self.items.as_mut().is_some_and(|i| i.paste(text)) {
                return;
            }
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
                    let next = if kind == "live" {
                        Tasks::Live
                    } else {
                        Tasks::Ready
                    };
                    // Only the full read, begun after the archive ended, can
                    // settle the row: the live read, and any read begun
                    // before the archive, still lists the task as open.
                    let settled =
                        kind == "tasks" && self.archiving.is_none() && !self.gone_open(&tasks);
                    self.deliver(|app| {
                        app.tasks = tasks;
                        app.tasks_load = next;
                        if settled {
                            app.gone = None;
                        }
                    });
                }
                Err(_) => self.tasks_failed(),
            },
            "tasks-error" => self.tasks_failed(),
            "usage" => self.usage.paste(body),
            "usage-error" => self.usage.error = Some(body.trim().to_string()),
            "preview" => self.preview = preview::parse(body).or(self.preview.take()),
            "changes" => self.changes = changes::parse(body).or(self.changes.take()),
            "current" => self.set_current(body),
            // cmd+1-4 on the picker's tab (see lua/switchyard/tab-picker.lua).
            "view" if !self.quick => {
                if let Some(view) = View::parse(body.trim()) {
                    self.set_view(view);
                }
            }
            // A fed float's pick: done, or why not (see fed.rs).
            "quit" if self.fed => self.quit = true,
            "note" if self.fed => {
                self.fed_pick = None;
                protocol::clear_title();
                self.note = Some((body.trim().to_string(), true));
            }
            _ => {}
        }
    }

    /// cmd+i came from the tab `current` (SESSION/WINDOW): highlight it, as
    /// a fresh picker would, unless a prompt is open over another row.
    /// Without a tab, just clear the search, keeping the highlighted row.
    fn set_current(&mut self, current: &str) {
        let current = current.trim();
        if current.is_empty() {
            if self.projects.is_none()
                && self.items.is_none()
                && self.snooze.is_none()
                && self.reply.is_none()
            {
                self.clear_search();
            }
            return;
        }
        let (s, w) = current.split_once('/').unwrap_or((current, ""));
        self.current = (s.to_string(), w.to_string());
        if self.projects.is_some()
            || self.items.is_some()
            || self.snooze.is_some()
            || self.reply.is_some()
        {
            return;
        }
        self.follow = true;
        self.query.clear();
        self.searching = false;
        // cmd+i shows Inbox, where the tab is a row: from Archive or Usage,
        // switch there first, or the highlight lands on nothing visible.
        self.set_view(View::Inbox);
        self.refilter_to(Some(self.current.clone()));
    }

    /// Empty and leave the search, keeping the highlighted row.
    fn clear_search(&mut self) {
        self.searching = false;
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
        let want = if self.follow {
            Some(self.current.clone())
        } else {
            keep.clone()
        };
        self.refilter_to(want.clone());
        // Following the current tab that isn't shown yet: keep the old row.
        if self.follow && self.selected_key() != want {
            self.select_key(keep.clone());
        }
        self.armed = if self.selected_key() == keep && self.can_arm(armed) {
            armed
        } else {
            Armed::No
        };
        // The snooze prompt goes with its task.
        if self
            .snooze
            .as_ref()
            .is_some_and(|z| self.selected_key().as_ref() != Some(&z.key))
        {
            self.snooze = None;
        }
        // As does the work items float.
        if self
            .items
            .as_ref()
            .is_some_and(|i| self.selected_key().as_ref() != Some(&i.key))
        {
            self.items = None;
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
        self.focus_launched();
    }

    /// Highlight the session the Work view started, once Inbox lists it.
    /// Leaving Inbox gives up on it, as does its row not showing in time;
    /// an open prompt or float keeps its row until it closes.
    fn focus_launched(&mut self) {
        let Some((key, at)) = self.launched.clone() else {
            return;
        };
        if self.view != View::Inbox || at.elapsed() > LAUNCHED_TTL {
            self.launched = None;
            return;
        }
        if self.projects.is_some()
            || self.items.is_some()
            || self.snooze.is_some()
            || self.reply.is_some()
        {
            return;
        }
        if let Some(row) = self
            .rows
            .iter()
            .position(|&i| self.key_of(i).as_ref() == Some(&key))
        {
            self.selected = Some(row);
            self.follow = false;
            self.armed = Armed::No;
            self.launched = None;
        }
    }

    /// Hold the pins and snoozes made here over data from before them.
    fn apply_pending(&mut self) {
        let tasks = &mut self.tasks;
        self.pending.retain(|(host, window), (pinned, until, at)| {
            if at.elapsed() > PENDING_TTL {
                return false;
            }
            match tasks
                .iter_mut()
                .find(|t| t.ended.is_none() && &t.host == host && &t.window_id == window)
            {
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

    /// The (`session_id`, `window_id`) a row stands for.
    fn key_of(&self, item: Item) -> Option<Key> {
        match item {
            Item::Status(_) => None,
            Item::Tab(t) => {
                let tab = &self.tabs[t];
                Some((self.sessions[tab.session].id.clone(), tab.window_id.clone()))
            }
            Item::Task(k) => Some((
                self.tasks[k].session_id.clone(),
                self.tasks[k].window_id.clone(),
            )),
        }
    }

    fn selected_key(&self) -> Option<Key> {
        self.selected_item().and_then(|i| self.key_of(i))
    }

    /// Highlight `want`'s row; for a row folded away under its spawner
    /// (see `Fold`), the spawner's, else the first row.
    fn select_key(&mut self, want: Option<Key>) {
        let at = want.and_then(|w| self.row_of_key(&w));
        self.selected = at.or_else(|| self.rows.iter().position(selectable));
    }

    /// The row with this key, or, folded away, its nearest listed spawner's.
    fn row_of_key(&self, want: &Key) -> Option<usize> {
        let at = self
            .rows
            .iter()
            .position(|&i| self.key_of(i).as_ref() == Some(want));
        if at.is_some() || self.view != View::Inbox {
            return at;
        }
        let mut up = self.item_of_key(want).and_then(|i| self.parent_of(i));
        let mut seen = HashSet::new();
        while let Some(i) = up.filter(|&i| seen.insert(i)) {
            if let Some(at) = self.rows.iter().position(|&r| r == i) {
                return Some(at);
            }
            up = self.parent_of(i);
        }
        None
    }

    /// The listed tab, else the open task, with this key.
    fn item_of_key(&self, key: &Key) -> Option<Item> {
        (0..self.tabs.len())
            .map(Item::Tab)
            .chain((0..self.tasks.len()).map(Item::Task))
            .find(|&i| self.key_of(i).as_ref() == Some(key))
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

    /// The highlighted open task, which ctrl-r can pin (not in the archive)
    /// and ctrl-z snooze (in Inbox).
    fn selected_open_task(&self) -> Option<usize> {
        self.selected_item()
            .and_then(|i| self.task_index(i))
            .filter(|&k| self.tasks[k].ended.is_none())
    }

    fn can_pin(&self) -> bool {
        self.view != View::Archive && self.selected_open_task().is_some()
    }

    fn can_snooze(&self) -> bool {
        self.view == View::Inbox && self.selected_open_task().is_some()
    }

    /// The Agents group a row goes in: `pinned`, `snoozed` (which wins), or
    /// its status (empty when unknown). A scheduled agent, which will resume
    /// itself, is snoozed unless pinned.
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
            "scheduled" => "snoozed",
            "idle" => "idle",
            _ => "",
        }
    }

    /// Whether ctrl-x can arm an archive for the highlighted row: any agent
    /// tab, whether or not it has an open task (the archive adopts one
    /// without, or reopens an ended one whose tab is still open), an open
    /// task, or an ended one that left its worktree behind.
    fn armable(&self) -> bool {
        self.selected_agent_tab().is_some()
            || self
                .selected_task()
                .is_some_and(|t| t.ended.is_none() || t.left)
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
            Armed::Interrupt => self.interruptible(),
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
        Some(preview::Pane {
            target: s.target.clone(),
            session: s.id.clone(),
            window: tab.window_id.clone(),
        })
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
            Item::Status(_) => false,
        }
    }

    /// Whether a row is the picker's own tab: the plain tab cmd+i opens on
    /// the config host, labelled `switchyard` (see
    /// lua/switchyard/tab-picker.lua). Inbox leaves it out, as it is always
    /// there and cmd+i already shows it.
    fn is_picker_tab(&self, item: Item) -> bool {
        let Item::Tab(t) = item else { return false };
        let tab = &self.tabs[t];
        tab.name == PICKER_TAB
            && self.sessions[tab.session].target == "-"
            && self.is_plain_tab(item)
    }

    /// Whether a row is a tab with no agent.
    fn is_plain_tab(&self, item: Item) -> bool {
        matches!(item, Item::Tab(_)) && !self.is_agent(item)
    }

    /// Whether Inbox lists a row: never the picker's own tab (cmd+i shows
    /// it already), and a plain tab only in the quick float, which jumps to
    /// any tab.
    fn listed(&self, item: Item) -> bool {
        !self.is_gone(item)
            && !self.is_picker_tab(item)
            && (self.quick || !self.is_plain_tab(item))
            && !self.left_out(item)
    }

    /// Whether a tab sits in a folder `[inbox] exclude` names, or under
    /// one: resolved on the config host, compared as written elsewhere.
    fn left_out(&self, item: Item) -> bool {
        let Item::Tab(t) = item else { return false };
        let tab = &self.tabs[t];
        if self.sessions[tab.session].target == "-" {
            in_folders(&tab.dir, &self.excluded)
        } else {
            in_raw_folders(&tab.dir, &self.excluded_raw)
        }
    }

    /// Open tasks with no listed tab.
    fn tabless(&self) -> impl Iterator<Item = Item> + '_ {
        (0..self.tasks.len())
            .filter(|&k| self.tasks[k].ended.is_none() && self.tasks[k].tab.is_none())
            .map(Item::Task)
    }

    /// Orphan worktrees still there, for Inbox's Orphans group; none in the
    /// quick float.
    fn orphan_worktrees(&self) -> impl Iterator<Item = Item> + '_ {
        (0..self.tasks.len())
            .filter(|&k| !self.quick && self.tasks[k].is_orphan())
            .map(Item::Task)
    }

    /// The listed session a row belongs to: a tab's, or for a task with
    /// no listed tab, the session it ran in if that is listed.
    fn session_of(&self, item: Item) -> Option<usize> {
        match item {
            Item::Tab(t) => Some(self.tabs[t].session),
            Item::Task(k) => self
                .sessions
                .iter()
                .position(|s| s.id == self.tasks[k].session_id),
            Item::Status(_) => None,
        }
    }

    /// Where an agent goes among others in its group: most urgent first
    /// and, within a state that wants John, the one waiting longest first
    /// (unknown times last); the snoozed by when they wake (a snooze, else a
    /// scheduled agent's timer; unknown times last). Working and idle
    /// agents go by name: their status time moves with every status
    /// message, and rows that swap as agents report are hard to pick. Ties
    /// go by name too, so the order never depends on which host answered
    /// first.
    fn agent_key(&self, i: Item) -> (std::cmp::Reverse<u8>, u64, (String, String, String)) {
        let f = self.flat(i);
        let name = (
            f.project.to_string(),
            f.name.to_string(),
            f.host.to_string(),
        );
        if self.agent_group(i) == "snoozed" {
            let at = match (f.wake, f.alarm) {
                (wake, _) if wake > 0 => wake,
                (_, alarm) if f.state == "scheduled" && alarm > 0 => alarm,
                _ => u64::MAX,
            };
            return (std::cmp::Reverse(0), at, name);
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
            .filter(|&k| !self.is_gone(Item::Task(k)))
            .collect();
        rows.sort_by_key(|&k| std::cmp::Reverse(self.tasks[k].since));
        rows.into_iter().map(Item::Task).collect()
    }

    /// The projects the Archive's project filter cycles through, sorted: the
    /// projects of the archive's rows that the profile lets through. A row
    /// with no project is listed only under all.
    fn archive_projects(&self) -> Vec<String> {
        let projects: BTreeSet<String> = self
            .archive()
            .into_iter()
            .filter_map(|item| {
                let f = self.flat(item);
                (self.profile_passes(f.work) && !f.project.is_empty())
                    .then(|| f.project.to_string())
            })
            .collect();
        projects.into_iter().collect()
    }

    /// Tab in the Archive: open the project chooser on the shown project.
    fn open_projects(&mut self) {
        let projects = self.archive_projects();
        if projects.is_empty() {
            self.note = Some(("no ended tasks name a project".into(), true));
            return;
        }
        self.projects = Some(projects::Chooser::new(projects, self.project.clone()));
    }

    /// Keys while the project chooser is open; its pick filters the Archive.
    fn projects_key(&mut self, k: KeyEvent) {
        let Some(c) = &mut self.projects else { return };
        match c.key(k) {
            projects::Outcome::Stay => {}
            projects::Outcome::Close => self.projects = None,
            projects::Outcome::Pick(project) => {
                self.projects = None;
                self.project = project;
                self.refilter();
            }
        }
    }

    /// A tab or task row as the flat views show it: an idle, done or
    /// scheduled agent whose child tasks are working is waiting on them, so
    /// shown as working.
    fn flat(&self, item: Item) -> Flat<'_> {
        let mut f = self.flat_own(item);
        let window = match item {
            Item::Tab(i) => self.tabs[i].window_id.as_str(),
            Item::Task(k) => self.tasks[k].window_id.as_str(),
            Item::Status(_) => "",
        };
        if f.ended.is_none()
            && matches!(f.state, "idle" | "done" | "scheduled")
            && self.kids_working(window)
        {
            f.state = "working";
            f.waiting = true;
        }
        f
    }

    /// Whether an open task the agent in `window` spawned, or one of
    /// theirs in turn, is working.
    fn kids_working(&self, window: &str) -> bool {
        let mut seen = HashSet::new();
        let mut todo = vec![window];
        while let Some(w) = todo.pop() {
            if w.is_empty() || !seen.insert(w) {
                continue;
            }
            for t in self
                .tasks
                .iter()
                .filter(|t| t.ended.is_none() && t.parent_window == w)
            {
                // The tab's own status is newer than the task's.
                let state = match t.tab.map(|i| self.tabs[i].state) {
                    Some(State::Idle) | None => t.state.as_str(),
                    Some(s) => s.name(),
                };
                if state == "working" {
                    return true;
                }
                todo.push(&t.window_id);
            }
        }
        false
    }

    /// A tab or task row with its agent's own status.
    fn flat_own(&self, item: Item) -> Flat<'_> {
        match item {
            Item::Tab(i) => {
                let tab = &self.tabs[i];
                let session = &self.sessions[tab.session];
                match tab.task.map(|k| &self.tasks[k]) {
                    Some(t) => Flat {
                        project: &t.project,
                        name: t.shown_name(),
                        msg: &t.msg,
                        host: &t.host,
                        // The tab's own status is newer, but an idle or
                        // done agent with a timer pending is scheduled.
                        state: match tab.state {
                            State::Ready | State::Done if t.state == "scheduled" => &t.state,
                            State::Idle => &t.state,
                            s => s.name(),
                        },
                        ended: None,
                        left: false,
                        work: t.work(),
                        dirty: t.dirty,
                        since: t.since,
                        links: &t.links,
                        merged: &t.merged,
                        wake: if t.is_snoozed(self.now) {
                            t.snoozed_until
                        } else {
                            0
                        },
                        alarm: t.wake_at,
                        waiting: false,
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
                        alarm: 0,
                        waiting: false,
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
                alarm: 0,
                waiting: false,
            },
            Item::Task(k) => {
                let t = &self.tasks[k];
                Flat {
                    project: &t.project,
                    name: t.shown_name(),
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
                    wake: if t.is_snoozed(self.now) {
                        t.snoozed_until
                    } else {
                        0
                    },
                    alarm: t.wake_at,
                    waiting: false,
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
        self.tree.clear();
        self.folds.clear();
        let pattern = Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart);
        let filtering = !self.query.trim().is_empty();
        let best = match self.view {
            View::Inbox => self.filter_merged(&pattern, filtering),
            View::Archive => self.filter_archive(&pattern, filtering),
            View::Work => {
                self.work.refilter(&self.query, &self.tasks);
                None
            }
            View::Usage => None,
        };
        let want = if filtering {
            best.and_then(|i| self.key_of(i))
        } else {
            want
        };
        self.select_key(want);
        self.offset = 0;
    }

    /// Score an Inbox row against the search, recording its highlights;
    /// None when it doesn't match. Matches name (or title), folder,
    /// project, session, host, work items and a titled task's name.
    fn score_row(&mut self, item: Item, pattern: &Pattern) -> Option<u32> {
        let (name, project, host) = {
            let f = self.flat(item);
            (
                f.name.to_string(),
                f.project.to_string(),
                f.host.to_string(),
            )
        };
        let dir = match item {
            Item::Tab(t) => self.tabs[t].dir.clone(),
            _ => String::new(),
        };
        let label = self
            .session_of(item)
            .map(|s| self.sessions[s].label.clone())
            .unwrap_or_default();
        let links = self.task_of(item).map(Task::links_text).unwrap_or_default();
        // A task shown by its title is still found by its name.
        let branch = self
            .task_of(item)
            .map(|t| t.name.clone())
            .unwrap_or_default();
        let (score, f) = score_fields(
            &mut self.matcher,
            pattern,
            &[&name, &dir, &project, &label, &host, &links, &branch],
        )?;
        self.hits.insert(
            item,
            Hits {
                name: f[0].clone(),
                second: f[2].clone(),
                dir: f[1].clone(),
            },
        );
        Some(score)
    }

    /// The Inbox section a row goes in (see `SECTIONS`): an agent's group,
    /// or `tabs` for a tab with no agent.
    fn section(&self, item: Item) -> &'static str {
        if self.task_of(item).is_some_and(Task::is_orphan) {
            "orphaned"
        } else if self.is_agent(item) {
            self.agent_group(item)
        } else {
            "tabs"
        }
    }

    /// Inbox: one flat list across every session, in sections under
    /// headings (see `SECTIONS`). Agents go by urgency and age within
    /// theirs (see `agent_key`); tabs with no agent keep Rex's order. Each
    /// row carries its project, so no session headings are needed. Returns
    /// the best match.
    fn filter_merged(&mut self, pattern: &Pattern, filtering: bool) -> Option<Item> {
        let mut best: Option<(u32, Item)> = None;
        let mut sections: Vec<Vec<Item>> = vec![Vec::new(); SECTIONS.len()];
        let items: Vec<Item> = (0..self.tabs.len())
            .map(Item::Tab)
            .chain(self.tabless())
            .chain(self.orphan_worktrees())
            .collect();
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
            let at = SECTIONS
                .iter()
                .position(|&s| s == section)
                .unwrap_or(SECTIONS.len() - 1);
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
        self.nest(filtering);
        best.map(|(_, i)| i)
    }

    /// The row for the agent that spawned `item`'s task: the tab in its
    /// `parent_window`, or the open task there when its tab isn't listed.
    fn parent_of(&self, item: Item) -> Option<Item> {
        let window = self
            .task_of(item)
            .map(|t| t.parent_window.as_str())
            .filter(|w| !w.is_empty())?;
        if let Some(t) = self.tabs.iter().position(|t| t.window_id == window) {
            return Some(Item::Tab(t));
        }
        (0..self.tasks.len())
            .find(|&k| self.tasks[k].ended.is_none() && self.tasks[k].window_id == window)
            .map(Item::Task)
    }

    /// Whether a nested row is in its spawner's project, which the row
    /// over it already names.
    fn same_project_as_parent(&self, item: Item) -> bool {
        self.tree.contains_key(&item)
            && self
                .parent_of(item)
                .is_some_and(|up| self.project_of(up).0 == self.project_of(item).0)
    }

    /// Move each Inbox row whose spawner is listed too under it, as a tree
    /// (see `Guide`), most urgent child first. A pinned or snoozed child
    /// stays in its group unless its spawner is in that group too, so a pin
    /// or snooze still says where the row goes. Headings left with no rows
    /// go. A spawner's children stay folded away under it (see `Fold`)
    /// unless John unfolded it or a search (`filtering`) is listing rows.
    fn nest(&mut self, filtering: bool) {
        let listed: HashSet<Item> = self.rows.iter().copied().filter(selectable).collect();
        let mut parent: HashMap<Item, Item> = HashMap::new();
        for &item in &listed {
            let Some(up) = self
                .parent_of(item)
                .filter(|p| *p != item && listed.contains(p))
            else {
                continue;
            };
            let group = self.agent_group(item);
            if matches!(group, "pinned" | "snoozed") && self.agent_group(up) != group {
                continue;
            }
            parent.insert(item, up);
        }
        // A loop of spawners (a resumed tab's window reused, say) has no
        // root: cut it at each row found on its own way up.
        let looped: Vec<Item> = parent
            .keys()
            .copied()
            .filter(|&start| {
                let mut seen = HashSet::new();
                let mut at = start;
                while let Some(&up) = parent.get(&at) {
                    if up == start || !seen.insert(up) {
                        return up == start;
                    }
                    at = up;
                }
                false
            })
            .collect();
        for item in looped {
            parent.remove(&item);
        }
        if parent.is_empty() {
            return;
        }
        let mut children: HashMap<Item, Vec<Item>> = HashMap::new();
        for (&child, &up) in &parent {
            children.entry(up).or_default().push(child);
        }
        for kids in children.values_mut() {
            kids.sort_by_cached_key(|&i| self.agent_key(i));
        }
        let open: HashSet<Item> = children
            .keys()
            .copied()
            .filter(|&up| filtering || self.key_of(up).is_some_and(|k| self.unfolded.contains(&k)))
            .collect();
        for &up in children.keys() {
            let below = descendants(up, &children);
            let blocked = below.iter().any(|&i| self.flat(i).state == "blocked");
            self.folds.insert(
                up,
                Fold {
                    rows: below.len(),
                    open: open.contains(&up),
                    blocked,
                },
            );
        }
        let old = std::mem::take(&mut self.rows);
        let mut rows = Vec::with_capacity(old.len());
        for item in old {
            match item {
                Item::Status(_) => {
                    // A heading whose rows all moved under others goes.
                    if rows.last().is_some_and(|r| !selectable(r)) {
                        rows.pop();
                    }
                    rows.push(item);
                }
                _ if parent.contains_key(&item) => {}
                _ => {
                    rows.push(item);
                    descend(item, "", &children, &open, &mut rows, &mut self.tree);
                }
            }
        }
        if rows.last().is_some_and(|r| !selectable(r)) {
            rows.pop();
        }
        self.rows = rows;
    }

    /// Archive: one flat list of ended tasks, each row naming its project,
    /// newest first, with no headings. Matches project, name, host and the task's work items
    /// (`#42`, `card 1041`, `PR #57`), so an ended task can be found by its
    /// issue or card and resumed. Returns the best match.
    fn filter_archive(&mut self, pattern: &Pattern, filtering: bool) -> Option<Item> {
        // A project whose last task has left the archive is no longer
        // listed, so the filter goes back to all.
        let projects = self.archive_projects();
        if self.project.as_ref().is_some_and(|p| !projects.contains(p)) {
            self.project = None;
        }
        let mut best: Option<(u32, Item)> = None;
        // Newest first, as archive() sorts.
        for item in self.archive() {
            let (project, name, host, work) = {
                let f = self.flat(item);
                (
                    f.project.to_string(),
                    f.name.to_string(),
                    f.host.to_string(),
                    f.work,
                )
            };
            if !self.profile_passes(work) {
                continue;
            }
            if self.project.as_deref().is_some_and(|p| p != project) {
                continue;
            }
            if filtering {
                let links = self.task_of(item).map(Task::links_text).unwrap_or_default();
                let Some((score, f)) = score_fields(
                    &mut self.matcher,
                    pattern,
                    &[&project, &name, &host, &links],
                ) else {
                    continue;
                };
                self.hits.insert(
                    item,
                    Hits {
                        name: f[1].clone(),
                        second: f[0].clone(),
                        dir: Vec::new(),
                    },
                );
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

    /// As `step`, but past either end of the list it wraps to the other.
    /// In Inbox it passes over the Snoozed and Orphans sections (the last), rows nested
    /// there included, unless nothing else is left to pick.
    fn wrap_step(&mut self, delta: isize) {
        let Some(at) = self.selected else { return };
        self.moved();
        let snoozed = if self.view == View::Inbox {
            self.rows.iter().position(bottom_heading)
        } else {
            None
        }
        .unwrap_or(self.rows.len());
        let indexed: Vec<(usize, Item)> = self.rows.iter().copied().enumerate().collect();
        let awake = |&(i, r): &(usize, Item)| i < snoozed && selectable(&r);
        self.selected = Some(if indexed.iter().any(awake) {
            draw::step_wrapping(&indexed, at, delta, awake)
        } else {
            draw::step_wrapping(&self.rows, at, delta, selectable)
        });
    }

    /// Right: list the highlighted spawner's children under it (see
    /// `Fold`), or, already listed, move to the first.
    fn unfold(&mut self) {
        let Some(item) = self.selected_item() else {
            return;
        };
        match self.folds.get(&item) {
            Some(f) if f.open => self.step(1),
            Some(_) => {
                if let Some(k) = self.key_of(item) {
                    self.unfolded.insert(k);
                }
                self.refilter();
            }
            None => {}
        }
    }

    /// Left: fold the highlighted spawner's children away, or a nested
    /// row's siblings, highlighting their spawner. While searching, every
    /// row stays listed.
    fn fold(&mut self) {
        let Some(item) = self.selected_item() else {
            return;
        };
        let up = if self.folds.get(&item).is_some_and(|f| f.open) {
            item
        } else if self.tree.contains_key(&item)
            && let Some(up) = self.parent_of(item)
        {
            up
        } else {
            return;
        };
        let Some(key) = self.key_of(up) else { return };
        self.moved();
        self.unfolded.remove(&key);
        self.refilter_to(Some(key));
    }

    /// Move to the first pick of the next group, or back to the first pick
    /// of this group (then the one before).
    fn jump_group(&mut self, forward: bool) {
        let Some(at) = self.selected else { return };
        let first_pick =
            |i: usize| selectable(&self.rows[i]) && i > 0 && !selectable(&self.rows[i - 1]);
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
        self.selected = if end {
            self.rows.iter().rposition(selectable)
        } else {
            self.rows.iter().position(selectable)
        };
    }

    fn set_view(&mut self, view: View) {
        // Only a picker with a feed lists anything (tests have none).
        if view == View::Work && self.view != View::Work && self.feed.is_some() {
            // On the highlighted row's project, when it's in the catalog.
            let project = self
                .selected_item()
                .map(|i| self.project_of(i).0)
                .filter(|p| !p.is_empty());
            self.work.open(project.as_deref());
        }
        self.armed = Armed::No;
        self.projects = None;
        self.items = None;
        self.snooze = None;
        self.close_reply();
        self.view = view;
        self.usage.offset = 0;
        // A search typed here carries over; an empty one ends.
        self.searching &= !self.query.is_empty();
        self.refilter();
    }

    // ------------------------------------------------------------ the pick

    /// What Enter would do with the highlighted row, as standalone.rs's
    /// `parse_pick` reads it.
    fn pick(&self) -> String {
        let Some(item) = self.selected_item() else {
            return String::new();
        };
        let task = self.task_of(item);
        let dirty = task.is_some_and(|t| t.dirty > 0);
        let action = match self.armed {
            Armed::Archive if dirty => "ask",
            Armed::Archive => "archive",
            Armed::Force => "force-archive",
            Armed::Interrupt => "interrupt",
            Armed::No => match task {
                Some(t) if t.ended.is_some() => "resume",
                Some(_) => "task",
                None => "go",
            },
        };
        match item {
            Item::Status(_) => String::new(),
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

    /// Open the work items float (ctrl-b) on the highlighted task's links,
    /// even just one; the footer says why when there are none.
    fn open_items(&mut self) {
        if self.view == View::Usage {
            return;
        }
        let Some(t) = self.selected_task() else {
            self.note = Some(("highlight a task to act on its work items".into(), true));
            return;
        };
        let (label, profile, links) = (t.label(), t.profile.clone(), chooser_links(&t.links));
        let Some(key) = self.selected_key() else {
            return;
        };
        match items::Items::new(key, label.clone(), profile, links) {
            Some(i) => {
                self.armed = Armed::No;
                self.moved();
                self.items = Some(i);
            }
            None => self.note = Some((format!("{label} has no linked work items"), true)),
        }
    }

    /// A key for the work items float: what it does to the float here, and
    /// what it asks the picker to run (opening or an action), if anything.
    fn items_key(&mut self, k: KeyEvent) -> Option<items::Outcome> {
        let i = self.items.as_mut()?;
        self.note = None;
        match i.key(k) {
            items::Outcome::Stay => None,
            items::Outcome::Close => {
                self.items = None;
                None
            }
            items::Outcome::Help => {
                self.help = Some(help::Help::default());
                None
            }
            items::Outcome::Refuse(why) => {
                self.note = Some((why, true));
                None
            }
            run => Some(run),
        }
    }

    /// Pin or unpin the highlighted open task here; whether it could.
    fn toggle_pin(&mut self) -> bool {
        if !self.can_pin() {
            return false;
        }
        let Some(k) = self.selected_open_task() else {
            return false;
        };
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
        self.pending.insert(
            (t.host.clone(), t.window_id.clone()),
            (pinned, until, Instant::now()),
        );
    }

    /// Open the snooze prompt on the highlighted open task in Inbox.
    fn open_snooze(&mut self) {
        if !self.can_snooze() {
            return;
        }
        let (Some(k), Some(key)) = (self.selected_open_task(), self.selected_key()) else {
            return;
        };
        let t = &self.tasks[k];
        let current = if t.is_snoozed(self.now) {
            t.snoozed_until
        } else {
            0
        };
        let mut z = Snooze {
            key,
            label: t.label(),
            current,
            text: String::new(),
            until: Ok(0),
        };
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
        let Some(k) = self.selected_open_task() else {
            return;
        };
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
    /// `finish_snooze`), or Escape or ctrl-c to close it.
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

    /// Open the reply box on the highlighted agent in Inbox. An agent waiting
    /// on a prompt is refused: the message would answer it.
    fn open_reply(&mut self) {
        let Some(item) = self.selected_item() else {
            return;
        };
        if self.preview_want().is_none() {
            self.note = Some(("highlight an agent to message it".into(), true));
            return;
        }
        let Some(key) = self.selected_key() else {
            return;
        };
        let label = self.selected_label();
        if self.flat(item).state == "blocked" {
            self.note = Some((
                format!("{label} is waiting on a prompt; answer it in its tab"),
                true,
            ));
            return;
        }
        self.moved();
        let mut prompt = PromptBox::new();
        if let Some(draft) = self.drafts.get(&key) {
            prompt.set_text(draft);
        }
        self.reply = Some(Reply {
            key,
            label,
            prompt,
            history: history::History::default(),
            taken: None,
        });
    }

    /// Close the reply box, keeping its message as the agent's draft (see
    /// `drafts`); an empty box, or one holding only the suggestion Tab
    /// took, drops it.
    fn close_reply(&mut self) {
        if let Some(r) = self.reply.take() {
            r.prompt.discard();
            let text = r.prompt.text();
            if text.is_empty() || r.taken.as_deref() == Some(&*text) {
                self.drafts.remove(&r.key);
            } else {
                self.drafts.insert(r.key, r.prompt.field.lines().join("\n"));
            }
        }
    }

    /// Close the reply box once its message is sent, dropping the draft
    /// (`switchyard task act message` also removes the prompt file).
    fn sent_reply(&mut self) -> Option<Reply> {
        let r = self.reply.take()?;
        r.prompt.discard();
        self.drafts.remove(&r.key);
        Some(r)
    }

    /// The prompt Claude Code suggests next to the reply box's agent, while
    /// the box is empty and the agent waits for one: the box shows it as
    /// ghost text, Tab or Right takes it and Enter sends it.
    fn reply_suggestion(&self) -> Option<String> {
        let r = self.reply.as_ref()?;
        let waiting = self
            .selected_item()
            .is_some_and(|i| matches!(self.flat_own(i).state, "idle" | "done"));
        if !waiting || !r.prompt.is_empty() {
            return None;
        }
        self.preview
            .as_ref()
            .filter(|pv| pv.pane.window == r.key.1)?
            .suggestion
            .clone()
    }

    /// The reply box's Enter once the message is sent (`switchyard task act
    /// message` also removes the prompt file).
    fn finish_reply(&mut self) {
        if let Some(r) = self.sent_reply() {
            self.note = Some((format!("sent to {}", r.label), false));
        }
    }

    /// Edit the reply box's message in `$VISUAL` or `$EDITOR` (ctrl-g), then
    /// put the saved text back; ctrl-z undoes that.
    fn edit_reply(&mut self, term: &mut DefaultTerminal) -> io::Result<()> {
        let Some(r) = &mut self.reply else {
            return Ok(());
        };
        let before = r.prompt.field.lines().join("\n");
        let fills = [self.p.overlay, self.p.surface];
        let edited = prompt_box::edit_in_editor(
            term,
            &r.prompt,
            &mut self.corners,
            self.cell,
            fills,
            || {},
        )?;
        if let Some(text) = edited.filter(|text| *text != before) {
            r.prompt.clear();
            r.prompt.paste(&text);
        }
        Ok(())
    }

    /// Keys while the reply box has focus: edit it (alt- or shift-Enter for
    /// a newline), scroll the preview with the page keys or shift and the
    /// arrows, Enter (see `finish_reply`), or Escape to leave it (Tab too,
    /// while it's empty and shows no suggestion, or holds only the one Tab
    /// took; see `reply_suggestion`).
    /// ctrl-c clears the message, then leaves the box;
    /// ctrl-g edits it in the editor (see `edit_reply`).
    fn reply_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let newline = k
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT);
        let page = self.page();
        let arrow = k.modifiers.is_empty() && matches!(k.code, KeyCode::Up | KeyCode::Down);
        let messages = if arrow {
            self.reply_messages()
        } else {
            Vec::new()
        };
        let ghost = self.reply_suggestion();
        let Some(r) = &mut self.reply else { return };
        // The slash-command menu, while open, has Tab, the arrows and Escape.
        if r.prompt.menu_key(k) {
            return;
        }
        // Up on the first row recalls an earlier message, Down on the last a
        // later one; elsewhere they move the cursor.
        if arrow {
            let before = r.prompt.field.cursor();
            r.prompt.key(k);
            if r.prompt.field.cursor() == before {
                let recalled = if k.code == KeyCode::Up {
                    r.history
                        .older(&messages, &r.prompt.field.lines().join("\n"))
                } else {
                    r.history.newer(&messages)
                };
                if let Some(text) = recalled {
                    r.prompt.set_text(&text);
                }
            }
            return;
        }
        // Tab or Right takes Claude's suggestion into the empty box.
        if let Some(g) = ghost
            .filter(|_| k.modifiers.is_empty() && matches!(k.code, KeyCode::Tab | KeyCode::Right))
        {
            r.prompt.set_text(&g);
            r.taken = Some(g);
            return;
        }
        let untouched = r.prompt.is_empty() || r.taken.as_deref() == Some(&*r.prompt.text());
        match k.code {
            KeyCode::Esc => self.close_reply(),
            KeyCode::Tab | KeyCode::BackTab if untouched => self.close_reply(),
            KeyCode::Tab | KeyCode::BackTab => {}
            KeyCode::Enter if newline => r.prompt.field.insert_newline(),
            KeyCode::Enter => self.finish_reply(),
            KeyCode::Char('c') if ctrl && !r.prompt.is_empty() => r.prompt.clear(),
            KeyCode::Char('c') if ctrl => self.close_reply(),
            KeyCode::Char('v') if ctrl => self.enter_scrollback(None),
            // ctrl-s collapses or expands the sidebar, keeping the box.
            KeyCode::Char('s') if ctrl && self.view == View::Inbox => {
                self.expanded = !self.expanded;
            }
            // ctrl-j and ctrl-k leave the box for the list and move in it;
            // ctrl-u and ctrl-d leave it for scrollback, half a page along.
            KeyCode::Char(c @ ('j' | 'k')) if ctrl => {
                self.close_reply();
                self.wrap_step(if c == 'j' { 1 } else { -1 });
            }
            KeyCode::Char('u' | 'd') if ctrl => {
                self.close_reply();
                self.enter_scrollback(Some(k));
            }
            // ctrl-x leaves it to arm an archive of its agent's task.
            KeyCode::Char('x') if ctrl => {
                self.close_reply();
                self.arm_archive();
            }
            KeyCode::Char('g') if ctrl => self.edit_requested = true,
            KeyCode::PageUp => self.scroll_preview(page),
            KeyCode::PageDown => self.scroll_preview(-page),
            KeyCode::Up if shift => self.scroll_preview(3),
            KeyCode::Down if shift => self.scroll_preview(-3),
            _ => r.prompt.key(k),
        }
    }

    /// What John said earlier in the reply box's session, oldest first, as
    /// far back as its transcript goes.
    fn reply_messages(&self) -> Vec<String> {
        let Some(r) = &self.reply else {
            return Vec::new();
        };
        self.preview
            .as_ref()
            .filter(|pv| pv.pane.window == r.key.1)
            .map(preview::Preview::user_messages)
            .unwrap_or_default()
    }

    /// Give the reply box its agent's slash commands, once the transcript
    /// has said where the agent runs: fetched in the background the first
    /// time, then kept for the picker's life.
    fn sync_commands(&mut self) {
        while let Ok((agent, got)) = self.commands_rx.try_recv() {
            let list = got.unwrap_or_else(|e| {
                log(&format!(
                    "slash commands for {} on {}: {e}",
                    agent.harness, agent.host
                ));
                Vec::new()
            });
            self.commands.insert(agent, Some(list));
        }
        let Some(r) = &mut self.reply else { return };
        if r.prompt.has_commands() {
            return;
        }
        let Some(agent) = self
            .preview
            .as_ref()
            .filter(|pv| pv.pane.window == r.key.1)
            .and_then(|pv| pv.agent.clone())
        else {
            return;
        };
        match self.commands.get(&agent) {
            Some(Some(list)) => r
                .prompt
                .set_commands(list.clone(), commands::anywhere(&agent.harness)),
            Some(None) => {}
            None => {
                self.commands.insert(agent.clone(), None);
                let tx = self.commands_tx.clone();
                std::thread::spawn(move || {
                    let got =
                        commands::fetch(&agent.host, &agent.profile, &agent.harness, &agent.dir);
                    let _ = tx.send((agent, got));
                });
            }
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
        let k = self.list_letter(k);
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
        if self.projects.is_some() {
            return self.projects_key(k);
        }
        // standalone_key runs what the float asks for before it gets here.
        if self.items.is_some() {
            self.items_key(k);
            return;
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
        // ctrl-x arms an archive; on a worktree with uncommitted changes, the
        // next ctrl-x (typed by the config when Enter answers `ask`) moves on
        // to "are you sure". Enter and Escape never get here.
        if matches!(k.code, KeyCode::Char('x') if ctrl) && self.view != View::Work {
            self.arm_archive();
            return;
        }
        // ctrl-c arms an interrupt of a working agent; Enter sends it.
        if matches!(k.code, KeyCode::Char('c') if ctrl) && self.view == View::Inbox {
            self.arm_interrupt();
            return;
        }
        // standalone_key acts on Enter before it gets here.
        if k.code == KeyCode::Enter {
            self.armed = Armed::No;
            return;
        }
        self.armed = Armed::No;
        if self.view == View::Usage && self.usage_key(k) {
            return;
        }
        self.list_key(k);
    }

    /// Plain `j` and `k` act as ctrl-j and ctrl-k while the list has the
    /// keys: no search focused, and no float, prompt or reply box open.
    fn list_letter(&self, k: KeyEvent) -> KeyEvent {
        let plain = k.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        if !plain
            || !matches!(k.code, KeyCode::Char('j' | 'k'))
            || self.searching
            || self.quick
            || self.help.is_some()
            || self.scrollback.is_some()
            || self.items.is_some()
            || self.projects.is_some()
            || self.snooze.is_some()
            || self.reply.is_some()
            || (self.view == View::Work && self.work.chooser.is_some())
        {
            return k;
        }
        KeyEvent::new(k.code, KeyModifiers::CONTROL)
    }

    /// ctrl-x: arm the selected task's archive, or move an armed archive of
    /// a dirty worktree on to forcing it; a note says why when it can't.
    fn arm_archive(&mut self) {
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
    }

    /// Whether the highlighted row is an agent that is working, so ctrl-c
    /// can interrupt it.
    fn interruptible(&self) -> bool {
        self.preview_want().is_some()
            && self
                .selected_item()
                .is_some_and(|i| self.flat_own(i).state == "working")
    }

    /// ctrl-c: arm an interrupt of the highlighted agent, which must be
    /// working; a note says why when it can't. Again disarms it.
    fn arm_interrupt(&mut self) {
        if self.armed == Armed::Interrupt {
            self.armed = Armed::No;
            return;
        }
        self.armed = Armed::No;
        if self.preview_want().is_none() {
            self.note = Some(("highlight an agent to interrupt it".into(), true));
            return;
        }
        let Some(item) = self.selected_item() else {
            return;
        };
        let label = self.selected_label();
        self.note = match self.flat_own(item).state {
            "working" => {
                self.armed = Armed::Interrupt;
                None
            }
            "blocked" => Some((
                format!("{label} is waiting on a prompt; answer it in its tab"),
                true,
            )),
            _ => Some((format!("{label} isn't working: nothing to interrupt"), true)),
        };
    }

    /// ctrl-y: copy where the highlighted agent's session log is, as
    /// `host:`, `session:` (the harness's session ID) and `log:` (its path
    /// on that host) lines, and say so in the footer; a note says why when
    /// there's none.
    fn copy_log(&mut self) {
        let Some(want) = self.preview_want() else {
            self.note = Some(("highlight an agent to copy its log path".into(), true));
            return;
        };
        let Some(pv) = self.preview.as_ref().filter(|pv| pv.pane == want) else {
            self.note = Some(("its transcript hasn't loaded yet".into(), true));
            return;
        };
        let Some(log) = pv.log.clone() else {
            self.note = Some(("no session log path for this agent".into(), true));
            return;
        };
        let host = pv.agent.as_ref().map(|a| a.host.clone());
        let id = log_session_id(&log);
        let host_line = host.as_ref().map(|h| format!("host: {h}\n"));
        let text = format!("{}session: {id}\nlog: {log}", host_line.unwrap_or_default());
        let parts: Vec<&str> = host.as_deref().into_iter().chain([id]).collect();
        self.note = Some((format!("copied {} and log path", parts.join(" · ")), false));
        self.copied = Some(text);
    }

    /// The Usage view's scrolling keys; false for keys it leaves to the
    /// list.
    fn usage_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.page();
        let delta = match k.code {
            KeyCode::Up | KeyCode::Char('p' | 'k') if k.code == KeyCode::Up || ctrl => Some(-1),
            KeyCode::Down | KeyCode::Char('n' | 'j') if k.code == KeyCode::Down || ctrl => Some(1),
            KeyCode::PageUp => Some(-page),
            KeyCode::PageDown => Some(page),
            KeyCode::Home => {
                self.usage.offset = 0;
                return true;
            }
            KeyCode::End => {
                self.usage.offset = usize::MAX;
                return true;
            }
            _ => None,
        };
        if let Some(delta) = delta {
            self.usage.offset = self.usage.offset.saturating_add_signed(delta);
            return true;
        }
        false
    }

    /// The list's keys: search, moving, views and the floats they open.
    fn list_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let page = self.page();
        if self.view == View::Archive && self.archive_jump(k, ctrl || alt) {
            return;
        }
        match k.code {
            // standalone_key acts on ctrl-r and ctrl-t.
            KeyCode::Char('z') if ctrl => self.open_snooze(),
            KeyCode::Char('b') if ctrl => self.open_items(),
            KeyCode::Tab | KeyCode::BackTab if self.view == View::Inbox => self.open_reply(),
            KeyCode::Char('s') if ctrl && self.view == View::Inbox => {
                self.expanded = !self.expanded;
            }
            KeyCode::Char('v') if ctrl && self.view == View::Inbox => self.enter_scrollback(None),
            KeyCode::Char('l') if ctrl && self.view == View::Inbox => self.lens = self.lens.next(),
            KeyCode::Char('y') if ctrl && self.view == View::Inbox => self.copy_log(),
            // ctrl-u and ctrl-d scroll the transcript (or the changes) half
            // a page, in scrollback; search finds a row faster than paging
            // the list.
            KeyCode::Char('u' | 'd') if ctrl && self.view == View::Inbox => {
                self.enter_scrollback(Some(k));
            }
            KeyCode::Char('p') if ctrl => self.step(-1),
            KeyCode::Char('n') if ctrl => self.step(1),
            // ctrl-j and ctrl-k wrap past either end of the list, passing
            // over Inbox's Snoozed rows.
            KeyCode::Char('k') if ctrl => self.wrap_step(-1),
            KeyCode::Char('j') if ctrl => self.wrap_step(1),
            // The archive is long and has no transcript, so ctrl-u and
            // ctrl-d page through it by half a page, as in scrollback.
            KeyCode::Char('u') if ctrl && self.view == View::Archive => {
                self.step(-(page / 2).max(1));
            }
            KeyCode::Char('d') if ctrl && self.view == View::Archive => {
                self.step((page / 2).max(1));
            }
            KeyCode::Char('u') if ctrl => {
                self.query.clear();
                self.refilter();
            }
            KeyCode::Char('w') if ctrl => self.delete_word(),
            KeyCode::Char('o') if ctrl => {
                self.profile = self.profile.next();
                self.refilter();
            }
            KeyCode::Char('a') if ctrl => self.set_view(if self.view == View::Archive {
                View::Inbox
            } else {
                View::Archive
            }),
            KeyCode::Char('f') if ctrl && self.view == View::Archive => {
                self.kept = !self.kept;
                self.refilter();
            }
            KeyCode::Tab | KeyCode::BackTab if self.view == View::Archive => self.open_projects(),
            KeyCode::Backspace if alt || ctrl => self.delete_word(),
            KeyCode::Backspace => {
                self.query.pop();
                self.refilter();
            }
            KeyCode::Esc if self.searching => self.clear_search(),
            KeyCode::Char('?') if !ctrl && !alt => self.help = Some(help::Help::default()),
            // `/` focuses the search box; until then, letters do nothing.
            KeyCode::Char('/') if !ctrl && !alt && !self.searching => {
                // Searching brings back the list it searches.
                self.expanded = false;
                self.searching = true;
            }
            KeyCode::Char(c) if !ctrl && !alt && self.searching => {
                self.query.push(c);
                self.moved();
                self.refilter();
            }
            // Shift scrolling back through the transcript, as the arrows
            // do with it expanded, goes into scrollback; it is already at
            // its end.
            KeyCode::Up | KeyCode::PageUp if shift || self.expanded => {
                self.enter_scrollback(Some(k));
            }
            KeyCode::Home if self.expanded => self.enter_scrollback(Some(k)),
            // The changes start at their top, so forward goes into
            // scrollback as back does.
            KeyCode::Down | KeyCode::PageDown | KeyCode::End
                if (shift || self.expanded) && self.lens != changes::Lens::Transcript =>
            {
                self.enter_scrollback(Some(k));
            }
            KeyCode::Down | KeyCode::PageDown if shift || self.expanded => {}
            KeyCode::End if self.expanded => {}
            KeyCode::Up if alt => self.jump_group(false),
            KeyCode::Down if alt => self.jump_group(true),
            KeyCode::Right if self.view == View::Inbox => self.unfold(),
            KeyCode::Left if self.view == View::Inbox => self.fold(),
            KeyCode::Up => self.step(-1),
            KeyCode::Down => self.step(1),
            KeyCode::PageUp => self.step(-page),
            KeyCode::PageDown => self.step(page),
            KeyCode::Home => self.edge(false),
            KeyCode::End => self.edge(true),
            _ => {}
        }
    }

    /// The Archive's vim jumps: `gg` and ctrl-g to the first row, `G` to
    /// the last. Letters only jump outside search, where they don't type.
    /// Whether `k` was one.
    fn archive_jump(&mut self, k: KeyEvent, modified: bool) -> bool {
        let g_typed = std::mem::take(&mut self.g_typed);
        match k.code {
            KeyCode::Char('g') if k.modifiers == KeyModifiers::CONTROL => self.edge(false),
            _ if modified || self.searching => return false,
            KeyCode::Char('g') if g_typed => self.edge(false),
            KeyCode::Char('g') => self.g_typed = true,
            KeyCode::Char('G') => self.edge(true),
            _ => return false,
        }
        true
    }

    /// How far the page keys move: a list's height in rows, less two for
    /// context (two-line rows in the sidebar).
    fn page(&self) -> isize {
        let lines = usize::from(self.list_area.height.saturating_sub(2).max(1));
        let rows = if self.sidebar && !self.expanded {
            (lines / SIDE_ROW_LINES).max(1)
        } else {
            lines
        };
        isize::try_from(rows).unwrap_or(isize::MAX)
    }

    /// Scroll the preview back (positive) or forward; `draw_preview` keeps it
    /// within the capture.
    fn scroll_preview(&mut self, delta: isize) {
        if self.lens == changes::Lens::Transcript {
            self.scroll.0 = self.scroll.0.saturating_add_signed(delta);
        } else {
            self.changes_top.0 = self.changes_top.0.saturating_add_signed(-delta);
        }
    }

    fn delete_word(&mut self) {
        delete_word(&mut self.query);
        self.refilter();
    }

    /// The highlighted agent's transcript (or the changes the lens shows),
    /// or in Work the highlighted item's side, as text, wrapped as last
    /// drawn, with its sections.
    fn transcript_text(&mut self) -> Option<(Vec<String>, scrollback::Nav)> {
        let p = self.p;
        if self.view == View::Work {
            let link = self.work.selected_link()?;
            return self.work.detail.text(&link, &p, self.now);
        }
        let pane = self.preview_want()?;
        let width = self.transcript.width;
        if width == 0 {
            return None;
        }
        if self.lens != changes::Lens::Transcript {
            let ch = self.changes.as_mut().filter(|c| c.pane == pane)?;
            let lines = ch.lines(self.lens, width, self.now, &p).ok()?;
            let text = lines.iter().map(scrollback::line_text).collect();
            return Some((text, ch.nav()));
        }
        let pv = self.preview.as_mut().filter(|pv| pv.pane == pane)?;
        let lines = pv.lines(width, &p).ok()?;
        let text = lines.iter().map(scrollback::line_text).collect();
        Some((text, pv.nav()))
    }

    /// Scrollback over the Work view's highlighted item, from the top of
    /// what its side shows.
    fn enter_work_scrollback(&mut self, k: Option<KeyEvent>) {
        let link = self.work.selected_link();
        let (Some(link), Some((lines, nav))) = (link, self.transcript_text()) else {
            self.note = Some(("nothing here to scroll through yet".into(), true));
            return;
        };
        if lines.is_empty() {
            return;
        }
        let h = self.work.detail.text_area().height as usize;
        let top = self.work.detail.top();
        let mut sb = scrollback::Scrollback::new(self.work.detail.pane(&link), lines.len(), h);
        sb.cursor = top.min(lines.len() - 1);
        sb.top = top;
        sb.nav = nav;
        sb.sync(lines.len(), h);
        if let Some(k) = k {
            sb.key(k, &lines, h);
            sb.sync(lines.len(), h);
        }
        self.scrollback = Some(sb);
    }

    /// Scrollback over the transcript, from the bottom of what the preview
    /// shows, then `k` in it.
    fn enter_scrollback(&mut self, k: Option<KeyEvent>) {
        if self.view == View::Work {
            return self.enter_work_scrollback(k);
        }
        let (Some(pane), Some((lines, nav))) = (self.preview_want(), self.transcript_text()) else {
            self.note = Some(("nothing here to scroll through".into(), true));
            return;
        };
        if lines.is_empty() {
            return;
        }
        let h = self.transcript.height as usize;
        let back = if self.lens != changes::Lens::Transcript {
            lines
                .len()
                .saturating_sub(h)
                .saturating_sub(self.changes_top.0)
        } else if self.scroll.1.as_ref() == Some(&pane) {
            self.scroll.0
        } else {
            0
        };
        let mut sb = scrollback::Scrollback::new(pane, lines.len(), h);
        sb.nav = nav;
        sb.cursor = sb.cursor.saturating_sub(back);
        sb.sync(lines.len(), h);
        if let Some(k) = k {
            sb.key(k, &lines, h);
            sb.sync(lines.len(), h);
        }
        self.scrollback = Some(sb);
    }

    fn scrollback_key(&mut self, k: KeyEvent) {
        // Tab, in Inbox, leaves for the reply box: back to the one it was
        // entered from, else a new one on the highlighted agent.
        if matches!(k.code, KeyCode::Tab | KeyCode::BackTab) && self.view == View::Inbox {
            self.leave_scrollback();
            if self.reply.is_none() {
                self.open_reply();
            }
            return;
        }
        // ctrl-s, in Inbox, still collapses or expands the sidebar, staying
        // in scrollback, even while a search is being typed.
        if k.code == KeyCode::Char('s')
            && k.modifiers.contains(KeyModifiers::CONTROL)
            && self.view == View::Inbox
        {
            self.expanded = !self.expanded;
            return;
        }
        let Some((lines, nav)) = self.transcript_text() else {
            return self.leave_scrollback();
        };
        let in_work = self.view == View::Work;
        let height = if in_work {
            self.work.detail.text_area().height
        } else {
            self.transcript.height
        } as usize;
        let Some(sb) = &mut self.scrollback else {
            return;
        };
        sb.nav = nav;
        match sb.key(k, &lines, height) {
            scrollback::Outcome::Stay => sb.sync(lines.len(), height),
            scrollback::Outcome::Exit => self.leave_scrollback(),
            scrollback::Outcome::Copy(from, to) => {
                let text = if !in_work && self.lens == changes::Lens::Transcript {
                    self.preview.as_ref().and_then(|pv| pv.source(from, to))
                } else {
                    lines.get(from..=to).map(|l| l.join("\n"))
                };
                let Some(text) = text else {
                    return self.leave_scrollback();
                };
                let count = text.lines().count();
                let what = format!("{count} line{}", if count == 1 { "" } else { "s" });
                self.yanked(text, &what);
            }
            scrollback::Outcome::Text(text) => {
                let count = text.lines().count();
                let what = if count > 1 {
                    format!("{count} lines")
                } else {
                    let n = text.chars().count();
                    format!("{n} char{}", if n == 1 { "" } else { "s" })
                };
                self.yanked(text, &what);
            }
        }
    }

    /// Copy `text` (`what` it is, for the note) and leave scrollback for
    /// the reply box, in Inbox, to paste it in: the one it was entered
    /// from, else a new one on the highlighted agent. Elsewhere it leaves
    /// for the picker.
    fn yanked(&mut self, text: String, what: &str) {
        self.copied = Some(text);
        self.leave_scrollback();
        self.note = None;
        if self.view == View::Inbox && self.reply.is_none() {
            self.open_reply();
        }
        // Why there is no reply box, if there is none, after the copy.
        let why = self.note.take().map(|(n, _)| format!(" · {n}"));
        self.note = Some((
            format!("copied {what}{}", why.as_deref().unwrap_or_default()),
            why.is_some(),
        ));
    }

    /// Back to the live end of the transcript.
    /// Leave scrollback: the transcript back at its end, the changes where
    /// scrollback left them.
    fn leave_scrollback(&mut self) {
        match self.scrollback.take() {
            Some(sb) if sb.pane.target == "work" => self.work.detail.set_top(sb.top),
            Some(sb) if self.lens != changes::Lens::Transcript => self.changes_top.0 = sb.top,
            _ => {}
        }
        self.scroll.0 = 0;
    }

    // ------------------------------------------------------------ drawing

    fn draw(&mut self, f: &mut Frame) {
        self.reply_card = None;
        let chips = self.header_chips();
        let room = (f.area().width as usize)
            .saturating_sub(4)
            .saturating_sub(spans_width(&chips));
        let badge = self.mode_badge();
        let room = room.saturating_sub(spans_width(&badge));
        let mut top = self.view_strip(room);
        top.extend(badge);
        if !chips.is_empty() {
            if top.is_empty() {
                top.push(Span::raw(" "));
            }
            top.extend(chips);
        }
        let inner = draw::frame_spans(f, &self.p, top, "", Margin::new(2, 1), self.border());
        // Down to the bottom border, which holds the footer.
        let content = Rect {
            height: inner.height + 1,
            ..inner
        };
        self.sidebar = !self.quick
            && self.view == View::Inbox
            && content.width >= SIDEBAR_MIN_WIDTH
            && content.height >= 10;
        if self.sidebar {
            self.draw_split(f, content);
        } else {
            self.draw_stacked(f, content);
        }
        self.draw_footer_in_border(f);
        // The slash-command menu goes over everything but the keys float.
        if let (Some((card, bounds)), Some(r)) = (self.reply_card, &mut self.reply) {
            r.prompt.draw_menu(f, &self.p, card, bounds);
        }
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
    /// ctrl-s collapses the sidebar, giving the right side the whole width.
    fn draw_split(&mut self, f: &mut Frame, area: Rect) {
        let p = self.p;
        let main = if self.expanded {
            area
        } else {
            let w = (area.width * 3 / 10).clamp(SIDEBAR_MIN, SIDEBAR_MAX);
            let [side, gap, main] = Layout::horizontal([
                Constraint::Length(w),
                Constraint::Length(3),
                Constraint::Min(0),
            ])
            .areas(area);
            let [search, rule, list] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .areas(side);
            self.draw_search(f, search);
            draw::rule(f, &p, rule);
            self.draw_list(f, list);
            let buf = f.buffer_mut();
            for y in gap.y..gap.y + gap.height {
                buf[(gap.x + 1, y)]
                    .set_symbol("│")
                    .set_style(Style::new().fg(p.hl_med));
            }
            main
        };
        let head_h = self.detail_rows(main.width);
        let rows = match &self.reply {
            Some(r) => r.prompt.rows(main.width, REPLY_MIN_ROWS, REPLY_MAX_ROWS),
            None => 1,
        };
        let card_h = if self.reply_hidden() {
            0
        } else {
            (rows + prompt_box::CHROME_ROWS).min(main.height.saturating_sub(head_h + 4))
        };
        let [head, rule, pane, _, card] = Layout::vertical([
            Constraint::Length(head_h),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(u16::from(card_h > 0)),
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
        self.draw_projects(f, main);
        self.draw_items(f, main);
        self.draw_snooze(f, main);
    }

    /// Rows the right side's header takes in `width`: the name line, its
    /// work items' titles (at most `DETAIL_LINK_ROWS`), then the status
    /// message wrapped to at most `DETAIL_MSG_ROWS`.
    fn detail_rows(&self, width: u16) -> u16 {
        let Some(item) = self.selected_item() else {
            return 1;
        };
        let t = self.flat(item);
        let titles = cells(titled(t.links).count());
        if t.msg.is_empty() {
            return 1 + titles;
        }
        1 + titles
            + cells(
                preview::wrap(t.msg, (width as usize).saturating_sub(2).max(1))
                    .len()
                    .min(DETAIL_MSG_ROWS),
            )
    }

    /// The right side's header: the highlighted row's status, project and
    /// name, with its work items, host and age on the right (a plain tab's
    /// folder), then its work items' titles and its whole status message.
    fn draw_detail(&self, f: &mut Frame, area: Rect) {
        let p = &self.p;
        let Some(item) = self.selected_item() else {
            f.render_widget(
                Line::styled(" Nothing highlighted", Style::new().fg(p.muted)),
                area,
            );
            return;
        };
        let t = self.flat(item);
        let titles = cells(titled(t.links).count());
        let [line, titles_area, msg_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(titles),
            Constraint::Min(0),
        ])
        .areas(area);
        let lines: Vec<Line> = titled(t.links)
            .map(|l| self.title_line(l, home_repo(t.links), titles_area.width as usize))
            .collect();
        f.render_widget(Paragraph::new(lines), titles_area);
        let plain = self.is_plain_tab(item);
        let (glyph, color, verb) = self.status_of(&t, item, tick(self.started));
        let (right, right_w) = match item {
            Item::Tab(tab) if plain => {
                let room = (area.width as usize).saturating_sub(str_width(t.name) + 6);
                let right = self.tab_right(tab, &[], p.base, true, room);
                let w = right.iter().map(|s| str_width(&s.content)).sum();
                (right, w)
            }
            _ => self.right_spans(&t, p.base, verb, area.width, None),
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
        let style = Style::new().fg(if t.state == "blocked" {
            p.gold
        } else {
            p.subtle
        });
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
        let lines: Vec<Line> = wrapped
            .into_iter()
            .map(|l| Line::styled(format!("  {l}"), style))
            .collect();
        f.render_widget(Paragraph::new(lines), msg_area);
    }

    /// A work item's ref and title, clipped to `width`, for the headers.
    fn title_line(&self, l: &Link, home: Option<&str>, width: usize) -> Line<'static> {
        let p = &self.p;
        let short = l.name(home);
        let room = width.saturating_sub(str_width(&short) + 4);
        Line::from(vec![
            Span::styled(format!("  {short}  "), Style::new().fg(p.muted)),
            Span::styled(
                preview::clip_width(&l.title, room),
                Style::new().fg(p.subtle),
            ),
        ])
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
        let body = if !self.quick
            && self.view == View::Inbox
            && !self.reply_hidden()
            && body.height >= prompt_box::CHROME_ROWS + 4
        {
            let rows = match &self.reply {
                Some(r) => r.prompt.rows(body.width, REPLY_MIN_ROWS, REPLY_MAX_ROWS),
                None => 1,
            };
            let h = (rows + prompt_box::CHROME_ROWS).min(body.height.saturating_sub(3));
            let [rest, card] =
                Layout::vertical([Constraint::Min(0), Constraint::Length(h)]).areas(body);
            reply_card = Some((card, h.saturating_sub(prompt_box::CHROME_ROWS)));
            rest
        } else {
            body
        };
        if self.view == View::Work {
            self.list_area = body;
            let p = self.p;
            let split = !self.quick && body.width >= SIDEBAR_MIN_WIDTH;
            let sb = self
                .scrollback
                .as_mut()
                .filter(|sb| sb.pane.target == "work");
            self.work.draw(f, body, &p, tick(self.started), split, sb);
        } else if self.view == View::Usage {
            self.list_area = body;
            let profile = match self.profile {
                Profile::All => None,
                p => Some(p.name()),
            };
            self.usage
                .draw(f, body, &self.p, self.now, &self.query, profile);
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
        if let Some((card, rows)) = reply_card {
            self.draw_reply(f, card, rows);
        }
        // Over the reply box, so their backdrop dims it too.
        self.draw_projects(f, body);
        self.draw_items(f, body);
        self.draw_snooze(f, body);
    }

    /// Inbox's list and the preview under it, when there is room for both:
    /// four fifths of the height for the preview, the transcript being
    /// what John reads, and the rest for the list; expanded (ctrl-s), the
    /// preview alone. The preview stays while John moves, whatever the
    /// row, so the list doesn't change height under him.
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
        let pane = body.height * 4 / 5;
        let [list, _, preview] = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(pane),
        ])
        .areas(body);
        Some((list, preview))
    }

    /// The highlighted agent's transcript (see preview.rs) under a rule
    /// naming it, scrolled to its end until John scrolls back.
    fn draw_preview(&mut self, f: &mut Frame, area: Rect) {
        let p = self.p;
        let p = &p;
        let [head, body] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
        let want = self.preview_want();
        let flat = want
            .as_ref()
            .and(self.selected_item())
            .map(|i| self.flat(i));
        let mut spans = vec![Span::styled("── ", Style::new().fg(p.overlay))];
        match &flat {
            Some(t) => {
                if !t.project.is_empty() && t.project != t.name {
                    spans.push(Span::styled(
                        format!("{} ", t.project),
                        Style::new().fg(p.subtle),
                    ));
                }
                spans.push(Span::styled(
                    format!("{} ", t.name),
                    Style::new().fg(p.text).add_modifier(Modifier::BOLD),
                ));
                // Its first work item's title, where there is room.
                let used: usize = spans.iter().map(|s| str_width(&s.content)).sum();
                if let Some(l) = titled(t.links).next() {
                    let short = l.name(home_repo(t.links));
                    let room = (head.width as usize).saturating_sub(used + str_width(&short) + 6);
                    if room >= 12 {
                        spans.push(Span::styled(
                            format!("· {short} "),
                            Style::new().fg(p.muted),
                        ));
                        spans.push(Span::styled(
                            format!("{} ", preview::clip_width(&l.title, room)),
                            Style::new().fg(p.subtle),
                        ));
                    }
                }
            }
            None => spans.push(Span::styled("Preview ", Style::new().fg(p.muted))),
        }
        let used: usize = spans.iter().map(|s| str_width(&s.content)).sum();
        spans.push(Span::styled(
            "─".repeat((head.width as usize).saturating_sub(used)),
            Style::new().fg(p.overlay),
        ));
        f.render_widget(Line::from(spans), head);
        self.draw_transcript(f, body);
    }

    /// The highlighted agent's model and context size, for the foot of the
    /// reply box, once its transcript has come and says.
    fn tokens_label(&self) -> Option<String> {
        let want = self.preview_want()?;
        let pv = self.preview.as_ref().filter(|pv| pv.pane == want)?;
        let model = pv.model.as_deref().map(preview::model_label);
        match (model, pv.tokens.map(preview::tokens_label)) {
            (Some(m), Some(t)) => Some(format!("{m} · {t}")),
            (Some(m), None) => Some(m.to_string()),
            (None, t) => t,
        }
    }

    /// The highlighted agent's transcript, scrolled to its end until John
    /// scrolls back.
    fn draw_transcript(&mut self, f: &mut Frame, screen: Rect) {
        let p = self.p;
        let p = &p;
        let muted = |text: &str| vec![Line::styled(text.to_string(), Style::new().fg(p.muted))];
        let want = self.preview_want();
        // Scrollback is for one agent's transcript.
        if self
            .scrollback
            .as_ref()
            .is_some_and(|sb| Some(&sb.pane) != want.as_ref())
        {
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
        if self.lens != changes::Lens::Transcript {
            return self.draw_changes(f, screen, &pane);
        }
        // Leave a column for the scrollbar.
        let text = Rect {
            width: screen.width.saturating_sub(2),
            ..screen
        };
        self.transcript = text;
        let Some(pv) = self.preview.as_mut().filter(|pv| pv.pane == pane) else {
            return draw::centered(f, screen, muted("Loading the transcript…"));
        };
        match pv.lines(text.width, p) {
            Err(e) => draw::centered(f, screen, muted(e)),
            Ok(lines) => {
                let h = text.height as usize;
                if let Some(sb) = &mut self.scrollback {
                    return draw_scrollback(f, p, screen, text, lines, sb);
                }
                let top = lines.len().saturating_sub(h);
                self.scroll.0 = self.scroll.0.min(top);
                let skip = top - self.scroll.0;
                f.render_widget(
                    Paragraph::new(lines[skip..lines.len().min(skip + h)].to_vec()),
                    text,
                );
                if lines.len() > h {
                    draw::scrollbar(f, p, screen, skip, lines.len());
                }
            }
        }
    }

    /// The highlighted task's changes, under a line naming the lenses
    /// (the one on lit) and summing up what it shows; from their top until
    /// John scrolls.
    fn draw_changes(&mut self, f: &mut Frame, area: Rect, pane: &preview::Pane) {
        let p = self.p;
        let p = &p;
        let muted = |text: &str| vec![Line::styled(text.to_string(), Style::new().fg(p.muted))];
        let [head, screen] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
        // Leave a column for the scrollbar.
        let text = Rect {
            width: screen.width.saturating_sub(2),
            ..screen
        };
        self.transcript = text;
        let lens = self.lens;
        let key = (pane.clone(), lens);
        if self.changes_top.1.as_ref() != Some(&key) {
            self.changes_top = (0, Some(key));
        }
        let ch = self.changes.as_mut().filter(|c| c.pane == *pane);
        let mut spans = Vec::new();
        for l in changes::Lens::ALL {
            let style = if l == lens {
                Style::new().fg(p.iris).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(p.muted)
            };
            spans.push(Span::styled(l.name(), style));
            spans.push(Span::raw("  "));
        }
        if let Some(sum) = ch.as_ref().and_then(|c| c.summary(lens)) {
            let used: usize = spans.iter().map(|s| str_width(&s.content)).sum();
            let room = (head.width as usize).saturating_sub(used);
            spans.push(Span::styled(
                preview::clip_width(&sum, room),
                Style::new().fg(p.subtle),
            ));
        }
        f.render_widget(Line::from(spans), head);
        let Some(ch) = ch else {
            return draw::centered(f, screen, muted("Reading the changes…"));
        };
        match ch.lines(lens, text.width, self.now, p) {
            Err(e) => draw::centered(f, screen, muted(e)),
            Ok(lines) => {
                if let Some(sb) = &mut self.scrollback {
                    return draw_scrollback(f, p, screen, text, lines, sb);
                }
                let h = text.height as usize;
                let top = self.changes_top.0.min(lines.len().saturating_sub(h));
                self.changes_top.0 = top;
                f.render_widget(
                    Paragraph::new(lines[top..lines.len().min(top + h)].to_vec()),
                    text,
                );
                if lines.len() > h {
                    draw::scrollbar(f, p, screen, top, lines.len());
                }
            }
        }
    }

    /// Whether the reply box gives way: nothing is highlighted (such as a
    /// search with no matches), so there is no one to message.
    fn reply_hidden(&self) -> bool {
        self.reply.is_none() && self.selected_item().is_none()
    }

    /// The reply box: the prompt box with a chip row naming the agent it
    /// goes to, filled brighter and barred in iris while it has focus.
    /// Unfocused, it says what Tab would do.
    fn draw_reply(&mut self, f: &mut Frame, card: Rect, rows: u16) {
        let p = self.p;
        let blocked = self
            .selected_item()
            .is_some_and(|i| self.flat(i).state == "blocked");
        let unfocused = if self.reply.is_some() {
            None
        } else {
            let label = self.preview_want().map(|_| self.selected_label());
            Some(match label {
                Some(l) if blocked => (
                    format!("{l} is waiting on a prompt; answer it in its tab"),
                    Some(l),
                ),
                Some(l) => ("Tab to type a message".to_string(), Some(l)),
                None => ("Highlight an agent to message it".to_string(), None),
            })
        };
        // Unfocused on an agent Tab would message, its draft shows.
        let draft = match &unfocused {
            Some((_, Some(_))) if !blocked => self
                .selected_key()
                .and_then(|k| self.drafts.get(&k).cloned())
                .unwrap_or_default(),
            _ => String::new(),
        };
        let tokens = self.tokens_label();
        let ghost = self.reply_suggestion();
        let (prompt, label, focused) = match (&mut self.reply, unfocused) {
            (Some(r), _) => {
                // The chip row already names the agent; Claude's suggestion
                // shows as ghost text.
                r.prompt
                    .set_placeholder(ghost.as_deref().unwrap_or("Type a message"));
                self.reply_card = Some((card, f.area()));
                (&mut r.prompt, Some(r.label.clone()), true)
            }
            (None, Some((placeholder, label))) => {
                if self.idle.field.lines().join("\n") != draft {
                    self.idle.set_text(&draft);
                }
                self.idle.set_placeholder(&placeholder);
                (&mut self.idle, label, false)
            }
            (None, None) => return,
        };
        let Some((chips, skip)) = prompt.draw(f, &p, card, rows, focused, self.corners.as_ref())
        else {
            return;
        };
        let fill = if focused { p.overlay } else { p.surface };
        if focused {
            // An iris bar down the left edge: the brighter fill alone
            // barely tells focus apart.
            let buf = f.buffer_mut();
            for y in card.y + 1..card.y + card.height.saturating_sub(1) {
                buf[(card.x, y)]
                    .set_symbol("▌")
                    .set_style(Style::new().fg(p.iris).bg(fill));
            }
        }
        let mut left = Vec::new();
        if let Some(label) = label {
            left.push(Span::styled("to ", Style::new().fg(p.muted).bg(fill)));
            let style = if focused {
                Style::new().fg(p.text).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(p.subtle)
            };
            left.push(Span::styled(label, style.bg(fill)));
        }
        let mut right = Vec::new();
        if skip > 0 {
            right.push(Span::styled(
                format!("+{skip} lines above  "),
                Style::new().fg(p.muted).bg(fill),
            ));
        }
        if !prompt.saved_ok {
            right.push(Span::styled("not saved ", Style::new().fg(p.love).bg(fill)));
        }
        if let Some(t) = tokens {
            right.push(Span::styled(
                format!("{t} "),
                Style::new().fg(p.muted).bg(fill),
            ));
        }
        let left_w: usize = left.iter().map(|s| str_width(&s.content)).sum();
        let right_w: usize = right.iter().map(|s| str_width(&s.content)).sum();
        if left_w + right_w < chips.width as usize {
            f.render_widget(Line::from(right).right_aligned(), chips);
        }
        f.render_widget(Line::from(left), chips);
    }

    /// The ctrl-z prompt, when open: a small box over the list with the
    /// typed text and when it means, or why it can't be read.
    fn draw_snooze(&self, f: &mut Frame, area: Rect) {
        let Some(prompt) = &self.snooze else { return };
        let p = &self.p;
        backdrop(f, p);
        let r = popup::list_rect(area, 4);
        let title = format!("Snooze {}", prompt.label);
        let inner = popup::open(f, r, popup::block(p, &title));
        if inner.height == 0 {
            return;
        }
        let input = Line::from(vec![
            Span::styled("until ", Style::new().fg(p.muted)),
            Span::styled(
                prompt.text.clone(),
                Style::new().fg(p.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled("▏", Style::new().fg(p.iris)),
        ]);
        f.render_widget(input, Rect { height: 1, ..inner });
        if inner.height < 2 {
            return;
        }
        let row = Rect {
            y: inner.y + 1,
            height: 1,
            ..inner
        };
        let line = match (&prompt.until, prompt.text.trim().is_empty()) {
            (Ok(0), _) => Line::from(vec![
                Span::styled(
                    format!(
                        "Snoozed until {} · ",
                        snooze::describe(prompt.current, self.now)
                    ),
                    Style::new().fg(p.muted),
                ),
                Span::styled("⏎", Style::new().fg(p.text).add_modifier(Modifier::BOLD)),
                Span::styled(" wakes it", Style::new().fg(p.muted)),
            ]),
            (Ok(until), _) => {
                Line::styled(snooze::describe(*until, self.now), Style::new().fg(p.pine))
            }
            (Err(_), true) => Line::styled(
                format!("Try {}", snooze::EXAMPLES),
                Style::new().fg(p.muted),
            ),
            (Err(e), false) => Line::styled(e.clone(), Style::new().fg(p.love)),
        };
        f.render_widget(line, row);
    }

    fn draw_projects(&self, f: &mut Frame, area: Rect) {
        if let Some(c) = &self.projects {
            c.draw(f, area, &self.p);
        }
    }

    /// The ctrl-b work items float, when open (see items.rs).
    fn draw_items(&mut self, f: &mut Frame, area: Rect) {
        if let Some(i) = &mut self.items {
            i.draw(f, area, &self.p);
        }
    }

    /// The frame's title strip of the views, the current one lit, in what
    /// `room` it has; just the current one's name if they all don't fit.
    /// None in the quick float, which has only its list. cmd+1-4 pick
    /// from it; the footer says so.
    /// The frame's border colour: iris in scrollback, so the whole window
    /// says it has the keys, else the usual quiet line.
    fn border(&self) -> Color {
        if self.scrollback.is_some() {
            self.p.iris
        } else {
            self.p.hl_med
        }
    }

    /// A pill in the title naming the mode that has the keys, if any.
    fn mode_badge(&self) -> Vec<Span<'static>> {
        let p = &self.p;
        let Some(sb) = &self.scrollback else {
            return Vec::new();
        };
        let label = match sb.anchor {
            Some(a) if a.linewise => "SCROLLBACK · SELECT LINES",
            Some(_) => "SCROLLBACK · SELECT",
            None => "SCROLLBACK",
        };
        let mut spans = draw::pill(
            vec![Span::styled(
                format!(" {label} "),
                Style::new().fg(p.base).add_modifier(Modifier::BOLD),
            )],
            p.iris,
            p.base,
        );
        spans.push(Span::raw(" "));
        spans
    }

    fn view_strip(&self, room: usize) -> Vec<Span<'static>> {
        if self.quick {
            return Vec::new();
        }
        let p = &self.p;
        let lit = Style::new().fg(p.iris).add_modifier(Modifier::BOLD);
        // After the corner and a dash, as the popups' titles start.
        let border = self.border();
        let dash = || Span::styled("─", Style::new().fg(border));
        let mut spans = vec![dash(), Span::raw(" ")];
        for (i, v) in View::ALL.into_iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" · ", Style::new().fg(p.overlay)));
            }
            let style = if v == self.view {
                lit
            } else {
                Style::new().fg(p.muted)
            };
            spans.push(Span::styled(v.name(), style));
        }
        spans.push(Span::raw(" "));
        if spans_width(&spans) > room {
            spans = vec![
                dash(),
                Span::raw(" "),
                Span::styled(self.view.name(), lit),
                Span::raw(" "),
            ];
        }
        spans
    }

    /// The active filters, for the frame's title.
    fn header_chips(&self) -> Vec<Span<'static>> {
        let p = &self.p;
        let mut spans = Vec::new();
        let mut chip = |text: &str, fg: Color| {
            spans.extend(pill(
                vec![Span::styled(text.to_string(), Style::new().fg(fg))],
                p.surface,
                p.base,
            ));
            spans.push(Span::raw(" "));
        };
        match self.profile {
            Profile::All => {}
            Profile::Work => chip("work", p.gold),
            Profile::Personal => chip("personal", p.subtle),
        }
        if let Some(project) = self
            .project
            .as_deref()
            .filter(|_| self.view == View::Archive)
        {
            chip(project, p.iris);
        }
        if self.view == View::Archive && self.kept {
            chip("not archived", p.foam);
        }
        if matches!(self.view, View::Usage | View::Work) {
            return spans;
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
        // The box lights up, with the cursor, only once `/` focuses it.
        let focused = self.searching || self.quick;
        let placeholder = match self.view {
            View::Inbox if self.sidebar => "Search",
            View::Inbox if self.quick => "Jump to a tab or agent",
            View::Inbox => "Search tabs, agents, and work items",
            View::Archive => "Search ended tasks",
            View::Usage => "Search accounts, harnesses, models, and hosts",
            View::Work if self.work.kind == work::Kind::Prs => "Search open pull requests",
            View::Work => "Search open work items",
        };
        if matches!(self.view, View::Usage | View::Work) {
            draw::search(f, p, area, &self.query, placeholder, focused, focused);
            return;
        }
        let shown = self.rows.iter().filter(|r| selectable(r)).count();
        let mut right = Vec::new();
        let spin = SPINNER[tick(self.started) % SPINNER.len()];
        match self.tasks_load {
            Tasks::Waiting | Tasks::Live if self.sidebar => {
                right.push(Span::styled(format!("{spin}  "), Style::new().fg(p.muted)));
            }
            Tasks::Waiting => right.push(Span::styled(
                format!("{spin} loading tasks  "),
                Style::new().fg(p.muted),
            )),
            Tasks::Live => right.push(Span::styled(
                format!("{spin} checking worktrees  "),
                Style::new().fg(p.muted),
            )),
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
                    right.push(Span::styled(
                        format!("{n} {label}  "),
                        Style::new().fg(color),
                    ));
                }
            }
        }
        let (total, kind) = if self.view == View::Archive {
            (
                self.tasks.iter().filter(|t| t.ended.is_some()).count(),
                "ended",
            )
        } else {
            let listed = (0..self.tabs.len())
                .filter(|&t| self.listed(Item::Tab(t)))
                .count();
            let total = listed + self.tabless().count() + self.orphan_worktrees().count();
            (total, if total == 1 { "tab" } else { "tabs" })
        };
        if typing || shown != total {
            right.push(Span::styled(shown.to_string(), Style::new().fg(p.text)));
            right.push(Span::styled(
                format!(" of {total} {kind} "),
                Style::new().fg(p.muted),
            ));
        } else {
            right.push(Span::styled(
                format!("{total} {kind} "),
                Style::new().fg(p.muted),
            ));
        }
        // The placeholder stops short of the counts, cut with … if need be:
        // less the icon's four cells, the counts and a space between.
        let right_w: usize = right.iter().map(|s| str_width(&s.content)).sum();
        let room = usize::from(area.width).saturating_sub(4 + right_w + 1);
        let placeholder = if room == 0 {
            String::new()
        } else {
            preview::clip_width(placeholder, room)
        };
        draw::search(f, p, area, &self.query, &placeholder, focused, focused);
        f.render_widget(Line::from(right).right_aligned(), area);
    }

    fn draw_list(&mut self, f: &mut Frame, area: Rect) {
        self.list_area = area;
        if self.rows.is_empty() {
            self.draw_empty(f, area);
            return;
        }
        if self.sidebar {
            return self.draw_side_list(f, area);
        }
        // The Archive's columns sit under headers, on the list's first line.
        let archive = self.view == View::Archive && area.height > 1;
        let (head, area) = if archive {
            let head = Rect { height: 1, ..area };
            let rest = Rect {
                y: area.y + 1,
                height: area.height - 1,
                ..area
            };
            self.list_area = rest;
            (Some(head), rest)
        } else {
            (None, area)
        };
        let h = area.height as usize;
        if let Some(sel) = self.selected {
            // Keep the group header in view when scrolling back to its
            // first pick.
            let header_above = sel > 0 && !selectable(&self.rows[sel - 1]);
            self.offset = draw::keep_in_view(self.offset, sel, h, header_above);
        }
        let tick = tick(self.started);
        let overflow = self.rows.len() > h && h > 0;
        // Leave a column for the scrollbar only when there is one.
        let list = Rect {
            width: area.width.saturating_sub(if overflow { 2 } else { 0 }),
            ..area
        };
        // Flat rows line up their project and name columns: size them for
        // the widest values, within what the busiest agent's right side
        // leaves (a plain tab's folder takes what its row has left).
        let flat = || self.rows.iter().copied().filter(selectable);
        let right_cols = archive.then(|| self.right_cols(list.width));
        let right_w = flat()
            .filter(|&i| !self.is_plain_tab(i))
            .map(|i| {
                self.right_spans(&self.flat(i), self.p.base, "", list.width, right_cols)
                    .1
            })
            .max()
            .unwrap_or(0);
        let room = (list.width as usize).saturating_sub(3 + right_w + 1);
        let proj_w = flat()
            .map(|i| str_width(self.flat(i).project))
            .max()
            .unwrap_or(0)
            .max(if archive { str_width("project") } else { 0 })
            .min(room / 4);
        // A plain tab's long name (a title a program set, say) stops at a
        // third of the row, so it can't crowd out the folder and the rows
        // around it.
        let name_cap = (list.width as usize / 3).max(24);
        let name_w = flat()
            .map(|i| {
                let w = str_width(self.flat(i).name);
                let guide = self.tree.get(&i).map_or(0, |g| str_width(&g.branch()));
                guide
                    + if self.is_plain_tab(i) {
                        w.min(name_cap)
                    } else {
                        w
                    }
            })
            .max()
            .unwrap_or(0)
            // A third is left for the message; the Archive shows none.
            .min(if archive {
                room.saturating_sub(proj_w + 4)
            } else {
                room.saturating_sub(proj_w + 2) * 2 / 3
            });
        let cols = (proj_w, name_w, right_cols);
        if let (Some(head), Some(right)) = (head, right_cols) {
            self.draw_archive_header(
                f,
                Rect {
                    width: list.width,
                    ..head
                },
                (proj_w, name_w),
                right,
            );
        }
        for (i, row) in self.rows.iter().enumerate().skip(self.offset).take(h) {
            let r = Rect {
                y: area.y + cells(i - self.offset),
                height: 1,
                ..list
            };
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

    /// The Archive's right-side columns, sized for its widest row.
    fn right_cols(&self, width: u16) -> RightCols {
        let flat = || self.rows.iter().copied().filter(selectable);
        RightCols {
            links: flat()
                .map(|i| {
                    let lead = self.lead_spans(&self.flat(i), self.p.base, width);
                    lead.iter().map(|s| str_width(&s.content)).sum::<usize>()
                })
                .max()
                .filter(|&w| w > 0)
                .map_or(0, |w| w.max(str_width("work items  "))),
            host: flat()
                .map(|i| str_width(self.flat(i).host))
                .max()
                .unwrap_or(0)
                .max(str_width("host")),
            when: flat()
                .map(|i| str_width(&self.when(&self.flat(i))) + 1)
                .max()
                .unwrap_or(0)
                .max(str_width(" age")),
        }
    }

    /// The Archive's column headers, over the columns its rows line up in.
    fn draw_archive_header(
        &self,
        f: &mut Frame,
        area: Rect,
        (proj_w, name_w): (usize, usize),
        right: RightCols,
    ) {
        let style = Style::new().fg(self.p.muted);
        let left = format!("   {:<w$}  {:<name_w$}", "project", "task", w = proj_w);
        f.render_widget(Line::styled(left, style), area);
        f.render_widget(
            Line::styled(right_header(right), style).right_aligned(),
            area,
        );
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
            "orphaned" => ("○", "Orphans", p.gold),
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
        let spin = || {
            Line::styled(
                SPINNER[tick(self.started) % SPINNER.len()],
                Style::new().fg(p.foam),
            )
        };
        let icon = |glyph: &'static str, color: Color| Line::styled(glyph, Style::new().fg(color));
        let text = |s: String| Line::styled(s, Style::new().fg(p.subtle));
        let muted = |s: String| Line::styled(s, Style::new().fg(p.muted));
        let filtered = match self.profile {
            Profile::All => None,
            profile => Some(profile.name()),
        };
        let lines = if self.query.trim().is_empty() {
            match self.view {
                View::Usage | View::Work => Vec::new(),
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
        } else {
            vec![
                icon(ICON_SEARCH, p.hl_high),
                Line::from(""),
                Line::from(vec![
                    Span::styled("Nothing matches ", Style::new().fg(p.subtle)),
                    Span::styled(format!("“{}”", self.query), Style::new().fg(p.text)),
                ]),
                if matches!(self.view, View::Archive | View::Inbox | View::Work) {
                    draw::key_hint(p, "⌃W", " deletes a word of the search")
                } else {
                    draw::key_hint(p, "⌃U", " clears the search")
                },
            ]
        };
        draw::centered(f, area, lines);
    }

    /// The row's background and left edge: iris when highlighted, love when
    /// an archive is armed for it.
    fn row_style(&self, selected: bool) -> (Color, Span<'static>) {
        let p = &self.p;
        let doomed = selected && self.armed.archives();
        let bg = if doomed {
            p.hl_med
        } else if selected {
            p.overlay
        } else {
            p.base
        };
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

    /// A flat row's work items and dirty count, the right side's lead.
    fn lead_spans(&self, t: &Flat, bg: Color, width: u16) -> Vec<Span<'static>> {
        let mut lead = self.badge_spans(t, bg, badge_budget(width));
        if t.dirty > 0 {
            lead.push(Span::styled(
                format!("✎{}  ", t.dirty),
                Style::new().fg(self.p.gold).bg(bg),
            ));
        }
        lead
    }

    /// The right side of a flat row, [work items] [dirty] [host chip]
    /// [state age], and its width; padded to `cols` when given, so rows
    /// line up whatever their links say.
    fn right_spans(
        &self,
        t: &Flat,
        bg: Color,
        verb: &str,
        width: u16,
        cols: Option<RightCols>,
    ) -> (Vec<Span<'static>>, usize) {
        let p = &self.p;
        let pad = |n: usize| Span::styled(" ".repeat(n), Style::new().bg(bg));
        let mut right = self.lead_spans(t, bg, width);
        if let Some(cols) = cols {
            let used: usize = right.iter().map(|s| str_width(&s.content)).sum();
            right.push(pad(cols.links.saturating_sub(used)));
        }
        let chip_fg = if t.work { p.gold } else { p.subtle };
        right.extend(pill(
            vec![Span::styled(t.host.to_string(), Style::new().fg(chip_fg))],
            p.surface,
            bg,
        ));
        if let Some(cols) = cols {
            right.push(pad(cols.host.saturating_sub(str_width(t.host))));
        }
        let when = match self.when(t) {
            text if text.is_empty() => text,
            text => format!(" {text}"),
        };
        let when_w = cols.map_or(4, |c| c.when);
        right.push(Span::styled(
            format!("  {verb:>9}{when:>when_w$} "),
            Style::new().fg(p.muted).bg(bg),
        ));
        let w = right.iter().map(|s| str_width(&s.content)).sum();
        (right, w)
    }

    /// A row's time: how long until a snoozed task wakes, or a scheduled
    /// agent resumes (nothing once due, or when unknown), else how long its
    /// status has held.
    fn when(&self, t: &Flat) -> String {
        match (t.wake, t.since) {
            (wake, _) if wake > 0 => age(wake.saturating_sub(self.now)),
            _ if t.state == "scheduled" && t.alarm > self.now => age(t.alarm - self.now),
            _ if t.state == "scheduled" => String::new(),
            (_, since) if since > 0 => age(self.now.saturating_sub(since)),
            _ => String::new(),
        }
    }

    /// A tab with no agent's right side: whether it is the current tab,
    /// and its folder within `room` columns.
    fn tab_right(
        &self,
        t: usize,
        hits: &[usize],
        bg: Color,
        selected: bool,
        room: usize,
    ) -> Vec<Span<'static>> {
        let p = &self.p;
        let tab = &self.tabs[t];
        let mut right = Vec::new();
        if self.is_current(t) {
            right.push(Span::styled(
                "current  ",
                Style::new()
                    .fg(p.iris)
                    .bg(bg)
                    .add_modifier(Modifier::ITALIC),
            ));
        }
        let used: usize = right.iter().map(|s| str_width(&s.content)).sum();
        let dir_w = str_width(&tab.dir)
            .min(room.saturating_sub(used + 1))
            .min(48);
        let hl = Style::new()
            .fg(p.rose)
            .bg(bg)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        let dir_style = Style::new()
            .fg(if selected { p.subtle } else { p.muted })
            .bg(bg);
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
            // Each outcome its own glyph: merged work landed; a closed
            // task left its worktree (open) or just its tab; an archived
            // one is boxed away.
            (Some("orphaned"), _) if t.left => ("○", p.gold, "orphan"),
            (Some(_), _) if t.left => ("○", p.gold, "closed"),
            (Some("merged"), _) => ("✓", p.iris, "merged"),
            (Some("archived" | "force-archived" | "discarded" | "force-discarded"), _) => {
                ("▢", p.subtle, "archived")
            }
            (Some("tab closed"), _) => ("×", p.muted, "closed"),
            (Some("worktree removed"), _) => ("−", p.muted, "removed"),
            (Some(_), _) => ("·", p.muted, "ended"),
            (None, "working") => (SPINNER[tick % SPINNER.len()], p.foam, "working"),
            (None, "scheduled") if t.alarm > self.now => ("◷", p.iris, "wakes in"),
            (None, "scheduled") if t.alarm == 0 => ("◷", p.iris, "bg job"),
            (None, "scheduled") => ("◷", p.iris, "waking"),
            (None, "blocked") => ("●", p.gold, "waiting"),
            (None, "done") => ("✓", p.pine, "done"),
            (None, "error") => ("✗", p.love, "failed"),
            (None, "idle") => ("·", p.muted, "idle"),
            (None, _) => ("?", p.muted, "no status"),
        };
        let (glyph, color) = if self.is_plain_tab(item) {
            ("·", p.muted)
        } else {
            (glyph, color)
        };
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
            Item::Status(_) => return (String::new(), false),
        };
        if let Some(k) = tab.task {
            return (self.tasks[k].project.clone(), true);
        }
        (folder_project(&tab.dir), false)
    }

    /// Lines a sidebar row takes: two for a tab or task, one for a heading
    /// or for a nested row with nothing to say under its name (see
    /// `side_detail`).
    fn side_height(&self, item: &Item) -> usize {
        if selectable(item) && self.side_detail(*item) {
            SIDE_ROW_LINES
        } else {
            1
        }
    }

    /// Whether a sidebar row has a second line: a project the row over it
    /// doesn't already name, a status message, or uncommitted files.
    fn side_detail(&self, item: Item) -> bool {
        let t = self.flat(item);
        !t.msg.is_empty()
            || t.dirty > 0
            || !(self.project_of(item).0.is_empty() || self.same_project_as_parent(item))
    }

    /// Inbox's list as the sidebar: each tab or task on two lines (see
    /// `draw_side_row`) under the one-line status headings, scrolled to keep
    /// the highlighted row, and the heading over it, in view. When it all
    /// fits, the Snoozed and Orphans groups sit at the bottom, the free space above them.
    fn draw_side_list(&mut self, f: &mut Frame, area: Rect) {
        let h = area.height as usize;
        let heights: Vec<usize> = self.rows.iter().map(|r| self.side_height(r)).collect();
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
        let list = Rect {
            width: area.width.saturating_sub(if overflow { 2 } else { 0 }),
            ..area
        };
        let tick = tick(self.started);
        // The free space goes above Snoozed or Orphans, even when it leads the list.
        let slack = if overflow { 0 } else { h.saturating_sub(total) };
        let snoozed = self.rows.iter().position(bottom_heading);
        let mut y = 0;
        for (i, row) in self.rows.iter().enumerate().skip(self.offset) {
            if Some(i) == snoozed {
                y += slack;
            }
            if y >= h {
                break;
            }
            let rows = cells(heights[i].min(h - y));
            let r = Rect {
                y: area.y + cells(y),
                height: rows,
                ..list
            };
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
        let hl = Style::new()
            .fg(p.rose)
            .bg(bg)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        let muted = Style::new().fg(p.muted).bg(bg);
        let width = r.width as usize;
        // The first line: status, name, age; a scheduled agent's countdown
        // says "in", which the wider rows' verb says.
        let when = match self.when(&t) {
            text if text.is_empty() => text,
            text if t.state == "scheduled" && t.wake == 0 => format!("in {text} "),
            text => format!("{text} "),
        };
        // A row an agent spawned hangs off its spawner's (see `nest`).
        let guide = self.tree.get(&item);
        let (branch, stem) = guide.map(|g| (g.branch(), g.stem())).unwrap_or_default();
        let guide_style = Style::new().fg(p.muted).bg(bg);
        let fold = self.fold_badge(item, bg);
        let room = width.saturating_sub(
            3 + str_width(&branch) + str_width(&when) + 1 + str_width(&fold.content),
        );
        let name_style = Style::new().fg(p.text).bg(bg);
        let name_style = if selected {
            name_style.add_modifier(Modifier::BOLD)
        } else {
            name_style
        };
        let mut left = vec![
            edge.clone(),
            Span::styled(branch, guide_style),
            Span::styled(format!("{glyph} "), Style::new().fg(color).bg(bg)),
        ];
        left.extend(highlight(t.name, &hits.name, room, false, name_style, hl));
        left.push(fold);
        let line = Rect { height: 1, ..r };
        f.render_widget(Line::from(left), line);
        // A span, not a styled Line: a Line's style colours its whole row.
        f.render_widget(Line::from(Span::styled(when, muted)).right_aligned(), line);
        if r.height < 2 {
            return;
        }
        // The second line, under the name: project · message, or a plain
        // tab's folder.
        let line = Rect {
            y: r.y + 1,
            height: 1,
            ..r
        };
        let dirty = if t.dirty > 0 {
            format!("✎{} ", t.dirty)
        } else {
            String::new()
        };
        let mut room = width.saturating_sub(3 + str_width(&stem) + str_width(&dirty) + 1);
        let mut left = vec![
            edge,
            Span::styled(stem, guide_style),
            Span::styled("  ", Style::new().bg(bg)),
        ];
        let push = |left: &mut Vec<Span<'static>>, spans: Vec<Span<'static>>, room: &mut usize| {
            *room = room.saturating_sub(spans.iter().map(|s| str_width(&s.content)).sum());
            left.extend(spans);
        };
        let (project, own) = self.project_of(item);
        if !project.is_empty() && !self.same_project_as_parent(item) {
            let style = Style::new().fg(p.iris).bg(bg);
            let hits = if own { hits.second.as_slice() } else { &[] };
            push(
                &mut left,
                highlight(&project, hits, room, false, style, hl),
                &mut room,
            );
            if !t.msg.is_empty() && room > 6 {
                push(&mut left, vec![Span::styled(" · ", muted)], &mut room);
            }
        }
        if !t.msg.is_empty() && room > 3 {
            let style = Style::new()
                .fg(if t.state == "blocked" {
                    p.gold
                } else {
                    p.muted
                })
                .bg(bg);
            let msg = t.msg.lines().next().unwrap_or("");
            push(
                &mut left,
                highlight(msg, &[], room, false, style, style),
                &mut room,
            );
        }
        f.render_widget(Line::from(left), line);
        f.render_widget(
            Line::from(Span::styled(dirty, Style::new().fg(p.gold).bg(bg))).right_aligned(),
            line,
        );
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
        cols: (usize, usize, Option<RightCols>),
    ) {
        let p = &self.p;
        let t = self.flat(item);
        let (bg, edge) = self.row_style(selected);
        f.buffer_mut().set_style(r, Style::new().bg(bg));
        let (glyph, color, verb) = self.status_of(&t, item, tick);
        let plain = self.is_plain_tab(item);
        // Right side first, so the columns and message get what is left; a
        // plain tab's folder gets what its columns leave.
        let (right, right_w) = if plain {
            (Vec::new(), 0)
        } else {
            self.right_spans(&t, bg, verb, r.width, cols.2)
        };
        let mut left = vec![edge];
        left.push(Span::styled(
            format!("{glyph} "),
            Style::new().fg(color).bg(bg),
        ));
        let room = (r.width as usize).saturating_sub(3 + right_w + 1);
        let (proj_w, name_w, _) = cols;
        let name_style = if selected {
            Style::new().fg(p.text).bg(bg).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(p.text).bg(bg)
        };
        let hl = Style::new()
            .fg(p.rose)
            .bg(bg)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        let hits = self.hits.get(&item).cloned().unwrap_or_default();
        let column =
            |left: &mut Vec<Span<'static>>, text: &str, hits: &[usize], w: usize, style: Style| {
                let spans = highlight(text, hits, w, false, style, hl);
                let used: usize = spans.iter().map(|s| str_width(&s.content)).sum();
                left.extend(spans);
                left.push(Span::styled(
                    " ".repeat(w.saturating_sub(used) + 2),
                    Style::new().bg(bg),
                ));
            };
        let project = if self.same_project_as_parent(item) {
            ""
        } else {
            t.project
        };
        column(
            &mut left,
            project,
            &hits.second,
            proj_w,
            Style::new().fg(p.subtle).bg(bg),
        );
        // A row an agent spawned hangs off its spawner's (see `nest`), its
        // guide in the name column.
        let branch = self.tree.get(&item).map(Guide::branch).unwrap_or_default();
        let branch_w = str_width(&branch);
        left.push(Span::styled(branch, Style::new().fg(p.muted).bg(bg)));
        // A spawner's fold badge follows its name, inside the column.
        let fold = self.fold_badge(item, bg);
        let fold_w = str_width(&fold.content);
        let name_room = name_w.saturating_sub(branch_w + fold_w);
        let spans = highlight(t.name, &hits.name, name_room, false, name_style, hl);
        let used: usize = spans.iter().map(|s| str_width(&s.content)).sum();
        left.extend(spans);
        left.push(fold);
        left.push(Span::styled(
            " ".repeat(name_room.saturating_sub(used) + 2),
            Style::new().bg(bg),
        ));
        if let Item::Tab(tab) = item
            && plain
        {
            let used = 3
                + proj_w
                + 2
                + branch_w
                + str_width(t.name).min(name_w.saturating_sub(branch_w))
                + 2;
            let right = self.tab_right(
                tab,
                &hits.dir,
                bg,
                selected,
                (r.width as usize).saturating_sub(used),
            );
            f.render_widget(Line::from(left), r);
            f.render_widget(Line::from(right).right_aligned(), r);
            return;
        }
        let msg_room = room.saturating_sub(proj_w + 2 + name_w + 2);
        if !t.msg.is_empty() && msg_room > 4 {
            let msg_style = Style::new()
                .fg(if t.state == "blocked" {
                    p.gold
                } else {
                    p.subtle
                })
                .bg(bg);
            left.extend(highlight(t.msg, &[], msg_room, false, msg_style, msg_style));
        }
        f.render_widget(Line::from(left), r);
        f.render_widget(Line::from(right).right_aligned(), r);
    }

    /// The keys float's bindings, the group for where John is first: the
    /// highlighted row's actions as the footer used to name them.
    fn help_sections(&self) -> Vec<help::Section> {
        use help::Section;
        let mut sections = Vec::new();
        if self.view == View::Work {
            sections.push(Section::new(
                "Work items",
                &[
                    (
                        "⏎",
                        "start a session from it (auto host, claude), or go to its task",
                    ),
                    ("⌃V", "scroll through it: search, copy, jump between files"),
                    ("⌃B", "open it in the browser"),
                    ("⇥", "choose the project"),
                    ("⌃F", "switch between work items and pull requests"),
                    ("⌃L", "show a pull request's description, diff or commits"),
                    ("⌃U ⌃D", "scroll it, half a page"),
                    ("⌃G", "arm a pull request's approval: ⏎ approves it"),
                    (
                        "⌃M",
                        "arm an approved, passing pull request's squash-merge: ⏎ merges it",
                    ),
                    ("⌃T", "review a pull request in tuicr; :submit sends it"),
                    ("⌃X", "arm moving a Fizzy card to Done: ⏎ closes it"),
                    ("⌃Z", "arm moving a Fizzy card to Not Now: ⏎ postpones it"),
                    ("⌃S", "give the highlighted item the whole view"),
                    ("⇧↑↓", "scroll the highlighted item beside the list"),
                    ("⌃R", "refresh: list the project again"),
                ],
            ));
            sections.push(Section::new(
                "Moving and searching",
                &[
                    ("↑↓ j k ⌃P ⌃N ⌃K ⌃J", "move"),
                    ("PgUp PgDn", "page"),
                    ("Home End", "first, last"),
                    ("/", "search titles and numbers"),
                    ("esc", "leave the search"),
                    ("⌃W ⌥⌫", "delete a word"),
                ],
            ));
        } else if self.view == View::Usage {
            sections.push(Section::new(
                "Usage",
                &[
                    ("↑↓ j k ⌃P ⌃N", "scroll"),
                    ("PgUp PgDn", "page"),
                    ("Home End", "top, bottom"),
                ],
            ));
        } else {
            let row = self.row_keys();
            if !row.is_empty() {
                sections.push(Section::new("Highlighted row", &row));
            }
            sections.push(Section::new("Moving and searching", &self.moving_keys()));
        }
        sections.push(self.views_section());
        sections.extend(help_float_sections());
        // Where John is goes first.
        let here = if self.scrollback.is_some() {
            Some("Scrollback")
        } else if self.reply.is_some() {
            Some("Reply box")
        } else if self.snooze.is_some() {
            Some("Snooze prompt")
        } else if self.items.is_some() {
            Some("Work items")
        } else {
            None
        };
        if let Some(i) = here.and_then(|t| sections.iter().position(|s| s.title == t)) {
            let s = sections.remove(i);
            sections.insert(0, s);
        }
        sections
    }

    /// The help float's keys for the highlighted row, as it stands.
    fn row_keys(&self) -> Vec<(&'static str, &'static str)> {
        let task = self.selected_task();
        let mut row: Vec<(&str, &str)> = match task {
            Some(t) if t.is_orphan() && t.safe() => vec![("⌃X", "remove worktree (merged)")],
            Some(t) if t.is_orphan() => vec![("⌃X", "remove worktree")],
            Some(t) if t.ended.is_some() && t.left && t.safe() => {
                vec![("⏎", "resume"), ("⌃X", "remove worktree (merged)")]
            }
            Some(t) if t.ended.is_some() && t.left => {
                vec![("⏎", "resume"), ("⌃X", "remove worktree")]
            }
            Some(t) if t.ended.is_some() => vec![("⏎", "resume")],
            Some(t) if t.safe() => vec![("⏎", "go"), ("⌃X", "archive (merged)")],
            Some(_) => vec![("⏎", "go"), ("⌃X", "archive")],
            None if self.selected_agent_tab().is_some() => vec![("⏎", "go"), ("⌃X", "archive")],
            None if self.selected_item().is_some() => vec![("⏎", "go")],
            None => vec![],
        };
        // Inbox jumps on cmd+Enter; Enter ends the search.
        if self.view == View::Inbox && !row.is_empty() {
            for (k, d) in &mut row {
                if *k == "⏎" && matches!(*d, "go" | "resume") {
                    *k = "⌘⏎";
                }
            }
            row.insert(0, ("⏎", "clear the search, keeping the highlight"));
        }
        if task.is_some_and(|t| !chooser_links(&t.links).is_empty()) {
            row.push(("⌃B", "work items: open, read, comment, close, mark ready"));
        }
        if self.view == View::Inbox && task.is_some_and(|t| t.open_pr().is_some()) {
            row.push(("⌃T", "review its pull request in tuicr; :submit sends it"));
        } else if self.preview_want().is_some() {
            row.push(("⌃T", "review its changes in tuicr; y sends the review"));
        }
        if self.view == View::Inbox && self.interruptible() {
            row.push(("⌃C", "interrupt the agent (asks first)"));
        }
        if self.can_pin() {
            row.push((
                "⌃R",
                if task.is_some_and(|t| t.pinned) {
                    "unpin"
                } else {
                    "pin"
                },
            ));
        }
        if self.can_snooze() {
            row.push((
                "⌃Z",
                if task.is_some_and(|t| t.is_snoozed(self.now)) {
                    "wake"
                } else {
                    "snooze"
                },
            ));
        }
        if self.preview_want().is_some() {
            row.push(("⇥", "message the agent"));
            row.push(("⌃U ⌃D", "scroll back, half a page"));
            row.push((
                "⌃L",
                match self.lens.next() {
                    changes::Lens::Transcript => "show the transcript",
                    changes::Lens::Diff => "show the diff",
                    changes::Lens::Commits => "show the commits",
                },
            ));
            row.push(("⌃Y", "host, session ID and log path to the clipboard"));
        }
        row
    }

    /// The badge after a spawner's name (see `Fold`), gold while a row
    /// folded under it waits on a prompt; empty for any other row.
    fn fold_badge(&self, item: Item, bg: Color) -> Span<'static> {
        let p = &self.p;
        match self.folds.get(&item) {
            Some(f) => {
                let fg = if f.blocked && !f.open {
                    p.gold
                } else {
                    p.muted
                };
                Span::styled(f.badge(), Style::new().fg(fg).bg(bg))
            }
            None => Span::raw(""),
        }
    }

    /// The help float's keys for moving and searching in this view.
    fn moving_keys(&self) -> Vec<(&'static str, &'static str)> {
        let mut moving = vec![
            ("↑↓ j k ⌃P ⌃N ⌃K ⌃J", "move"),
            ("PgUp PgDn", "page"),
            ("Home End", "first, last"),
        ];
        if self.view == View::Inbox {
            moving.push(("⌥↑↓", "previous, next group"));
            moving.push(("→ ←", "unfold, fold the agents a task spawned"));
        }
        if self.view == View::Archive {
            moving.extend([
                ("⌃D ⌃U", "half a page"),
                ("gg ⌃G G", "first, last"),
                ("/", "search"),
                ("esc", "leave the search"),
                ("⌃W ⌥⌫", "delete a word"),
            ]);
        } else {
            moving.extend([
                ("/", "search"),
                ("esc", "leave the search"),
                ("⌃W ⌥⌫", "delete a word"),
            ]);
            // Inbox's ctrl-u scrolls the transcript instead.
            if self.view != View::Inbox {
                moving.push(("⌃U", "clear the search"));
            }
        }
        moving
    }

    /// The help float's keys for switching views and what this view shows.
    fn views_section(&self) -> help::Section {
        let profile = format!("show {}", self.profile.next().name());
        let mut views = vec![
            ("⌘1-4", "Inbox, Work, Usage, Archive"),
            ("⌃A", "archive, and back"),
            ("⌃O", profile.as_str()),
        ];
        match self.view {
            View::Inbox if self.sidebar => views.push((
                "⌃S",
                if self.expanded {
                    "show the sidebar"
                } else {
                    "hide the sidebar"
                },
            )),
            View::Inbox => views.push((
                "⌃S",
                if self.expanded {
                    "back to the list"
                } else {
                    "expand the preview"
                },
            )),
            View::Archive => {
                views.push(("⇥", "choose the project, or all"));
                views.push((
                    "⌃F",
                    if self.kept {
                        "show archived tasks"
                    } else {
                        "hide archived tasks"
                    },
                ));
            }
            View::Usage | View::Work => {}
        }
        views.push(("? F1", "these keys"));
        help::Section::new("Views", &views)
    }

    /// Scrollback's footer: the search being typed, else its keys, with
    /// where the cursor is on the right.
    fn draw_scrollback_footer(&self, f: &mut Frame, area: Rect, sb: &scrollback::Scrollback) {
        let p = &self.p;
        if let Some(t) = &sb.typing {
            let line = Line::from(vec![
                Span::styled(
                    if t.forward { " /" } else { " ?" },
                    Style::new().fg(p.gold).add_modifier(Modifier::BOLD),
                ),
                Span::styled(t.query.clone(), Style::new().fg(p.text)),
                Span::styled("▏", Style::new().fg(p.subtle)),
            ]);
            return f.render_widget(line, area);
        }
        let right = Span::styled(
            format!(
                "{}SCROLLBACK {}/{} ",
                Some(sb.typed())
                    .filter(|t| !t.is_empty())
                    .map_or_else(String::new, |t| format!("{t}  ")),
                sb.cursor + 1,
                sb.len()
            ),
            Style::new().fg(p.iris),
        );
        let right_w = str_width(&right.content);
        let room = (area.width as usize).saturating_sub(right_w + 1);
        let mut spans = if let Some(note) = &sb.note {
            vec![
                Span::raw(" "),
                Span::styled(note.clone(), Style::new().fg(p.love)),
            ]
        } else {
            let mut keys = if sb.anchor.is_some() {
                vec![("y", "copy selection", p.subtle)]
            } else {
                vec![("yy", "copy line", p.subtle), ("v V", "select", p.subtle)]
            };
            if !sb.nav.marks.is_empty() {
                keys.push(("[ ]", sb.nav.what, p.subtle));
            }
            keys.extend([("q", "done", p.subtle), ("F1", "keys", p.subtle)]);
            draw::hints(p, &keys, room).0
        };
        let used: usize = spans.iter().map(|s| str_width(&s.content)).sum();
        spans.push(Span::raw(
            " ".repeat((area.width as usize).saturating_sub(used + right_w)),
        ));
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
        let row = Rect {
            x: outer.x + 2,
            y: outer.bottom() - 1,
            width: outer.width - 4,
            height: 1,
        };
        let border = Style::new().fg(self.border()).bg(self.p.base);
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
            let (from, to) = (
                if start == row.x { start } else { start + 1 },
                if x == row.right() { x } else { x - 1 },
            );
            if x - start > 3 && from < to {
                for bx in from..to {
                    buf[(bx, row.y)].set_symbol("─").set_style(border);
                }
            }
        }
    }

    /// The footer while ctrl-x has armed the selected task's archive: what
    /// Enter will do, and what it throws away.
    fn armed_task_spans(&self, t: &Task) -> Vec<Span<'static>> {
        let p = &self.p;
        let mut spans = vec![Span::raw(" ")];
        let key = Style::new().fg(p.love).add_modifier(Modifier::BOLD);
        let files = || {
            let mut list = t.dirty_files.join(", ");
            if t.dirty > t.dirty_files.len() as u64 {
                let _ = write!(list, " +{}", t.dirty - t.dirty_files.len() as u64);
            }
            list
        };
        match self.armed {
            Armed::Archive if t.dirty > 0 => {
                spans.push(Span::styled("⏎", key));
                spans.push(Span::styled(
                    format!(" archive {}", t.label()),
                    Style::new().fg(p.love),
                ));
                spans.push(Span::styled(
                    format!(" (uncommitted: {})", files()),
                    Style::new().fg(p.gold),
                ));
                spans.push(Span::styled(
                    " · other keys cancel",
                    Style::new().fg(p.muted),
                ));
            }
            Armed::Force => {
                spans.push(Span::styled(
                    "Are you sure? ",
                    Style::new().fg(p.love).add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled("⏎", key));
                spans.push(Span::styled(
                    format!(" throws away {}", files()),
                    Style::new().fg(p.love),
                ));
                spans.push(Span::styled(
                    " · other keys cancel",
                    Style::new().fg(p.muted),
                ));
            }
            _ => {
                let what = if t.ended.is_some() {
                    "removes worktree"
                } else {
                    "closes tab, removes worktree"
                };
                spans.push(Span::styled("⏎", key));
                spans.push(Span::styled(
                    format!(" archive {}", t.label()),
                    Style::new().fg(p.love),
                ));
                spans.push(Span::styled(
                    format!(": {what} · other keys cancel"),
                    Style::new().fg(p.muted),
                ));
            }
        }
        spans
    }

    /// The footer while ctrl-g, ctrl-m, ctrl-x or ctrl-z has armed an
    /// approval or a squash-merge of the Work view's highlighted pull
    /// request, or a move of its highlighted card to Done or Not Now.
    fn armed_pr_line(&self) -> Option<Line<'static>> {
        let (action, name, title) = self.work.armed_item().filter(|_| self.view == View::Work)?;
        let p = &self.p;
        let key = Style::new().fg(p.love).add_modifier(Modifier::BOLD);
        let what = match action {
            Action::Close => format!(" move {name} {title} to Done"),
            Action::Postpone => format!(" move {name} {title} to Not Now"),
            _ => format!(" {} {name} {title}", action.verb()),
        };
        Some(Line::from(vec![
            Span::raw(" "),
            Span::styled("⏎", key),
            Span::styled(what, Style::new().fg(p.love)),
            Span::styled(" · other keys cancel", Style::new().fg(p.muted)),
        ]))
    }

    /// The footer while ctrl-c has armed an interrupt of the highlighted
    /// agent.
    fn armed_interrupt_line(&self) -> Line<'static> {
        let p = &self.p;
        let key = Style::new().fg(p.love).add_modifier(Modifier::BOLD);
        Line::from(vec![
            Span::raw(" "),
            Span::styled("⏎", key),
            Span::styled(
                format!(" interrupt {}", self.selected_label()),
                Style::new().fg(p.love),
            ),
            Span::styled(
                ": presses Escape in its tab · other keys cancel",
                Style::new().fg(p.muted),
            ),
        ])
    }

    /// Footer key hints (key, what it does), as many as fit.
    fn draw_hints(&self, f: &mut Frame, area: Rect, hints: &[(&str, &str)]) {
        let p = &self.p;
        let hints: Vec<(&str, &str, Color)> =
            hints.iter().map(|&(k, d)| (k, d, p.subtle)).collect();
        let (spans, _) = draw::hints(p, &hints, area.width as usize);
        f.render_widget(Line::from(spans), area);
    }

    /// The footer's hints for the snooze prompt, work items or project
    /// chooser, when one is open: the one place their keys are shown.
    fn popup_hints(&self) -> Option<Vec<(&'static str, &'static str)>> {
        if let Some(z) = &self.snooze {
            let enter = match z.until {
                Ok(0) => "wake",
                _ => "snooze",
            };
            return Some(vec![("⏎", enter), ("esc", "cancel"), ("F1", "keys")]);
        }
        if let Some(i) = &self.items {
            return Some(i.hints());
        }
        self.projects.as_ref().map(|_| projects::Chooser::hints())
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        let p = &self.p;
        if let Some((msg, failed)) = &self.note {
            let style = Style::new().fg(if *failed { p.love } else { p.pine });
            f.render_widget(
                Line::from(vec![Span::raw(" "), Span::styled(msg.clone(), style)]),
                area,
            );
            return;
        }
        if let Some(h) = &self.help {
            return self.draw_hints(f, area, &h.hints());
        }
        if let Some(sb) = &self.scrollback {
            return self.draw_scrollback_footer(f, area, sb);
        }
        if let Some(r) = &self.reply {
            let ghost = self.reply_suggestion().is_some();
            let ready = ghost || (!r.prompt.text().is_empty() && r.prompt.saved_ok);
            let mut hints = vec![("⏎", "send", if ready { p.pine } else { p.muted })];
            if ghost {
                hints.push(("⇥", "take suggestion", p.subtle));
            }
            hints.extend([("⌃G", "editor", p.subtle), ("F1", "keys", p.subtle)]);
            let (spans, _) = draw::hints(p, &hints, area.width as usize);
            f.render_widget(Line::from(spans), area);
            return;
        }
        if let Some(hints) = self.popup_hints() {
            return self.draw_hints(f, area, &hints);
        }
        if let (true, Some(t)) = (self.armed.archives(), self.selected_task()) {
            f.render_widget(Line::from(self.armed_task_spans(t)), area);
            return;
        }
        if self.armed == Armed::Interrupt {
            f.render_widget(self.armed_interrupt_line(), area);
            return;
        }
        if let Some(line) = self.armed_pr_line() {
            f.render_widget(line, area);
            return;
        }
        if let (true, Some(t)) = (self.armed.archives(), self.selected_agent_tab()) {
            let key = Style::new().fg(p.love).add_modifier(Modifier::BOLD);
            let line = Line::from(vec![
                Span::raw(" "),
                Span::styled("⏎", key),
                Span::styled(
                    format!(" archive {}", self.tabs[t].name),
                    Style::new().fg(p.love),
                ),
                Span::styled(
                    ": closes tab, removes its worktree · other keys cancel",
                    Style::new().fg(p.muted),
                ),
            ]);
            f.render_widget(line, area);
            return;
        }
        if self.view == View::Work {
            let mut hints = self.work.hints();
            // The view strip's keys, before the keys float's.
            let at = hints.iter().position(|h| h.0 == "?").unwrap_or(hints.len());
            hints.insert(at, VIEW_HINT);
            return self.draw_hints(f, area, &hints);
        }
        let hints: &[(&str, &str, Color)] = if self.quick {
            &[("⏎", "jump", p.subtle), ("esc", "close", p.subtle)]
        } else if self.view == View::Archive {
            &[
                ("⇥", "project", p.subtle),
                (VIEW_HINT.0, VIEW_HINT.1, p.subtle),
                ("?", "keys", p.subtle),
            ]
        } else {
            &[
                (VIEW_HINT.0, VIEW_HINT.1, p.subtle),
                ("?", "keys", p.subtle),
            ]
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
        f.render_widget(
            Line::styled(format!("{summary} "), Style::new().fg(p.muted)).right_aligned(),
            area,
        );
    }
}

// ---------------------------------------------------------------- standalone

impl App {
    /// Start a standalone picker's data (see `standalone::Feed`).
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
        let changes = want
            .clone()
            .filter(|_| self.lens != changes::Lens::Transcript);
        if changes != self.asked_changes {
            feed.changes(changes.clone());
            self.asked_changes = changes;
        }
        if want != self.asked {
            feed.preview(want.clone());
            self.asked = want;
        }
    }

    /// Whether `item` is the row an archive is taking away (see `gone`).
    fn is_gone(&self, item: Item) -> bool {
        self.gone
            .as_ref()
            .is_some_and(|g| self.key_of(item).as_ref() == Some(g))
    }

    /// Whether `tasks` still list the row an archive took away as open.
    fn gone_open(&self, tasks: &[Task]) -> bool {
        self.gone.as_ref().is_some_and(|(session, window)| {
            tasks
                .iter()
                .any(|t| t.ended.is_none() && &t.session_id == session && &t.window_id == window)
        })
    }

    /// Take the highlighted row out of the lists before its archive runs,
    /// with the highlight moved to the row below it (above, at the end).
    fn hide_for_archive(&mut self) {
        let Some(key) = self.selected_key() else {
            return;
        };
        let at = self.selected.unwrap_or(0);
        let below = self
            .rows
            .iter()
            .enumerate()
            .skip(at + 1)
            .find(|(_, i)| selectable(i));
        let above = self
            .rows
            .iter()
            .enumerate()
            .take(at)
            .rev()
            .find(|(_, i)| selectable(i));
        let want = below.or(above).and_then(|(_, &i)| self.key_of(i));
        self.gone = Some(key);
        self.refilter_to(want);
    }

    /// A finished archive's outcome, as the note; then fetch again.
    fn take_archived(&mut self) {
        let Some(rx) = &self.archiving else { return };
        match rx.try_recv() {
            Ok(r) => {
                self.archiving = None;
                // A failed archive left the row where it was: show it now.
                // A done one stays hidden until the full read settles it.
                if r.is_err() {
                    self.gone = None;
                    self.refilter();
                }
                self.note = Some(match r {
                    Ok(msg) => (msg, false),
                    Err(e) => (e, true),
                });
                self.refetch();
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.archiving = None;
                self.gone = None;
                self.refetch();
            }
        }
    }

    /// Data that has arrived from the feed.
    fn take_fetched(&mut self) {
        let Some(feed) = &self.feed else { return };
        let mut got = Vec::new();
        while let Ok(text) = feed.rx.try_recv() {
            got.push(text);
        }
        for text in got {
            self.paste(&text);
        }
    }

    /// The keys that act: Enter, ctrl-r, ctrl-t, and Enter in the
    /// snooze prompt and reply box; false for other keys.
    fn standalone_key(&mut self, k: KeyEvent, term: &mut DefaultTerminal) -> bool {
        // The keys float and scrollback have every key while they last.
        if self.help.is_some() || self.scrollback.is_some() {
            return false;
        }
        // The work items float has every key while it lasts; this runs
        // what it asks for (see items.rs).
        if self.items.is_some() {
            self.run_items_key(k, term);
            return true;
        }
        // The Work view's own keys (see work.rs); the rest search and
        // switch views as in the others.
        if self.view == View::Work {
            return self.run_work_key(k, term);
        }
        // The Archive's project chooser has every key, Enter included.
        if self.projects.is_some() {
            return false;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        // The reply box's Enter sends the message; other keys edit it. A
        // failed send keeps the box, so the message isn't lost.
        if self.reply.is_some() {
            return self.send_reply(k, term);
        }
        // The snooze prompt's Enter saves the snooze; other keys edit it.
        if self.snooze.is_some() {
            return self.save_snooze(k);
        }
        // Pin or unpin in the registry, then here.
        if matches!(k.code, KeyCode::Char('r') if ctrl) {
            self.pin_selected();
            return true;
        }
        if matches!(k.code, KeyCode::Char('t') if ctrl) && self.view == View::Inbox {
            self.review_task();
            return true;
        }
        if !matches!(k.code, KeyCode::Enter) {
            return false;
        }
        self.note = None;
        if self.inbox_enter(k) {
            return true;
        }
        let Some(pick) = standalone::parse_pick(&self.pick()) else {
            return true;
        };
        let label = self.selected_label();
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
                    self.note = Some((
                        "one archive at a time: wait for this one to finish".into(),
                        true,
                    ));
                    return true;
                }
                let note = (format!("Archiving {label} on {}…", pick.host), false);
                // The row goes now; the archive takes a moment to run.
                self.hide_for_archive();
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(standalone::archive(&pick.action, &pick));
                });
                self.archiving = Some(rx);
                note
            }
            "interrupt" => self.interrupt(&pick, &label),
            // The picker stays: on to Inbox, highlighting the resumed task
            // once it lists, as for a session started from Work.
            "resume" => {
                self.busy(term, format!("Resuming {label}…"));
                match standalone::resume_here(&pick) {
                    Ok((host, session, window)) => {
                        self.query.clear();
                        self.set_view(View::Inbox);
                        self.launched = Some(((session, window), Instant::now()));
                        (format!("resumed {label} on {host}"), false)
                    }
                    Err(e) => (e, true),
                }
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

    /// Inbox's plain Enter: end a search on the row it found, which stays
    /// highlighted (cmd+Enter jumps to it). False for cmd+Enter, other
    /// views and an armed archive or interrupt, which act as before.
    fn inbox_enter(&mut self, k: KeyEvent) -> bool {
        if self.view != View::Inbox
            || self.armed != Armed::No
            || k.modifiers.contains(KeyModifiers::SUPER)
        {
            return false;
        }
        if self.query.is_empty() {
            self.note = Some(("⌘⏎ jumps to the highlighted tab".into(), false));
        } else {
            self.clear_search();
        }
        true
    }

    /// ctrl-t in Inbox: review the highlighted task's open pull request in
    /// tuicr, whose :submit sends it to GitHub; without one, the agent's
    /// changes, which the float sends to the agent once John says so. Both
    /// run in a float over the picker (see the CLI's review.rs).
    fn review_task(&mut self) {
        if let Some((profile, pr, name)) = self.selected_task().and_then(|t| {
            let pr = t.open_pr()?;
            let reference = if pr.url.is_empty() {
                pr.reference.clone()
            } else {
                pr.url.clone()
            };
            Some((t.profile.clone(), reference, pr.name(None)))
        }) {
            self.note = Some(match standalone::review_float(&["pr", &profile, &pr]) {
                Ok(()) => (format!("reviewing {name} in tuicr"), false),
                Err(e) => (e, true),
            });
            return;
        }
        let Some(want) = self.preview_want() else {
            self.note = Some(("highlight an agent to review its changes".into(), true));
            return;
        };
        let label = self.selected_label();
        self.note = Some(
            match standalone::review_float(&["task", &want.target, &want.session, &want.window]) {
                Ok(()) => (format!("reviewing {label} in tuicr"), false),
                Err(e) => (e, true),
            },
        );
    }

    /// Enter on an armed interrupt: press Escape in the agent's tab; the
    /// outcome to show.
    fn interrupt(&mut self, pick: &standalone::Pick, label: &str) -> (String, bool) {
        self.armed = Armed::No;
        match standalone::interrupt(pick) {
            Ok(msg) => (msg, false),
            Err(e) => (format!("{label}: {e}"), true),
        }
    }

    /// Run what a key in the Work view asks for: start a session from the
    /// highlighted item, show its task, read it or open it; false for the
    /// keys it leaves to the list.
    fn run_work_key(&mut self, k: KeyEvent, term: &mut DefaultTerminal) -> bool {
        // Any other key drops a ctrl-v waiting on the item.
        self.work.detail.want_scroll = false;
        self.work.detail.want_key = None;
        let page = usize::try_from(self.page()).unwrap_or(1);
        let shown = (self.work.project, self.work.kind);
        let outcome = self.work.key(k, page);
        // Another project or kind: its listing may already be kept, and no
        // fetch will come to build its rows.
        if (self.work.project, self.work.kind) != shown {
            self.refilter();
        }
        match outcome {
            work::Outcome::Pass => return false,
            work::Outcome::Stay => {}
            work::Outcome::Refuse(why) => self.note = Some((why, true)),
            work::Outcome::Arm => self.note = None,
            work::Outcome::Act {
                action,
                item,
                url,
                name,
            } => self.act_on_pr(action, &item, &url, &name, term),
            work::Outcome::Open(url) => {
                self.note = Some(match standalone::open_url(&url) {
                    Ok(()) => (format!("opened {url}"), false),
                    Err(e) => (e, true),
                });
            }
            work::Outcome::Go(to) => {
                let pick = standalone::Pick {
                    action: "go".into(),
                    host: to.host,
                    session: to.session,
                    window: to.window,
                };
                self.note = Some(match standalone::go(&pick) {
                    Ok(()) => (format!("showed {}", to.label), false),
                    Err(e) => (e, true),
                });
            }
            work::Outcome::Scroll(k) => {
                let link = self.work.selected_link();
                if self.work.detail.visible && !self.work.detail.busy(link.as_ref()) {
                    self.enter_scrollback(k);
                } else {
                    // No room beside the list, or still reading: give it
                    // the view, then scroll once it is drawn and has come
                    // (see `take_work`).
                    self.work.detail.expanded |= !self.work.detail.visible;
                    self.work.detail.want_scroll = true;
                    self.work.detail.want_key = k;
                }
            }
            work::Outcome::Launch { path, url, name } => self.launch(path, url, name),
            work::Outcome::Review { profile, url, name } => {
                self.note = Some(match standalone::review_float(&["pr", &profile, &url]) {
                    Ok(()) => (format!("reviewing {name} in tuicr"), false),
                    Err(e) => (e, true),
                });
            }
        }
        true
    }

    /// Approve or squash-merge a pull request the Work view lists, or move
    /// a card it lists to Done or Not Now, under its project's profile,
    /// then list the project again to show it.
    fn act_on_pr(
        &mut self,
        action: Action,
        item: &switchyard_core::item_actions::Item,
        url: &str,
        name: &str,
        term: &mut DefaultTerminal,
    ) {
        let profile = self
            .work
            .current()
            .map(|p| p.profile.clone())
            .unwrap_or_default();
        let doing = match action {
            Action::Merge => format!("Squash-merging {name}"),
            Action::Close => format!("Moving {name} to Done"),
            Action::Postpone => format!("Moving {name} to Not Now"),
            _ => format!("Approving {name}"),
        };
        self.busy(term, format!("{doing}…"));
        self.note = Some(
            match standalone::item_action(action, item, "", &profile, url) {
                Ok(done) => {
                    self.work.load(true);
                    (done, false)
                }
                Err(e) => (e, true),
            },
        );
    }

    /// Start a session from a work item in the background: auto host,
    /// claude, named from the item (see core's `launch`). The registry and
    /// Rex hosts it needs are reached from the Mac.
    fn launch(&mut self, path: String, url: String, name: String) {
        if !cfg!(target_os = "macos") {
            self.note = Some(("sessions start from the Mac's picker".into(), true));
            return;
        }
        self.note = Some((format!("Starting {name}…"), false));
        self.work.starting.insert(url.clone());
        // Inbox is where the new session will show: go there now, with the
        // Work view's search cleared, and highlight it when it lists.
        self.query.clear();
        self.set_view(View::Inbox);
        let (tx, rx) = chan::channel();
        let item = url.clone();
        std::thread::spawn(move || {
            let _ = tx.send(standalone::launch(&path, &item));
        });
        self.launching.push(Launching { url, name, rx });
    }

    /// Finished listings and launches from the Work view.
    fn take_work(&mut self) {
        if self.work.receive() && self.view == View::Work {
            self.refilter();
            self.note_work_missed();
        }
        if self.view == View::Work {
            self.work.poll_detail();
            // Scrollback is for one item and lens.
            let want = self.work.selected_link().map(|l| self.work.detail.pane(&l));
            if self
                .scrollback
                .as_ref()
                .is_some_and(|sb| sb.pane.target == "work" && Some(&sb.pane) != want.as_ref())
            {
                self.leave_scrollback();
            }
            // Once its side is drawn and has come, if ctrl-v asked first.
            if self.work.detail.want_scroll
                && self.work.detail.visible
                && !self.work.detail.busy(self.work.selected_link().as_ref())
            {
                self.work.detail.want_scroll = false;
                let k = self.work.detail.want_key.take();
                self.enter_scrollback(k);
            }
        }
        let mut done = Vec::new();
        self.launching.retain(|l| match l.rx.try_recv() {
            Ok(r) => {
                done.push((l.url.clone(), l.name.clone(), Some(r)));
                false
            }
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => {
                done.push((l.url.clone(), l.name.clone(), None));
                false
            }
        });
        for (url, name, r) in done {
            self.note = Some(match r {
                Some(Ok((host, session, window))) => {
                    self.launched = Some(((session, window), Instant::now()));
                    (format!("started {name} on {host}"), false)
                }
                Some(Err(e)) => {
                    self.work.starting.remove(&url);
                    (format!("{name}: {e}"), true)
                }
                None => {
                    self.work.starting.remove(&url);
                    (format!("{name}: the launch went away"), true)
                }
            });
            self.focus_launched();
            self.refetch();
        }
    }

    /// Switch to the Work view on the highlighted task's project and
    /// highlight `link` there once it lists (see work.rs's `focus_on`).
    fn show_in_work(&mut self, link: Link) {
        self.items = None;
        self.query.clear();
        self.work.focus_on(link);
        self.set_view(View::Work);
        self.note_work_missed();
    }

    /// Say in the footer when an item the Work view was asked to show
    /// isn't among its project's open ones.
    fn note_work_missed(&mut self) {
        if let Some(l) = self.work.missed.take() {
            let project = self.work.current().map_or("", |p| p.name.as_str());
            let what = if self.work.kind == work::Kind::Prs {
                "open pull requests"
            } else {
                "open work items"
            };
            self.note = Some((
                format!("{} isn't among {project}'s {what}", l.name(None)),
                true,
            ));
        }
    }

    /// Run what the work items float asks for on `k`: open a URL (closing
    /// the float), or take an action on its item, keeping the float open
    /// with the text if it fails.
    fn run_items_key(&mut self, k: KeyEvent, term: &mut DefaultTerminal) {
        match self.items_key(k) {
            Some(items::Outcome::Open(url)) => {
                self.note = Some(match standalone::open_url(&url) {
                    Ok(()) => {
                        self.items = None;
                        (format!("opened {url}"), false)
                    }
                    Err(e) => (e, true),
                });
            }
            Some(items::Outcome::Show(link)) => self.show_in_work(link),
            Some(items::Outcome::Read) => {
                let Some((link, profile, name)) = self.items.as_ref().map(|i| {
                    (
                        i.entries[i.selected].link.clone(),
                        i.profile.clone(),
                        i.selected_name(),
                    )
                }) else {
                    return;
                };
                self.busy(term, format!("Reading {name}…"));
                match standalone::item_thread(&link, &profile) {
                    Ok(thread) => {
                        self.note = None;
                        if let Some(i) = &mut self.items {
                            i.show(thread);
                        }
                    }
                    Err(e) => self.note = Some((e, true)),
                }
            }
            Some(items::Outcome::Run { action, item, body }) => {
                let profile = self
                    .items
                    .as_ref()
                    .map(|i| i.profile.clone())
                    .unwrap_or_default();
                let url = self
                    .items
                    .as_ref()
                    .map(|i| i.entries[i.selected].link.url.clone())
                    .unwrap_or_default();
                let name = self
                    .items
                    .as_ref()
                    .map_or_else(|| item.name(), items::Items::selected_name);
                self.busy(term, format!("{} {name}…", action.verb()));
                // Core names the item with its repo; say it as the float does.
                match standalone::item_action(action, &item, &body, &profile, &url) {
                    Ok(msg) => {
                        self.items = None;
                        self.note = Some((msg.replace(&item.name(), &name), false));
                        self.refetch();
                    }
                    Err(e) => {
                        if let Some(i) = &mut self.items {
                            i.failed(action, &body);
                        }
                        self.note = Some((e, true));
                    }
                }
            }
            _ => {}
        }
    }

    /// ctrl-r: pin or unpin the selected row in the registry, then here.
    fn pin_selected(&mut self) {
        if !self.can_pin() {
            return;
        }
        let Some(pick) = standalone::parse_pick(&self.pick()) else {
            return;
        };
        self.note = Some(match standalone::pin(&pick) {
            Ok(msg) => {
                self.toggle_pin();
                (msg, false)
            }
            Err(e) => (e, true),
        });
        self.refetch();
    }

    /// The reply box's Enter: send the message; other keys (false) edit
    /// it. A failed send keeps the box, so the message isn't lost.
    fn send_reply(&mut self, k: KeyEvent, term: &mut DefaultTerminal) -> bool {
        let Some(r) = &self.reply else { return false };
        let newline = k
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT);
        if k.code != KeyCode::Enter || newline {
            return false;
        }
        // An empty box sends Claude's suggestion, if it shows one.
        let text = match r.prompt.text() {
            t if t.is_empty() => match self.reply_suggestion() {
                Some(s) => s,
                None => return true,
            },
            t => t,
        };
        let Some(pick) = standalone::parse_pick(&self.pick()) else {
            return true;
        };
        let label = r.label.clone();
        self.busy(term, format!("sending to {label}…"));
        self.note = Some(match standalone::message(&pick, &text) {
            Ok(msg) => {
                self.sent_reply();
                (msg, false)
            }
            Err(e) => (format!("{label}: {e}"), true),
        });
        true
    }

    /// The snooze prompt's Enter: save the snooze; other keys (false) edit
    /// it.
    fn save_snooze(&mut self, k: KeyEvent) -> bool {
        let Some(z) = &self.snooze else { return false };
        // Unreadable text: Enter does nothing.
        let Ok(until) = z.until else {
            return matches!(k.code, KeyCode::Enter);
        };
        if !matches!(k.code, KeyCode::Enter) {
            return false;
        }
        let Some(pick) = standalone::parse_pick(&self.pick()) else {
            return true;
        };
        let label = z.label.clone();
        self.note = Some(match standalone::snooze(&pick, until) {
            Ok(msg) => {
                self.finish_snooze();
                let msg = if until > 0 {
                    format!("{msg} until {}", snooze::describe(until, self.now))
                } else {
                    msg
                };
                (msg, false)
            }
            Err(e) => {
                self.snooze = None;
                (format!("{label}: {e}"), true)
            }
        });
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
            KeyCode::Enter if self.fed => self.report_pick(),
            KeyCode::Enter => {
                let Some(pick) = standalone::parse_pick(&self.pick()) else {
                    return false;
                };
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
            KeyCode::Char('p' | 'k' | 'n' | 'j' | 'u' | 'd' | 'w' | 'o') if ctrl => self.key(k),
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::Backspace
                if !shift =>
            {
                self.note = None;
                self.key(k);
            }
            _ => {}
        }
        false
    }

    /// A fed float's Enter: report the pick in the title for the config
    /// host to act on (see fed.rs), which answers with `quit` or `note`.
    fn report_pick(&mut self) {
        use std::io::Write;
        let pick = self.pick();
        if pick.is_empty() || self.fed_pick.is_some() {
            return;
        }
        let mut out = io::stdout();
        let _ = write!(out, "\x1b]2;{}:{pick}\x07", fed::PICK_TITLE).and_then(|()| out.flush());
        self.fed_pick = Some(Instant::now());
        self.note = Some((format!("Jumping to {}…", self.selected_label()), false));
    }

    /// A fed float's pick the config host hasn't answered in time.
    fn check_fed_pick(&mut self) {
        if self.fed_pick.is_some_and(|t| t.elapsed() > FED_ANSWER) {
            self.fed_pick = None;
            protocol::clear_title();
            self.note = Some(("The Mac didn't answer the jump".into(), true));
        }
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

// ---------------------------------------------------------------- main

/// The project a folder is in: the repository a `.worktrees` worktree
/// belongs to, else the folder's own name (`~` for home).
fn folder_project(dir: &str) -> String {
    let dir = match dir.trim_end_matches('/') {
        "" => return dir.to_string(),
        d => d,
    };
    let dir = dir.split_once("/.worktrees/").map_or(dir, |(repo, _)| repo);
    dir.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(dir)
        .to_string()
}

fn run(term: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    loop {
        if app.quit {
            return Ok(());
        }
        app.check_fed_pick();
        app.take_fetched();
        app.take_archived();
        app.take_work();
        app.ask_preview();
        app.sync_commands();
        let before = app.now;
        app.now = now();
        // A snooze ran out: back to its group.
        if app
            .tasks
            .iter()
            .any(|t| t.snoozed_until > before && t.snoozed_until <= app.now)
        {
            app.refilter();
        }
        term.draw(|f| app.draw(f))?;
        app.save_reply();
        let animate = app.archiving.is_some()
            || !app.launching.is_empty()
            || (app.view == View::Work && app.work.loading())
            || matches!(app.load, Load::Waiting)
            || matches!(app.tasks_load, Tasks::Waiting | Tasks::Live)
            || app.tabs.iter().any(|t| t.state == State::Working);
        let wait = if animate {
            Duration::from_millis(90)
        } else {
            Duration::from_millis(250)
        };
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
                let k = app.list_letter(k);
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
                    let _ = out
                        .write_all(scrollback::osc52(&text).as_bytes())
                        .and_then(|()| out.flush());
                }
            }
            // cmd+k's `current`, and John's own pastes, for the reply box
            // or a work item comment.
            Event::Paste(text)
                if app.fed && protocol::data(&text).is_some()
                    || app.reply.is_some()
                    || app
                        .items
                        .as_ref()
                        .is_some_and(|i| i.mode == items::Mode::Comment)
                    || protocol::data(&text)
                        .is_some_and(|(k, _)| k == "current" || k == "view") =>
            {
                app.paste(&text);
            }
            _ => {}
        }
    }
}

/// The picker from a terminal: it fetches its own tabs and tasks and acts on
/// Enter itself. `current` is the SESSION/WINDOW to highlight, if any.
///
/// # Errors
///
/// Returns an error if the terminal cannot be drawn to or read from.
pub fn standalone(view: View, current: Option<&str>) -> io::Result<()> {
    start(view, current, false, false)
}

/// The cmd+k quick jump float (see lua/switchyard/tab-picker.lua): Inbox's
/// list alone over the current tab, `current` highlighted. Enter shows the
/// highlighted tab and quits, closing the float; Escape just quits.
/// `fed_by` names the config host: on any other host, it feeds the float
/// and acts on its picks (see fed.rs).
///
/// # Errors
///
/// Returns an error if the terminal cannot be drawn to or read from.
pub fn quick(current: Option<&str>, fed_by: Option<&str>) -> io::Result<()> {
    let fed = fed_by.is_some_and(|h| h != switchyard_core::rex::local_label());
    start(View::Inbox, current, true, fed)
}

/// Feed a cmd+k quick jump float on another host from this, the config
/// host, and act on its picks, until it closes (see fed.rs).
///
/// # Errors
///
/// Fails when rex can't run or the float can't be found.
pub fn feed_float(session: &str, block: &str) -> io::Result<()> {
    fed::feed(session, block)
}

fn start(view: View, current: Option<&str>, quick: bool, fed: bool) -> io::Result<()> {
    standalone::detach_env();
    let mut app = App::new((String::new(), String::new()), view);
    app.quick = quick;
    app.fed = fed;
    app.excluded_raw = switchyard_core::config::read()
        .map(|c| c.inbox.exclude)
        .unwrap_or_default();
    app.excluded = resolve_folders(&app.excluded_raw);
    if let Some(c) = current {
        app.set_current(c);
    }
    if view == View::Work {
        app.work.open(None);
    }
    if !fed {
        app.start_feed();
    }
    picker(app)
}

/// Sanitized usage accounting as JSON: per-model totals across configured
/// hosts (what the Usage view renders), or with `local` this host's raw
/// samples (the remote collector protocol, deduplicated by the caller).
///
/// # Errors
///
/// Returns an error if the accounting cannot be serialized.
pub fn usage_json(local: bool) -> io::Result<()> {
    let mut collector = switchyard_core::usage::Collector::default();
    let json = if local {
        let settings = switchyard_core::config::read()
            .map(|c| c.usage)
            .unwrap_or_default();
        serde_json::to_string(&collector.local(
            &settings,
            &switchyard_core::rex::local_label(),
            now(),
        ))
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
        app.corners =
            chrome::Corners::new(cell, &[app.p.overlay, app.p.surface], &mut io::stdout()).ok();
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
    let _ = execute!(
        io::stdout(),
        PopKeyboardEnhancementFlags,
        DisableBracketedPaste
    );
    if let Some(corners) = &app.corners {
        let _ = corners.delete(&mut io::stdout());
    }
    ratatui::restore();
    // A message typed but never sent.
    app.close_reply();
    result
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
    fn ctrl_l_turns_the_preview_and_slash_searches() {
        let mut app = inbox();
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let plain = |code| KeyEvent::new(code, KeyModifiers::NONE);
        // Letters do nothing until `/` focuses the search box.
        app.key(plain(KeyCode::Char('h')));
        assert!(!app.searching && app.query.is_empty());
        app.key(plain(KeyCode::Char('/')));
        app.key(plain(KeyCode::Char('h')));
        app.key(plain(KeyCode::Char('l')));
        assert!(app.view == View::Inbox && app.searching && app.query == "hl");
        // cmd+1-4 pick the view; ctrl-h and ctrl-l no longer cycle it.
        app.key(ctrl('h'));
        assert!(app.view == View::Inbox && app.query == "hl");
        app.key(ctrl('l'));
        assert!(app.view == View::Inbox && app.lens == changes::Lens::Diff);
        app.key(ctrl('l'));
        assert_eq!(app.lens, changes::Lens::Commits);
        app.key(ctrl('l'));
        assert_eq!(app.lens, changes::Lens::Transcript);
        // Escape clears and leaves the search; letters stop typing.
        app.key(plain(KeyCode::Esc));
        app.key(plain(KeyCode::Char('h')));
        assert!(!app.searching && app.query.is_empty());
        // Unfocused, j and k move as ctrl-j and ctrl-k do, wrapping.
        let first = app.selected;
        app.key(plain(KeyCode::Char('k')));
        let wrapped = app.selected;
        app.key(ctrl('j'));
        assert_eq!(app.selected, first);
        app.key(ctrl('k'));
        assert_eq!(app.selected, wrapped);
        app.key(plain(KeyCode::Char('j')));
        assert!(app.selected == first && app.query.is_empty());
        // Searching, they type.
        app.key(plain(KeyCode::Char('/')));
        app.key(plain(KeyCode::Char('j')));
        assert_eq!(app.query, "j");
    }

    #[test]
    fn cmd_digits_paste_the_view_to_show() {
        let mut app = inbox();
        app.paste("rex-data:view\narchive");
        assert!(app.view == View::Archive);
        app.paste("rex-data:view\nusage");
        assert!(app.view == View::Usage);
        app.paste("rex-data:view\nbogus");
        assert!(app.view == View::Usage);
        app.paste("rex-data:view\ninbox");
        assert!(app.view == View::Inbox);
    }

    #[test]
    fn cmd_k_without_a_tab_clears_the_search_and_keeps_the_row() {
        let mut app = inbox();
        app.key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
        let picked = app.selected_key();
        assert!(picked.is_some());
        app.paste("rex-data:current\n");
        assert_eq!(app.query, "");
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
        assert_eq!(
            app.rows,
            vec![
                Item::Status("blocked"),
                Item::Task(1),
                Item::Task(2),
                Item::Tab(1),
                Item::Status("error"),
                Item::Tab(2),
                Item::Status("done"),
                Item::Tab(3),
                Item::Status("working"),
                Item::Tab(0),
                Item::Status("idle"),
                Item::Tab(4),
                Item::Status(""),
                Item::Task(3),
                Item::Task(4),
            ]
        );
        assert_eq!(app.selected_item(), Some(Item::Task(1)));
    }

    #[test]
    fn a_task_is_shown_by_its_session_title_and_found_by_its_name() {
        let mut app = App::new((String::new(), String::new()), View::Inbox);
        app.follow = false;
        app.deliver(|app| {
            (app.sessions, app.tabs) = parse_rows(concat!(
                "S\t-\tlocal\ts\tdemo\n",
                "T\ttitled\tidle\tcrash-fix\n",
                "T\tbare\tidle\tother\n",
            ));
            app.tasks = parse_tasks(&serde_json::json!([
                {"session_id":"s", "window_id":"titled", "project":"calc", "name":"fix/crash-fix",
                 "title":"Slack thread issue", "status":"idle"},
                {"session_id":"s", "window_id":"bare", "project":"calc", "name":"other", "status":"idle"}
            ]).to_string()).unwrap();
            app.load = Load::Ready;
            app.tasks_load = Tasks::Ready;
        });
        assert_eq!(app.flat(Item::Tab(0)).name, "Slack thread issue");
        assert_eq!(app.flat(Item::Tab(1)).name, "other");
        for query in ["slack", "crash-fix"] {
            app.query = query.into();
            app.refilter();
            assert_eq!(app.selected_item(), Some(Item::Tab(0)), "{query}");
        }
    }

    #[test]
    fn agents_with_a_timer_pending_are_snoozed_soonest_first() {
        let mut app = App::new((String::new(), String::new()), View::Inbox);
        app.follow = false;
        app.now = 1000;
        app.deliver(|app| {
            (app.sessions, app.tabs) = parse_rows(concat!(
                "S\t-\tlocal\ts\tdemo\n",
                "T\tlate\tidle\tlate\n",
                "T\tsoon\tdone\tsoon\n",
                "T\tawake\tworking\tawake\n",
                "T\tplain\tidle\tplain\n",
                "T\tjob\tidle\tjob\n",
            ));
            app.tasks = parse_tasks(&serde_json::json!([
                {"session_id":"s", "window_id":"job", "name":"job", "status":"scheduled"},
                {"session_id":"s", "window_id":"late", "name":"late", "status":"scheduled", "wake_at":1600},
                {"session_id":"s", "window_id":"soon", "name":"soon", "status":"scheduled", "wake_at":1120},
                // The tab has moved on since the server last looked.
                {"session_id":"s", "window_id":"awake", "name":"awake", "status":"scheduled", "wake_at":1100}
            ]).to_string()).unwrap();
            app.load = Load::Ready;
            app.tasks_load = Tasks::Ready;
        });
        assert_eq!(
            app.rows,
            vec![
                Item::Status("working"),
                Item::Tab(2),
                Item::Status("idle"),
                Item::Tab(3),
                Item::Status("snoozed"),
                Item::Tab(1),
                Item::Tab(0),
                Item::Tab(4),
            ]
        );
        assert_eq!(
            app.status_of(&app.flat(Item::Tab(4)), Item::Tab(4), 0).2,
            "bg job"
        );
        let soon = app.flat(Item::Tab(1));
        assert_eq!(app.status_of(&soon, Item::Tab(1), 0).2, "wakes in");
        let (right, _) = app.right_spans(&soon, Color::Reset, "wakes in", 120, None);
        assert!(right.last().unwrap().content.contains("wakes in  2m"));
        app.now = 2000;
        assert_eq!(
            app.status_of(&app.flat(Item::Tab(1)), Item::Tab(1), 0).2,
            "waking"
        );
    }

    fn spawned() -> App {
        let mut app = App::new((String::new(), String::new()), View::Inbox);
        app.follow = false;
        app.now = 1000;
        app.deliver(|app| {
            (app.sessions, app.tabs) = parse_rows(concat!(
                "S\t-\tlocal\ts\tdemo\n",
                "T\tlead\tidle\tlead\n",
                "T\tkid\tblocked\tkid\n",
                "T\tgrand\tworking\tgrand\n",
                "T\tsib\tdone\tsib\n",
                "T\tnap\tworking\tnap\n",
                "T\tlost\terror\tlost\n",
            ));
            app.tasks = parse_tasks(&serde_json::json!([
                {"session_id":"s", "window_id":"kid", "name":"kid", "parent_window":"lead"},
                {"session_id":"s", "window_id":"grand", "name":"grand", "parent_window":"kid"},
                {"session_id":"s", "window_id":"sib", "name":"sib", "parent_window":"lead"},
                {"session_id":"s", "window_id":"nap", "name":"nap", "parent_window":"lead", "snoozed_until":5000},
                {"session_id":"s", "window_id":"lost", "name":"lost", "parent_window":"gone"}
            ]).to_string()).unwrap();
            app.load = Load::Ready;
            app.tasks_load = Tasks::Ready;
        });
        app
    }

    /// `spawned` with every spawner unfolded.
    fn spawned_open() -> App {
        let mut app = spawned();
        app.unfolded = (0..app.tabs.len())
            .filter_map(|t| app.key_of(Item::Tab(t)))
            .collect();
        app.refilter();
        app
    }

    #[test]
    fn spawned_agents_nest_under_their_spawner_most_urgent_first() {
        let app = spawned_open();
        // The kid (blocked) and sib (done) leave their groups for the
        // lead's; a spawner that's gone leaves `lost` where it was, and the
        // snoozed `nap` stays snoozed. The idle lead waits on its working
        // grandchild, so is working too.
        assert_eq!(
            app.rows,
            vec![
                Item::Status("error"),
                Item::Tab(5),
                Item::Status("working"),
                Item::Tab(0),
                Item::Tab(1),
                Item::Tab(2),
                Item::Tab(3),
                Item::Status("snoozed"),
                Item::Tab(4),
            ]
        );
        assert!(app.flat(Item::Tab(0)).waiting);
        assert_eq!(app.flat_own(Item::Tab(0)).state, "idle");
        assert_eq!(app.flat(Item::Tab(3)).state, "done");
        let branch = |t| app.tree.get(&Item::Tab(t)).map(Guide::branch);
        assert_eq!(branch(0), None);
        assert_eq!(branch(1).as_deref(), Some("├ "));
        assert_eq!(branch(2).as_deref(), Some("│ └ "));
        assert_eq!(branch(3).as_deref(), Some("└ "));
        assert_eq!(branch(4), None);
        // The children are in the lead's project, which its row names.
        assert!(
            app.same_project_as_parent(Item::Tab(1)) && app.same_project_as_parent(Item::Tab(2))
        );
        assert!(
            !app.same_project_as_parent(Item::Tab(0)) && !app.same_project_as_parent(Item::Tab(4))
        );
    }

    #[test]
    fn spawned_agents_fold_under_their_spawner_until_unfolded() {
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let plain = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = spawned();
        // Folded by default: the lead stands for its three, one blocked.
        assert_eq!(
            app.rows,
            vec![
                Item::Status("error"),
                Item::Tab(5),
                Item::Status("working"),
                Item::Tab(0),
                Item::Status("snoozed"),
                Item::Tab(4),
            ]
        );
        let lead = app.folds[&Item::Tab(0)];
        assert_eq!((lead.rows, lead.open, lead.blocked), (3, false, true));
        assert_eq!(lead.badge(), " ▸3");
        // ctrl-j passes from the lead to the next group's row, wrapping.
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(0));
        app.key(ctrl('j'));
        assert_eq!(app.selected_item(), Some(Item::Tab(5)));
        // A folded row asked for highlights its spawner.
        app.select_key(app.key_of(Item::Tab(2)));
        assert_eq!(app.selected_item(), Some(Item::Tab(0)));
        // Right lists the children; the kid keeps its own folded.
        app.key(plain(KeyCode::Right));
        assert_eq!(&app.rows[3..6], &[Item::Tab(0), Item::Tab(1), Item::Tab(3)]);
        assert_eq!(app.selected_item(), Some(Item::Tab(0)));
        assert_eq!(app.folds[&Item::Tab(1)].badge(), " ▸1");
        // Right again moves into them, and on the kid unfolds its own.
        app.key(plain(KeyCode::Right));
        assert_eq!(app.selected_item(), Some(Item::Tab(1)));
        app.key(plain(KeyCode::Right));
        assert_eq!(app.rows[5], Item::Tab(2));
        app.key(ctrl('j'));
        assert_eq!(app.selected_item(), Some(Item::Tab(2)));
        // Left folds a nested row's siblings away, onto their spawner.
        app.key(plain(KeyCode::Left));
        assert_eq!(app.selected_item(), Some(Item::Tab(1)));
        assert!(!app.rows.contains(&Item::Tab(2)));
        app.key(plain(KeyCode::Left));
        assert_eq!(app.selected_item(), Some(Item::Tab(0)));
        assert!(!app.rows.contains(&Item::Tab(1)));
        // A search lists every match, folded or not.
        app.query = "s".into();
        app.refilter();
        assert!(app.rows.contains(&Item::Tab(3)));
    }

    #[test]
    fn spawned_agents_stay_in_their_groups_when_the_spawner_is_filtered_out() {
        let mut app = spawned();
        app.query = "kid".into();
        app.refilter();
        assert_eq!(app.rows, vec![Item::Status("blocked"), Item::Tab(1)]);
        assert!(app.tree.is_empty());
    }

    #[test]
    fn a_loop_of_spawners_nests_nothing() {
        let mut app = spawned_open();
        app.deliver(|app| {
            app.tasks.push(parse_tasks(&serde_json::json!([
                {"session_id":"s", "window_id":"lead", "name":"lead", "parent_window":"grand"}
            ]).to_string()).unwrap().remove(0));
        });
        assert_eq!(app.rows.iter().filter(|r| selectable(r)).count(), 6);
        let mut seen = HashSet::new();
        assert!(app.rows.iter().all(|r| !selectable(r) || seen.insert(*r)));
    }

    #[test]
    fn a_work_launch_is_highlighted_in_inbox_once_it_lists() {
        let mut app = inbox();
        let first = app.selected_key();
        app.launched = Some((("s".into(), "fresh".into()), Instant::now()));
        app.deliver(|_| {});
        // Not listed yet: the highlight stays and the launch waits.
        assert_eq!(app.selected_key(), first);
        assert!(app.launched.is_some());
        app.deliver(|app| {
            app.tasks.push(
                parse_tasks(
                    &serde_json::json!([
                        {"session_id":"s", "window_id":"fresh", "name":"fresh", "status":"working"}
                    ])
                    .to_string(),
                )
                .unwrap()
                .remove(0),
            );
        });
        assert_eq!(app.selected_key(), Some(("s".into(), "fresh".into())));
        assert!(app.launched.is_none());
        // Leaving Inbox gives it up.
        app.launched = Some((("s".into(), "later".into()), Instant::now()));
        app.set_view(View::Usage);
        app.deliver(|_| {});
        assert!(app.launched.is_none());
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
    fn inbox_enter_clears_the_search_and_cmd_enter_jumps() {
        let mut app = inbox();
        app.query = "older".into();
        app.refilter();
        let found = app.selected_key();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.inbox_enter(enter));
        assert_eq!(app.query, "");
        assert!(app.rows.len() > 2);
        assert_eq!(app.selected_key(), found);
        // cmd+Enter, and Enter on an armed interrupt, act as before.
        assert!(!app.inbox_enter(KeyEvent::new(KeyCode::Enter, KeyModifiers::SUPER)));
        app.armed = Armed::Interrupt;
        assert!(!app.inbox_enter(enter));
        app.armed = Armed::No;
        app.set_view(View::Archive);
        assert!(!app.inbox_enter(enter));
    }

    #[test]
    fn archive_jumps_with_gg_ctrl_g_and_shift_g() {
        let key = |app: &mut App, c, m| app.key(KeyEvent::new(KeyCode::Char(c), m));
        let mut app = App::new((String::new(), String::new()), View::Archive);
        app.follow = false;
        app.deliver(|app| {
            app.tasks = parse_tasks(
                &serde_json::json!([
                    {"session_id":"s", "window_id":"a", "name":"a", "host":"m4", "ended_at":50},
                    {"session_id":"s", "window_id":"b", "name":"b", "host":"m4", "ended_at":40},
                    {"session_id":"s", "window_id":"c", "name":"c", "host":"m4", "ended_at":30}
                ])
                .to_string(),
            )
            .unwrap();
            app.tasks_load = Tasks::Ready;
        });
        assert_eq!(app.rows.len(), 3);
        let (first, last) = (
            app.rows.iter().position(selectable),
            app.rows.iter().rposition(selectable),
        );
        key(&mut app, 'G', KeyModifiers::SHIFT);
        assert_eq!(app.selected, last);
        // One g waits for the second; another key in between drops it.
        key(&mut app, 'g', KeyModifiers::NONE);
        assert_eq!(app.selected, last);
        key(&mut app, 'k', KeyModifiers::NONE);
        let moved = app.selected;
        key(&mut app, 'g', KeyModifiers::NONE);
        assert_eq!(app.selected, moved);
        key(&mut app, 'g', KeyModifiers::NONE);
        assert_eq!(app.selected, first);
        app.edge(true);
        key(&mut app, 'g', KeyModifiers::CONTROL);
        assert_eq!(app.selected, first);
        // Searching, g types, and ctrl-g still jumps.
        app.searching = true;
        key(&mut app, 'g', KeyModifiers::NONE);
        key(&mut app, 'g', KeyModifiers::NONE);
        assert_eq!(app.query, "gg");
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
        assert_eq!(app.rows, [] as [Item; 0]);
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
        assert_eq!(app.pick(), "");
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
        assert_eq!(
            app.rows,
            vec![
                Item::Status("pinned"),
                Item::Tab(0),
                Item::Status("done"),
                Item::Tab(2),
                Item::Status("snoozed"),
                Item::Tab(3),
                Item::Tab(1),
            ]
        );
        // A snoozed blocked task isn't counted as waiting.
        let mut terminal = Terminal::new(TestBackend::new(100, 1)).unwrap();
        terminal.draw(|f| app.draw_search(f, f.area())).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(
            !text.contains("waiting") && text.contains("1 done"),
            "{text}"
        );
        let mut terminal = Terminal::new(TestBackend::new(100, 8)).unwrap();
        terminal.draw(|f| app.draw_list(f, f.area())).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(
            text.contains("Pinned") && text.contains("Snoozed") && text.contains("snoozed  1h"),
            "{text}"
        );
    }

    #[test]
    fn frame_names_the_views_and_footer_their_keys() {
        // The frame drawn `width` wide, and its top and bottom rows.
        let draw = |app: &mut App, width: u16| {
            let mut terminal = Terminal::new(TestBackend::new(width, 12)).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            let buf = terminal.backend().buffer().clone();
            let row = |y| (0..width).map(|x| buf[(x, y)].symbol()).collect::<String>();
            (row(0), row(11), buf)
        };
        let mut app = App::new((String::new(), String::new()), View::Archive);
        let (top, footer, buf) = draw(&mut app, 100);
        assert!(
            top.starts_with("╭─ Inbox · Work · Usage · Archive "),
            "{top}"
        );
        assert!(footer.contains("⌘1-4 views"), "{footer}");
        // The current view lit, the others not.
        let fg = |name: &str| {
            let x = top[..top.find(name).unwrap()].chars().count();
            buf[(u16::try_from(x).unwrap(), 0)].fg
        };
        assert_eq!(fg("Archive"), app.p.iris);
        assert_eq!(fg("Inbox"), app.p.muted);
        // Too narrow for every name: the current one alone.
        let (top, ..) = draw(&mut app, 24);
        assert!(
            top.starts_with("╭─ Archive ") && !top.contains("Inbox"),
            "{top}"
        );
        // The quick float has one list and no views to move between.
        app.view = View::Inbox;
        app.quick = true;
        let (top, footer, _) = draw(&mut app, 100);
        assert!(!top.contains("Inbox") && !footer.contains("views"), "{top}");
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
        assert_eq!(
            app.rows,
            vec![
                Item::Status("working"),
                Item::Tab(2),
                Item::Status("tabs"),
                Item::Tab(1),
                Item::Tab(3),
            ]
        );
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
            app.tasks = parse_tasks(
                &serde_json::json!([
                    {"session_id":"gone", "window_id":"w", "name":"stray", "status":"done"}
                ])
                .to_string(),
            )
            .unwrap();
            app.load = Load::Ready;
            app.tasks_load = Tasks::Ready;
        });
        // Agents from every session share the status sections, and the open
        // task with no listed session among them. The plain tabs are left
        // out of Inbox.
        assert_eq!(
            app.rows,
            vec![
                Item::Status("blocked"),
                Item::Tab(3),
                Item::Status("done"),
                Item::Task(0),
                Item::Status("working"),
                Item::Tab(1),
            ]
        );
        let mut terminal = Terminal::new(TestBackend::new(100, 10)).unwrap();
        terminal.draw(|f| app.draw_list(f, f.area())).unwrap();
        let line = |y: u16| -> String {
            let buf = terminal.backend().buffer();
            (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
        };
        for label in ["Waiting", "Done", "Working", "approval", "stray"] {
            assert!((0..10).any(|y| line(y).contains(label)), "{label}");
        }
        assert!(
            (0..10).all(|y| !line(y).contains("Tabs")),
            "plain tabs in Inbox"
        );
        // Each row names its session: no session headings or tree guides.
        assert!(
            line(1).contains("busy") && line(5).contains("quiet"),
            "{}",
            line(5)
        );
        assert!((0..10).all(|y| !line(y).contains('├') && !line(y).contains('╰')));
    }

    #[test]
    fn ctrl_r_pins_and_unpins_and_holds_over_stale_data() {
        let mut app = inbox_with_lifecycle();
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(2));
        // ctrl-r saves the pin in the registry (standalone_key), then shows it.
        assert!(app.can_pin());
        app.toggle_pin();
        assert!(app.tasks[2].pinned);
        assert_eq!(app.selected_item(), Some(Item::Tab(2)));
        // Done ranks above working, pinned or not.
        assert_eq!(
            &app.rows[..3],
            &[Item::Status("pinned"), Item::Tab(2), Item::Tab(0)]
        );
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
    fn ctrl_z_snoozes_until_a_typed_time_and_wakes() {
        let mut app = inbox_with_lifecycle();
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let plain = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(2));
        let target = app.pick().split_once(':').unwrap().1.to_string();
        app.key(ctrl('z'));
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
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(
            text.contains("Snooze /woke") && text.contains("until 2h"),
            "{text}"
        );
        app.key(plain(KeyCode::Enter));
        assert!(app.snooze.is_none());
        assert_eq!(app.tasks[2].snoozed_until, 8200);
        assert_eq!(app.rows.last(), Some(&Item::Tab(2)));
        // The highlight moved on to the row that followed.
        assert_eq!(app.selected_item(), Some(Item::Tab(3)));
        // On a snoozed task, an empty prompt wakes it.
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(2));
        app.key(ctrl('z'));
        assert_eq!(app.snooze.as_ref().unwrap().until, Ok(0));
        app.key(plain(KeyCode::Enter));
        assert_eq!(app.tasks[2].snoozed_until, 0);
        assert_eq!(app.selected_item(), Some(Item::Tab(2)));
        // Escape closes the prompt; the archive doesn't snooze.
        app.key(ctrl('z'));
        app.key(plain(KeyCode::Esc));
        assert!(app.snooze.is_none());
        app.set_view(View::Archive);
        app.key(ctrl('z'));
        assert!(app.snooze.is_none());
    }

    #[test]
    fn ctrl_j_and_k_pass_over_snoozed_rows() {
        let mut app = inbox_with_lifecycle();
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        app.deliver(|app| app.tasks[2].snoozed_until = 8200);
        assert_eq!(app.rows.last(), Some(&Item::Tab(2)));
        let snoozed = app
            .rows
            .iter()
            .position(|r| *r == Item::Status("snoozed"))
            .unwrap();
        let first = app.rows.iter().position(selectable).unwrap();
        let last = app.rows[..snoozed].iter().rposition(selectable).unwrap();
        // ctrl-j wraps from the last awake row to the first ...
        app.selected = Some(last);
        app.key(ctrl('j'));
        assert_eq!(app.selected, Some(first));
        // ... and ctrl-k back, over the snoozed one.
        app.key(ctrl('k'));
        assert_eq!(app.selected, Some(last));
        // From a snoozed row, they move onto awake ones.
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(2));
        app.key(ctrl('j'));
        assert_eq!(app.selected, Some(first));
    }

    #[test]
    fn ctrl_y_copies_the_highlighted_agents_log_path() {
        let mut app = inbox_with_lifecycle();
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(3));
        // Before its transcript loads, there's nothing to copy.
        app.key(ctrl('y'));
        assert!(app.copied.is_none());
        let want = app.preview_want().unwrap();
        let body = |target: &str| {
            format!(
                "{target}/{}/{}\n{{\"entries\":[],\"log\":\"/l/x.jsonl\",\
                 \"agent\":{{\"host\":\"box\",\"profile\":\"p\",\"harness\":\"claude\",\"dir\":\"/d\"}}}}",
                want.session, want.window
            )
        };
        app.preview = preview::parse(&body(&want.target));
        app.key(ctrl('y'));
        assert_eq!(
            app.copied.as_deref(),
            Some("host: box\nsession: x\nlog: /l/x.jsonl")
        );
        assert_eq!(
            app.note,
            Some(("copied box · x and log path".into(), false))
        );
        // Pi's logs carry a timestamp before the ID.
        assert_eq!(log_session_id("/p/2026-11-20T09-00_abc-1.jsonl"), "abc-1");
    }

    #[test]
    fn tab_leaves_a_box_holding_only_the_suggestion_it_took() {
        let mut app = inbox_with_lifecycle();
        let plain = |code| KeyEvent::new(code, KeyModifiers::NONE);
        // Tab 3 is idle, so the box shows its suggestion.
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(3));
        app.preview = preview::parse("local/s/d\n{\"entries\":[],\"suggestion\":\"run it\"}");
        app.key(plain(KeyCode::Tab));
        app.key(plain(KeyCode::Tab));
        assert_eq!(app.reply.as_ref().unwrap().prompt.text(), "run it");
        // Tab leaves the box while it holds the suggestion unedited.
        app.key(plain(KeyCode::Tab));
        assert!(app.reply.is_none());
        // Once edited, it keeps the message.
        app.key(plain(KeyCode::Tab));
        app.key(plain(KeyCode::Tab));
        app.key(plain(KeyCode::Char('!')));
        app.key(plain(KeyCode::Tab));
        assert_eq!(app.reply.as_ref().unwrap().prompt.text(), "run it!");
    }

    #[test]
    fn tab_messages_the_highlighted_agent() {
        let mut app = inbox_with_lifecycle();
        let plain = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(2));
        let target = app.pick().split_once(':').unwrap().1.to_string();
        app.key(plain(KeyCode::Tab));
        assert!(
            app.reply
                .as_ref()
                .is_some_and(|r| r.prompt.text().is_empty())
        );
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
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(
            text.contains("to /woke") && text.contains("report") && text.contains("F1 keys"),
            "{text}"
        );
        assert!(
            help_text(&app).starts_with(
                "Reply box\n⏎ send (empty: Claude's suggestion)\n⇥ → take Claude's suggestion\n⌥⏎ ⇧⏎ new line"
            ),
            "{}",
            help_text(&app)
        );
        // Enter, typed back by the config once the message is sent, closes
        // the box with a note.
        app.key(plain(KeyCode::Enter));
        assert!(app.reply.is_none());
        assert_eq!(app.note, Some(("sent to /woke".into(), false)));
        // Tab leaves an empty box, but not one with a message.
        app.key(plain(KeyCode::Tab));
        app.key(KeyEvent::from(KeyCode::Char('x')));
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
        assert!(
            app.note
                .as_ref()
                .is_some_and(|(n, failed)| *failed && n.contains("waiting on a prompt"))
        );
    }

    #[test]
    fn the_reply_box_stays_under_the_preview_expanded_or_not() {
        let mut app = inbox_with_lifecycle();
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(2));
        let screen = |app: &mut App| {
            let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
        };
        let text = screen(&mut app);
        assert!(
            text.contains("Tab to type a message") && text.contains("to /woke"),
            "{text}"
        );
        app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(app.expanded);
        let text = screen(&mut app);
        assert!(text.contains("Tab to type a message"), "{text}");
        // ctrl-y no longer opens it.
        app.key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert!(app.reply.is_none());
        // With nothing highlighted, it gives way, expanded or not.
        app.selected = None;
        for _ in 0..2 {
            let text = screen(&mut app);
            assert!(
                text.contains("Highlight an agent") && !text.contains("message it"),
                "{text}"
            );
            app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        }
    }

    #[test]
    fn ctrl_s_toggles_the_sidebar_in_scrollback() {
        let mut app = inbox_with_lifecycle();
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(2));
        app.scrollback = Some(scrollback::Scrollback::new(
            app.preview_want().unwrap(),
            10,
            5,
        ));
        let ctrl_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        app.key(ctrl_s);
        assert!(app.expanded && app.scrollback.is_some());
        app.key(ctrl_s);
        assert!(!app.expanded && app.scrollback.is_some());
        // So does a search being typed, which keeps its query.
        app.scrollback.as_mut().unwrap().typing = Some(scrollback::Typing {
            forward: true,
            query: "x".into(),
        });
        app.key(ctrl_s);
        let sb = app.scrollback.as_ref().unwrap();
        assert!(app.expanded && sb.typing.as_ref().is_some_and(|t| t.query == "x"));
    }

    #[test]
    fn leaving_the_reply_box_keeps_its_message_as_a_draft() {
        let mut app = inbox_with_lifecycle();
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(2));
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let text = |app: &App| app.reply.as_ref().map(|r| r.prompt.text());
        app.open_reply();
        app.key(key(KeyCode::Char('h')));
        app.key(key(KeyCode::Char('i')));
        // Escape leaves the box; Tab comes back to the message.
        app.key(key(KeyCode::Esc));
        assert!(app.reply.is_none());
        app.open_reply();
        assert_eq!(text(&app).as_deref(), Some("hi"));
        // As does ctrl-u for scrollback; the box shows it meanwhile.
        app.key(ctrl('u'));
        assert!(app.reply.is_none());
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(
            screen.contains("hi") && !screen.contains("Tab to type"),
            "{screen}"
        );
        app.open_reply();
        assert_eq!(text(&app).as_deref(), Some("hi"));
        // ctrl-c clears it for good.
        app.key(ctrl('c'));
        app.key(key(KeyCode::Esc));
        app.open_reply();
        assert_eq!(text(&app).as_deref(), Some(""));
        // Sending drops the draft.
        app.reply.as_mut().unwrap().prompt.set_text("sent");
        app.sent_reply();
        assert!(app.drafts.is_empty());
    }

    #[test]
    fn ctrl_s_toggles_the_sidebar_from_the_reply_box() {
        let mut app = inbox_with_lifecycle();
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(2));
        app.open_reply();
        let ctrl_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        app.key(ctrl_s);
        assert!(app.expanded && app.reply.is_some());
        app.key(ctrl_s);
        assert!(!app.expanded && app.reply.is_some());
        // ctrl-e is the box's again: the end of the line.
        let r = app.reply.as_mut().unwrap();
        r.prompt.set_text("ab");
        app.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        app.key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        app.key(KeyEvent::from(KeyCode::Char('c')));
        assert!(!app.expanded);
        assert_eq!(&*app.reply.as_ref().unwrap().prompt.text(), "abc");
    }

    #[test]
    fn a_narrow_search_cuts_its_placeholder_before_the_counts() {
        let mut app = inbox_with_lifecycle();
        let mut terminal = Terminal::new(TestBackend::new(44, 20)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let buf = terminal.backend().buffer();
        let row = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .find(|r| r.contains("Search"))
            .unwrap();
        assert!(row.contains("Search… 1 done  1 working  4 tabs"), "{row}");
    }

    #[test]
    fn failed_hosts_keep_the_other_rows_and_show_in_the_header() {
        let mut app = App::new((String::new(), String::new()), View::Inbox);
        let chips = |app: &App| {
            app.header_chips()
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        };
        app.paste(
            "rex-data:rows\nS\t-\tlocal\ts\tdemo\nT\tw\t\tshell\t~\nE\tmiller\tmiller: timed out\n",
        );
        assert!(matches!(app.load, Load::Ready));
        assert_eq!(
            (app.tabs.len(), app.failed.as_slice()),
            (1, ["miller".to_string()].as_slice())
        );
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
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text.contains("PR #57 ✓ merged · issue #42"), "{text}");
        assert!(text.contains("merged"));
        let mut terminal = Terminal::new(TestBackend::new(160, 3)).unwrap();
        terminal.draw(|f| app.draw_footer(f, f.area())).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text.contains("? keys"), "{text}");
        assert!(help_text(&app).contains("⌃B work items"));
    }

    #[test]
    fn picks_carry_no_link_count_and_the_header_shows_titled_items() {
        let mut app = App::new((String::new(), String::new()), View::Inbox);
        app.follow = false;
        app.deliver(|app| {
            app.tasks = parse_tasks(&serde_json::json!([
                {"session_id":"s", "window_id":"a", "name":"many", "status":"done", "links":[
                    {"kind":"issue","system":"github","ref":"o/r#42","url":"https://i"},
                    {"kind":"card","system":"fizzy","ref":"1041","url":"https://c","title":"Work item titles"},
                    {"kind":"pr","system":"github","ref":"o/r#57","url":"https://p"}]}
            ]).to_string()).unwrap();
            app.tasks_load = Tasks::Ready;
        });
        app.selected = app.rows.iter().position(|&i| i == Item::Task(0));
        assert_eq!(app.pick(), format!("task:{}/s/a", hex("")));
        assert_eq!(standalone::parse_pick(&app.pick()).unwrap().window, "a");
        // The header shows the titled work item under the name line.
        assert_eq!(app.detail_rows(80), 2);
        let mut terminal = Terminal::new(TestBackend::new(80, 2)).unwrap();
        terminal.draw(|f| app.draw_detail(f, f.area())).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text.contains("  card 1041  Work item titles"), "{text}");
    }

    #[test]
    fn ctrl_b_opens_work_items_even_for_one_link_and_asks_before_closing() {
        let mut app = App::new((String::new(), String::new()), View::Inbox);
        app.follow = false;
        app.deliver(|app| {
            app.tasks = parse_tasks(&serde_json::json!([
                {"session_id":"s", "window_id":"a", "name":"one", "status":"idle", "links":[
                    {"kind":"card","system":"fizzy","ref":"1042","url":"https://app.fizzy.do/6284043/cards/1042"}]},
                {"session_id":"s", "window_id":"b", "name":"none", "status":"idle"}
            ]).to_string()).unwrap();
            app.tasks_load = Tasks::Ready;
        });
        let plain = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let ctrl_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
        let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        app.selected = app.rows.iter().position(|&i| i == Item::Task(1));
        app.key(ctrl_b);
        assert!(app.items.is_none());
        assert_eq!(
            app.note.as_ref().map(|n| n.0.as_str()),
            Some("/none has no linked work items")
        );
        app.selected = app.rows.iter().position(|&i| i == Item::Task(0));
        assert!(help_text(&app).contains("⌃B work items"));
        assert!(!help_text(&app).contains("⌃R work items"));
        // ctrl-r is only the Work view's reload now.
        app.key(ctrl_r);
        assert!(app.items.is_none());
        app.key(ctrl_b);
        assert!(app.items.is_some());
        // Its letters don't search, and x asks first, naming the card.
        app.key(KeyEvent::from(KeyCode::Char('x')));
        assert_eq!(app.query, "");
        assert_eq!(
            app.items
                .as_ref()
                .and_then(super::items::Items::question)
                .as_deref(),
            Some("Close card 1042?")
        );
        let mut terminal = Terminal::new(TestBackend::new(100, 16)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(
            text.contains("Work items")
                && text.contains("Close card 1042?")
                && text.contains("yes"),
            "{text}"
        );
        assert!(help_text(&app).starts_with("Work items"));
        // Escape backs out of the confirmation, then closes the float.
        app.key(plain(KeyCode::Esc));
        assert!(app.items.as_ref().is_some_and(|i| i.question().is_none()));
        app.key(plain(KeyCode::Esc));
        assert!(app.items.is_none());
        // ctrl-b again closes it too.
        app.key(ctrl_b);
        app.key(ctrl_b);
        assert!(app.items.is_none());
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
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
        };
        let plain = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        app.key(plain('?'));
        assert_eq!(app.query, "");
        let text = screen(&mut app);
        assert!(
            text.contains(" Keys ") && text.contains("Highlighted row") && text.contains("archive"),
            "{text}"
        );
        for c in "/copy".chars() {
            app.key(plain(c));
        }
        let text = screen(&mut app);
        assert!(
            text.contains("copy the line, then reply") && !text.contains("Highlighted row"),
            "{text}"
        );
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
    fn up_and_down_in_the_reply_box_step_through_the_sessions_messages() {
        let mut app = App::new((String::new(), String::new()), View::Inbox);
        app.follow = false;
        app.paste("rex-data:rows\nS\t-\tlocal\ts\tdemo\nT\tagent\tdone\tclaude\t~/r\n");
        app.paste("rex-data:tasks\n[]");
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(0));
        app.paste(&format!(
            "rex-data:preview\n-/s/agent\n{}",
            serde_json::json!({"entries": [
                {"kind": "user", "text": "fix it"},
                {"kind": "assistant", "text": "done"},
                {"kind": "user", "text": "now\ntest it"},
            ]})
        ));
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        let text = |app: &App| app.reply.as_ref().unwrap().prompt.field.lines().join("\n");
        app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        app.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        app.key(up);
        assert_eq!(text(&app), "now\ntest it");
        // Up on the second line moves to the first; again recalls further.
        app.key(up);
        assert_eq!(text(&app), "now\ntest it");
        app.key(up);
        assert_eq!(text(&app), "fix it");
        app.key(up);
        assert_eq!(text(&app), "fix it");
        app.key(down);
        assert_eq!(text(&app), "now\ntest it");
        app.key(down);
        assert_eq!(text(&app), "x");
    }

    #[test]
    fn scrollback_moves_searches_and_copies_the_transcript() {
        let mut app = App::new((String::new(), String::new()), View::Inbox);
        app.follow = false;
        app.paste("rex-data:rows\nS\t-\tlocal\ts\tdemo\nT\tagent\tdone\tclaude\t~/r\n");
        app.paste("rex-data:tasks\n[]");
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(0));
        let entries: Vec<_> = (0..40)
            .map(|i| serde_json::json!({"kind": "assistant", "text": format!("answer {i}")}))
            .collect();
        app.paste(&format!(
            "rex-data:preview\n-/s/agent\n{}",
            serde_json::json!({"entries": entries})
        ));
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        let mut screen = |app: &mut App| {
            terminal.draw(|f| app.draw(f)).unwrap();
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
        };
        let text = screen(&mut app);
        assert!(
            app.sidebar && text.contains("answer 39") && !text.contains("answer 30 "),
            "{text}"
        );
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let plain = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        app.key(ctrl('v'));
        let text = screen(&mut app);
        assert!(text.contains("SCROLLBACK 79/79"), "{text}");
        // Search back for an early answer: the view moves to it.
        for c in "?answer 3\n".chars() {
            app.key(if c == '\n' {
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
            } else {
                plain(c)
            });
        }
        app.key(plain('N'));
        app.key(plain('N'));
        let text = screen(&mut app);
        assert!(
            text.contains("answer 3 ") && !text.contains("answer 39"),
            "{text}"
        );
        // New output arriving leaves the view where it is.
        let more: Vec<_> = (0..45)
            .map(|i| serde_json::json!({"kind": "assistant", "text": format!("answer {i}")}))
            .collect();
        app.paste(&format!(
            "rex-data:preview\n-/s/agent\n{}",
            serde_json::json!({"entries": more})
        ));
        let text = screen(&mut app);
        assert!(
            text.contains("answer 3 ") && !text.contains("answer 44"),
            "{text}"
        );
        app.key(plain('V'));
        app.key(plain('j'));
        app.key(plain('j'));
        app.key(plain('y'));
        assert_eq!(app.copied.as_deref(), Some("answer 3\n\nanswer 4"));
        // The copy leaves for the reply box, to paste it in.
        assert!(app.scrollback.is_none() && app.reply.is_some());
        let text = screen(&mut app);
        assert!(
            text.contains("copied 3 lines") && text.contains("answer 44"),
            "{text}"
        );
        // From the reply box, ctrl-v toggles scrollback and leaves the box
        // as it was.
        app.key(plain('h'));
        app.key(ctrl('v'));
        assert!(app.scrollback.is_some() && app.reply.is_some());
        app.key(plain('k'));
        app.key(ctrl('v'));
        assert!(app.scrollback.is_none());
        assert_eq!(
            app.reply.as_ref().map(|r| r.prompt.text()).as_deref(),
            Some("h")
        );
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
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(
            text.contains("archive claude") && text.contains("removes its worktree"),
            "{text}"
        );
        // An agent waiting at its prompt is one too. (The plain shell tab
        // isn't listed in Inbox, so there is no row to arm there.)
        app.selected = Some(at(&app, 2));
        app.key(ctrl('x'));
        assert_eq!(app.pick(), "archive:-/s/ready");
    }

    #[test]
    fn archive_takes_the_row_away_and_keeps_it_away_until_the_full_read_settles_it() {
        let mut app = App::new((String::new(), String::new()), View::Inbox);
        app.follow = false;
        app.paste("rex-data:rows\nS\t-\tlocal\ts\tdemo\nT\ta\tdone\tclaude\t~/r\nT\tb\tdone\tclaude\t~/r\n");
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(0));
        let key = app.selected_key().unwrap();
        let open = format!(
            r#"[{{"session_id":"{}","window_id":"{}","name":"a"}}]"#,
            key.0, key.1
        );
        app.paste(&format!("rex-data:tasks\n{open}"));
        let shows = |app: &App| {
            app.rows
                .iter()
                .any(|&i| app.key_of(i).as_ref() == Some(&key))
        };
        assert!(shows(&app));
        // Hidden at once, with the highlight on the row below.
        app.hide_for_archive();
        assert!(!shows(&app), "{:?}", app.rows);
        assert_eq!(app.selected_item(), Some(Item::Tab(1)));
        // Still hidden while the archive runs, whatever data arrives.
        let (tx, rx) = std::sync::mpsc::channel();
        app.archiving = Some(rx);
        app.paste(&format!("rex-data:tasks\n{open}"));
        assert!(!shows(&app));
        // The archive succeeds. The next live read has the tab gone but
        // the task still open, from before the archive: still hidden, and
        // no orphan row for the task.
        tx.send(Ok("archived a".into())).unwrap();
        app.take_archived();
        assert!(app.archiving.is_none());
        app.paste("rex-data:rows\nS\t-\tlocal\ts\tdemo\nT\tb\tdone\tclaude\t~/r\n");
        app.paste(&format!("rex-data:live\n{open}"));
        assert!(!shows(&app), "{:?}", app.rows);
        assert!(app.gone.is_some());
        // The full read, after the archive, shows the task ended: settled.
        let ended = format!(
            r#"[{{"session_id":"{}","window_id":"{}","name":"a","ended_at":50,"ended_reason":"archived"}}]"#,
            key.0, key.1
        );
        app.paste(&format!("rex-data:tasks\n{ended}"));
        assert!(app.gone.is_none());
        assert!(!shows(&app));
    }

    #[test]
    fn a_failed_archive_brings_the_row_back_at_once() {
        let mut app = App::new((String::new(), String::new()), View::Inbox);
        app.follow = false;
        app.paste("rex-data:rows\nS\t-\tlocal\ts\tdemo\nT\ta\tdone\tclaude\t~/r\nT\tb\tdone\tclaude\t~/r\n");
        app.paste("rex-data:tasks\n[]");
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(0));
        app.hide_for_archive();
        assert!(!app.rows.contains(&Item::Tab(0)));
        let (tx, rx) = std::sync::mpsc::channel();
        app.archiving = Some(rx);
        tx.send(Err("refused".into())).unwrap();
        app.take_archived();
        assert!(app.gone.is_none());
        assert!(app.rows.contains(&Item::Tab(0)), "{:?}", app.rows);
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
                 "links":[{"kind":"pr","system":"github","ref":"o/r#2","url":"p","state":"merged"}]},
                {"session_id":"s", "window_id":"e", "name":"closed card",
                 "links":[{"kind":"card","system":"fizzy","ref":"2000","url":"c","state":"closed","column":"Done"}]},
                {"session_id":"s", "window_id":"f", "name":"one card open",
                 "links":[{"kind":"card","system":"fizzy","ref":"2001","url":"c","state":"closed"},
                          {"kind":"card","system":"fizzy","ref":"2002","url":"c","state":"open"}]}
            ]).to_string()).unwrap();
            app.tasks_load = Tasks::Ready;
        });
        app.query = "card 1041".into();
        app.refilter();
        assert_eq!(app.rows, vec![Item::Task(0)]);
        assert_eq!(app.pick(), format!("resume:{}/s/a", hex("")));
        assert!(app.tasks[0].archived());
        assert!(
            !app.tasks[0].safe()
                && app.tasks[1].safe()
                && !app.tasks[2].safe()
                && app.tasks[3].safe()
        );
        assert!(
            app.tasks[4].safe(),
            "all its cards closed counts as merged work"
        );
        assert!(!app.tasks[5].safe(), "one open card means work remains");
    }

    #[test]
    fn status_headings_render_without_changing_other_views() {
        let mut app = inbox();
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal.draw(|f| app.draw_list(f, f.area())).unwrap();
        let buffer = terminal.backend().buffer();
        let text: String = buffer
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        for label in ["Waiting", "Failed", "Done", "Working", "Idle", "No status"] {
            assert!(text.contains(label), "missing status heading: {label}");
        }
        app.set_view(View::Archive);
        assert_eq!(app.rows, vec![Item::Task(5)]);
    }

    #[test]
    fn archive_columns_line_up_under_headers_and_glyphs_name_the_outcome() {
        let mut app = App::new((String::new(), String::new()), View::Archive);
        app.follow = false;
        app.now = 60 * 60 * 24 * 30;
        app.deliver(|app| {
            app.tasks = parse_tasks(&serde_json::json!([
                {"session_id":"s", "window_id":"a", "name":"landed", "host":"m4", "ended_at":50,
                 "ended_reason":"merged", "links":[{"kind":"card","system":"fizzy","ref":"1","url":"c"}]},
                {"session_id":"s", "window_id":"b", "name":"kept", "host":"drummer", "ended_at":40,
                 "ended_reason":"tab closed", "worktree_left":true,
                 "links":[{"kind":"card","system":"fizzy","ref":"1160","url":"c","state":"closed","column":"Done"}]},
                {"session_id":"s", "window_id":"c", "name":"boxed", "host":"m4", "ended_at":30,
                 "ended_reason":"archived"}
            ]).to_string()).unwrap();
            app.tasks_load = Tasks::Ready;
        });
        let mut terminal = Terminal::new(TestBackend::new(110, 5)).unwrap();
        terminal.draw(|f| app.draw_list(f, f.area())).unwrap();
        let buffer = terminal.backend().buffer();
        let lines: Vec<Vec<&str>> = (0..4)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect()
            })
            .collect();
        // The column a word starts in (cells, not bytes: the glyphs are
        // multibyte).
        let at = |line: &[&str], word: &str| {
            let text = line.concat();
            text[..text.find(word).unwrap()].chars().count()
        };
        let head = &lines[0];
        let (items_at, host_at) = (at(head, "work items"), at(head, "host"));
        assert_eq!(at(&lines[1], "card 1"), items_at);
        assert_eq!(at(&lines[2], "card 1160"), items_at);
        assert_eq!(at(&lines[1], "m4"), host_at);
        assert_eq!(at(&lines[2], "drummer"), host_at);
        assert_eq!(at(&lines[3], "m4"), host_at);
        let outcome_end = at(head, "outcome") + "outcome".len();
        for (y, verb) in [(1, "merged"), (2, "closed"), (3, "archived")] {
            assert_eq!(at(&lines[y], verb) + verb.len(), outcome_end);
        }
        let glyphs: Vec<&str> = lines[1..].iter().map(|l| l[1]).collect();
        assert_eq!(glyphs, ["✓", "○", "▢"]);
    }

    #[test]
    fn an_orphan_pick_keeps_the_slashes_in_its_window() {
        let p = standalone::parse_pick("archive:6d//orphan:mac:/r/.worktrees/x").unwrap();
        assert_eq!(
            (p.session.as_str(), p.window.as_str()),
            ("", "orphan:mac:/r/.worktrees/x")
        );
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
        assert_eq!(
            app.rows,
            vec![
                Item::Task(4),
                Item::Task(0),
                Item::Task(3),
                Item::Task(1),
                Item::Task(2)
            ]
        );
        assert_eq!(app.selected, Some(0));
        // ctrl-d and ctrl-u page by half a page, leaving the search be.
        app.list_area = Rect::new(0, 0, 100, 6);
        app.query = "a".into();
        app.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!((app.selected, app.query.as_str()), (Some(2), "a"));
        app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!((app.selected, app.query.as_str()), (Some(0), "a"));
        app.query.clear();
        // ctrl-f hides the archived (and merged) ones.
        app.key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert_eq!(app.rows, vec![Item::Task(1), Item::Task(2)]);
        let mut terminal = Terminal::new(TestBackend::new(100, 10)).unwrap();
        terminal.draw(|f| app.draw_list(f, f.area())).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        // Each row names its project and how it ended; no headings.
        assert!(text.contains("beta") && text.contains("closed"), "{text}");
        assert!(
            !text.contains("Left behind") && !text.contains("─"),
            "{text}"
        );
    }

    #[test]
    fn archive_tab_chooses_a_project_and_drops_one_that_empties() {
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
        let press = |app: &mut App, code: KeyCode| app.key(KeyEvent::new(code, KeyModifiers::NONE));
        let all = vec![
            Item::Task(4),
            Item::Task(0),
            Item::Task(3),
            Item::Task(1),
            Item::Task(2),
        ];
        assert_eq!(app.rows, all);
        // Tab opens the chooser; typing filters it and Enter shows the pick.
        press(&mut app, KeyCode::Tab);
        assert!(app.projects.is_some());
        for c in "bet".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        // Letters went to the chooser, not the archive's search.
        assert_eq!(app.query, "");
        press(&mut app, KeyCode::Enter);
        assert!(app.projects.is_none());
        assert_eq!(app.project.as_deref(), Some("beta"));
        assert_eq!(app.rows, vec![Item::Task(1), Item::Task(2)]);
        // It reopens on beta; Home picks All projects again.
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Home);
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.project.clone(), app.rows.clone()), (None, all));
        // Escape closes it unchanged.
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Esc);
        assert!(app.projects.is_none() && app.project.is_none());
        // ctrl-f hides alpha's every task, so a filter on alpha goes back to all.
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.project.as_deref(), Some("alpha"));
        app.key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert_eq!(app.project, None);
        assert_eq!(app.rows, vec![Item::Task(1), Item::Task(2)]);
    }

    #[test]
    fn usage_is_integrated_read_only_and_handles_narrow_frames() {
        let mut app = inbox();
        app.set_view(View::Usage);
        assert_eq!(app.rows, [] as [Item; 0]);
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
        assert_ne!(app.rows, [] as [Item; 0]);
    }

    #[test]
    fn long_tab_names_stop_at_a_third_of_the_row() {
        // Plain tabs are listed only in the quick float.
        let mut app = App::new((String::new(), String::new()), View::Inbox);
        app.quick = true;
        let long = "x".repeat(200);
        app.paste(&format!(
            "rex-data:rows\nS\t-\tlocal\ts\tdemo\nT\tw\t\t{long}\t~/dev/src/project\n"
        ));
        let mut terminal = Terminal::new(TestBackend::new(100, 3)).unwrap();
        terminal.draw(|f| app.draw_list(f, f.area())).unwrap();
        let buf = terminal.backend().buffer();
        let row: String = (0..100).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        let shown = row.matches('x').count();
        assert!(
            row.contains('…') && row.contains("~/dev/src/project") && (24..=34).contains(&shown),
            "{row}"
        );
    }
    #[test]
    fn wide_all_shows_a_collapsible_sidebar_beside_the_highlighted_agent() {
        let mut app = inbox_with_lifecycle();
        app.selected = app.rows.iter().position(|&i| i == Item::Tab(2));
        let screen = |app: &mut App, w: u16| {
            let mut terminal = Terminal::new(TestBackend::new(w, 30)).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            let buf = terminal.backend().buffer();
            (0..30)
                .map(|y| {
                    (0..w)
                        .map(|x| buf[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
        };
        // The list on the left, the agent's name, transcript and reply box
        // on the right, on the same rows.
        let rows = screen(&mut app, 140);
        assert!(app.sidebar);
        let row = rows.iter().find(|r| r.contains("Search")).unwrap();
        assert!(
            row.contains('│') && row.contains("woke") && row.contains("done"),
            "{row}"
        );
        assert!(
            rows.iter().any(|r| r.contains("pinned") && r.contains('│')),
            "{rows:#?}"
        );
        assert!(
            rows.iter()
                .any(|r| r.contains("Tab to type a message") && r.contains('│')),
            "{rows:#?}"
        );
        // ctrl-s collapses the sidebar; `/` to search brings it back.
        app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        let rows = screen(&mut app, 140);
        assert!(
            !rows
                .iter()
                .any(|r| r.contains("Search") || r.contains("pinned")),
            "{rows:#?}"
        );
        assert!(help_text(&app).contains("⌃S show the sidebar"));
        app.key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        app.key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
        assert!(!app.expanded && app.query == "w");
        // Too narrow, Inbox stacks the list over the reply box as before.
        app.key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        let rows = screen(&mut app, 80);
        assert!(!app.sidebar);
        assert!(
            rows.iter()
                .any(|r| r.contains("Search tabs, agents, and work items")),
            "{rows:#?}"
        );
    }

    #[test]
    fn sidebar_rows_take_two_lines_and_scroll_to_the_highlight() {
        let mut app = App::new((String::new(), String::new()), View::Inbox);
        app.follow = false;
        app.now = 1000;
        let mut rows = String::from("S\t-\tlocal\ts\tdemo\n");
        let mut tasks = Vec::new();
        for i in 0..12 {
            let _ = writeln!(rows, "T\tw{i}\tworking\tt{i}");
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
            (0..20)
                .map(|y| {
                    (0..50)
                        .map(|x| buf[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
        };
        // Every project is the same colour, edits or not; only the pencil
        // is gold, and the name keeps the text colour beside the age.
        app.tasks[1].dirty = 3;
        let mut terminal = Terminal::new(TestBackend::new(140, 20)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let buf = terminal.backend().buffer();
        let find = |text: &str| {
            (0..20u16)
                .find_map(|y| {
                    let row: String = (0..50).map(|x| buf[(x, y)].symbol().to_string()).collect();
                    row.find(text).map(|i| (cells(row[..i].chars().count()), y))
                })
                .unwrap()
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
        assert!(
            lines[at].contains("1m") && lines[at + 1].contains("proj · step 0"),
            "{lines:#?}"
        );
        // The last row scrolls into view with both its lines.
        app.edge(true);
        let lines = screen(&mut app);
        let at = lines.iter().position(|l| l.contains("task-11")).unwrap();
        assert!(
            lines[at + 1].contains("step 11") && !lines.iter().any(|l| l.contains("task-00")),
            "{lines:#?}"
        );
    }

    #[test]
    fn plain_tabs_name_their_project_like_tasks_do() {
        assert_eq!(folder_project("~/dev/src/amfaro"), "amfaro");
        assert_eq!(
            folder_project("~/dev/src/playground/switchyard/.worktrees/more-t3-code-like/crates"),
            "switchyard"
        );
        assert_eq!(folder_project("~"), "~");
        assert_eq!(folder_project("/"), "/");
    }
}
