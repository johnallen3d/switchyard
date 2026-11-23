//! Scrollback over the highlighted agent's transcript, in the manner of a
//! terminal multiplexer's copy mode: a cursor moved with vi keys, literal
//! search, and a selection, of lines or of exact text, to copy.
//!
//! ctrl-v enters it, from the reply box too (which it returns to), as do the keys that scroll the transcript back. The
//! transcript stays live: with the cursor on the last line it follows new
//! output; anywhere above, the view stays where it is as lines arrive.
//!
//! - `j`/`k` or the arrows move a line; `ctrl-d`/`ctrl-u` half a page;
//!   `ctrl-f`/`ctrl-b` or the page keys a page; `{`/`}` to the blank line
//!   before or after (between turns); `g`/`G` or Home/End to the ends
//!   (`NG` to line N).
//! - Within and across lines, as vi does: `h`/`l` a char; `w`/`b`/`e` by
//!   words and `W`/`B`/`E` by blank-separated WORDs; `0`, `^` and `$` to the
//!   line's start, first non-blank and end; `f`/`F`/`t`/`T` and a char to
//!   it on the line, `;` and `,` again. A count before a motion repeats it.
//! - `]`/`[` jump to the next or previous section the text marks (see
//!   `Nav`): a transcript's prompts from John, a diff's files, a work
//!   item's comments. Enter on a row that
//!   names one, such as a file in a diff's list, jumps to it.
//! - `/` searches forward and `?` back, ignoring case unless the query has
//!   a capital; `n` repeats it, `N` the other way.
//! - `v` starts a selection of text at the cursor, `V` or Space one of
//!   lines (again: drops it, the other: switches to it). `iw`, `aw`, `i"`,
//!   `i(` and the like select a text object.
//! - `y` copies the selection, or with a motion or text object (`yw`,
//!   `ye`, `y$`, `yiw`, `yi"`, `yt)`) the text it covers; `yy`, `Y` or
//!   Enter (elsewhere than on a jump) copy the cursor's line. Lines copy as
//!   the markdown they were drawn from, text as drawn. A copy leaves, in
//!   Inbox for the reply box, to paste it in.
//! - Escape drops a pending `y`, the selection, then the search, then
//!   leaves; `q`, ctrl-c and ctrl-v again leave. Tab, in Inbox, leaves for
//!   the reply box.

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
    /// Copy these lines (an inclusive range) and leave; the picker copies
    /// the markdown they were drawn from (see `Preview::source`).
    Copy(usize, usize),
    /// Copy this text, exactly as drawn, and leave.
    Text(String),
}

/// A place in the text: a line and a char within it.
pub type Pos = (usize, usize);

/// Where a selection started, and whether it takes whole lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub line: usize,
    pub col: usize,
    pub linewise: bool,
}

/// A key that waits for the next: `f`/`F`/`t`/`T` for the char to find,
/// `i`/`a` for the text object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    Find(char),
    Object { inner: bool },
}

/// What a motion covers when `y` takes it, as in vi: up to the char it
/// lands on, through it, or whole lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Exclusive,
    Inclusive,
    Linewise,
}

/// What vi's word motions tell apart: an empty line, blanks, punctuation
/// and word chars (letters, digits and `_`; for WORDs, all non-blanks).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Empty,
    Blank,
    Punct,
    Word,
}

/// Where the text's sections start, for `]` and `[`, and the rows that
/// name a section, for Enter: (row, the line it jumps to). `what` names the
/// sections for the footer (`files`, `comments`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Nav {
    pub marks: Vec<usize>,
    pub jumps: Vec<(usize, usize)>,
    pub what: &'static str,
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
    /// The cursor's char in its line, as last set: lines it moves to that
    /// are shorter hold it at their end (`usize::MAX` keeps it there).
    pub col: usize,
    /// Where the selection started, if one is under way.
    pub anchor: Option<Anchor>,
    /// Whether `y` waits for a motion or text object.
    yank: bool,
    pending: Option<Pending>,
    /// The count typed so far, 0 for none.
    count: usize,
    /// The last `f`/`F`/`t`/`T` and its char, for `;` and `,`.
    last_find: Option<(char, char)>,
    pub typing: Option<Typing>,
    /// The last search, and whether it went forward; its matches stay lit.
    pub found: Option<(String, bool)>,
    /// Lines as of the last sync, to follow new output from the end.
    len: usize,
    /// Why the last search or copy came to nothing, for the footer.
    pub note: Option<String>,
    /// The text's sections, as the picker last gave them.
    pub nav: Nav,
}

impl Scrollback {
    /// Scrollback over `len` lines with the cursor at the end.
    pub fn new(pane: Pane, len: usize, height: usize) -> Self {
        let mut s = Scrollback {
            pane,
            cursor: len.saturating_sub(1),
            top: 0,
            col: 0,
            anchor: None,
            yank: false,
            pending: None,
            count: 0,
            last_find: None,
            typing: None,
            found: None,
            len,
            note: None,
            nav: Nav::default(),
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
        if let Some(a) = &mut self.anchor {
            a.line = a.line.min(last);
        }
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

    /// The selected lines, if any, as an inclusive range: those a
    /// selection of lines takes, or that one of text touches.
    pub fn selection(&self) -> Option<(usize, usize)> {
        self.anchor
            .map(|a| (a.line.min(self.cursor), a.line.max(self.cursor)))
    }

    /// Whether a selection of lines is under way.
    pub fn linewise(&self) -> bool {
        self.anchor.is_some_and(|a| a.linewise)
    }

    /// The cursor's char on its line, `text`.
    pub fn col_on(&self, text: &str) -> usize {
        self.col.min(text.chars().count().saturating_sub(1))
    }

    /// The text selected, if a selection of text is under way, from the
    /// first char through the last, given the cursor line's `width` in
    /// chars.
    pub fn text_selection(&self, width: usize) -> Option<(Pos, Pos)> {
        let a = self.anchor.filter(|a| !a.linewise)?;
        let here = (self.cursor, self.col.min(width.saturating_sub(1)));
        let there = (a.line, a.col);
        Some((here.min(there), here.max(there)))
    }

    /// What has been typed towards a command, for the footer: a count, `y`,
    /// `i` or `f`.
    pub fn typed(&self) -> String {
        let mut out = String::new();
        if self.count > 0 {
            out.push_str(&self.count.to_string());
        }
        if self.yank {
            out.push('y');
        }
        match self.pending {
            Some(Pending::Find(c)) => out.push(c),
            Some(Pending::Object { inner }) => out.push(if inner { 'i' } else { 'a' }),
            None => {}
        }
        out
    }

    /// The cursor's place, its char held within its line.
    fn here(&self, lines: &[String]) -> Pos {
        (
            self.cursor,
            self.col.min(width(lines, self.cursor).saturating_sub(1)),
        )
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
        let count = std::mem::take(&mut self.count);
        if let Some(p) = self.pending.take() {
            return self.pending_key(p, k, lines, count.max(1));
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if !ctrl
            && let KeyCode::Char(d) = k.code
            && let Some(d) = d.to_digit(10)
            && (d > 0 || count > 0)
        {
            self.count = count
                .saturating_mul(10)
                .saturating_add(d as usize)
                .min(99_999);
            return Outcome::Stay;
        }
        if !ctrl && let Some((to, kind)) = self.motion(k.code, lines, count) {
            let col = match k.code {
                KeyCode::Char('$') => Some(usize::MAX),
                KeyCode::Char('j' | 'k' | 'g' | 'G')
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Home
                | KeyCode::End => None,
                _ => Some(to.1),
            };
            return self.apply(lines, to, kind, col);
        }
        let yank = std::mem::take(&mut self.yank);
        let height = isize::try_from(height).unwrap_or(isize::MAX);
        let page = height.max(2) - 1;
        let half = (height / 2).max(1);
        let last = lines.len().saturating_sub(1);
        match k.code {
            KeyCode::Char('c' | 'v') if ctrl => return Outcome::Exit,
            KeyCode::Char('d') if ctrl => self.scroll(half, last),
            KeyCode::Char('u') if ctrl => self.scroll(-half, last),
            KeyCode::Char('f') if ctrl => self.go(page, last),
            KeyCode::Char('b') if ctrl => self.go(-page, last),
            KeyCode::PageDown => self.go(page, last),
            KeyCode::PageUp => self.go(-page, last),
            KeyCode::Char(c @ ('f' | 'F' | 't' | 'T')) if !ctrl => {
                self.yank = yank;
                self.count = count;
                self.pending = Some(Pending::Find(c));
            }
            KeyCode::Char(c @ ('i' | 'a'))
                if !ctrl && (yank || self.anchor.is_some_and(|a| !a.linewise)) =>
            {
                self.yank = yank;
                self.pending = Some(Pending::Object { inner: c == 'i' });
            }
            KeyCode::Char(c @ ('/' | '?')) => {
                self.typing = Some(Typing {
                    forward: c == '/',
                    query: String::new(),
                });
            }
            KeyCode::Char(']') => self.jump(true),
            KeyCode::Char('[') => self.jump(false),
            KeyCode::Enter if self.anchor.is_none() && self.jump_at().is_some() => {
                if let Some(to) = self.jump_at() {
                    self.land(to);
                }
            }
            KeyCode::Char('n') => self.repeat(lines, false),
            KeyCode::Char('N') => self.repeat(lines, true),
            KeyCode::Char(c @ ('v' | 'V' | ' ')) => self.select(lines, c != 'v'),
            KeyCode::Char('y') | KeyCode::Enter if self.anchor.is_some() => {
                return self.copy_selection(lines);
            }
            KeyCode::Char('y') if !yank => self.yank = true,
            KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                let to = (self.cursor + count.max(1) - 1).min(last);
                return copy_lines(lines, self.cursor, to);
            }
            KeyCode::Esc if yank => {}
            KeyCode::Esc if self.anchor.is_some() => self.anchor = None,
            KeyCode::Esc if self.found.is_some() => self.found = None,
            KeyCode::Esc | KeyCode::Char('q') => return Outcome::Exit,
            _ => {}
        }
        Outcome::Stay
    }

    /// Start a selection of lines (or text) at the cursor; again, drop it;
    /// in the other kind, switch to this.
    fn select(&mut self, lines: &[String], linewise: bool) {
        let (line, col) = self.here(lines);
        self.anchor = match self.anchor {
            Some(a) if a.linewise == linewise => None,
            Some(a) => Some(Anchor { linewise, ..a }),
            None => Some(Anchor {
                line,
                col,
                linewise,
            }),
        };
    }

    /// The key after `f`/`F`/`t`/`T` or `i`/`a`, `n` times over.
    fn pending_key(&mut self, p: Pending, k: KeyEvent, lines: &[String], n: usize) -> Outcome {
        let KeyCode::Char(c) = k.code else {
            self.yank = false;
            return Outcome::Stay;
        };
        match p {
            Pending::Find(kind) => {
                self.last_find = Some((kind, c));
                if let Some((to, how)) = find_char(lines, self.here(lines), kind, c, n) {
                    self.apply(lines, to, how, Some(to.1))
                } else {
                    self.yank = false;
                    Outcome::Stay
                }
            }
            Pending::Object { inner } => {
                let Some((from, to)) = object(lines, self.here(lines), c, inner) else {
                    self.yank = false;
                    self.note = Some("no such text object here".into());
                    return Outcome::Stay;
                };
                if std::mem::take(&mut self.yank) {
                    return copy_text(lines, from, (to.0, to.1 + 1));
                }
                self.anchor = Some(Anchor {
                    line: from.0,
                    col: from.1,
                    linewise: false,
                });
                self.cursor = to.0;
                self.col = to.1;
                Outcome::Stay
            }
        }
    }

    /// Where a motion key lands, `count` times over (0 for none given), and
    /// what `y` takes with it.
    fn motion(&self, code: KeyCode, lines: &[String], count: usize) -> Option<(Pos, Kind)> {
        let reps = count.max(1);
        let (l, col) = self.here(lines);
        let last = lines.len().saturating_sub(1);
        let end = width(lines, l);
        let times = |f: fn(&[String], Pos, bool) -> Pos, big: bool| {
            (0..reps).fold((l, col), |p, _| f(lines, p, big))
        };
        let blank_line = |from: usize, forward: bool| {
            if forward {
                (from + 1..=last)
                    .find(|&i| lines[i].is_empty())
                    .unwrap_or(last)
            } else {
                (0..from).rev().find(|&i| lines[i].is_empty()).unwrap_or(0)
            }
        };
        Some(match code {
            KeyCode::Char('h') | KeyCode::Left => ((l, col.saturating_sub(reps)), Kind::Exclusive),
            // Up to just past the end, so `yl` takes the last char.
            KeyCode::Char('l') | KeyCode::Right => ((l, (col + reps).min(end)), Kind::Exclusive),
            KeyCode::Char('0') => ((l, 0), Kind::Exclusive),
            KeyCode::Char('^') => ((l, first_nonblank(&lines[l])), Kind::Exclusive),
            KeyCode::Char('$') => {
                let to = (l + reps - 1).min(last);
                ((to, width(lines, to).saturating_sub(1)), Kind::Inclusive)
            }
            KeyCode::Char('w') => (times(word_forward, false), Kind::Exclusive),
            KeyCode::Char('W') => (times(word_forward, true), Kind::Exclusive),
            KeyCode::Char('b') => (times(word_back, false), Kind::Exclusive),
            KeyCode::Char('B') => (times(word_back, true), Kind::Exclusive),
            KeyCode::Char('e') => (times(word_end, false), Kind::Inclusive),
            KeyCode::Char('E') => (times(word_end, true), Kind::Inclusive),
            KeyCode::Char('j') | KeyCode::Down => (((l + reps).min(last), col), Kind::Linewise),
            KeyCode::Char('k') | KeyCode::Up => ((l.saturating_sub(reps), col), Kind::Linewise),
            KeyCode::Char('g') | KeyCode::Home => ((0, col), Kind::Linewise),
            KeyCode::Char('G') if count > 0 => (((count - 1).min(last), col), Kind::Linewise),
            KeyCode::Char('G') | KeyCode::End => ((last, col), Kind::Linewise),
            KeyCode::Char('}') => {
                let to = (0..reps).fold(l, |at, _| blank_line(at, true));
                ((to, 0), Kind::Exclusive)
            }
            KeyCode::Char('{') => {
                let to = (0..reps).fold(l, |at, _| blank_line(at, false));
                ((to, 0), Kind::Exclusive)
            }
            KeyCode::Char(key @ (';' | ',')) => {
                let (kind, ch) = self.last_find?;
                let kind = if key == ',' { reverse(kind) } else { kind };
                return find_char(lines, (l, col), kind, ch, reps);
            }
            _ => return None,
        })
    }

    /// Move to `to`, or with `y` pending copy what the motion covers. `col`
    /// is the column to keep, if the motion sets one.
    fn apply(&mut self, lines: &[String], to: Pos, kind: Kind, col: Option<usize>) -> Outcome {
        if std::mem::take(&mut self.yank) {
            let here = self.here(lines);
            let (from, mut end) = (here.min(to), here.max(to));
            return match kind {
                Kind::Linewise => copy_lines(lines, from.0, end.0),
                Kind::Inclusive => copy_text(lines, from, (end.0, end.1 + 1)),
                Kind::Exclusive => {
                    // As in vi, an exclusive motion that ends at the start
                    // of a later line stops at the end of the one before.
                    if end.1 == 0 && end.0 > from.0 {
                        end = (end.0 - 1, usize::MAX);
                    }
                    copy_text(lines, from, end)
                }
            };
        }
        self.cursor = to.0;
        if let Some(col) = col {
            self.col = col;
        }
        Outcome::Stay
    }

    /// Copy the selection, of lines or of text.
    fn copy_selection(&mut self, lines: &[String]) -> Outcome {
        let Some(a) = self.anchor.take() else {
            return Outcome::Stay;
        };
        if a.linewise {
            let (from, to) = (a.line.min(self.cursor), a.line.max(self.cursor));
            return copy_lines(lines, from, to);
        }
        let here = self.here(lines);
        let there = (a.line, a.col);
        let (from, to) = (here.min(there), here.max(there));
        copy_text(lines, from, (to.0, to.1 + 1))
    }

    /// The line the cursor's row jumps to, if it names a section.
    fn jump_at(&self) -> Option<usize> {
        self.nav
            .jumps
            .iter()
            .find(|(row, _)| *row == self.cursor)
            .map(|&(_, to)| to)
    }

    /// To the next (or previous) section's start, or say there is none.
    fn jump(&mut self, forward: bool) {
        let to = if forward {
            self.nav.marks.iter().copied().find(|&m| m > self.cursor)
        } else {
            self.nav
                .marks
                .iter()
                .copied()
                .rev()
                .find(|&m| m < self.cursor)
        };
        match to {
            Some(m) => self.land(m),
            None if self.nav.marks.is_empty() => {
                self.note = Some("nothing here to jump between".into());
            }
            None => {
                let what = if self.nav.what.is_empty() {
                    "sections"
                } else {
                    self.nav.what
                };
                let edge = if forward { "after" } else { "before" };
                self.note = Some(format!("no {what} {edge} this"));
            }
        }
    }

    /// The cursor on `line`, shown at the top of the view; sync keeps it
    /// within the text.
    fn land(&mut self, line: usize) {
        self.cursor = line;
        self.top = line;
    }

    fn go(&mut self, delta: isize, last: usize) {
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
    }

    /// Move the view and the cursor together, as vi's ctrl-d/ctrl-u do, so
    /// the view moves on the first press; sync clamps the view at the ends.
    fn scroll(&mut self, delta: isize, last: usize) {
        self.top = self.top.saturating_add_signed(delta);
        self.go(delta, last);
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
        let Some((query, forward)) = &self.found else {
            return;
        };
        match find(lines, query, self.cursor, *forward != reverse) {
            Some(i) => {
                self.cursor = i;
                self.col = matches(&lines[i], query).first().map_or(0, |m| m.0);
            }
            None => self.note = Some(format!("not found: {query}")),
        }
    }
}

/// Lines `a..=b` to copy, or leave if there are none.
fn copy_lines(lines: &[String], a: usize, b: usize) -> Outcome {
    if lines.is_empty() {
        return Outcome::Exit;
    }
    let last = lines.len() - 1;
    Outcome::Copy(a.min(last), b.min(last))
}

/// The text from `from` up to (not including) `to`, lines a newline apart
/// and their trailing blanks left out, to copy; nothing copies nothing.
fn copy_text(lines: &[String], from: Pos, to: Pos) -> Outcome {
    let text = text_between(lines, from, to);
    if text.is_empty() {
        Outcome::Stay
    } else {
        Outcome::Text(text)
    }
}

fn text_between(lines: &[String], (l0, c0): Pos, (l1, c1): Pos) -> String {
    let part = |l: usize, a: usize, b: usize| -> String {
        lines.get(l).map_or_else(String::new, |s| {
            s.chars().skip(a).take(b.saturating_sub(a)).collect()
        })
    };
    if l0 == l1 {
        return part(l0, c0, c1);
    }
    let mut out = vec![part(l0, c0, usize::MAX).trim_end().to_string()];
    for line in lines.iter().take(l1).skip(l0 + 1) {
        out.push(line.trim_end().to_string());
    }
    if l1 < lines.len() {
        out.push(part(l1, 0, c1).trim_end().to_string());
    }
    out.join("\n")
}

/// Chars in line `l`.
fn width(lines: &[String], l: usize) -> usize {
    lines.get(l).map_or(0, |s| s.chars().count())
}

fn char_at(lines: &[String], (l, c): Pos) -> Option<char> {
    lines.get(l)?.chars().nth(c)
}

fn first_nonblank(line: &str) -> usize {
    line.chars().position(|c| !c.is_whitespace()).unwrap_or(0)
}

fn char_class(c: char, big: bool) -> Class {
    if c.is_whitespace() {
        Class::Blank
    } else if big || c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

fn class(lines: &[String], p: Pos, big: bool) -> Class {
    char_at(lines, p).map_or(Class::Empty, |c| char_class(c, big))
}

/// The next char, on to the next line's first.
fn next(lines: &[String], (l, c): Pos) -> Option<Pos> {
    if c + 1 < width(lines, l) {
        Some((l, c + 1))
    } else if l + 1 < lines.len() {
        Some((l + 1, 0))
    } else {
        None
    }
}

/// The char before, back to the previous line's last.
fn prev(lines: &[String], (l, c): Pos) -> Option<Pos> {
    if c > 0 {
        Some((l, c - 1))
    } else if l > 0 {
        Some((l - 1, width(lines, l - 1).saturating_sub(1)))
    } else {
        None
    }
}

/// vi's `w`: the next word's start, an empty line, or past the end.
fn word_forward(lines: &[String], p: Pos, big: bool) -> Pos {
    let start = class(lines, p, big);
    let mut apart = matches!(start, Class::Blank | Class::Empty);
    let mut q = p;
    while let Some(n) = next(lines, q) {
        apart |= n.0 != q.0;
        q = n;
        match class(lines, q, big) {
            Class::Empty => return q,
            Class::Blank => apart = true,
            c if apart || c != start => return q,
            _ => {}
        }
    }
    (q.0, width(lines, q.0))
}

/// vi's `e`: the end of this word, or the next if at its end.
fn word_end(lines: &[String], p: Pos, big: bool) -> Pos {
    let mut q = p;
    loop {
        let Some(n) = next(lines, q) else { return q };
        q = n;
        if !matches!(class(lines, q, big), Class::Blank | Class::Empty) {
            break;
        }
    }
    let c = class(lines, q, big);
    while let Some(n) = next(lines, q) {
        if n.0 != q.0 || class(lines, n, big) != c {
            break;
        }
        q = n;
    }
    q
}

/// vi's `b`: the start of this word, or the previous if at its start.
fn word_back(lines: &[String], p: Pos, big: bool) -> Pos {
    let mut q = p;
    loop {
        let Some(n) = prev(lines, q) else { return q };
        q = n;
        match class(lines, q, big) {
            Class::Empty => return q,
            Class::Blank => {}
            _ => break,
        }
    }
    let c = class(lines, q, big);
    while let Some(n) = prev(lines, q) {
        if n.0 != q.0 || class(lines, n, big) != c {
            break;
        }
        q = n;
    }
    q
}

/// `f`/`F`/`t`/`T` the other way, for `,`.
fn reverse(kind: char) -> char {
    match kind {
        'f' => 'F',
        'F' => 'f',
        't' => 'T',
        _ => 't',
    }
}

/// Where `f`, `F`, `t` or `T` (`kind`) and `ch` land from `p`, the `n`th
/// `ch` along its line, and what `y` takes with it.
fn find_char(lines: &[String], (l, c): Pos, kind: char, ch: char, n: usize) -> Option<(Pos, Kind)> {
    let chars: Vec<char> = lines.get(l)?.chars().collect();
    let hit = |i: &usize| chars[*i] == ch;
    let col = match kind {
        'f' => (c + 1..chars.len()).filter(hit).nth(n - 1)?,
        't' => (c + 2..chars.len()).filter(hit).nth(n - 1)? - 1,
        'F' => (0..c).rev().filter(hit).nth(n - 1)?,
        _ => (0..c.saturating_sub(1)).rev().filter(hit).nth(n - 1)? + 1,
    };
    let how = if kind.is_lowercase() {
        Kind::Inclusive
    } else {
        Kind::Exclusive
    };
    Some(((l, col), how))
}

/// The text object `c` names at `p`, inner or with its surround, from its
/// first char through its last: `w`/`W` a word, a quote's contents, or a
/// bracket's (`b` for parens, `B` for braces).
fn object(lines: &[String], p: Pos, c: char, inner: bool) -> Option<(Pos, Pos)> {
    match c {
        'w' => word_object(lines, p, false, inner),
        'W' => word_object(lines, p, true, inner),
        '"' | '\'' | '`' => quote_object(lines, p, c, inner),
        '(' | ')' | 'b' => bracket_object(lines, p, ('(', ')'), inner),
        '[' | ']' => bracket_object(lines, p, ('[', ']'), inner),
        '{' | '}' | 'B' => bracket_object(lines, p, ('{', '}'), inner),
        '<' | '>' => bracket_object(lines, p, ('<', '>'), inner),
        _ => None,
    }
}

/// `iw`/`aw`: the run of like chars at `p`; with `a`, and the blanks after
/// it (else before it), or for blanks the word after them.
fn word_object(lines: &[String], (line, col): Pos, big: bool, inner: bool) -> Option<(Pos, Pos)> {
    let chars: Vec<char> = lines.get(line)?.chars().collect();
    let len = chars.len();
    if col >= len {
        return None;
    }
    let cls = |i: usize| char_class(chars[i], big);
    let run = |at: usize| {
        let kind = cls(at);
        let (mut a, mut b) = (at, at);
        while a > 0 && cls(a - 1) == kind {
            a -= 1;
        }
        while b + 1 < len && cls(b + 1) == kind {
            b += 1;
        }
        (a, b)
    };
    let (mut a, mut b) = run(col);
    if !inner {
        if cls(col) == Class::Blank {
            if b + 1 < len {
                b = run(b + 1).1;
            }
        } else if b + 1 < len && cls(b + 1) == Class::Blank {
            b = run(b + 1).1;
        } else if a > 0 && cls(a - 1) == Class::Blank {
            a = run(a - 1).0;
        }
    }
    Some(((line, a), (line, b)))
}

/// `i"`/`a"` and the like: the quoted text on `p`'s line that holds it or
/// comes after it.
fn quote_object(
    lines: &[String],
    (line, col): Pos,
    quote: char,
    inner: bool,
) -> Option<(Pos, Pos)> {
    let at: Vec<usize> = lines
        .get(line)?
        .chars()
        .enumerate()
        .filter(|&(_, ch)| ch == quote)
        .map(|(i, _)| i)
        .collect();
    let &[a, b] = at.as_chunks::<2>().0.iter().find(|pair| col <= pair[1])?;
    if !inner {
        return Some(((line, a), (line, b)));
    }
    (b > a + 1).then_some(((line, a + 1), (line, b - 1)))
}

/// `i(`/`a(` and the like: the innermost brackets around `p`, across lines.
fn bracket_object(
    lines: &[String],
    p: Pos,
    (open, close): (char, char),
    inner: bool,
) -> Option<(Pos, Pos)> {
    let mut q = p;
    let mut depth = 0usize;
    let a = loop {
        match char_at(lines, q) {
            Some(ch) if ch == close && q != p => depth += 1,
            Some(ch) if ch == open => {
                if depth == 0 {
                    break q;
                }
                depth -= 1;
            }
            _ => {}
        }
        q = prev(lines, q)?;
    };
    let mut q = a;
    let b = loop {
        q = next(lines, q)?;
        match char_at(lines, q) {
            Some(ch) if ch == open => depth += 1,
            Some(ch) if ch == close => {
                if depth == 0 {
                    break q;
                }
                depth -= 1;
            }
            _ => {}
        }
    };
    if !inner {
        return Some((a, b));
    }
    let from = next(lines, a)?;
    if from == b {
        return None;
    }
    Some((from, prev(lines, b)?))
}

/// The next line after (or before) `from` holding `query`, wrapping round.
fn find(lines: &[String], query: &str, from: usize, forward: bool) -> Option<usize> {
    let n = lines.len();
    (1..=n)
        .map(|step| {
            if forward {
                (from + step) % n
            } else {
                (from + n * 2 - step % n) % n
            }
        })
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
    let norm = |c: char| {
        if fold {
            c.to_lowercase().next().unwrap_or(c)
        } else {
            c
        }
    };
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
                let s = if run_lit == Some(true) {
                    span.style.patch(style)
                } else {
                    span.style
                };
                spans.push(Span::styled(std::mem::take(&mut run), s));
            }
            run_lit = Some(l);
            run.push(c);
            at += 1;
        }
        if !run.is_empty() {
            let s = if run_lit == Some(true) {
                span.style.patch(style)
            } else {
                span.style
            };
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
        Pane {
            target: "-".into(),
            session: "s".into(),
            window: "w".into(),
        }
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn lines() -> Vec<String> {
        [
            "› hello",
            "",
            "● first answer",
            "  goes on",
            "",
            "› Again",
            "",
            "● second",
        ]
        .iter()
        .map(std::string::ToString::to_string)
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
    fn half_pages_move_the_view_at_once() {
        let l: Vec<String> = (0..20).map(|i| i.to_string()).collect();
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let mut s = Scrollback::new(pane(), l.len(), 4);
        assert_eq!((s.cursor, s.top), (19, 16));
        s.key(ctrl('u'), &l, 4);
        s.sync(l.len(), 4);
        assert_eq!((s.cursor, s.top), (17, 14));
        s.key(ctrl('d'), &l, 4);
        s.sync(l.len(), 4);
        assert_eq!((s.cursor, s.top), (19, 16));
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
    fn copies_the_selected_lines() {
        let l = lines();
        let mut s = Scrollback::new(pane(), l.len(), 4);
        s.cursor = 2;
        s.key(key('V'), &l, 4);
        s.key(key('j'), &l, 4);
        assert_eq!(s.key(key('y'), &l, 4), Outcome::Copy(2, 3));
        // yy and Y copy the line, a count more.
        s.cursor = 1;
        s.key(key('y'), &l, 4);
        assert_eq!(s.key(key('y'), &l, 4), Outcome::Copy(1, 1));
        s.key(key('2'), &l, 4);
        assert_eq!(s.key(key('Y'), &l, 4), Outcome::Copy(1, 2));
        // Escape drops the selection before it leaves.
        s.key(key('v'), &l, 4);
        assert_eq!(
            s.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &l, 4),
            Outcome::Stay
        );
        assert_eq!(
            s.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &l, 4),
            Outcome::Exit
        );
    }

    #[test]
    fn marks_matches_across_spans() {
        let line = Line::from(vec![Span::raw("ab"), Span::raw("cd")]);
        let style = Style::new().add_modifier(ratatui::style::Modifier::REVERSED);
        let out = mark(line, &matches("abcd", "bc"), style);
        let parts: Vec<_> = out
            .spans
            .iter()
            .map(|s| (s.content.to_string(), s.style == style))
            .collect();
        assert_eq!(
            parts,
            [
                ("a".into(), false),
                ("b".into(), true),
                ("c".into(), true),
                ("d".into(), false)
            ]
        );
    }

    #[test]
    fn brackets_jump_between_sections_and_enter_follows_a_row() {
        let lines: Vec<String> = (0..40).map(|i| format!("line {i}")).collect();
        let mut sb = Scrollback::new(pane(), lines.len(), 10);
        sb.cursor = 0;
        sb.sync(lines.len(), 10);
        sb.nav = Nav {
            marks: vec![5, 20],
            jumps: vec![(1, 20)],
            what: "files",
        };
        sb.key(key(']'), &lines, 10);
        assert_eq!((sb.cursor, sb.top), (5, 5));
        sb.key(key(']'), &lines, 10);
        sb.sync(lines.len(), 10);
        assert_eq!((sb.cursor, sb.top), (20, 20));
        sb.key(key(']'), &lines, 10);
        assert_eq!(sb.note.as_deref(), Some("no files after this"));
        sb.key(key('['), &lines, 10);
        assert_eq!(sb.cursor, 5);
        sb.cursor = 1;
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(sb.key(enter, &lines, 10), Outcome::Stay);
        assert_eq!(sb.cursor, 20);
        // Elsewhere, Enter still copies.
        assert_eq!(sb.key(enter, &lines, 10), Outcome::Copy(20, 20));
    }

    fn keys(s: &mut Scrollback, l: &[String], typed: &str) -> Outcome {
        let mut out = Outcome::Stay;
        for c in typed.chars() {
            out = s.key(key(c), l, 10);
        }
        out
    }

    fn text(s: &str) -> Outcome {
        Outcome::Text(s.into())
    }

    fn code() -> Vec<String> {
        [
            "let foo_bar = call(a, \"x y\").unwrap();",
            "",
            "  next line here",
        ]
        .map(String::from)
        .to_vec()
    }

    #[test]
    fn moves_by_chars_and_words() {
        let l = code();
        let mut s = Scrollback::new(pane(), l.len(), 10);
        s.cursor = 0;
        keys(&mut s, &l, "w");
        assert_eq!(s.here(&l), (0, 4));
        keys(&mut s, &l, "w");
        assert_eq!(s.here(&l), (0, 12), "= is punctuation");
        keys(&mut s, &l, "e");
        assert_eq!(s.here(&l), (0, 17));
        keys(&mut s, &l, "b");
        assert_eq!(s.here(&l), (0, 14));
        keys(&mut s, &l, "0W");
        assert_eq!(s.here(&l), (0, 4));
        keys(&mut s, &l, "2l");
        assert_eq!(s.here(&l), (0, 6));
        keys(&mut s, &l, "h$");
        assert_eq!(s.here(&l), (0, 37));
        // $ sticks to the end; w stops on the empty line, then the next
        // word; ^ finds the first non-blank.
        keys(&mut s, &l, "jj");
        assert_eq!(s.here(&l), (2, 15));
        keys(&mut s, &l, "gg$w");
        assert_eq!(s.here(&l), (1, 0));
        keys(&mut s, &l, "w");
        assert_eq!(s.here(&l), (2, 2));
        keys(&mut s, &l, "0");
        assert_eq!(s.here(&l), (2, 0));
        keys(&mut s, &l, "^");
        assert_eq!(s.here(&l), (2, 2));
        keys(&mut s, &l, "2G");
        assert_eq!(s.cursor, 1);
    }

    #[test]
    fn finds_chars_on_the_line() {
        let l = code();
        let mut s = Scrollback::new(pane(), l.len(), 10);
        s.cursor = 0;
        keys(&mut s, &l, "fa");
        assert_eq!(s.here(&l), (0, 9));
        keys(&mut s, &l, ";;");
        assert_eq!(s.here(&l), (0, 19));
        keys(&mut s, &l, ",");
        assert_eq!(s.here(&l), (0, 15));
        keys(&mut s, &l, "t)");
        assert_eq!(s.here(&l), (0, 26));
        keys(&mut s, &l, "F(");
        assert_eq!(s.here(&l), (0, 18));
        keys(&mut s, &l, "fz");
        assert_eq!(s.here(&l), (0, 18), "no z: it stays");
    }

    #[test]
    fn yanks_motions_and_text_objects() {
        let l = code();
        let mut s = Scrollback::new(pane(), l.len(), 10);
        s.cursor = 0;
        s.col = 5;
        assert_eq!(keys(&mut s, &l, "yiw"), text("foo_bar"));
        assert_eq!(keys(&mut s, &l, "yaw"), text("foo_bar "));
        assert_eq!(keys(&mut s, &l, "yw"), text("oo_bar "));
        assert_eq!(keys(&mut s, &l, "ye"), text("oo_bar"));
        assert_eq!(keys(&mut s, &l, "y2e"), text("oo_bar ="));
        assert_eq!(keys(&mut s, &l, "yb"), text("f"));
        assert_eq!(keys(&mut s, &l, "yl"), text("o"));
        assert_eq!(keys(&mut s, &l, "y0"), text("let f"));
        s.col = 29;
        assert_eq!(keys(&mut s, &l, "y$"), text("unwrap();"));
        assert_eq!(keys(&mut s, &l, "yt("), text("unwrap"));
        s.col = 15;
        assert_eq!(keys(&mut s, &l, "yiW"), text("call(a,"));
        s.col = 23;
        assert_eq!(keys(&mut s, &l, "yi\""), text("x y"));
        assert_eq!(keys(&mut s, &l, "ya\""), text("\"x y\""));
        assert_eq!(keys(&mut s, &l, "yib"), text("a, \"x y\""));
        assert_eq!(keys(&mut s, &l, "ya("), text("(a, \"x y\")"));
        assert_eq!(keys(&mut s, &l, "yi{"), Outcome::Stay, "no braces");
        // yw at a line's last word stops at its end, not on the next line.
        s.col = 29;
        assert_eq!(keys(&mut s, &l, "yW"), text("unwrap();"));
        // Across lines, text joins with newlines.
        assert_eq!(keys(&mut s, &l, "yj"), Outcome::Copy(0, 1));
        // Escape drops a pending y.
        keys(&mut s, &l, "y");
        s.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &l, 10);
        assert_eq!(s.typed(), "");
        assert_eq!(keys(&mut s, &l, "w"), Outcome::Stay);
    }

    #[test]
    fn selects_text_and_copies_it() {
        let l = code();
        let mut s = Scrollback::new(pane(), l.len(), 10);
        s.cursor = 0;
        s.col = 4;
        keys(&mut s, &l, "ve");
        assert_eq!(s.text_selection(37), Some(((0, 4), (0, 10))));
        assert_eq!(keys(&mut s, &l, "y"), text("foo_bar"));
        // Across lines, each but the last to its end.
        s.col = 29;
        keys(&mut s, &l, "vjj");
        assert!(s.selection().is_some() && !s.linewise());
        assert_eq!(keys(&mut s, &l, "y"), text("unwrap();\n\n  next line here"));
        // A text object selects; V switches to lines and v back.
        s.cursor = 0;
        s.col = 19;
        keys(&mut s, &l, "vi(");
        assert_eq!(s.text_selection(37), Some(((0, 19), (0, 26))));
        keys(&mut s, &l, "V");
        assert!(s.linewise());
        keys(&mut s, &l, "v");
        assert!(!s.linewise());
        assert_eq!(keys(&mut s, &l, "y"), text("a, \"x y\""));
    }

    #[test]
    fn copies_by_osc52() {
        assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x07");
        assert_eq!(osc52("hey"), "\x1b]52;c;aGV5\x07");
    }
}
