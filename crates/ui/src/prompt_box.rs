//! The prompt box the floats share: a word-wrapping, readline-style text
//! field drawn as a filled card with soft half-block edges, and kept in a
//! prompt file (see core's `prompts`) so a float's title can carry a
//! `LABEL:ID` reference instead of the text. The new-session picker takes a
//! new agent's first prompt in one; the tab picker, a message to a running
//! agent.
//!
//! Rex's float modes take plain Enter to act, so a newline is alt- or
//! shift-Enter (or arrives in a paste).
//!
//! Given the agent's slash commands (`set_commands`), it completes them:
//! see `slash`. Callers offer keys to `menu_key` first and draw the menu
//! with `draw_menu` after everything else.

use std::io;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use ratatui::crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, KeyCode, KeyEvent, KeyModifiers, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::{DefaultTerminal, Frame};
use ratatui_textarea::{CursorMove, TextArea, WrapMode};
use switchyard_core::commands::SlashCommand;
use switchyard_core::prompts::{parse_prompt_ref, prompt_dir};
use switchyard_core::rex::{local_label, log, now};
use unicode_width::UnicodeWidthChar;

use crate::Palette;
use crate::chrome;
use crate::draw::str_width;
use crate::slash;

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
    /// The agent's slash commands and their menu.
    slash: slash::Menu,
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
            slash: slash::Menu::default(),
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

    /// Complete these commands after `/`: anywhere in the text, or (for
    /// Pi) only first in it.
    pub fn set_commands(&mut self, commands: Vec<SlashCommand>, anywhere: bool) {
        self.slash.set(commands, anywhere);
    }

    pub fn has_commands(&self) -> bool {
        !self.slash.is_empty()
    }

    fn menu(&mut self) -> Option<(slash::Word, Vec<slash::Hit>)> {
        let c = self.field.cursor();
        self.slash.open(self.field.lines(), (c.0, c.1))
    }

    /// Whether the slash-command menu is showing.
    pub fn menu_open(&mut self) -> bool {
        self.menu().is_some()
    }

    /// Take a key for the open menu: Tab completes the highlighted command,
    /// Up/Down (ctrl-p/n, shift-Tab) move, Escape closes it. False when the
    /// menu is closed or the key isn't one of these.
    pub fn menu_key(&mut self, k: KeyEvent) -> bool {
        let Some((word, found)) = self.menu() else { return false };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Tab => self.complete(&word, found[self.slash.selected].index),
            KeyCode::Down => self.slash.step(1, found.len()),
            KeyCode::Char('n') if ctrl => self.slash.step(1, found.len()),
            KeyCode::Up | KeyCode::BackTab => self.slash.step(-1, found.len()),
            KeyCode::Char('p') if ctrl => self.slash.step(-1, found.len()),
            KeyCode::Esc => self.slash.dismiss(),
            _ => return false,
        }
        true
    }

    /// Replace `word` with command `i` and a space after it.
    fn complete(&mut self, word: &slash::Word, i: usize) {
        let name = format!("/{}", self.slash.command(i).name);
        let line_len = self.field.lines()[word.row].chars().count();
        self.field
            .move_cursor(CursorMove::Jump(word.row as u16, word.start as u16));
        self.field.delete_str(word.end - word.start);
        self.field.insert_str(&name);
        // Step over a space already there rather than adding a second.
        if word.end < line_len {
            self.field.move_cursor(CursorMove::Forward);
        } else {
            self.field.insert_char(' ');
        }
    }

    /// Draw the open slash-command menu next to the box drawn in `card`,
    /// within `bounds`; call it after everything else.
    pub fn draw_menu(&mut self, f: &mut Frame, p: &Palette, card: Rect, bounds: Rect) {
        if let Some((_, found)) = self.menu() {
            self.slash.draw(f, p, &found, card, bounds);
        }
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

/// Edit `prompt`'s text in `$VISUAL` or `$EDITOR` (else nvim-editor, nvim or
/// vi), in its own `.edit.md` file, taking the terminal until the editor
/// exits. `before_editor` runs once the file is written, to leave any key
/// mode the editor's keys would reach. Returns the text the editor saved,
/// without trailing newlines, or `None` when it failed (such as nvim's
/// `:cq`); the caller puts the text back if it changed. The corners are
/// deleted while the editor runs and drawn again at `cell` in `fills`.
pub fn edit_in_editor(
    term: &mut DefaultTerminal,
    prompt: &PromptBox,
    corners: &mut Option<chrome::Corners>,
    cell: Option<chrome::CellSize>,
    fills: [Color; 2],
    before_editor: impl FnOnce(),
) -> io::Result<Option<String>> {
    let Ok((_, id)) = parse_prompt_ref(&prompt.file_ref) else { return Ok(None) };
    let dir = prompt_dir();
    let path = dir.join(format!("{id}.edit.md"));
    let before = prompt.field.lines().join("\n");
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, format!("{before}\n"))) {
        log(&format!("could not write the prompt for the editor: {e}"));
        return Ok(None);
    }
    before_editor();
    let mut out = io::stdout();
    let _ = execute!(out, PopKeyboardEnhancementFlags, DisableBracketedPaste);
    if let Some(c) = corners.take() {
        let _ = c.delete(&mut out);
    }
    ratatui::restore();
    let status = Command::new("sh")
        .args([
            "-c",
            r#"e=${VISUAL:-$EDITOR}; [ -n "$e" ] || e=$(command -v nvim-editor || command -v nvim || echo vi); exec $e "$1""#,
            "sh",
        ])
        .arg(&path)
        .status();
    ratatui::crossterm::terminal::enable_raw_mode()?;
    execute!(out, ratatui::crossterm::terminal::EnterAlternateScreen)?;
    term.clear()?;
    if let Some(cell) = cell {
        *corners = chrome::Corners::new(cell, &fills, &mut out).ok();
    }
    let _ = execute!(
        out,
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );
    let edited = match status {
        Ok(s) if s.success() => std::fs::read_to_string(&path).ok(),
        Ok(_) => None,
        Err(e) => {
            log(&format!("could not run the editor: {e}"));
            None
        }
    };
    let _ = std::fs::remove_file(&path);
    Ok(edited.map(|text| text.trim_end_matches(['\n', '\r']).to_string()))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(name: &str) -> SlashCommand {
        SlashCommand {
            name: name.into(),
            description: String::new(),
            hint: String::new(),
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(b: &mut PromptBox, s: &str) {
        for c in s.chars() {
            b.key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn tab_completes_the_highlighted_command() {
        let mut b = PromptBox::new();
        b.set_commands(
            vec![cmd("issue-create"), cmd("issue-plan"), cmd("wrap")],
            true,
        );
        typed(&mut b, "then /iss");
        assert!(b.menu_open());
        assert!(b.menu_key(key(KeyCode::Down)));
        assert!(b.menu_key(key(KeyCode::Tab)));
        assert_eq!(b.text(), "then /issue-plan");
        assert_eq!(b.field.lines()[0], "then /issue-plan ");
        assert!(!b.menu_open());
        typed(&mut b, "now");
        assert_eq!(b.text(), "then /issue-plan now");
    }

    #[test]
    fn completing_mid_text_replaces_the_word_and_keeps_one_space() {
        let mut b = PromptBox::new();
        b.set_commands(vec![cmd("wrap")], true);
        b.paste("/w please");
        b.field.move_cursor(CursorMove::Jump(0, 2));
        assert!(b.menu_key(key(KeyCode::Tab)));
        assert_eq!(b.field.lines()[0], "/wrap please");
        assert_eq!(b.field.cursor(), ratatui_textarea::DataCursor(0, 6));
    }

    #[test]
    fn the_menu_leaves_keys_alone_when_closed_or_dismissed() {
        let mut b = PromptBox::new();
        assert!(!b.menu_key(key(KeyCode::Tab)));
        b.set_commands(vec![cmd("wrap")], false);
        typed(&mut b, "do /w");
        assert!(!b.menu_open(), "Pi takes commands only first");
        b.clear();
        typed(&mut b, "/w");
        assert!(b.menu_key(key(KeyCode::Esc)));
        assert!(!b.menu_key(key(KeyCode::Tab)));
        typed(&mut b, "r");
        assert!(b.menu_open());
    }
}
