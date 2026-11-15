//! The Archive's project chooser, opened with Tab as in the Work view: a
//! filter over the projects of the ended tasks, led by "All projects".
//! Type to filter, Enter to show the highlighted entry, Escape or Tab to
//! close it unchanged.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use switchyard_ui::Palette;
use switchyard_ui::draw::delete_word;
use switchyard_ui::fuzzy::score_fields;
use switchyard_ui::popup;

/// The first entry, which lists every project.
const ALL: &str = "All projects";

/// What a key asks of the picker.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Still open.
    Stay,
    /// Closed, the filter unchanged.
    Close,
    /// Closed on a pick: a project, or None for all of them.
    Pick(Option<String>),
}

pub struct Chooser {
    /// The archive's projects when it opened, sorted.
    projects: Vec<String>,
    /// The project shown when it opened, marked in its list.
    current: Option<String>,
    pub query: String,
    /// The highlighted entry: 0 is all, else 1 + an index into `projects`.
    selected: usize,
}

impl Chooser {
    /// A chooser over `projects`, on `current` (None is all).
    pub fn new(projects: Vec<String>, current: Option<String>) -> Self {
        let selected = current
            .as_ref()
            .and_then(|c| projects.iter().position(|p| p == c))
            .map_or(0, |i| i + 1);
        Chooser {
            projects,
            current,
            query: String::new(),
            selected,
        }
    }

    fn label(&self, entry: usize) -> &str {
        match entry {
            0 => ALL,
            i => &self.projects[i - 1],
        }
    }

    /// The entries matching the filter, best first: every one, in order,
    /// while it is empty.
    fn shown(&self) -> Vec<usize> {
        let all = 0..=self.projects.len();
        if self.query.trim().is_empty() {
            return all.collect();
        }
        let pattern = Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart);
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut scored: Vec<(u32, usize)> = all
            .filter_map(|e| {
                score_fields(&mut matcher, &pattern, &[self.label(e)]).map(|(s, _)| (s, e))
            })
            .collect();
        scored.sort_by_key(|a| std::cmp::Reverse(a.0));
        scored.into_iter().map(|(_, e)| e).collect()
    }

    pub fn key(&mut self, k: KeyEvent) -> Outcome {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shown = self.shown();
        let at = shown.iter().position(|&e| e == self.selected).unwrap_or(0);
        let last = shown.len().saturating_sub(1);
        let to = |i: usize| shown.get(i).copied().unwrap_or(0);
        match k.code {
            KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab => return Outcome::Close,
            KeyCode::Char('c') if ctrl => return Outcome::Close,
            KeyCode::Enter => {
                return match shown.get(at) {
                    Some(0) => Outcome::Pick(None),
                    Some(&e) => Outcome::Pick(Some(self.label(e).to_string())),
                    None => Outcome::Stay,
                };
            }
            KeyCode::Up => self.selected = to(at.saturating_sub(1)),
            KeyCode::Char('p' | 'k') if ctrl => self.selected = to(at.saturating_sub(1)),
            KeyCode::Down => self.selected = to((at + 1).min(last)),
            KeyCode::Char('n' | 'j') if ctrl => self.selected = to((at + 1).min(last)),
            KeyCode::Home => self.selected = to(0),
            KeyCode::End => self.selected = to(last),
            KeyCode::Char('u') if ctrl => self.edit(String::clear),
            KeyCode::Char('w') if ctrl => self.edit(delete_word),
            KeyCode::Backspace if alt || ctrl => self.edit(delete_word),
            KeyCode::Backspace => self.edit(|q| {
                q.pop();
            }),
            KeyCode::Char(ch) if !ctrl && !alt => self.edit(|q| q.push(ch)),
            _ => {}
        }
        Outcome::Stay
    }

    /// Change the filter, then highlight its best match.
    fn edit(&mut self, change: impl FnOnce(&mut String)) {
        change(&mut self.query);
        if let Some(&best) = self.shown().first() {
            self.selected = best;
        }
    }

    pub fn hints() -> Vec<(&'static str, &'static str)> {
        vec![("⏎", "show"), ("esc", "back")]
    }

    /// A box over `area` with the filter and the matching entries, the
    /// one shown now in iris.
    pub fn draw(&self, f: &mut Frame, area: Rect, p: &Palette) {
        let shown = self.shown();
        crate::backdrop(f, p);
        let height = crate::cells(shown.len() + 3).max(4);
        let r = popup::list_rect(area, height);
        let inner = popup::open(f, r, popup::block(p, "Project"));
        if inner.height < 2 {
            return;
        }
        let query = if self.query.is_empty() {
            Line::styled("type to filter", Style::new().fg(p.muted))
        } else {
            Line::from(vec![
                Span::styled(self.query.clone(), Style::new().fg(p.text)),
                Span::styled("▏", Style::new().fg(p.iris)),
            ])
        };
        f.render_widget(query, Rect { height: 1, ..inner });
        let rows = inner.height as usize - 1;
        let at = shown.iter().position(|&e| e == self.selected).unwrap_or(0);
        let offset = at.saturating_sub(rows.saturating_sub(1));
        let current = match &self.current {
            None => 0,
            Some(c) => self
                .projects
                .iter()
                .position(|p| p == c)
                .map_or(0, |i| i + 1),
        };
        for (n, &e) in shown.iter().enumerate().skip(offset).take(rows) {
            let selected = e == self.selected;
            let row = Rect {
                y: inner.y + 1 + crate::cells(n - offset),
                height: 1,
                ..inner
            };
            if selected {
                popup::select(f, p, inner, row);
            }
            let bg = if selected { p.overlay } else { p.base };
            let mut style = Style::new().fg(p.text).bg(bg);
            if e == current {
                style = style.fg(p.iris);
            }
            if selected {
                style = style.add_modifier(Modifier::BOLD);
            }
            f.render_widget(Span::styled(self.label(e).to_string(), style), row);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: &mut Chooser, code: KeyCode) -> Outcome {
        c.key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn opens_on_the_shown_project_and_filters_to_a_pick() {
        let projects = vec!["alpha".to_string(), "beta".to_string()];
        let mut c = Chooser::new(projects.clone(), Some("beta".into()));
        assert_eq!(c.selected, 2);
        // All leads the list, so Home and Enter show every project.
        key(&mut c, KeyCode::Home);
        assert_eq!(key(&mut c, KeyCode::Enter), Outcome::Pick(None));
        // Typing highlights the best match.
        let mut c = Chooser::new(projects, None);
        for ch in "alp".chars() {
            key(&mut c, KeyCode::Char(ch));
        }
        assert_eq!(
            key(&mut c, KeyCode::Enter),
            Outcome::Pick(Some("alpha".into()))
        );
        assert_eq!(key(&mut c, KeyCode::Tab), Outcome::Close);
    }
}
