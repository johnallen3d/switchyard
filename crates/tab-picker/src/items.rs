//! The ctrl-r work items float: the highlighted task's linked items, with
//! what can be done to each (see core's `item_actions`). It is separate
//! from ctrl-b's chooser, which only opens links, and opens even for a
//! task with one link.
//!
//! On the highlighted item: Enter or `o` opens it in the browser; `c`
//! types a one-line comment for a card; `x` closes a card; `r` marks a
//! draft PR ready for review. Nothing mutates without its explicit key and
//! then a confirmation naming the item (Enter or `y` runs it, anything
//! else goes back). Escape, ctrl-c or ctrl-r again close the float.
//!
//! This holds the float's state and keys; standalone_key runs what they
//! ask for (see `Outcome`) and shows the outcome in the footer.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear};
use switchyard_core::item_actions::{Action, Item};
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
        self.item.as_ref().map_or_else(|| self.link.short(), Item::name)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    List,
    /// Typing a comment for the highlighted card.
    Comment(String),
    /// Asking whether to run `action` (with a comment's `body`).
    Confirm { action: Action, body: String },
}

pub struct Items {
    /// The task's tab, as (session_id, window_id), which stays highlighted.
    pub key: (String, String),
    pub label: String,
    /// The task's profile, for gh.
    pub profile: String,
    pub entries: Vec<Entry>,
    pub selected: usize,
    pub mode: Mode,
}

/// What a key asks of the picker.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Close,
    Help,
    /// Open the URL in the browser here.
    Open(String),
    /// Run the confirmed action on the item.
    Run { action: Action, item: Item, body: String },
    /// Why the key does nothing here, for the footer.
    Refuse(String),
}

/// The key that asks for an action, as the float names it.
fn action_key(a: Action) -> (&'static str, &'static str) {
    match a {
        Action::Comment => ("c", "comment"),
        Action::Close => ("x", "close card"),
        Action::Ready => ("r", "mark ready"),
    }
}

impl Items {
    /// The float over a task's links (those with a URL, most likely first;
    /// see model's `chooser_links`), or None when it has none.
    pub fn new(key: (String, String), label: String, profile: String, links: Vec<Link>) -> Option<Items> {
        let entries: Vec<Entry> = links
            .into_iter()
            .map(|link| {
                let item = Item::from_link(&link.kind, &link.system, &link.reference, &link.url, &link.state);
                Entry { link, item }
            })
            .collect();
        (!entries.is_empty()).then_some(Items { key, label, profile, entries, selected: 0, mode: Mode::List })
    }

    fn entry(&self) -> &Entry {
        &self.entries[self.selected]
    }

    pub fn key(&mut self, k: KeyEvent) -> Outcome {
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
                        self.mode = Mode::Confirm { action: Action::Comment, body: text.trim().to_string() };
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
                me.mode = if a == Action::Comment { Mode::Comment(String::new()) } else { Mode::Confirm { action: a, body: String::new() } };
                return Outcome::Stay;
            }
            let why = match a {
                Action::Comment | Action::Close if e.link.kind == "card" => format!("{}: its URL names no Fizzy account", e.name()),
                Action::Comment => format!("{} isn't a card: c comments on Fizzy cards", e.name()),
                Action::Close => format!("{} isn't a card: x closes Fizzy cards", e.name()),
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
            KeyCode::Char('c') => want(Action::Comment, self),
            KeyCode::Char('x') if !ctrl => want(Action::Close, self),
            KeyCode::Char('r') => want(Action::Ready, self),
            _ => Outcome::Stay,
        }
    }

    fn step(&mut self, to: usize) -> Outcome {
        self.selected = to;
        Outcome::Stay
    }

    /// After a failed run: back to the list, or to a comment's text so it
    /// isn't lost.
    pub fn failed(&mut self, action: Action, body: String) {
        self.mode = if action == Action::Comment { Mode::Comment(body) } else { Mode::List };
    }

    /// The footer's hints while the float is open.
    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        match &self.mode {
            Mode::List => {
                let mut h = vec![("⏎", "open")];
                h.extend(self.entry().actions().into_iter().map(action_key));
                h.extend([("esc", "close"), ("?", "keys")]);
                h
            }
            Mode::Comment(_) => vec![("⏎", "review"), ("esc", "back"), ("F1", "keys")],
            Mode::Confirm { .. } => vec![("⏎ y", "yes"), ("esc", "no")],
        }
    }

    /// What the float's bottom line asks, for confirmations: "Close card
    /// 1041?" or "Post a comment on card 1041: “…”?".
    pub fn question(&self) -> Option<String> {
        let Mode::Confirm { action, body } = &self.mode else { return None };
        let name = self.entry().name();
        Some(match action {
            Action::Comment => format!("Post a comment on {name}: “{body}”?"),
            Action::Close => format!("Close {name}?"),
            Action::Ready => format!("Mark {name} ready for review?"),
        })
    }

    /// A small box over the list: one item a line with its state, the
    /// actions it takes and its URL, then a line for the mode.
    pub fn draw(&self, f: &mut Frame, area: Rect, p: &Palette) {
        let row_text = |e: &Entry| {
            let acts: Vec<&str> = e.actions().into_iter().map(|a| action_key(a).0).collect();
            (e.link.short(), e.link.badge_state(), acts.join(" "), e.link.url.clone())
        };
        let title = format!(" Work items · {} ", self.label);
        let want = self
            .entries
            .iter()
            .map(|e| {
                let (short, state, acts, url) = row_text(e);
                str_width(&short) + state.map_or(0, |(s, _)| str_width(s) + 1) + str_width(&acts) + str_width(&url) + 8
            })
            .chain([str_width(&title) + 4, self.question().map_or(0, |q| str_width(&q) + 16), 56])
            .max()
            .unwrap_or(0);
        let w = (want as u16 + 2).min(area.width);
        let h = (self.entries.len() as u16 + 4).min(area.height);
        let r = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
        f.render_widget(Clear, r);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(p.iris))
            .style(Style::new().bg(p.base))
            .title(Span::styled(title, Style::new().fg(p.text)));
        let inner = block.inner(r);
        f.render_widget(block, r);
        let rows = inner.height.saturating_sub(2) as usize;
        for (i, e) in self.entries.iter().enumerate().take(rows) {
            let (short, state, acts, url) = row_text(e);
            let selected = i == self.selected;
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
            if !acts.is_empty() {
                spans.push(Span::styled(format!("  [{acts}]"), Style::new().fg(p.gold).bg(bg)));
            }
            spans.push(Span::styled(format!("  {url}"), Style::new().fg(p.muted).bg(bg)));
            f.render_widget(Line::from(spans), row);
        }
        if inner.height < 2 {
            return;
        }
        let bottom = Rect { y: inner.bottom() - 1, height: 1, ..inner };
        let key = |k: &str| Span::styled(k.to_string(), Style::new().fg(p.text).add_modifier(Modifier::BOLD));
        let muted = |s: &str| Span::styled(s.to_string(), Style::new().fg(p.muted));
        let line = match &self.mode {
            Mode::List => {
                let mut spans = vec![Span::raw(" ")];
                for (i, (k, d)) in self.hints().into_iter().filter(|(k, _)| *k != "?").enumerate() {
                    if i > 0 {
                        spans.push(muted(" · "));
                    }
                    spans.extend([key(k), muted(&format!(" {d}"))]);
                }
                Line::from(spans)
            }
            Mode::Comment(text) => Line::from(vec![
                muted(&format!(" comment on {}: ", self.entry().name())),
                Span::styled(text.clone(), Style::new().fg(p.text).add_modifier(Modifier::BOLD)),
                Span::styled("▏", Style::new().fg(p.iris)),
            ]),
            Mode::Confirm { .. } => Line::from(vec![
                Span::styled(format!(" {} ", self.question().unwrap_or_default()), Style::new().fg(p.love).add_modifier(Modifier::BOLD)),
                key("⏎"),
                muted(" yes · "),
                key("esc"),
                muted(" no"),
            ]),
        };
        f.render_widget(line, bottom);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(kind: &str, system: &str, reference: &str, url: &str, state: &str) -> Link {
        Link { kind: kind.into(), system: system.into(), reference: reference.into(), url: url.into(), state: state.into(), ..Link::default() }
    }

    fn items() -> Items {
        let links = vec![
            link("pr", "github", "o/r#57", "https://github.com/o/r/pull/57", "draft"),
            link("card", "fizzy", "1041", "https://app.fizzy.do/6284043/cards/1041", ""),
            link("issue", "github", "o/r#3", "https://github.com/o/r/issues/3", ""),
        ];
        Items::new(("s".into(), "w".into()), "demo/task".into(), "personal".into(), links).unwrap()
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
        assert!(Items::new(("s".into(), "w".into()), "t".into(), "personal".into(), vec![]).is_none());
    }

    #[test]
    fn enter_opens_and_nothing_mutates_without_a_confirmation() {
        let mut i = items();
        assert_eq!(press(&mut i, KeyCode::Enter), Outcome::Open("https://github.com/o/r/pull/57".into()));
        // r on the draft PR asks first, naming it.
        assert_eq!(press(&mut i, KeyCode::Char('r')), Outcome::Stay);
        assert_eq!(i.question().unwrap(), "Mark PR o/r#57 ready for review?");
        // Any other key goes back without running.
        assert_eq!(press(&mut i, KeyCode::Char('q')), Outcome::Stay);
        assert_eq!(i.mode, Mode::List);
        press(&mut i, KeyCode::Char('r'));
        let pr = Item::Pr { repo: "o/r".into(), number: "57".into(), draft: true };
        assert_eq!(press(&mut i, KeyCode::Char('y')), Outcome::Run { action: Action::Ready, item: pr, body: String::new() });
    }

    #[test]
    fn actions_refuse_items_they_dont_fit() {
        let mut i = items();
        assert!(matches!(press(&mut i, KeyCode::Char('x')), Outcome::Refuse(w) if w.contains("isn't a card")));
        press(&mut i, KeyCode::Down);
        assert!(matches!(press(&mut i, KeyCode::Char('r')), Outcome::Refuse(w) if w.contains("isn't a PR")));
        press(&mut i, KeyCode::Down);
        assert!(matches!(press(&mut i, KeyCode::Char('c')), Outcome::Refuse(w) if w == "#3 isn't a card: c comments on Fizzy cards"));
        assert_eq!(i.mode, Mode::List);
        let mut open = Items::new(("s".into(), "w".into()), "t".into(), "p".into(), vec![link("pr", "github", "o/r#1", "u", "open")]).unwrap();
        assert_eq!(press(&mut open, KeyCode::Char('r')), Outcome::Refuse("PR o/r#1 isn't a draft".into()));
    }

    #[test]
    fn close_card_confirms_naming_the_card() {
        let mut i = items();
        press(&mut i, KeyCode::Down);
        assert_eq!(i.hints()[1..3], [("c", "comment"), ("x", "close card")]);
        press(&mut i, KeyCode::Char('x'));
        assert_eq!(i.question().unwrap(), "Close card 1041?");
        assert_eq!(press(&mut i, KeyCode::Esc), Outcome::Stay);
        assert_eq!(i.mode, Mode::List);
        press(&mut i, KeyCode::Char('x'));
        let card = Item::Card { account: "6284043".into(), number: "1041".into() };
        assert_eq!(press(&mut i, KeyCode::Enter), Outcome::Run { action: Action::Close, item: card, body: String::new() });
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
        assert_eq!(i.question().unwrap(), "Post a comment on card 1041: “rx ready”?");
        // No goes back to the text, which a failed run also keeps.
        press(&mut i, KeyCode::Esc);
        assert_eq!(i.mode, Mode::Comment("rx ready".into()));
        press(&mut i, KeyCode::Enter);
        let Outcome::Run { action, body, .. } = press(&mut i, KeyCode::Enter) else { panic!("no run") };
        i.failed(action, body);
        assert_eq!(i.mode, Mode::Comment("rx ready".into()));
        // Escape leaves the comment, then the float.
        press(&mut i, KeyCode::Esc);
        assert_eq!(i.mode, Mode::List);
        assert_eq!(press(&mut i, KeyCode::Esc), Outcome::Close);
    }
}
