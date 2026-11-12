//! The prompt box the floats share: a word-wrapping, readline-style text
//! field drawn as a filled card with soft half-block edges, and kept in a
//! prompt file (see core's `prompts`) so a float's title can carry a
//! `LABEL:ID` reference instead of the text. The new-session picker takes a
//! new agent's first prompt in one; the tab picker, a message to a running
//! agent.
//!
//! Rex's float modes take plain Enter to act, so a newline is alt- or
//! shift-Enter (or arrives in a paste).

use std::io;
use std::sync::atomic::{AtomicU32, Ordering};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui_textarea::{CursorMove, TextArea, WrapMode};
use switchyard_core::prompts::{parse_prompt_ref, prompt_dir};
use switchyard_core::rex::{local_label, now};
use unicode_width::UnicodeWidthChar;

use crate::Palette;
use crate::chrome;
use crate::draw::str_width;

/// The rows a prompt box adds around its text: soft top and bottom edges, a
/// gap and the chip row.
pub const CHROME_ROWS: u16 = 4;

/// A prompt being typed, and the file it is kept in.
pub struct PromptBox {
    pub field: TextArea<'static>,
    /// The prompt file's reference, `LABEL:ID`.
    pub file_ref: String,
    /// The text as last saved, and whether saving it worked.
    saved: String,
    pub saved_ok: bool,
}

impl Default for PromptBox {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptBox {
    /// An empty box with a reference no other box in this process shares.
    pub fn new() -> Self {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        PromptBox {
            field: text_field(),
            file_ref: format!("{}:{}-{}-{seq}", local_label(), std::process::id(), now()),
            saved: String::new(),
            saved_ok: true,
        }
    }

    /// The prompt as sent: surrounding blank space dropped.
    pub fn text(&self) -> String {
        self.field.lines().join("\n").trim().to_string()
    }

    pub fn is_empty(&self) -> bool {
        self.field.is_empty()
    }

    pub fn clear(&mut self) {
        self.field.clear();
    }

    /// Edit with readline keys (see `text_key`).
    pub fn key(&mut self, k: KeyEvent) {
        text_key(&mut self.field, k);
    }

    /// Insert pasted text: line endings as newlines, tabs as spaces, and no
    /// other control characters.
    pub fn paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n").replace('\t', "    ");
        self.field.insert_str(text.chars().filter(|&c| c == '\n' || !c.is_control()).collect::<String>());
    }

    /// The reference to hand on: Some("") for an empty prompt (no file),
    /// the file's when it is saved, None when it couldn't be.
    pub fn saved_ref(&self) -> Option<&str> {
        if self.text().is_empty() {
            Some("")
        } else if self.saved_ok {
            Some(&self.file_ref)
        } else {
            None
        }
    }

    /// Keep the prompt file in step with the field; an empty prompt has none.
    pub fn save(&mut self) {
        let text = self.text();
        if text == self.saved {
            return;
        }
        let Ok((_, id)) = parse_prompt_ref(&self.file_ref) else {
            self.saved_ok = false;
            return;
        };
        let dir = prompt_dir();
        let path = dir.join(id);
        self.saved_ok = if text.is_empty() {
            !matches!(std::fs::remove_file(&path), Err(e) if e.kind() != io::ErrorKind::NotFound)
        } else {
            // Atomically, so a reader never sees half a prompt.
            let tmp = dir.join(format!("{id}.tmp"));
            std::fs::create_dir_all(&dir)
                .and_then(|_| std::fs::write(&tmp, &text))
                .and_then(|_| std::fs::rename(&tmp, &path))
                .is_ok()
        };
        if self.saved_ok {
            self.saved = text;
        }
    }

    /// Remove the prompt file, if there is one.
    pub fn discard(&self) {
        if let Ok((_, id)) = parse_prompt_ref(&self.file_ref) {
            let _ = std::fs::remove_file(prompt_dir().join(id));
        }
    }

    /// Text rows the box needs in a card `card_width` wide, between `min`
    /// and `max`; longer prompts scroll to their end.
    pub fn rows(&self, card_width: u16, min: u16, max: u16) -> u16 {
        (wrap(&self.field.lines().join("\n"), text_width(card_width)).len() as u16).clamp(min, max)
    }

    /// Show `text` in the empty box.
    pub fn set_placeholder(&mut self, text: &str) {
        self.field.set_placeholder_text(text.to_string());
    }

    /// Draw the box in `card` with `rows` rows of text, filled brighter when
    /// `focused` (which also places the terminal cursor), with rounded
    /// `corners` when the terminal can show them. Returns the chip row
    /// under the text, for the caller's chips, and how many text rows are
    /// scrolled out of view above.
    pub fn draw(
        &mut self,
        f: &mut Frame,
        p: &Palette,
        card: Rect,
        rows: u16,
        focused: bool,
        corners: Option<&chrome::Corners>,
    ) -> Option<(Rect, usize)> {
        if card.height < CHROME_ROWS + 1 {
            return None;
        }
        let rows = rows.min(card.height - CHROME_ROWS);
        let fill = if focused { p.overlay } else { p.surface };
        let w = card.width as usize;
        if w < 4 {
            return None;
        }
        let buf = f.buffer_mut();
        buf.set_style(Rect { y: card.y + 1, height: card.height - 2, ..card }, Style::new().bg(fill));
        let edge = |l: &str, mid: &str, r: &str| {
            Line::from(vec![
                Span::styled(l.to_string(), Style::new().fg(fill).bg(p.base)),
                Span::styled(mid.repeat(w - 2), Style::new().fg(fill).bg(p.base)),
                Span::styled(r.to_string(), Style::new().fg(fill).bg(p.base)),
            ])
        };
        // Square corners unless the terminal can show rounded ones; quadrant
        // blocks would leave a step.
        f.render_widget(edge("▄", "▄", "▄"), Rect { height: 1, ..card });
        let bottom = card.y + card.height - 1;
        f.render_widget(edge("▀", "▀", "▀"), Rect { y: bottom, height: 1, ..card });
        if let Some(corners) = corners {
            let right = card.x + card.width - 1;
            for (x, y, corner) in [
                (card.x, card.y, chrome::Corner::TopLeft),
                (right, card.y, chrome::Corner::TopRight),
                (card.x, bottom, chrome::Corner::BottomLeft),
                (right, bottom, chrome::Corner::BottomRight),
            ] {
                corners.put(f.buffer_mut(), x, y, corner, fill, p.base);
            }
        }

        let text = Rect { x: card.x + 2, y: card.y + 1, width: card.width.saturating_sub(4), height: rows };
        self.field.set_style(Style::new().fg(p.text).bg(fill));
        self.field.set_placeholder_style(Style::new().fg(p.muted).bg(fill));
        f.render_widget(&self.field, text);
        if focused {
            place_cursor(f, &self.field, text);
        }
        let skip = self.field.scroll_offset().0 as usize;
        let chips = Rect { x: card.x + 1, y: card.y + 2 + rows, width: card.width.saturating_sub(2), height: 1 };
        Some((chips, skip))
    }
}

/// An empty text field: word-wrapped, with no line highlight or drawn
/// cursor (the terminal's own cursor is placed instead).
pub fn text_field() -> TextArea<'static> {
    let mut t = TextArea::default();
    t.set_wrap_mode(WrapMode::WordOrGlyph);
    t.set_cursor_line_style(Style::default());
    t.set_cursor_style(Style::default());
    t
}

/// Edit a field with ratatui-textarea's emacs/readline keys (ctrl-a/e/b/f,
/// alt-b/f, ctrl-k, ctrl-w, ctrl-y to yank, …), except that ctrl-u deletes
/// to the line start as in a shell, so undo moves to ctrl-z or ctrl-_.
pub fn text_key(field: &mut TextArea<'static>, k: KeyEvent) {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    match k.code {
        KeyCode::Char('u') if ctrl => {
            field.delete_line_by_head();
        }
        // ctrl-_ arrives as ctrl-7 from terminals without key reporting.
        KeyCode::Char('z' | '_' | '/' | '7') if ctrl => {
            field.undo();
        }
        // The textarea has no alt-e; make it end of word, just past its
        // last character as in a shell (WordEnd stops on that character).
        KeyCode::Char('e') if k.modifiers.contains(KeyModifiers::ALT) => {
            field.move_cursor(CursorMove::WordEnd);
            field.move_cursor(CursorMove::Forward);
        }
        _ => {
            field.input(k);
        }
    }
}

/// Put the terminal cursor where `field`, drawn in `area`, has its cursor.
pub fn place_cursor(f: &mut Frame, field: &TextArea, area: Rect) {
    let c = field.screen_cursor();
    let (top, left) = field.scroll_offset();
    let (row, col) = (c.row.saturating_sub(top as usize) as u16, c.col.saturating_sub(left as usize) as u16);
    if row < area.height && col < area.width {
        f.set_cursor_position((area.x + col, area.y + row));
    }
}

/// Columns of prompt text in a prompt box `card_width` wide: two of padding
/// on each side, plus one so the cursor fits after a full row.
pub fn text_width(card_width: u16) -> usize {
    (card_width as usize).saturating_sub(5).max(1)
}

/// `text` word-wrapped into display rows of at most `width` columns,
/// newlines starting new rows; a word longer than a row is split. Spaces
/// stay at the end of the row they follow, so the rows join back into the
/// text. A full last row gets an empty one after it, where the cursor goes.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for line in text.split('\n') {
        let mut row = String::new();
        for c in line.chars() {
            if str_width(&row) + c.width().unwrap_or(0) > width && !row.is_empty() && c != ' ' {
                // Break after the row's last space, unless that leaves nothing.
                match row.rfind(' ').map(|i| i + 1).filter(|&i| i < row.len()) {
                    Some(i) if !row[..i].trim().is_empty() => {
                        let rest = row.split_off(i);
                        rows.push(std::mem::replace(&mut row, rest));
                    }
                    _ => rows.push(std::mem::take(&mut row)),
                }
            }
            row.push(c);
        }
        rows.push(row);
    }
    if rows.last().is_some_and(|r| str_width(r) >= width) {
        rows.push(String::new());
    }
    rows
}
