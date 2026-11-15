//! The keys float: every binding the picker has, grouped by where it
//! works, with the group for where John is first. `?` opens it from the
//! lists, F1 from anywhere (the reply box, snooze prompt and scrollback
//! take `?` as text or a search).
//!
//! `/` fuzzy-filters the bindings by key, action or group (Backspace edits,
//! ctrl-u clears, Enter or Escape stops typing); `j`/`k`, the arrows and
//! the page keys scroll; Escape clears the filter, then closes, as do `q`,
//! `?` and F1.

use nucleo_matcher::Matcher;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use switchyard_ui::Palette;
use switchyard_ui::draw::{self, str_width};
use switchyard_ui::fuzzy::score_fields;
use switchyard_ui::glyphs::ICON_SEARCH;
use switchyard_ui::popup;

/// A group of bindings: its title and (keys, what they do).
pub struct Section {
    pub title: &'static str,
    pub keys: Vec<(String, String)>,
}

impl Section {
    pub fn new(title: &'static str, keys: &[(&str, &str)]) -> Self {
        Section {
            title,
            keys: keys
                .iter()
                .map(|(k, d)| (k.to_string(), d.to_string()))
                .collect(),
        }
    }
}

#[derive(Default)]
pub struct Help {
    pub filter: String,
    /// The filter has the keys (after `/`).
    pub typing: bool,
    offset: usize,
    matcher: Matcher,
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

    /// The footer's hints while the keys are open.
    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.typing {
            vec![("⏎", "done"), ("⌃U", "clear")]
        } else if self.filter.is_empty() {
            vec![("/", "filter"), ("↑↓ j k", "scroll"), ("esc", "close")]
        } else {
            vec![("/", "filter"), ("↑↓ j k", "scroll"), ("esc", "clear")]
        }
    }

    /// The float over `area`: the filter, then the matching bindings under
    /// their group titles.
    pub fn draw(&mut self, f: &mut Frame, p: &Palette, area: Rect, sections: &[Section]) {
        let width = 64.min(area.width.saturating_sub(4));
        // Less the borders, the padding and the scrollbar's column.
        let lines = self.lines(p, sections, usize::from(width.saturating_sub(5)));
        let height = crate::cells(lines.len())
            .saturating_add(4)
            .clamp(8, area.height.saturating_sub(2).max(8))
            .min(area.height);
        let r = crate::centered(area, width, height);
        crate::backdrop(f, p);
        let inner = popup::open(f, r, popup::block(p, "Keys"));
        if inner.height < 3 {
            return;
        }
        let filter = Rect { height: 1, ..inner };
        let body = Rect {
            y: inner.y + 2,
            height: inner.height - 2,
            ..inner
        };
        let line = if self.typing || !self.filter.is_empty() {
            let mut spans = vec![
                Span::styled(format!("{ICON_SEARCH}  "), Style::new().fg(p.iris)),
                Span::styled(self.filter.clone(), Style::new().fg(p.text)),
            ];
            if self.typing {
                spans.push(Span::styled("▏", Style::new().fg(p.subtle)));
            }
            Line::from(spans)
        } else {
            Line::from(vec![
                Span::styled(format!("{ICON_SEARCH}  "), Style::new().fg(p.subtle)),
                Span::styled("Filter keys", Style::new().fg(p.muted)),
            ])
        };
        f.render_widget(line, filter);
        let h = body.height as usize;
        self.offset = self.offset.min(lines.len().saturating_sub(h));
        if lines.is_empty() {
            let none = Line::styled("No keys match", Style::new().fg(p.muted));
            return f.render_widget(none, body);
        }
        let shown: Vec<Line> = lines.iter().skip(self.offset).take(h).cloned().collect();
        let text = Rect {
            width: body.width.saturating_sub(1),
            ..body
        };
        f.render_widget(Paragraph::new(shown), text);
        if lines.len() > h {
            draw::scrollbar(f, p, body, self.offset, lines.len());
        }
    }

    /// The bindings the filter matches, as drawn: a title over each group
    /// with any, its keys in a column as wide as they need, each action cut
    /// to fit `width`.
    fn lines(&mut self, p: &Palette, sections: &[Section], width: usize) -> Vec<Line<'static>> {
        let pattern = Pattern::parse(&self.filter, CaseMatching::Smart, Normalization::Smart);
        let mut out = Vec::new();
        for s in sections {
            let keys: Vec<_> = s
                .keys
                .iter()
                .filter(|(k, d)| self.matches(&pattern, s.title, k, d))
                .collect();
            if keys.is_empty() {
                continue;
            }
            // Each group's keys get a column as wide as its widest key, so
            // one long key doesn't push every group's actions far right.
            let col = keys.iter().map(|(k, _)| str_width(k)).max().unwrap_or(0);
            if !out.is_empty() {
                out.push(Line::default());
            }
            out.push(Line::styled(
                s.title.to_string(),
                Style::new().fg(p.iris).add_modifier(Modifier::BOLD),
            ));
            for (k, d) in keys {
                let pad = " ".repeat(col.saturating_sub(str_width(k)) + 2);
                out.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        k.clone(),
                        Style::new().fg(p.gold).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(pad),
                    Span::styled(
                        crate::preview::clip_width(d, width.saturating_sub(col + 4)),
                        Style::new().fg(p.text),
                    ),
                ]));
            }
        }
        out
    }

    /// Whether the filter fuzzily matches a binding's group, keys and
    /// action, taken together so words can match across them.
    fn matches(&mut self, pattern: &Pattern, title: &str, key: &str, does: &str) -> bool {
        self.filter.is_empty()
            || score_fields(&mut self.matcher, pattern, &[title, key, does]).is_some()
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
        assert_eq!(h.lines(&p, &sections, 61).len(), 7);
        press(&mut h, KeyCode::Char('/'));
        for c in "copy".chars() {
            press(&mut h, KeyCode::Char(c));
        }
        let text: Vec<String> = h
            .lines(&p, &sections, 61)
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        assert_eq!(text.len(), 2, "{text:?}");
        assert!(text[0].contains("Scrollback") && text[1].contains("copy"));
        // It matches fuzzily, across the key and its action.
        h.filter = "⌃x arcv".into();
        let text: Vec<String> = h
            .lines(&p, &sections, 61)
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        assert!(text.len() == 2 && text[1].contains("archive"), "{text:?}");
        // A group's name matches all of it.
        h.filter = "picker".into();
        assert_eq!(h.lines(&p, &sections, 61).len(), 3);
        // `q` is text while typing; Enter stops typing.
        press(&mut h, KeyCode::Char('q'));
        assert_eq!(h.filter, "pickerq");
        press(&mut h, KeyCode::Enter);
        assert_eq!(press(&mut h, KeyCode::Esc), Outcome::Stay);
        assert_eq!(h.filter, "");
        assert_eq!(press(&mut h, KeyCode::Esc), Outcome::Close);
    }

    #[test]
    fn each_group_sizes_its_key_column_to_its_own_keys() {
        let p = Palette::load();
        let sections = [
            Section::new("Short", &[("y", "copy")]),
            Section::new("Long", &[("⌃F ⌃B PgDn PgUp", "a page")]),
        ];
        let text: Vec<String> = Help::default()
            .lines(&p, &sections, 61)
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        assert_eq!(text[1], "  y  copy");
        assert_eq!(text[4], "  ⌃F ⌃B PgDn PgUp  a page");
    }
}
