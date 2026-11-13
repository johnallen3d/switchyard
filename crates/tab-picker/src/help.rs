//! The keys float: every binding the picker has, grouped by where it
//! works, with the group for where John is first. `?` opens it from the
//! lists, F1 from anywhere (the reply box, snooze prompt and scrollback
//! take `?` as text or a search).
//!
//! `/` filters the bindings by key, action or group (Backspace edits,
//! ctrl-u clears, Enter or Escape stops typing); `j`/`k`, the arrows and
//! the page keys scroll; Escape clears the filter, then closes, as do `q`,
//! `?` and F1.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;
use switchyard_ui::Palette;
use switchyard_ui::draw::{self, str_width};

/// A group of bindings: its title and (keys, what they do).
pub struct Section {
    pub title: &'static str,
    pub keys: Vec<(String, String)>,
}

impl Section {
    pub fn new(title: &'static str, keys: &[(&str, &str)]) -> Self {
        Section { title, keys: keys.iter().map(|(k, d)| (k.to_string(), d.to_string())).collect() }
    }
}

#[derive(Default)]
pub struct Help {
    pub filter: String,
    /// The filter has the keys (after `/`).
    pub typing: bool,
    offset: usize,
}

/// What a key did to the float.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Close,
}

impl Help {
    pub fn key(&mut self, k: KeyEvent) -> Outcome {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if self.typing {
            match k.code {
                KeyCode::Enter | KeyCode::Esc => self.typing = false,
                KeyCode::Char('u') if ctrl => self.filter.clear(),
                KeyCode::Char('c') if ctrl => return Outcome::Close,
                KeyCode::Backspace if self.filter.is_empty() => self.typing = false,
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c) if !ctrl => self.filter.push(c),
                KeyCode::F(1) => return Outcome::Close,
                _ => {}
            }
            self.offset = 0;
            return Outcome::Stay;
        }
        match k.code {
            KeyCode::Char('/') => self.typing = true,
            KeyCode::Char('u') if ctrl => self.filter.clear(),
            KeyCode::Char('j') | KeyCode::Down => self.offset += 1,
            KeyCode::Char('k') | KeyCode::Up => self.offset = self.offset.saturating_sub(1),
            KeyCode::Char('d') if ctrl => self.offset += 10,
            KeyCode::PageDown => self.offset += 10,
            KeyCode::PageUp => self.offset = self.offset.saturating_sub(10),
            KeyCode::Char('g') | KeyCode::Home => self.offset = 0,
            KeyCode::Char('G') | KeyCode::End => self.offset = usize::MAX,
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.offset = 0;
            }
            KeyCode::Esc | KeyCode::Char('q' | '?') | KeyCode::F(1) => return Outcome::Close,
            KeyCode::Char('c') if ctrl => return Outcome::Close,
            _ => {}
        }
        Outcome::Stay
    }

    /// The float over `area`: the filter, then the matching bindings under
    /// their group titles.
    pub fn draw(&mut self, f: &mut Frame, p: &Palette, area: Rect, sections: &[Section]) {
        let lines = self.lines(p, sections);
        let w = 64.min(area.width.saturating_sub(4));
        let h = (lines.len() as u16 + 4).clamp(8, area.height.saturating_sub(2).max(8)).min(area.height);
        let r = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
        f.render_widget(Clear, r);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(p.iris))
            .style(Style::new().bg(p.base))
            .title(Span::styled(" Keys ", Style::new().fg(p.text).add_modifier(Modifier::BOLD)));
        let inner = block.inner(r);
        f.render_widget(block, r);
        if inner.height < 3 {
            return;
        }
        let filter = Rect { height: 1, ..inner };
        let body = Rect { y: inner.y + 2, height: inner.height - 2, ..inner };
        let line = if self.typing || !self.filter.is_empty() {
            let mut spans = vec![
                Span::styled(" / ", Style::new().fg(p.gold).add_modifier(Modifier::BOLD)),
                Span::styled(self.filter.clone(), Style::new().fg(p.text)),
            ];
            if self.typing {
                spans.push(Span::styled("▏", Style::new().fg(p.subtle)));
            }
            Line::from(spans)
        } else {
            Line::from(vec![
                Span::styled(" / ", Style::new().fg(p.gold).add_modifier(Modifier::BOLD)),
                Span::styled("filter", Style::new().fg(p.muted)),
            ])
        };
        f.render_widget(line, filter);
        let close = Line::styled("esc close ", Style::new().fg(p.muted)).right_aligned();
        f.render_widget(close, filter);
        let h = body.height as usize;
        self.offset = self.offset.min(lines.len().saturating_sub(h));
        if lines.is_empty() {
            let none = Line::styled(" No keys match", Style::new().fg(p.muted));
            return f.render_widget(none, body);
        }
        let shown: Vec<Line> = lines.iter().skip(self.offset).take(h).cloned().collect();
        let text = Rect { width: body.width.saturating_sub(1), ..body };
        f.render_widget(Paragraph::new(shown), text);
        if lines.len() > h {
            draw::scrollbar(f, p, body, self.offset, lines.len());
        }
    }

    /// The bindings the filter matches, as drawn: a title over each group
    /// with any, keys in one column.
    fn lines(&self, p: &Palette, sections: &[Section]) -> Vec<Line<'static>> {
        let col = sections.iter().flat_map(|s| &s.keys).map(|(k, _)| str_width(k)).max().unwrap_or(0);
        let mut out = Vec::new();
        for s in sections {
            let keys: Vec<_> = s.keys.iter().filter(|(k, d)| self.matches(s.title, k, d)).collect();
            if keys.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push(Line::default());
            }
            out.push(Line::styled(format!(" {}", s.title), Style::new().fg(p.iris).add_modifier(Modifier::BOLD)));
            for (k, d) in keys {
                let pad = " ".repeat(col.saturating_sub(str_width(k)) + 2);
                out.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(k.clone(), Style::new().fg(p.gold).add_modifier(Modifier::BOLD)),
                    Span::raw(pad),
                    Span::styled(d.clone(), Style::new().fg(p.text)),
                ]));
            }
        }
        out
    }

    fn matches(&self, title: &str, key: &str, does: &str) -> bool {
        let q = self.filter.to_lowercase();
        q.is_empty() || [title, key, does].iter().any(|s| s.to_lowercase().contains(&q))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(h: &mut Help, code: KeyCode) -> Outcome {
        h.key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn filters_by_key_action_or_group_and_escape_clears_then_closes() {
        let p = Palette::load();
        let sections = [
            Section::new("Picker", &[("⏎", "go"), ("⌃X", "archive")]),
            Section::new("Scrollback", &[("y", "copy"), ("/", "search")]),
        ];
        let mut h = Help::default();
        assert_eq!(h.lines(&p, &sections).len(), 7);
        press(&mut h, KeyCode::Char('/'));
        for c in "copy".chars() {
            press(&mut h, KeyCode::Char(c));
        }
        let text: Vec<String> = h.lines(&p, &sections).iter().map(|l| l.to_string()).collect();
        assert_eq!(text.len(), 2, "{text:?}");
        assert!(text[0].contains("Scrollback") && text[1].contains("copy"));
        // A group's name matches all of it.
        h.filter = "picker".into();
        assert_eq!(h.lines(&p, &sections).len(), 3);
        // `q` is text while typing; Enter stops typing.
        press(&mut h, KeyCode::Char('q'));
        assert_eq!(h.filter, "pickerq");
        press(&mut h, KeyCode::Enter);
        assert_eq!(press(&mut h, KeyCode::Esc), Outcome::Stay);
        assert!(h.filter.is_empty());
        assert_eq!(press(&mut h, KeyCode::Esc), Outcome::Close);
    }
}
