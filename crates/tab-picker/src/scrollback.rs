//! Scrollback over the highlighted agent's transcript, in the manner of a
//! terminal multiplexer's copy mode: a line cursor moved with vi keys,
//! literal search, and a line selection to copy.
//!
//! ctrl-v enters it, from the reply box too (which it returns to), as do the keys that scroll the transcript back. The
//! transcript stays live: with the cursor on the last line it follows new
//! output; anywhere above, the view stays where it is as lines arrive.
//!
//! - `j`/`k` or the arrows move a line; `ctrl-d`/`ctrl-u` half a page;
//!   `ctrl-f`/`ctrl-b` or the page keys a page; `{`/`}` to the blank line
//!   before or after (between turns); `g`/`G` or Home/End to the ends.
//! - `/` searches forward and `?` back, ignoring case unless the query has
//!   a capital; `n` repeats it, `N` the other way.
//! - `v`, `V` or Space starts a selection at the cursor (again: drops it);
//!   `y` or Enter copies it (or the cursor's line) and leaves.
//! - Escape drops the selection, then the search, then leaves; `q`, ctrl-c
//!   and ctrl-v again leave.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::preview::Pane;

/// What a key did.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Exit,
    /// Copy this text and leave.
    Copy(String),
}

/// A search query being typed after `/` or `?`.
pub struct Typing {
    pub forward: bool,
    pub query: String,
}

pub struct Scrollback {
    /// The transcript this is for; another highlight leaves scrollback.
    pub pane: Pane,
    /// The cursor's line and the first line shown, from the top.
    pub cursor: usize,
    pub top: usize,
    /// Where the selection started, if one is under way.
    pub anchor: Option<usize>,
    pub typing: Option<Typing>,
    /// The last search, and whether it went forward; its matches stay lit.
    pub found: Option<(String, bool)>,
    /// Lines as of the last sync, to follow new output from the end.
    len: usize,
    /// Why the last search or copy came to nothing, for the footer.
    pub note: Option<String>,
}

impl Scrollback {
    /// Scrollback over `len` lines with the cursor at the end.
    pub fn new(pane: Pane, len: usize, height: usize) -> Self {
        let mut s = Scrollback {
            pane,
            cursor: len.saturating_sub(1),
            top: 0,
            anchor: None,
            typing: None,
            found: None,
            len,
            note: None,
        };
        s.sync(len, height);
        s
    }

    /// Keep up with the transcript, now `len` lines in a view `height`
    /// high: follow it from the last line, else stay put, and keep the
    /// cursor in view.
    pub fn sync(&mut self, len: usize, height: usize) {
        let last = len.saturating_sub(1);
        if self.cursor + 1 >= self.len {
            self.cursor = last;
        }
        self.len = len;
        self.cursor = self.cursor.min(last);
        self.anchor = self.anchor.map(|a| a.min(last));
        let height = height.max(1);
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + height {
            self.top = self.cursor + 1 - height;
        }
        self.top = self.top.min(len.saturating_sub(height));
    }

    /// Lines in the transcript, as of the last sync.
    pub fn len(&self) -> usize {
        self.len
    }

    /// The selected lines, if any, as an inclusive range.
    pub fn selection(&self) -> Option<(usize, usize)> {
        self.anchor.map(|a| (a.min(self.cursor), a.max(self.cursor)))
    }

    /// The query whose matches are lit: the one being typed, else the last.
    pub fn query(&self) -> Option<&str> {
        match &self.typing {
            Some(t) if !t.query.is_empty() => Some(&t.query),
            Some(_) => None,
            None => self.found.as_ref().map(|(q, _)| q.as_str()),
        }
    }

    /// Act on `k` over `lines`, the transcript as text, in a view `height`
    /// high.
    pub fn key(&mut self, k: KeyEvent, lines: &[String], height: usize) -> Outcome {
        self.note = None;
        if self.typing.is_some() {
            self.type_key(k, lines);
            return Outcome::Stay;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let page = height.max(2) as isize - 1;
        let half = (height as isize / 2).max(1);
        let last = lines.len().saturating_sub(1);
        match k.code {
            KeyCode::Char('c' | 'v') if ctrl => return Outcome::Exit,
            KeyCode::Char('d') if ctrl => self.go(half, last),
            KeyCode::Char('u') if ctrl => self.go(-half, last),
            KeyCode::Char('f') if ctrl => self.go(page, last),
            KeyCode::Char('b') if ctrl => self.go(-page, last),
            KeyCode::PageDown => self.go(page, last),
            KeyCode::PageUp => self.go(-page, last),
            KeyCode::Char('j') | KeyCode::Down => self.go(1, last),
            KeyCode::Char('k') | KeyCode::Up => self.go(-1, last),
            KeyCode::Char('g') | KeyCode::Home => self.cursor = 0,
            KeyCode::Char('G') | KeyCode::End => self.cursor = last,
            KeyCode::Char('}') => self.cursor = (self.cursor + 1..=last).find(|&i| lines[i].is_empty()).unwrap_or(last),
            KeyCode::Char('{') => self.cursor = (0..self.cursor).rev().find(|&i| lines[i].is_empty()).unwrap_or(0),
            KeyCode::Char('/') => self.typing = Some(Typing { forward: true, query: String::new() }),
            KeyCode::Char('?') => self.typing = Some(Typing { forward: false, query: String::new() }),
            KeyCode::Char('n') => self.repeat(lines, false),
            KeyCode::Char('N') => self.repeat(lines, true),
            KeyCode::Char('v' | 'V' | ' ') => {
                self.anchor = if self.anchor.is_some() { None } else { Some(self.cursor) };
            }
            KeyCode::Char('y') | KeyCode::Enter => {
                let (a, b) = self.selection().unwrap_or((self.cursor, self.cursor));
                self.anchor = None;
                return match lines.get(a..=b.min(last)) {
                    Some(sel) if !sel.is_empty() => Outcome::Copy(copy_text(sel)),
                    _ => Outcome::Exit,
                };
            }
            KeyCode::Esc if self.anchor.is_some() => self.anchor = None,
            KeyCode::Esc if self.found.is_some() => self.found = None,
            KeyCode::Esc | KeyCode::Char('q') => return Outcome::Exit,
            _ => {}
        }
        Outcome::Stay
    }

    fn go(&mut self, delta: isize, last: usize) {
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
    }

    fn type_key(&mut self, k: KeyEvent, lines: &[String]) {
        let Some(t) = &mut self.typing else { return };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc => self.typing = None,
            KeyCode::Char('c') if ctrl => self.typing = None,
            KeyCode::Char('u') if ctrl => t.query.clear(),
            KeyCode::Backspace if t.query.is_empty() => self.typing = None,
            KeyCode::Backspace => {
                t.query.pop();
            }
            KeyCode::Enter => {
                let t = self.typing.take().unwrap();
                if !t.query.is_empty() {
                    self.found = Some((t.query, t.forward));
                    self.repeat(lines, false);
                }
            }
            KeyCode::Char(c) if !ctrl => t.query.push(c),
            _ => {}
        }
    }

    /// Move to the next match of the last search, the other way if
    /// `reverse`, wrapping around.
    fn repeat(&mut self, lines: &[String], reverse: bool) {
        let Some((query, forward)) = &self.found else { return };
        match find(lines, query, self.cursor, *forward != reverse) {
            Some(i) => self.cursor = i,
            None => self.note = Some(format!("not found: {query}")),
        }
    }
}

/// The next line after (or before) `from` holding `query`, wrapping round.
fn find(lines: &[String], query: &str, from: usize, forward: bool) -> Option<usize> {
    let n = lines.len();
    (1..=n)
        .map(|step| if forward { (from + step) % n } else { (from + n * 2 - step % n) % n })
        .find(|&i| !matches(&lines[i], query).is_empty())
}

/// Whether a search for `query` ignores case: unless it has a capital.
fn fold(query: &str) -> bool {
    !query.chars().any(char::is_uppercase)
}

/// Where `query` appears in `text`, as char ranges.
pub fn matches(text: &str, query: &str) -> Vec<(usize, usize)> {
    if query.is_empty() {
        return Vec::new();
    }
    let fold = fold(query);
    let norm = |c: char| if fold { c.to_lowercase().next().unwrap_or(c) } else { c };
    let hay: Vec<char> = text.chars().map(norm).collect();
    let needle: Vec<char> = query.chars().map(norm).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if hay[i..i + needle.len()] == needle[..] {
            out.push((i, i + needle.len()));
            i += needle.len();
        } else {
            i += 1;
        }
    }
    out
}

/// A drawn line as text.
pub fn line_text(line: &Line) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// Lines as copied: without the two-column gutter of turn marks.
fn copy_text(lines: &[String]) -> String {
    let body: Vec<String> = lines.iter().map(|l| l.chars().skip(2).collect::<String>().trim_end().to_string()).collect();
    body.join("\n")
}

/// `line` with `style` patched over the chars in `ranges`.
pub fn mark(line: Line<'static>, ranges: &[(usize, usize)], style: Style) -> Line<'static> {
    if ranges.is_empty() {
        return line;
    }
    let lit = |i: usize| ranges.iter().any(|&(a, b)| i >= a && i < b);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut at = 0;
    for span in line.spans {
        let mut run = String::new();
        let mut run_lit = None;
        for c in span.content.chars() {
            let l = lit(at);
            if run_lit.is_some_and(|r| r != l) {
                let s = if run_lit == Some(true) { span.style.patch(style) } else { span.style };
                spans.push(Span::styled(std::mem::take(&mut run), s));
            }
            run_lit = Some(l);
            run.push(c);
            at += 1;
        }
        if !run.is_empty() {
            let s = if run_lit == Some(true) { span.style.patch(style) } else { span.style };
            spans.push(Span::styled(run, s));
        }
    }
    Line { spans, ..line }
}

/// Text for the clipboard, as an OSC 52 escape.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", STANDARD.encode(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane() -> Pane {
        Pane { target: "-".into(), session: "s".into(), window: "w".into() }
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn lines() -> Vec<String> {
        ["› hello", "", "● first answer", "  goes on", "", "› Again", "", "● second"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn follows_from_the_end_and_stays_put_above_it() {
        let mut s = Scrollback::new(pane(), 8, 4);
        assert_eq!((s.cursor, s.top), (7, 4));
        s.sync(10, 4);
        assert_eq!((s.cursor, s.top), (9, 6));
        s.key(key('k'), &lines(), 4);
        s.sync(10, 4);
        let (cursor, top) = (s.cursor, s.top);
        s.sync(14, 4);
        assert_eq!((s.cursor, s.top), (cursor, top));
    }

    #[test]
    fn moves_by_blocks_and_ends() {
        let l = lines();
        let mut s = Scrollback::new(pane(), l.len(), 4);
        s.key(key('{'), &l, 4);
        assert_eq!(s.cursor, 6);
        s.key(key('{'), &l, 4);
        assert_eq!(s.cursor, 4);
        s.key(key('g'), &l, 4);
        assert_eq!(s.cursor, 0);
        s.key(key('}'), &l, 4);
        assert_eq!(s.cursor, 1);
        s.key(key('G'), &l, 4);
        assert_eq!(s.cursor, 7);
    }

    #[test]
    fn searches_with_smart_case_and_wraps() {
        let l = lines();
        let mut s = Scrollback::new(pane(), l.len(), 4);
        s.key(key('?'), &l, 4);
        for c in "again".chars() {
            s.key(key(c), &l, 4);
        }
        s.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &l, 4);
        assert_eq!(s.cursor, 5);
        // A capital makes it match case.
        s.key(key('/'), &l, 4);
        s.key(key('H'), &l, 4);
        s.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &l, 4);
        assert_eq!(s.cursor, 5);
        assert_eq!(s.note.as_deref(), Some("not found: H"));
        s.key(key('/'), &l, 4);
        s.key(key('o'), &l, 4);
        s.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &l, 4);
        assert_eq!(s.cursor, 7);
        s.key(key('n'), &l, 4);
        assert_eq!(s.cursor, 0);
        s.key(key('n'), &l, 4);
        assert_eq!(s.cursor, 3);
        s.key(key('N'), &l, 4);
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn copies_the_selection_without_the_gutter() {
        let l = lines();
        let mut s = Scrollback::new(pane(), l.len(), 4);
        s.cursor = 2;
        s.key(key('v'), &l, 4);
        s.key(key('j'), &l, 4);
        assert_eq!(s.key(key('y'), &l, 4), Outcome::Copy("first answer\ngoes on".into()));
        // Escape drops the selection before it leaves.
        s.key(key('v'), &l, 4);
        assert_eq!(s.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &l, 4), Outcome::Stay);
        assert_eq!(s.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &l, 4), Outcome::Exit);
    }

    #[test]
    fn marks_matches_across_spans() {
        let line = Line::from(vec![Span::raw("ab"), Span::raw("cd")]);
        let style = Style::new().add_modifier(ratatui::style::Modifier::REVERSED);
        let out = mark(line, &matches("abcd", "bc"), style);
        let parts: Vec<_> = out.spans.iter().map(|s| (s.content.to_string(), s.style == style)).collect();
        assert_eq!(parts, [("a".into(), false), ("b".into(), true), ("c".into(), true), ("d".into(), false)]);
    }

    #[test]
    fn copies_by_osc52() {
        assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x07");
        assert_eq!(osc52("hey"), "\x1b]52;c;aGV5\x07");
    }
}
