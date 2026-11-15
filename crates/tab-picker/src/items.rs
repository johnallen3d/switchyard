//! The ctrl-r work items float: the highlighted task's linked items, each
//! with its state, checks and title; what can be done to the highlighted
//! one is in the footer (see core's `item_actions`).
//! It is separate from ctrl-b's chooser, which only opens links, and opens
//! even for a task with one link.
//!
//! On the highlighted item: Enter or `o` opens it in the browser; `v`
//! reads its description and comments here, in a scrolling reader over
//! the float (a card, issue or PR; see core's `item_view`); `c`
//! types a one-line comment for a card, issue or PR; `x` closes a card or
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

use ratatui::widgets::{Block, Clear};
use switchyard_core::item_actions::{Action, Item};
use switchyard_core::item_view::{Comment, Thread};
use switchyard_ui::Palette;
use switchyard_ui::draw::{delete_word, str_width};
use switchyard_ui::popup;

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
    /// Typing a comment for the highlighted card, issue or PR.
    Comment(String),
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
    /// Opened on one item to read it (the Work view's ctrl-v): leaving the
    /// reader closes the float rather than showing its list.
    pub reader_only: bool,
    /// The reader's last drawn height and furthest scroll, for its keys.
    page: Cell<(usize, usize)>,
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

/// The widest a row's status column gets before it is clipped.
const STATUS_MAX: usize = 22;

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
            reader_only: false,
            page: Cell::new((0, 0)),
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
        let out = self.mode_key(k);
        // Back from reading one item: nothing else to show.
        if self.reader_only && self.mode == Mode::List && out == Outcome::Stay {
            return Outcome::Close;
        }
        out
    }

    fn mode_key(&mut self, k: KeyEvent) -> Outcome {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        match std::mem::replace(&mut self.mode, Mode::List) {
            Mode::List => self.list_key(k, ctrl),
            Mode::Comment(mut text) => {
                match k.code {
                    KeyCode::Esc => return Outcome::Stay,
                    KeyCode::Char('c') if ctrl => return Outcome::Stay,
                    KeyCode::Enter if text.trim().is_empty() => {}
                    KeyCode::Enter => {
                        self.mode = Mode::Confirm {
                            action: Action::Comment,
                            body: text.trim().to_string(),
                        };
                        return Outcome::Stay;
                    }
                    KeyCode::Char('u') if ctrl => text.clear(),
                    KeyCode::Char('w') if ctrl => delete_word(&mut text),
                    KeyCode::Backspace if alt || ctrl => delete_word(&mut text),
                    KeyCode::Backspace => {
                        text.pop();
                    }
                    KeyCode::Char(c) if !ctrl && !alt => text.push(c),
                    _ => {}
                }
                self.mode = Mode::Comment(text);
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
                    self.mode = Mode::Comment(body);
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
                    Mode::Comment(String::new())
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
                self.mode = Mode::Comment(String::new());
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
    pub fn failed(&mut self, action: Action, body: String) {
        self.mode = if action == Action::Comment {
            Mode::Comment(body)
        } else {
            Mode::List
        };
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
            Mode::Comment(_) => vec![("⏎", "review"), ("esc", "back"), ("F1", "keys")],
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

    /// A box over the list: one item a line with its name, its state and
    /// checks (see `status`) and its title (its URL until the tracker names
    /// it), in columns, then,
    /// while typing a comment or confirming, the comment or the question.
    /// Its keys are in the footer (see `hints`).
    pub fn draw(&self, f: &mut Frame, area: Rect, p: &Palette) {
        if let Mode::Read { thread, scroll } = &self.mode {
            self.draw_reader(f, area, p, thread, *scroll);
            return;
        }
        crate::backdrop(f, p);
        let pieces = |e: &Entry| status(&e.link, p);
        let joined = |ps: &[(String, Color)]| {
            ps.iter()
                .map(|(t, _)| t.as_str())
                .collect::<Vec<_>>()
                .join(" · ")
        };
        // Columns: name, status, then the title in what is left.
        let name_w = self
            .entries
            .iter()
            .map(|e| str_width(&e.name()))
            .max()
            .unwrap_or(0);
        let status_w = self
            .entries
            .iter()
            .map(|e| str_width(&joined(&pieces(e))))
            .max()
            .unwrap_or(0)
            .min(STATUS_MAX);
        let title = format!("Work items · {}", self.label);
        // Less the borders and padding.
        let text_w = usize::from(popup::LIST_WIDTH.min(area.width).saturating_sub(4)).max(1);
        // A confirmation's question, wrapped whole; a comment being typed.
        let foot = match &self.mode {
            Mode::Confirm { .. } => self
                .question()
                .map_or_else(Vec::new, |q| crate::preview::wrap(&q, text_w)),
            Mode::Comment(_) => vec![String::new()],
            _ => Vec::new(),
        };
        // A blank line between the list and what is under it.
        let below = if foot.is_empty() { 0 } else { foot.len() + 1 };
        let height = crate::cells(self.entries.len() + below).saturating_add(2);
        let r = popup::list_rect(area, height);
        let inner = popup::open(f, r, popup::block(p, &title));
        let rows = usize::from(inner.height).saturating_sub(below);
        for (i, e) in self.entries.iter().enumerate().take(rows) {
            let selected = i == self.selected;
            let row = Rect {
                y: inner.y + crate::cells(i),
                height: 1,
                ..inner
            };
            if selected {
                popup::select(f, p, inner, row);
            }
            let bg = if selected { p.overlay } else { p.base };
            let mut style = Style::new().fg(p.text).bg(bg);
            if selected {
                style = style.add_modifier(Modifier::BOLD);
            }
            let name = e.name();
            let pad = " ".repeat(name_w.saturating_sub(str_width(&name)));
            let mut spans = vec![Span::styled(format!("{name}{pad}"), style)];
            if status_w > 0 {
                let mut room = status_w;
                spans.push(Span::styled("  ", Style::new().bg(bg)));
                for (j, (text, fg)) in pieces(e).into_iter().enumerate() {
                    let sep = if j == 0 { "" } else { " · " };
                    for (text, fg) in [(sep.to_string(), p.muted), (text, fg)] {
                        let text = crate::preview::clip_width(&text, room);
                        room = room.saturating_sub(str_width(&text));
                        spans.push(Span::styled(text, Style::new().fg(fg).bg(bg)));
                    }
                }
                spans.push(Span::styled(" ".repeat(room), Style::new().bg(bg)));
            }
            // Its title, which Enter's URL says nothing of; the URL only
            // while the tracker hasn't said (see core's `titles`).
            let (about, fg) = match e.link.title.trim() {
                "" => (e.link.url.clone(), p.muted),
                t => (t.to_string(), p.text),
            };
            let room = usize::from(row.width).saturating_sub(Line::from(spans.clone()).width());
            spans.push(Span::styled(
                crate::preview::clip_width(&format!("  {about}"), room),
                Style::new().fg(fg).bg(bg),
            ));
            f.render_widget(Line::from(spans), row);
        }
        self.draw_foot(f, inner, foot, p);
    }

    /// The box's last lines: a confirmation's question, wrapped, or the
    /// comment being typed.
    fn draw_foot(&self, f: &mut Frame, inner: Rect, foot: Vec<String>, p: &Palette) {
        let top = inner.bottom().saturating_sub(crate::cells(foot.len()));
        let line = |i: usize| Rect {
            y: top + crate::cells(i),
            height: 1,
            ..inner
        };
        match &self.mode {
            Mode::Comment(text) => {
                if top < inner.y {
                    return;
                }
                let muted = Style::new().fg(p.muted);
                f.render_widget(
                    Line::from(vec![
                        Span::styled(format!("comment on {}: ", self.entry().name()), muted),
                        Span::styled(
                            text.clone(),
                            Style::new().fg(p.text).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled("▏", Style::new().fg(p.iris)),
                    ]),
                    line(0),
                );
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
        let lines = reader_lines(thread, text.width as usize, p);
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
fn reader_lines(thread: &Thread, width: usize, p: &Palette) -> Vec<Line<'static>> {
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
    let mut meta: Vec<String> = Vec::new();
    if !thread.state.is_empty() {
        meta.push(thread.state.clone());
    }
    if !thread.post.author.is_empty() {
        meta.push(format!("by {}", thread.post.author));
    }
    if !thread.post.at.is_empty() {
        meta.push(when(&thread.post.at));
    }
    let n = thread.comments.len();
    meta.push(match n {
        1 => "1 comment".into(),
        n => format!("{n} comments"),
    });
    out.extend(wrap(Line::styled(
        meta.join(" · "),
        Style::new().fg(p.muted),
    )));
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
        assert_eq!(i.mode, Mode::Comment(String::new()));
        typed(&mut i, "rx ready ");
        press(&mut i, KeyCode::Enter);
        assert_eq!(
            i.question().unwrap(),
            "Post a comment on card 1041: “rx ready”?"
        );
        // No goes back to the text, which a failed run also keeps.
        press(&mut i, KeyCode::Esc);
        assert_eq!(i.mode, Mode::Comment("rx ready".into()));
        press(&mut i, KeyCode::Enter);
        let Outcome::Run { action, body, .. } = press(&mut i, KeyCode::Enter) else {
            panic!("no run")
        };
        i.failed(action, body);
        assert_eq!(i.mode, Mode::Comment("rx ready".into()));
        // Escape leaves the comment, then the float.
        press(&mut i, KeyCode::Esc);
        assert_eq!(i.mode, Mode::List);
        assert_eq!(press(&mut i, KeyCode::Esc), Outcome::Close);
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
        assert_eq!(i.mode, Mode::Comment(String::new()));
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
        // Nothing known, nothing shown: no lookup yet, or an issue.
        let none: [String; 0] = [];
        assert_eq!(text(&card("", "", "")), none);
        assert_eq!(text(&link("issue", "github", "o/r#3", "u", "")), none);
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
        let text: Vec<String> = reader_lines(&t, 80, &Palette::load())
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
        let lines = reader_lines(&t, 80, &Palette::load());
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
