//! The ctrl-r work items float: the highlighted task's linked items, each
//! with its state, checks and title; what can be done to the highlighted
//! one is in the footer (see core's `item_actions`).
//! It is separate from ctrl-b's chooser, which only opens links, and opens
//! even for a task with one link.
//!
//! On the highlighted item: Enter or `o` opens it in the browser; `v`
//! reads its description and comments here, in a scrolling reader over
//! the float (a card, issue or PR; see core's `item_view`); `c`
//! types a comment for a card, issue or PR, in a wrapping prompt box; `x` closes a card or
//! an issue (not a PR); `r` marks a
//! draft PR ready for review. Nothing mutates without its explicit key and
//! then a confirmation naming the item (Enter or `y` runs it, anything
//! else goes back). Escape, ctrl-c or ctrl-r again close the float; in
//! the reader, Escape or `q` goes back to the list.
//!
//! This holds the float's state and keys; `standalone_key` runs what they
//! ask for (see `Outcome`) and shows the outcome in the footer.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::cell::Cell;

use ratatui::widgets::{Block, Clear, Padding};
use switchyard_core::item_actions::{Action, Item};
use switchyard_core::item_view::{Comment, Thread};
use switchyard_ui::Palette;
use switchyard_ui::draw::str_width;
use switchyard_ui::popup;
use switchyard_ui::prompt_box::{self, PromptBox};

use crate::model::Link;

/// A linked item, and what actions it takes (None: it can only be opened).
pub struct Entry {
    pub link: Link,
    pub item: Option<Item>,
    /// The task's repo, which its name leaves out (see model's `home_repo`).
    home: Option<String>,
}

impl Entry {
    fn actions(&self) -> Vec<Action> {
        self.item.as_ref().map(Item::actions).unwrap_or_default()
    }

    /// How the list, a note, a confirmation and the reader name it: as
    /// the header does (see model's `Link::name`).
    fn name(&self) -> String {
        self.link.name(self.home.as_deref())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    List,
    /// Typing a comment for the highlighted card, issue or PR (in
    /// `Items::comment`).
    Comment,
    /// Asking whether to run `action` (with a comment's `body`).
    Confirm {
        action: Action,
        body: String,
    },
    /// Reading the highlighted item's thread, scrolled `scroll` rows down.
    Read {
        thread: Box<Thread>,
        scroll: usize,
    },
}

pub struct Items {
    /// The task's tab, as (`session_id`, `window_id`), which stays highlighted.
    pub key: (String, String),
    pub label: String,
    /// The task's profile, for gh.
    pub profile: String,
    pub entries: Vec<Entry>,
    pub selected: usize,
    pub mode: Mode,
    /// The reader's last drawn height and furthest scroll, for its keys.
    page: Cell<(usize, usize)>,
    /// The comment being typed, kept while confirming or after a failed
    /// run. It is never saved to a prompt file.
    comment: PromptBox,
}

/// Text rows the comment box shows, growing with the comment; longer ones
/// scroll to their end.
const COMMENT_MIN_ROWS: u16 = 3;
const COMMENT_MAX_ROWS: u16 = 8;

/// An empty comment box.
fn comment_box() -> PromptBox {
    let mut b = PromptBox::new();
    b.set_placeholder("Type a comment");
    b
}

/// What a key asks of the picker.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Close,
    Help,
    /// Open the URL in the browser here.
    Open(String),
    /// Fetch the highlighted item's thread to read here (then `show`).
    Read,
    /// Run the confirmed action on the item.
    Run {
        action: Action,
        item: Item,
        body: String,
    },
    /// Why the key does nothing here, for the footer.
    Refuse(String),
}

/// What the header doesn't say of an item, as coloured pieces: a PR's
/// state (`open`, `draft`, `merged`, `closed`) and checks, a card's column
/// and assignee (`✓ Done` when closed). Only what its link already holds.
fn status(l: &Link, p: &Palette) -> Vec<(String, Color)> {
    let mut out = Vec::new();
    match l.kind.as_str() {
        "pr" => {
            let state = match l.state.as_str() {
                "open" => Some(p.pine),
                "draft" => Some(p.muted),
                "merged" => Some(p.iris),
                "closed" => Some(p.love),
                _ => None,
            };
            if let Some(fg) = state {
                out.push((l.state.clone(), fg));
            }
            let checks = match l.checks.as_str() {
                "pass" => Some(("✓ checks", p.pine)),
                "fail" => Some(("✗ checks", p.love)),
                "pending" => Some(("● checks", p.gold)),
                _ => None,
            };
            // A merged or closed PR's checks no longer matter.
            if let Some((text, fg)) =
                checks.filter(|_| !matches!(l.state.as_str(), "merged" | "closed"))
            {
                out.push((text.to_string(), fg));
            }
        }
        "card" if l.state == "closed" => {
            let column = if l.column.is_empty() {
                "closed"
            } else {
                &l.column
            };
            out.push((format!("✓ {column}"), p.iris));
        }
        "card" if l.state == "open" => {
            let column = if l.column.is_empty() {
                "open"
            } else {
                &l.column
            };
            out.push((column.to_string(), p.foam));
            if !l.assignee.is_empty() {
                out.push((format!("@{}", l.assignee), p.subtle));
            }
        }
        _ => {}
    }
    out
}

/// What a list row says beside an item's name: its `status`, less an open
/// card's board column (`Maybe?`, `Doing`), which the reader still shows.
fn row_status(l: &Link, p: &Palette) -> Vec<(String, Color)> {
    let mut out = status(l, p);
    if l.kind == "card" && l.state == "open" && !out.is_empty() {
        out.remove(0);
    }
    out
}

/// Title lines an item shows in the list before the rest is cut with `…`.
const TITLE_LINES: usize = 2;

/// An item's icon, by kind, and its colour, by state, as GitHub draws
/// them: a PR is open, draft, merged or closed; an issue or card open or
/// done. (Nerd Font glyphs; see `switchyard_ui::glyphs`.)
fn icon(l: &Link, p: &Palette) -> (&'static str, Color) {
    let closed = l.state == "closed";
    match l.kind.as_str() {
        "pr" => match l.state.as_str() {
            "draft" => ("\u{f4dd}", p.muted), // nf-oct-git_pull_request_draft
            "merged" => ("\u{f419}", p.iris), // nf-oct-git_merge
            "closed" => ("\u{f4dc}", p.love), // nf-oct-git_pull_request_closed
            _ => ("\u{f407}", p.pine),        // nf-oct-git_pull_request
        },
        "issue" if closed => ("\u{f41d}", p.iris), // nf-oct-issue_closed
        "issue" => ("\u{f41b}", p.pine),           // nf-oct-issue_opened
        "card" if closed => ("\u{f49e}", p.iris),  // nf-oct-check_circle
        "card" => ("\u{f0b78}", p.foam),           // nf-md-card_text_outline
        "commit" => ("\u{f417}", p.subtle),        // nf-oct-git_commit
        _ => ("\u{f44c}", p.subtle),               // nf-oct-link
    }
}

/// An item as the list lays it out, `width` columns of text wide: its
/// title (or its URL until the tracker names it), wrapped to at most
/// `TITLE_LINES`, then one line of what it is, its name and `row_status`.
struct Laid {
    title: Vec<String>,
    /// Whether the title is the URL standing in for one.
    url: bool,
}

impl Laid {
    fn new(e: &Entry, width: usize) -> Self {
        let (text, url) = match e.link.title.trim() {
            "" => (e.link.url.clone(), true),
            t => (t.to_string(), false),
        };
        let mut title = if url {
            vec![crate::preview::clip_width(&text, width)]
        } else {
            crate::preview::wrap(&text, width)
        };
        if title.len() > TITLE_LINES {
            title.truncate(TITLE_LINES);
            let last = &mut title[TITLE_LINES - 1];
            *last = crate::preview::clip_width(&format!("{last} …"), width);
            if !last.ends_with('…') {
                let keep = width.saturating_sub(1);
                *last = format!("{}…", crate::preview::clip_width(last, keep).trim_end());
            }
        }
        if title.is_empty() {
            title.push(String::new());
        }
        Self { title, url }
    }

    /// Its rows: the title's, then the line under it.
    fn height(&self) -> usize {
        self.title.len() + 1
    }
}

/// The first of `heights` to show so that `selected` is in view in
/// `rows`, with a blank row between items: the top, until the selection
/// would fall off the bottom.
fn first_shown(heights: &[usize], selected: usize, rows: usize) -> usize {
    let span = |from: usize| -> usize {
        heights[from..=selected].iter().sum::<usize>() + (selected - from)
    };
    let mut first = 0;
    while first < selected && span(first) > rows {
        first += 1;
    }
    first
}

/// The line under an item's title: its name, then what `row_status` says,
/// in colour, clipped to `width`.
fn meta_spans(e: &Entry, width: usize, bg: Color, p: &Palette) -> Vec<Span<'static>> {
    let mut room = width;
    let mut spans = Vec::new();
    let mut push = |text: &str, fg: Color, spans: &mut Vec<Span<'static>>| {
        let text = crate::preview::clip_width(text, room);
        room = room.saturating_sub(str_width(&text));
        spans.push(Span::styled(text, Style::new().fg(fg).bg(bg)));
    };
    push(&e.name(), p.subtle, &mut spans);
    for (text, fg) in row_status(&e.link, p) {
        push("  ·  ", p.muted, &mut spans);
        push(&text, fg, &mut spans);
    }
    spans
}

/// Columns before an item's text: its icon and two spaces.
const GUTTER: usize = 3;

/// The items of `heights` shown in `rows` with `selected` in view (see
/// `first_shown`), as many as fit from the first.
fn shown(heights: &[usize], selected: usize, rows: usize) -> std::ops::Range<usize> {
    let first = first_shown(heights, selected, rows);
    let mut used = 0;
    let mut end = first;
    for h in &heights[first..] {
        if used + h > rows {
            break;
        }
        used += h + 1;
        end += 1;
    }
    first..end.max((first + 1).min(heights.len()))
}

/// The list's box: `Work items` at the left of its top border and the
/// task's `label`, muted, at the right; in its bottom border, how many
/// items are scrolled out of view `above` and `beneath`.
fn list_block(p: &Palette, label: &str, above: usize, beneath: usize) -> Block<'static> {
    let right = |text: String| {
        Line::from(vec![
            Span::styled(text, Style::new().fg(p.muted)),
            Span::styled("─", Style::new().fg(p.iris)),
        ])
        .right_aligned()
    };
    let block = popup::block(p, "Work items")
        .padding(Padding::uniform(1))
        .title_top(right(format!(" {label} ")));
    let mut more = Vec::new();
    if above > 0 {
        more.push(format!("↑ {above}"));
    }
    if beneath > 0 {
        more.push(format!("↓ {beneath}"));
    }
    if more.is_empty() {
        block
    } else {
        block.title_bottom(right(format!(" {} more ", more.join("  "))))
    }
}

/// One item of the list in `item` (rows of the box's `inner`): its icon,
/// its title lines and the line under them; banded, with an iris bar in
/// the padding at its left, when `selected`.
fn draw_item(
    f: &mut Frame,
    p: &Palette,
    inner: Rect,
    item: Rect,
    e: &Entry,
    laid: &Laid,
    selected: bool,
) {
    let bg = if selected { p.overlay } else { p.base };
    let line = |n: usize| Rect {
        x: item.x + crate::cells(GUTTER),
        y: item.y + crate::cells(n),
        width: item.width.saturating_sub(crate::cells(GUTTER)),
        height: 1,
    };
    if selected {
        popup::select(f, p, inner, item);
        for dy in 0..item.height {
            let bar = Rect {
                x: item.x.saturating_sub(1),
                y: item.y + dy,
                width: 1,
                height: 1,
            };
            f.render_widget(Span::styled("▌", Style::new().fg(p.iris).bg(bg)), bar);
        }
    }
    let (glyph, fg) = icon(&e.link, p);
    let at = Rect {
        width: 1,
        height: 1,
        ..item
    };
    f.render_widget(Span::styled(glyph, Style::new().fg(fg).bg(bg)), at);
    let mut style = Style::new().fg(p.text).bg(bg);
    if laid.url {
        style = style.fg(p.muted);
    } else if selected {
        style = style.add_modifier(Modifier::BOLD);
    }
    for (n, t) in laid.title.iter().enumerate() {
        f.render_widget(Span::styled(t.clone(), style), line(n));
    }
    let width = usize::from(line(0).width);
    f.render_widget(
        Line::from(meta_spans(e, width, bg, p)),
        line(laid.title.len()),
    );
}

/// The key that asks for an action, as the float names it.
fn action_key(a: Action) -> (&'static str, &'static str) {
    match a {
        Action::Comment => ("c", "comment"),
        Action::Close => ("x", "close"),
        Action::Ready => ("r", "mark ready"),
    }
}

impl Items {
    /// The float over a task's links (those with a URL, most likely first;
    /// see model's `chooser_links`), or None when it has none.
    pub fn new(
        key: (String, String),
        label: String,
        profile: String,
        links: Vec<Link>,
    ) -> Option<Items> {
        let home = crate::model::home_repo(&links).map(str::to_string);
        let entries: Vec<Entry> = links
            .into_iter()
            .map(|link| {
                let item = Item::from_link(
                    &link.kind,
                    &link.system,
                    &link.reference,
                    &link.url,
                    &link.state,
                );
                Entry {
                    link,
                    item,
                    home: home.clone(),
                }
            })
            .collect();
        (!entries.is_empty()).then_some(Items {
            key,
            label,
            profile,
            entries,
            selected: 0,
            mode: Mode::List,
            page: Cell::new((0, 0)),
            comment: comment_box(),
        })
    }

    fn entry(&self) -> &Entry {
        &self.entries[self.selected]
    }

    /// The highlighted item's name, for the picker's busy and result notes.
    pub fn selected_name(&self) -> String {
        self.entry().name()
    }

    pub fn key(&mut self, k: KeyEvent) -> Outcome {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        match std::mem::replace(&mut self.mode, Mode::List) {
            Mode::List => self.list_key(k, ctrl),
            Mode::Comment => {
                let newline = alt || k.modifiers.contains(KeyModifiers::SHIFT);
                match k.code {
                    KeyCode::Esc => return Outcome::Stay,
                    KeyCode::Char('c') if ctrl => return Outcome::Stay,
                    KeyCode::Enter if newline => self.comment.field.insert_newline(),
                    KeyCode::Enter if self.comment.text().is_empty() => {}
                    KeyCode::Enter => {
                        self.mode = Mode::Confirm {
                            action: Action::Comment,
                            body: self.comment.text(),
                        };
                        return Outcome::Stay;
                    }
                    _ => self.comment.key(k),
                }
                self.mode = Mode::Comment;
                Outcome::Stay
            }
            Mode::Read { thread, scroll } => self.read_key(k, ctrl, thread, scroll),
            Mode::Confirm { action, body } => match k.code {
                KeyCode::Enter | KeyCode::Char('y') if !ctrl => match self.entry().item.clone() {
                    Some(item) => Outcome::Run { action, item, body },
                    None => Outcome::Stay,
                },
                // Back, keeping a comment's text to edit.
                _ if action == Action::Comment => {
                    self.mode = Mode::Comment;
                    Outcome::Stay
                }
                _ => Outcome::Stay,
            },
        }
    }

    fn list_key(&mut self, k: KeyEvent, ctrl: bool) -> Outcome {
        let last = self.entries.len() - 1;
        let want = |a: Action, me: &mut Items| {
            let e = me.entry();
            if e.actions().contains(&a) {
                me.mode = if a == Action::Comment {
                    me.comment.clear();
                    Mode::Comment
                } else {
                    Mode::Confirm {
                        action: a,
                        body: String::new(),
                    }
                };
                return Outcome::Stay;
            }
            let why = match a {
                Action::Comment | Action::Close if e.link.kind == "card" => {
                    format!("{}: its URL names no Fizzy account", e.name())
                }
                Action::Comment => format!(
                    "{}: c comments on Fizzy cards and GitHub issues and PRs",
                    e.name()
                ),
                Action::Close if e.link.kind == "pr" => {
                    format!("{}: x closes cards and issues, not PRs", e.name())
                }
                Action::Close => format!("{}: x closes Fizzy cards and GitHub issues", e.name()),
                Action::Ready if e.link.kind == "pr" => format!("{} isn't a draft", e.name()),
                Action::Ready => format!("{} isn't a PR: r marks a draft PR ready", e.name()),
            };
            Outcome::Refuse(why)
        };
        match k.code {
            KeyCode::Esc => Outcome::Close,
            KeyCode::Char('c' | 'r') if ctrl => Outcome::Close,
            KeyCode::Char('?') => Outcome::Help,
            KeyCode::Up | KeyCode::Char('k') => self.step(self.selected.saturating_sub(1)),
            KeyCode::Char('p') if ctrl => self.step(self.selected.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => self.step((self.selected + 1).min(last)),
            KeyCode::Char('n') if ctrl => self.step((self.selected + 1).min(last)),
            KeyCode::Home => self.step(0),
            KeyCode::End => self.step(last),
            KeyCode::Enter | KeyCode::Char('o') if !ctrl => match self.entry().link.url.as_str() {
                "" => Outcome::Refuse(format!("{} has no URL to open", self.entry().name())),
                url => Outcome::Open(url.to_string()),
            },
            KeyCode::Char('v') if !ctrl => self.read(),
            KeyCode::Char('c') => want(Action::Comment, self),
            KeyCode::Char('x') if !ctrl => want(Action::Close, self),
            KeyCode::Char('r') => want(Action::Ready, self),
            _ => Outcome::Stay,
        }
    }

    /// Ask to read the highlighted item, when it's one that can be read.
    fn read(&self) -> Outcome {
        let e = self.entry();
        let readable = matches!(
            (e.link.kind.as_str(), e.link.system.as_str()),
            ("card", "fizzy") | ("issue" | "pr", "github")
        );
        if readable {
            Outcome::Read
        } else {
            Outcome::Refuse(format!(
                "{}: v reads Fizzy cards and GitHub issues and PRs",
                e.name()
            ))
        }
    }

    /// Show a fetched thread for the highlighted item, from its top.
    pub fn show(&mut self, thread: Thread) {
        self.mode = Mode::Read {
            thread: Box::new(thread),
            scroll: 0,
        };
    }

    /// A key in the reader: scroll, open, comment, or go back.
    fn read_key(&mut self, k: KeyEvent, ctrl: bool, thread: Box<Thread>, scroll: usize) -> Outcome {
        let (page, max) = self.page.get();
        let half = (page / 2).max(1);
        let page = page.saturating_sub(1).max(1);
        let to = match k.code {
            KeyCode::Esc | KeyCode::Char('q') => return Outcome::Stay,
            KeyCode::Char('c' | 'r') if ctrl => return Outcome::Close,
            KeyCode::Char('?') => {
                self.mode = Mode::Read { thread, scroll };
                return Outcome::Help;
            }
            KeyCode::Char('o') | KeyCode::Enter => {
                self.mode = Mode::Read { thread, scroll };
                return match self.entry().link.url.as_str() {
                    "" => Outcome::Refuse(format!("{} has no URL to open", self.entry().name())),
                    url => Outcome::Open(url.to_string()),
                };
            }
            KeyCode::Char('c') if self.entry().actions().contains(&Action::Comment) => {
                self.comment.clear();
                self.mode = Mode::Comment;
                return Outcome::Stay;
            }
            KeyCode::Up | KeyCode::Char('k') => scroll.saturating_sub(1),
            KeyCode::Char('p') if ctrl => scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => scroll + 1,
            KeyCode::Char('n') if ctrl => scroll + 1,
            KeyCode::Char('u') if ctrl => scroll.saturating_sub(half),
            KeyCode::Char('d') if ctrl => scroll + half,
            KeyCode::PageUp | KeyCode::Char('b') => scroll.saturating_sub(page),
            KeyCode::PageDown | KeyCode::Char(' ') => scroll + page,
            KeyCode::Home | KeyCode::Char('g') => 0,
            KeyCode::End | KeyCode::Char('G') => max,
            _ => scroll,
        };
        self.mode = Mode::Read {
            thread,
            scroll: to.min(max),
        };
        Outcome::Stay
    }

    fn step(&mut self, to: usize) -> Outcome {
        self.selected = to;
        Outcome::Stay
    }

    /// After a failed run: back to the list, or to a comment's text so it
    /// isn't lost.
    pub fn failed(&mut self, action: Action, body: &str) {
        self.mode = if action == Action::Comment {
            if self.comment.text() != body {
                self.comment.clear();
                self.comment.paste(body);
            }
            Mode::Comment
        } else {
            Mode::List
        };
    }

    /// Text John pasted, into the comment being typed; false when no
    /// comment is.
    pub fn paste(&mut self, text: &str) -> bool {
        let typing = self.mode == Mode::Comment;
        if typing {
            self.comment.paste(text);
        }
        typing
    }

    /// The footer's hints while the float is open.
    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        match &self.mode {
            Mode::List => {
                let mut h = vec![("⏎", "open"), ("v", "read")];
                let card = matches!(self.entry().item, Some(Item::Card { .. }));
                h.extend(self.entry().actions().into_iter().map(|a| match a {
                    // Not to be confused with esc dismissing the float.
                    Action::Close if card => ("x", "close card"),
                    Action::Close => ("x", "close issue"),
                    _ => action_key(a),
                }));
                h.extend([("esc", "dismiss"), ("?", "keys")]);
                h
            }
            Mode::Comment => vec![
                ("⏎", "review"),
                ("⌥⏎", "newline"),
                ("esc", "back"),
                ("F1", "keys"),
            ],
            Mode::Confirm { .. } => vec![("⏎ y", "yes"), ("esc", "no")],
            Mode::Read { .. } => {
                let mut h = vec![("↑↓ j k", "scroll"), ("o", "open")];
                if self.entry().actions().contains(&Action::Comment) {
                    h.push(("c", "comment"));
                }
                h.extend([("esc", "back"), ("?", "keys")]);
                h
            }
        }
    }

    /// What the float's bottom line asks, for confirmations: "Close card
    /// 1041?" or "Post a comment on card 1041: “…”?".
    pub fn question(&self) -> Option<String> {
        let Mode::Confirm { action, body } = &self.mode else {
            return None;
        };
        let name = self.entry().name();
        Some(match action {
            Action::Comment => format!("Post a comment on {name}: “{body}”?"),
            Action::Close => format!("Close {name}?"),
            Action::Ready => format!("Mark {name} ready for review?"),
        })
    }

    /// A box over the list, each item a block: its icon, coloured by
    /// state (see `icon`), beside its title, wrapped to two lines, over a
    /// muted line naming it with its state and checks (see `row_status`).
    /// A blank line separates items and pads the box inside its border;
    /// the highlighted item is banded with an iris bar at its left. The
    /// list scrolls to keep the highlight in view, and the bottom border
    /// says how many are out of it. While typing a comment or confirming,
    /// the comment or the question is under the list. Its keys are in the
    /// footer (see `hints`).
    pub fn draw(&mut self, f: &mut Frame, area: Rect, p: &Palette) {
        if let Mode::Read { thread, scroll } = &self.mode {
            self.draw_reader(f, area, p, thread, *scroll);
            return;
        }
        crate::backdrop(f, p);
        // Less the borders and padding.
        let text_w = usize::from(popup::LIST_WIDTH.min(area.width).saturating_sub(4)).max(1);
        // A confirmation's question, wrapped whole; the box a comment is
        // typed in, as rows of its text and chrome.
        let foot = match &self.mode {
            Mode::Confirm { .. } => self
                .question()
                .map_or_else(Vec::new, |q| crate::preview::wrap(&q, text_w)),
            Mode::Comment => {
                let card_w = u16::try_from(text_w).unwrap_or(u16::MAX);
                let rows = self
                    .comment
                    .rows(card_w, COMMENT_MIN_ROWS, COMMENT_MAX_ROWS);
                vec![String::new(); usize::from(rows + prompt_box::CHROME_ROWS)]
            }
            _ => Vec::new(),
        };
        let body_w = text_w.saturating_sub(GUTTER).max(1);
        let laid: Vec<Laid> = self.entries.iter().map(|e| Laid::new(e, body_w)).collect();
        let heights: Vec<usize> = laid.iter().map(Laid::height).collect();
        let list = heights.iter().sum::<usize>() + heights.len().saturating_sub(1);
        // A blank line between the list and what is under it.
        let below = if foot.is_empty() { 0 } else { foot.len() + 1 };
        // The border and a blank line inside it, above and below.
        let height = crate::cells(list + below).saturating_add(4);
        let rect = popup::list_rect(area, height);
        let rows = usize::from(rect.height.saturating_sub(4)).saturating_sub(below);
        let shown = shown(&heights, self.selected, rows);
        let block = list_block(p, &self.label, shown.start, self.entries.len() - shown.end);
        let inner = popup::open(f, rect, block);
        let mut top = inner.y;
        for (i, (e, l)) in self.entries.iter().zip(&laid).enumerate() {
            if !shown.contains(&i) {
                continue;
            }
            let item = Rect {
                y: top,
                height: crate::cells(l.height()),
                ..inner
            };
            draw_item(f, p, inner, item, e, l, i == self.selected);
            top = item.bottom() + 1;
        }
        self.draw_foot(f, inner, foot, p);
    }

    /// The box's last lines: a confirmation's question, wrapped, or the
    /// comment being typed, in a prompt box naming what it is on under its
    /// text.
    fn draw_foot(&mut self, f: &mut Frame, inner: Rect, foot: Vec<String>, p: &Palette) {
        let top = inner.bottom().saturating_sub(crate::cells(foot.len()));
        let line = |i: usize| Rect {
            y: top + crate::cells(i),
            height: 1,
            ..inner
        };
        match &self.mode {
            Mode::Comment => {
                let top = top.max(inner.y);
                let card = Rect {
                    y: top,
                    height: inner.bottom() - top,
                    ..inner
                };
                let rows = crate::cells(foot.len()).saturating_sub(prompt_box::CHROME_ROWS);
                let name = self.entry().name();
                let Some((chips, skip)) = self.comment.draw(f, p, card, rows, true, None) else {
                    return;
                };
                let bg = p.overlay;
                let mut left = vec![
                    Span::styled("on ", Style::new().fg(p.muted).bg(bg)),
                    Span::styled(
                        name,
                        Style::new().fg(p.text).bg(bg).add_modifier(Modifier::BOLD),
                    ),
                ];
                if skip > 0 {
                    left.push(Span::styled(
                        format!("  +{skip} lines above"),
                        Style::new().fg(p.muted).bg(bg),
                    ));
                }
                f.render_widget(Line::from(left), chips);
            }
            Mode::Confirm { action, .. } => {
                // Red only for what can't be undone.
                let fg = if *action == Action::Close {
                    p.love
                } else {
                    p.text
                };
                let style = Style::new().fg(fg).add_modifier(Modifier::BOLD);
                for (i, q) in foot.into_iter().enumerate() {
                    if line(i).y >= inner.y {
                        f.render_widget(Span::styled(q, style), line(i));
                    }
                }
            }
            Mode::List | Mode::Read { .. } => {}
        }
    }

    /// The reader: a large box over the picker with the item's title,
    /// state and opening post, then each comment under a header naming
    /// its author and time, wrapped to the box and scrolled; how far down
    /// in its bottom border. Its keys are in the footer.
    fn draw_reader(&self, f: &mut Frame, area: Rect, p: &Palette, thread: &Thread, scroll: usize) {
        let width = area.width.saturating_sub(4).min(100);
        let height = area.height.saturating_sub(2);
        let r = crate::centered(area, width, height);
        crate::backdrop(f, p);
        // Blank the whole pane, so nothing under it shows round the box.
        f.render_widget(Clear, area);
        f.render_widget(Block::new().style(Style::new().bg(p.base)), area);
        let title = format!("{} · {}", self.entry().name(), self.label);
        let mut block = popup::block(p, &title);
        let text = block.inner(r);
        if text.height < 1 || text.width < 2 {
            return;
        }
        let lines = reader_lines(thread, &self.entry().link, text.width as usize, p);
        let rows = text.height as usize;
        let max = lines.len().saturating_sub(rows);
        self.page.set((rows, max));
        let scroll = scroll.min(max);
        if let Some(pct) = (scroll * 100).checked_div(max) {
            block = block.title_bottom(
                Line::from(vec![
                    Span::styled(format!(" {pct}% "), Style::new().fg(p.muted)),
                    Span::styled("─", Style::new().fg(p.iris)),
                ])
                .right_aligned(),
            );
        }
        popup::open(f, r, block);
        for (i, line) in lines.into_iter().skip(scroll).take(rows).enumerate() {
            let row = Rect {
                y: text.y + crate::cells(i),
                height: 1,
                ..text
            };
            f.render_widget(line, row);
        }
    }
}

/// The reader's state pieces: the rows' `status` for its link, with a
/// PR's state as the fetched thread gives it (`OPEN` stays `draft` for a
/// draft, which the thread doesn't say); the tracker's own state, muted,
/// for what `status` doesn't colour.
fn reader_state(thread: &Thread, link: &Link, p: &Palette) -> Vec<(String, Color)> {
    let mut l = link.clone();
    if l.kind == "pr" && !thread.state.is_empty() {
        let state = thread.state.to_lowercase();
        if !(state == "open" && l.state == "draft") {
            l.state = state;
        }
    }
    let pieces = status(&l, p);
    if pieces.is_empty() && !thread.state.is_empty() {
        return vec![(thread.state.clone(), p.muted)];
    }
    pieces
}

/// When a comment was made, as `2026-11-14 19:27` (UTC, as the trackers
/// give it), or as given when it isn't ISO 8601.
fn when(at: &str) -> String {
    match at.get(..16) {
        Some(day_time) if at.as_bytes().get(10) == Some(&b'T') => day_time.replacen('T', " ", 1),
        _ => at.to_string(),
    }
}

/// A thread as wrapped lines `width` columns wide; GitHub's Markdown
/// bodies drawn as the preview draws an agent's (see preview's `markdown`).
/// Its state is coloured as the work-items rows colour it (see
/// `reader_state`).
pub(crate) fn reader_lines(
    thread: &Thread,
    link: &Link,
    width: usize,
    p: &Palette,
) -> Vec<Line<'static>> {
    let wrap = |line: Line<'static>| crate::preview::wrap_line(line, width);
    let mut out: Vec<Line<'static>> = Vec::new();
    let title = if thread.title.is_empty() {
        "(untitled)".to_string()
    } else {
        thread.title.clone()
    };
    out.extend(wrap(Line::styled(
        title,
        Style::new().fg(p.text).add_modifier(Modifier::BOLD),
    )));
    let mut meta = reader_state(thread, link, p);
    if !thread.post.author.is_empty() {
        meta.push((format!("by {}", thread.post.author), p.muted));
    }
    if !thread.post.at.is_empty() {
        meta.push((when(&thread.post.at), p.muted));
    }
    let n = thread.comments.len();
    meta.push((
        match n {
            1 => "1 comment".into(),
            n => format!("{n} comments"),
        },
        p.muted,
    ));
    let mut spans = Vec::new();
    for (i, (text, fg)) in meta.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", Style::new().fg(p.muted)));
        }
        spans.push(Span::styled(text, Style::new().fg(fg)));
    }
    out.extend(wrap(Line::from(spans)));
    out.push(Line::default());
    let body = |out: &mut Vec<Line<'static>>, c: &Comment, empty: &str| {
        if c.body.is_empty() {
            out.push(Line::styled(empty.to_string(), Style::new().fg(p.muted)));
            return;
        }
        if thread.markdown {
            out.extend(
                crate::preview::markdown(&c.body, width, p)
                    .into_iter()
                    .map(|(line, _)| line),
            );
            return;
        }
        for l in c.body.lines() {
            out.extend(wrap(Line::styled(
                l.trim_end().to_string(),
                Style::new().fg(p.text),
            )));
        }
    };
    body(&mut out, &thread.post, "(no description)");
    for c in &thread.comments {
        out.push(Line::default());
        let who = if c.author.is_empty() {
            "someone"
        } else {
            c.author.as_str()
        };
        let mut head = vec![
            Span::styled("── ", Style::new().fg(p.muted)),
            Span::styled(
                who.to_string(),
                Style::new().fg(p.gold).add_modifier(Modifier::BOLD),
            ),
        ];
        if !c.at.is_empty() {
            head.push(Span::styled(
                format!(" · {}", when(&c.at)),
                Style::new().fg(p.muted),
            ));
        }
        out.extend(wrap(Line::from(head)));
        body(&mut out, c, "(empty)");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(kind: &str, system: &str, reference: &str, url: &str, state: &str) -> Link {
        Link {
            kind: kind.into(),
            system: system.into(),
            reference: reference.into(),
            url: url.into(),
            state: state.into(),
            ..Link::default()
        }
    }

    fn items() -> Items {
        let links = vec![
            link(
                "pr",
                "github",
                "o/r#57",
                "https://github.com/o/r/pull/57",
                "draft",
            ),
            link(
                "card",
                "fizzy",
                "1041",
                "https://app.fizzy.do/6284043/cards/1041",
                "",
            ),
            link(
                "issue",
                "github",
                "o/r#3",
                "https://github.com/o/r/issues/3",
                "",
            ),
        ];
        Items::new(
            ("s".into(), "w".into()),
            "demo/task".into(),
            "personal".into(),
            links,
        )
        .unwrap()
    }

    fn press(i: &mut Items, code: KeyCode) -> Outcome {
        i.key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn typed(i: &mut Items, text: &str) {
        for c in text.chars() {
            press(i, KeyCode::Char(c));
        }
    }

    #[test]
    fn no_links_no_float() {
        assert!(
            Items::new(
                ("s".into(), "w".into()),
                "t".into(),
                "personal".into(),
                vec![]
            )
            .is_none()
        );
    }

    #[test]
    fn enter_opens_and_nothing_mutates_without_a_confirmation() {
        let mut i = items();
        assert_eq!(
            press(&mut i, KeyCode::Enter),
            Outcome::Open("https://github.com/o/r/pull/57".into())
        );
        // r on the draft PR asks first, naming it.
        assert_eq!(press(&mut i, KeyCode::Char('r')), Outcome::Stay);
        assert_eq!(i.question().unwrap(), "Mark PR #57 ready for review?");
        // Any other key goes back without running.
        assert_eq!(press(&mut i, KeyCode::Char('q')), Outcome::Stay);
        assert_eq!(i.mode, Mode::List);
        press(&mut i, KeyCode::Char('r'));
        let pr = Item::Pr {
            repo: "o/r".into(),
            number: "57".into(),
            draft: true,
        };
        assert_eq!(
            press(&mut i, KeyCode::Char('y')),
            Outcome::Run {
                action: Action::Ready,
                item: pr,
                body: String::new()
            }
        );
    }

    #[test]
    fn actions_refuse_items_they_dont_fit() {
        let mut i = items();
        assert_eq!(
            press(&mut i, KeyCode::Char('x')),
            Outcome::Refuse("PR #57: x closes cards and issues, not PRs".into())
        );
        press(&mut i, KeyCode::Down);
        assert!(
            matches!(press(&mut i, KeyCode::Char('r')), Outcome::Refuse(w) if w.contains("isn't a PR"))
        );
        press(&mut i, KeyCode::Down);
        assert!(
            matches!(press(&mut i, KeyCode::Char('r')), Outcome::Refuse(w) if w.contains("isn't a PR"))
        );
        assert_eq!(i.mode, Mode::List);
        let mut commit = Items::new(
            ("s".into(), "w".into()),
            "t".into(),
            "p".into(),
            vec![link("commit", "github", "abc", "u", "")],
        )
        .unwrap();
        assert!(
            matches!(press(&mut commit, KeyCode::Char('c')), Outcome::Refuse(w) if w.ends_with("c comments on Fizzy cards and GitHub issues and PRs"))
        );
        let mut open = Items::new(
            ("s".into(), "w".into()),
            "t".into(),
            "p".into(),
            vec![link("pr", "github", "o/r#1", "u", "open")],
        )
        .unwrap();
        assert_eq!(
            press(&mut open, KeyCode::Char('r')),
            Outcome::Refuse("PR #1 isn't a draft".into())
        );
    }

    #[test]
    fn close_card_confirms_naming_the_card() {
        let mut i = items();
        press(&mut i, KeyCode::Down);
        assert_eq!(i.hints()[2..4], [("c", "comment"), ("x", "close card")]);
        press(&mut i, KeyCode::Char('x'));
        assert_eq!(i.question().unwrap(), "Close card 1041?");
        assert_eq!(press(&mut i, KeyCode::Esc), Outcome::Stay);
        assert_eq!(i.mode, Mode::List);
        press(&mut i, KeyCode::Char('x'));
        let card = Item::Card {
            account: "6284043".into(),
            number: "1041".into(),
        };
        assert_eq!(
            press(&mut i, KeyCode::Enter),
            Outcome::Run {
                action: Action::Close,
                item: card,
                body: String::new()
            }
        );
    }

    #[test]
    fn issues_take_comments_and_close_prs_take_comments() {
        let mut i = items();
        assert_eq!(i.hints()[2..4], [("c", "comment"), ("r", "mark ready")]);
        press(&mut i, KeyCode::Down);
        press(&mut i, KeyCode::Down);
        assert_eq!(i.hints()[2..4], [("c", "comment"), ("x", "close issue")]);
        press(&mut i, KeyCode::Char('x'));
        assert_eq!(i.question().unwrap(), "Close issue #3?");
        press(&mut i, KeyCode::Esc);
        press(&mut i, KeyCode::Char('c'));
        typed(&mut i, "on it");
        press(&mut i, KeyCode::Enter);
        assert_eq!(
            i.question().unwrap(),
            "Post a comment on issue #3: “on it”?"
        );
    }

    #[test]
    fn comment_is_typed_then_confirmed_and_kept_on_the_way_back() {
        let mut i = items();
        press(&mut i, KeyCode::Down);
        press(&mut i, KeyCode::Char('c'));
        // Empty: Enter does nothing; letters (even x and r) are text.
        press(&mut i, KeyCode::Enter);
        assert_eq!(i.mode, Mode::Comment);
        typed(&mut i, "rx ready ");
        press(&mut i, KeyCode::Enter);
        assert_eq!(
            i.question().unwrap(),
            "Post a comment on card 1041: “rx ready”?"
        );
        // No goes back to the text, which a failed run also keeps.
        press(&mut i, KeyCode::Esc);
        assert_eq!(
            (&i.mode, i.comment.text()),
            (&Mode::Comment, "rx ready".into())
        );
        press(&mut i, KeyCode::Enter);
        let Outcome::Run { action, body, .. } = press(&mut i, KeyCode::Enter) else {
            panic!("no run")
        };
        i.failed(action, &body);
        assert_eq!(
            (&i.mode, i.comment.text()),
            (&Mode::Comment, "rx ready".into())
        );
        // Escape leaves the comment, then the float.
        press(&mut i, KeyCode::Esc);
        assert_eq!(i.mode, Mode::List);
        assert_eq!(press(&mut i, KeyCode::Esc), Outcome::Close);
    }

    #[test]
    fn a_comment_takes_newlines_and_pastes() {
        let mut i = items();
        assert!(!i.paste("not typing"));
        press(&mut i, KeyCode::Down);
        press(&mut i, KeyCode::Char('c'));
        typed(&mut i, "one");
        i.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert!(i.paste("two\r\nthree"));
        assert_eq!(i.mode, Mode::Comment);
        press(&mut i, KeyCode::Enter);
        assert_eq!(
            i.question().unwrap(),
            "Post a comment on card 1041: “one\ntwo\nthree”?"
        );
    }

    #[test]
    fn v_reads_the_item_in_place_and_esc_goes_back() {
        let mut i = items();
        assert_eq!(i.hints()[..2], [("⏎", "open"), ("v", "read")]);
        // The PR, then the card, can be read; the float fetches, then shows.
        assert_eq!(press(&mut i, KeyCode::Char('v')), Outcome::Read);
        press(&mut i, KeyCode::Down);
        assert_eq!(press(&mut i, KeyCode::Char('v')), Outcome::Read);
        i.show(Thread {
            title: "t".into(),
            ..Thread::default()
        });
        assert!(matches!(i.mode, Mode::Read { scroll: 0, .. }));
        assert!(i.hints().contains(&("c", "comment")));
        // Scrolling stops at what was last drawn as the end.
        i.page.set((10, 3));
        press(&mut i, KeyCode::Char('j'));
        assert!(matches!(i.mode, Mode::Read { scroll: 1, .. }));
        press(&mut i, KeyCode::Char(' '));
        assert!(matches!(i.mode, Mode::Read { scroll: 3, .. }));
        press(&mut i, KeyCode::Char('g'));
        assert!(matches!(i.mode, Mode::Read { scroll: 0, .. }));
        // o opens it and stays in the reader; c starts a comment.
        assert_eq!(
            press(&mut i, KeyCode::Char('o')),
            Outcome::Open("https://app.fizzy.do/6284043/cards/1041".into())
        );
        assert!(matches!(i.mode, Mode::Read { .. }));
        press(&mut i, KeyCode::Char('c'));
        assert_eq!((&i.mode, i.comment.text()), (&Mode::Comment, String::new()));
        press(&mut i, KeyCode::Esc);
        i.show(Thread::default());
        // Esc goes back to the list, not out of the float.
        assert_eq!(press(&mut i, KeyCode::Esc), Outcome::Stay);
        assert_eq!(i.mode, Mode::List);
        let mut other = Items::new(
            ("s".into(), "w".into()),
            "t".into(),
            "p".into(),
            vec![link("commit", "github", "abc", "u", "")],
        )
        .unwrap();
        assert!(
            matches!(press(&mut other, KeyCode::Char('v')), Outcome::Refuse(w) if w.contains("v reads"))
        );
    }

    #[test]
    fn items_are_named_as_the_header_names_them() {
        let mut links = items()
            .entries
            .into_iter()
            .map(|e| e.link)
            .collect::<Vec<_>>();
        links.push(link("issue", "github", "x/y#9", "u", ""));
        let i = Items::new(("s".into(), "w".into()), "t".into(), "p".into(), links).unwrap();
        let names: Vec<String> = i.entries.iter().map(Entry::name).collect();
        // The repo only where it isn't the task's (its PR's).
        assert_eq!(names, ["PR #57", "card 1041", "issue #3", "issue x/y#9"]);
    }

    #[test]
    fn rows_show_state_checks_and_card_columns_in_colour() {
        let p = Palette::load();
        let text =
            |l: &Link| -> Vec<String> { status(l, &p).into_iter().map(|(t, _)| t).collect() };
        let pr = |state: &str, checks: &str| Link {
            checks: checks.into(),
            ..link("pr", "github", "o/r#1", "u", state)
        };
        assert_eq!(text(&pr("open", "pass")), ["open", "✓ checks"]);
        assert_eq!(status(&pr("open", "pass"), &p)[1].1, p.pine);
        assert_eq!(text(&pr("draft", "fail")), ["draft", "✗ checks"]);
        assert_eq!(status(&pr("draft", "fail"), &p)[1].1, p.love);
        assert_eq!(text(&pr("open", "pending")), ["open", "● checks"]);
        // Once merged or closed, checks don't matter.
        assert_eq!(text(&pr("merged", "fail")), ["merged"]);
        assert_eq!(status(&pr("closed", ""), &p)[0].1, p.love);
        let card = |state: &str, column: &str, assignee: &str| Link {
            column: column.into(),
            assignee: assignee.into(),
            ..link("card", "fizzy", "1", "u", state)
        };
        assert_eq!(text(&card("open", "Doing", "John")), ["Doing", "@John"]);
        assert_eq!(text(&card("closed", "", "")), ["✓ closed"]);
        // The list leaves out an open card's column; the reader keeps it.
        let row =
            |l: &Link| -> Vec<String> { row_status(l, &p).into_iter().map(|(t, _)| t).collect() };
        assert_eq!(row(&card("open", "Maybe?", "John")), ["@John"]);
        assert_eq!(row(&card("open", "Maybe?", "")), Vec::<String>::new());
        assert_eq!(row(&card("closed", "Done", "")), ["✓ Done"]);
        assert_eq!(row(&pr("open", "pass")), ["open", "✓ checks"]);
        // Nothing known, nothing shown: no lookup yet, or an issue.
        let none: [String; 0] = [];
        assert_eq!(text(&card("", "", "")), none);
        assert_eq!(text(&link("issue", "github", "o/r#3", "u", "")), none);
    }

    #[test]
    fn the_list_scrolls_to_keep_the_highlight_in_view() {
        // Three 3-row items need 11 rows with the gaps between them.
        let h = [3, 3, 3];
        assert_eq!(shown(&h, 0, 11), 0..3);
        assert_eq!(shown(&h, 0, 7), 0..2);
        assert_eq!(shown(&h, 2, 7), 1..3);
        // Too short for even one: the highlighted one still shows.
        assert_eq!(shown(&h, 1, 2), 1..2);
    }

    #[test]
    fn titles_wrap_to_two_lines_then_ellipsis() {
        let mut e = items().entries.remove(0);
        e.link.title = "one two three four five six seven".into();
        let l = Laid::new(&e, 10);
        assert_eq!(l.title.len(), 2);
        assert!(l.title[1].ends_with('…'), "{:?}", l.title);
        assert!(l.title.iter().all(|t| str_width(t) <= 10));
        assert_eq!(l.height(), 3);
        // No title yet: the URL, on one line, muted.
        e.link.title = String::new();
        let l = Laid::new(&e, 80);
        assert!(l.url && l.title.len() == 1);
    }

    #[test]
    fn icons_follow_kind_and_state() {
        let p = Palette::load();
        let pr = |state: &str| icon(&link("pr", "github", "o/r#1", "u", state), &p);
        assert_eq!(pr("open"), ("\u{f407}", p.pine));
        assert_eq!(pr("merged"), ("\u{f419}", p.iris));
        assert_eq!(pr("closed").1, p.love);
        let card = |state: &str| icon(&link("card", "fizzy", "1", "u", state), &p);
        assert_eq!(card("open").1, p.foam);
        assert_eq!(card("closed"), ("\u{f49e}", p.iris));
    }

    #[test]
    fn the_reader_lays_out_post_then_comments() {
        let t = Thread {
            title: "Fix it".into(),
            state: "Doing".into(),
            post: Comment {
                author: "a".into(),
                at: "2026-11-14T19:27:49.362Z".into(),
                body: "line one\nline two".into(),
            },
            comments: vec![Comment {
                author: "b".into(),
                at: "2026-11-15T08:00:00Z".into(),
                body: String::new(),
            }],
            markdown: false,
        };
        let text: Vec<String> = reader_lines(&t, &Link::default(), 80, &Palette::load())
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(
            text,
            [
                "Fix it",
                "Doing · by a · 2026-11-14 19:27 · 1 comment",
                "",
                "line one",
                "line two",
                "",
                "── b · 2026-11-15 08:00",
                "(empty)",
            ]
        );
    }

    #[test]
    fn the_reader_colours_state_and_checks_as_the_rows_do() {
        let p = Palette::load();
        let thread = |state: &str| Thread {
            title: "t".into(),
            state: state.into(),
            ..Thread::default()
        };
        let pr = |state: &str| Link {
            kind: "pr".into(),
            state: state.into(),
            checks: "pass".into(),
            ..Link::default()
        };
        let meta = |t: &Thread, l: &Link| -> Vec<(String, Color)> {
            reader_lines(t, l, 80, &p)[1]
                .spans
                .iter()
                .map(|s| (s.content.to_string(), s.style.fg.unwrap()))
                .collect()
        };
        let open = meta(&thread("OPEN"), &pr("open"));
        assert_eq!(open[0], ("open".into(), p.pine));
        assert_eq!(open[2], ("✓ checks".into(), p.pine));
        // The thread is fresher than the link, but doesn't say draft.
        assert_eq!(
            meta(&thread("MERGED"), &pr("open"))[0],
            ("merged".into(), p.iris)
        );
        assert_eq!(
            meta(&thread("OPEN"), &pr("draft"))[0],
            ("draft".into(), p.muted)
        );
        // What the rows don't colour stays as the tracker says it.
        let issue = Link {
            kind: "issue".into(),
            ..Link::default()
        };
        assert_eq!(
            meta(&thread("CLOSED"), &issue)[0],
            ("CLOSED".into(), p.muted)
        );
    }

    #[test]
    fn github_bodies_are_drawn_as_markdown() {
        let t = Thread {
            title: "t".into(),
            post: Comment {
                body: "## Plan\n\nRun **this**:\n\n- one\n- two".into(),
                ..Comment::default()
            },
            markdown: true,
            ..Thread::default()
        };
        let lines = reader_lines(&t, &Link::default(), 80, &Palette::load());
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        // No literal `**`; the heading, paragraph and list each drawn.
        assert!(!text.iter().any(|l| l.contains("**")), "{text:?}");
        assert!(text.iter().any(|l| l.contains("Plan")));
        assert!(text.iter().any(|l| l.contains("one")));
        let bold = lines
            .iter()
            .flat_map(|l| &l.spans)
            .find(|s| s.content.contains("this"))
            .unwrap();
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
    }
}
