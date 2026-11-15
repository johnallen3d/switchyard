//! The ctrl-r work items float: the highlighted task's linked items, each
//! with its title and what can be done to it (see core's `item_actions`).
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
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::cell::Cell;

use ratatui::widgets::{Block, Borders, Clear};
use switchyard_core::item_actions::{Action, Item};
use switchyard_core::item_view::{Comment, Thread};
use switchyard_ui::Palette;
use switchyard_ui::draw::{delete_word, str_width};

use crate::model::{Link, Tone};

/// A linked item, and what actions it takes (None: it can only be opened).
pub struct Entry {
    pub link: Link,
    pub item: Option<Item>,
}

impl Entry {
    fn actions(&self) -> Vec<Action> {
        self.item.as_ref().map(Item::actions).unwrap_or_default()
    }

    /// How a note or confirmation names it.
    fn name(&self) -> String {
        self.item
            .as_ref()
            .map_or_else(|| self.link.short(), Item::name)
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

/// The widest the list's box grows for its rows, in columns.
const MAX_WIDTH: usize = 100;

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
                Entry { link, item }
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

    /// A small box over the list: one item a line with its state, the
    /// actions it takes and its title (its URL until the tracker names
    /// it), then a line for the mode.
    pub fn draw(&self, f: &mut Frame, area: Rect, p: &Palette) {
        if let Mode::Read { thread, scroll } = &self.mode {
            self.draw_reader(f, area, p, thread, *scroll);
            return;
        }
        let row_text = |e: &Entry| {
            let acts: Vec<&str> = e.actions().into_iter().map(|a| action_key(a).0).collect();
            // Its title, which Enter's URL says nothing of; the URL only
            // while the tracker hasn't said (see core's `titles`).
            let about = match e.link.title.trim() {
                "" => (e.link.url.clone(), false),
                t => (t.to_string(), true),
            };
            (e.link.short(), e.link.badge_state(), acts.join(" "), about)
        };
        let title = format!(" Work items · {} ", self.label);
        let want = self
            .entries
            .iter()
            .map(|e| {
                let (short, state, acts, (about, _)) = row_text(e);
                // A long title is cut at the box's edge rather than widening it.
                (str_width(&short)
                    + state.as_ref().map_or(0, |(s, _)| str_width(s) + 1)
                    + str_width(&acts)
                    + str_width(&about)
                    + 8)
                .min(MAX_WIDTH)
            })
            .chain([
                str_width(&title) + 4,
                self.question()
                    .map_or(0, |q| (str_width(&q) + 4).min(MAX_WIDTH)),
                56,
            ])
            .max()
            .unwrap_or(0);
        let width = crate::cells(want).saturating_add(2).min(area.width);
        // A confirmation's question, wrapped whole, above its keys.
        let asked = self.question().map_or_else(Vec::new, |q| {
            crate::preview::wrap(&q, usize::from(width.saturating_sub(4)).max(1))
        });
        let height = crate::cells(self.entries.len() + asked.len())
            .saturating_add(4)
            .min(area.height);
        let r = crate::centered(area, width, height);
        f.render_widget(Clear, r);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(p.iris))
            .style(Style::new().bg(p.base))
            .title(Span::styled(title, Style::new().fg(p.text)));
        let inner = block.inner(r);
        f.render_widget(block, r);
        let rows = usize::from(inner.height).saturating_sub(2 + asked.len());
        for (i, e) in self.entries.iter().enumerate().take(rows) {
            let (short, state, acts, (about, titled)) = row_text(e);
            let selected = i == self.selected;
            let bg = if selected { p.overlay } else { p.base };
            let row = Rect {
                y: inner.y + crate::cells(i),
                height: 1,
                ..inner
            };
            f.buffer_mut().set_style(row, Style::new().bg(bg));
            let edge = if selected {
                Span::styled("▎", Style::new().fg(p.iris).bg(bg))
            } else {
                Span::styled(" ", Style::new().bg(bg))
            };
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
            if !acts.is_empty() {
                spans.push(Span::styled(
                    format!("  [{acts}]"),
                    Style::new().fg(p.gold).bg(bg),
                ));
            }
            let fg = if titled { p.text } else { p.muted };
            let room = usize::from(row.width).saturating_sub(Line::from(spans.clone()).width());
            spans.push(Span::styled(
                crate::preview::clip_width(&format!("  {about}"), room),
                Style::new().fg(fg).bg(bg),
            ));
            f.render_widget(Line::from(spans), row);
        }
        self.draw_foot(f, inner, asked, p);
    }

    /// The box's last lines: a confirmation's question, wrapped, then the
    /// mode line.
    fn draw_foot(&self, f: &mut Frame, inner: Rect, asked: Vec<String>, p: &Palette) {
        if inner.height < 2 {
            return;
        }
        let bottom = Rect {
            y: inner.bottom() - 1,
            height: 1,
            ..inner
        };
        // Red only for what can't be undone.
        let fg = match self.mode {
            Mode::Confirm {
                action: Action::Close,
                ..
            } => p.love,
            _ => p.text,
        };
        let above = bottom.y.saturating_sub(crate::cells(asked.len()));
        for (i, q) in asked.into_iter().enumerate() {
            let y = above + crate::cells(i);
            if y >= inner.y {
                let row = Rect { y, ..bottom };
                let style = Style::new().fg(fg).add_modifier(Modifier::BOLD);
                f.render_widget(Span::styled(format!(" {q}"), style), row);
            }
        }
        f.render_widget(self.mode_line(p), bottom);
    }

    /// The box's bottom line for its mode: the list's keys, the comment
    /// being typed, or a confirmation's keys.
    fn mode_line(&self, p: &Palette) -> Line<'static> {
        let key = |k: &str| {
            Span::styled(
                k.to_string(),
                Style::new().fg(p.text).add_modifier(Modifier::BOLD),
            )
        };
        let muted = |s: &str| Span::styled(s.to_string(), Style::new().fg(p.muted));
        match &self.mode {
            Mode::List => {
                let mut spans = vec![Span::raw(" ")];
                for (i, (k, d)) in self
                    .hints()
                    .into_iter()
                    .filter(|(k, _)| *k != "?")
                    .enumerate()
                {
                    if i > 0 {
                        spans.push(muted(" · "));
                    }
                    spans.extend([key(k), muted(&format!(" {d}"))]);
                }
                Line::from(spans)
            }
            Mode::Comment(text) => Line::from(vec![
                muted(&format!(" comment on {}: ", self.entry().name())),
                Span::styled(
                    text.clone(),
                    Style::new().fg(p.text).add_modifier(Modifier::BOLD),
                ),
                Span::styled("▏", Style::new().fg(p.iris)),
            ]),
            Mode::Read { .. } => Line::default(),
            // The question is drawn above, wrapped (see `draw_foot`).
            Mode::Confirm { .. } => Line::from(vec![
                Span::raw(" "),
                key("⏎ y"),
                muted(" yes · "),
                key("esc"),
                muted(" no"),
            ]),
        }
    }
    /// The reader: a large box over the picker with the item's title,
    /// state and opening post, then each comment under a header naming
    /// its author and time, wrapped to the box and scrolled.
    fn draw_reader(&self, f: &mut Frame, area: Rect, p: &Palette, thread: &Thread, scroll: usize) {
        let width = area.width.saturating_sub(4).min(100);
        let height = area.height.saturating_sub(2);
        let r = crate::centered(area, width, height);
        // Blank the whole pane, so nothing under it shows round the box.
        f.render_widget(Clear, area);
        f.render_widget(Block::new().style(Style::new().bg(p.base)), area);
        let title = format!(" {} · {} ", self.entry().name(), self.label);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(p.iris))
            .style(Style::new().bg(p.base))
            .title(Span::styled(title, Style::new().fg(p.text)));
        let inner = block.inner(r);
        f.render_widget(block, r);
        if inner.height < 2 || inner.width < 4 {
            return;
        }
        // A column each side, and the bottom row for keys.
        let text = Rect {
            x: inner.x + 1,
            width: inner.width - 2,
            height: inner.height - 1,
            ..inner
        };
        let lines = reader_lines(thread, text.width as usize, p);
        let rows = text.height as usize;
        let max = lines.len().saturating_sub(rows);
        self.page.set((rows, max));
        let scroll = scroll.min(max);
        for (i, line) in lines.into_iter().skip(scroll).take(rows).enumerate() {
            let row = Rect {
                y: text.y + crate::cells(i),
                height: 1,
                ..text
            };
            f.render_widget(line, row);
        }
        let bottom = Rect {
            y: inner.bottom() - 1,
            height: 1,
            ..inner
        };
        let key = |k: &str| {
            Span::styled(
                k.to_string(),
                Style::new().fg(p.text).add_modifier(Modifier::BOLD),
            )
        };
        let muted = |s: &str| Span::styled(s.to_string(), Style::new().fg(p.muted));
        let mut spans = vec![Span::raw(" ")];
        for (i, (k, d)) in self
            .hints()
            .into_iter()
            .filter(|(k, _)| *k != "?")
            .enumerate()
        {
            if i > 0 {
                spans.push(muted(" · "));
            }
            spans.extend([key(k), muted(&format!(" {d}"))]);
        }
        if let Some(pct) = (scroll * 100).checked_div(max) {
            spans.push(muted(&format!("   {pct}%")));
        }
        f.render_widget(Line::from(spans), bottom);
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
        assert_eq!(i.question().unwrap(), "Mark PR o/r#57 ready for review?");
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
            Outcome::Refuse("PR o/r#57: x closes cards and issues, not PRs".into())
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
            Outcome::Refuse("PR o/r#1 isn't a draft".into())
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
        assert_eq!(i.question().unwrap(), "Close issue o/r#3?");
        press(&mut i, KeyCode::Esc);
        press(&mut i, KeyCode::Char('c'));
        typed(&mut i, "on it");
        press(&mut i, KeyCode::Enter);
        assert_eq!(
            i.question().unwrap(),
            "Post a comment on issue o/r#3: “on it”?"
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
