//! The ctrl-e switch-harness float: continue a live or archived task in
//! another harness (and model) in the same worktree via Atuin Threads.
//!
//! Lists the target harnesses (other than the task's current one) and their
//! configured models (from `naming::models`), with harnesses Atuin doesn't
//! support as `--in` targets (Antigravity `agy`) shown muted so the
//! degradation is clear. Enter on a supported target asks for confirmation
//! (naming what happens to the tab and worktree); Enter or `y` runs it,
//! anything else goes back. Escape, ctrl-c or ctrl-e closes the float.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Padding};
use switchyard_core::atuin;
use switchyard_core::naming::{HARNESSES, harness_spec, models};
use switchyard_ui::Palette;
use switchyard_ui::popup;

/// One selectable row in the harness chooser.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    harness: &'static str,
    model: &'static str,
    supported: bool,
}

impl Entry {
    fn spec(&self) -> String {
        harness_spec(self.harness, (!self.model.is_empty()).then_some(self.model))
    }
}

/// What a key asks of the picker.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Close,
    Help,
    /// Continue the task in this `HARNESS[:MODEL]` spec.
    Switch(String),
    /// Why the highlighted target can't be used, for the footer.
    Refuse(String),
}

pub struct Chooser {
    /// The task's tab, as (`session_id`, `window_id`), which stays highlighted.
    pub key: (String, String),
    pub label: String,
    pub source: String,
    pub current_model: String,
    pub ended: bool,
    entries: Vec<Entry>,
    selected: usize,
    /// When Some, asking whether to continue in that `HARNESS[:MODEL]` spec.
    confirm: Option<String>,
}

impl Chooser {
    /// Build a chooser for continuing a task currently on `source` (and
    /// optional `current_model`) in another harness.
    pub fn new(
        key: (String, String),
        label: String,
        source: String,
        current_model: String,
        ended: bool,
    ) -> Self {
        let source = if source.is_empty() {
            "claude".to_string()
        } else {
            source
        };
        let mut supported = Vec::new();
        let mut unsupported = Vec::new();
        for harness in HARNESSES {
            if harness == source {
                continue;
            }
            if atuin::harness_name(harness).is_some() {
                for &model in models(harness) {
                    supported.push(Entry {
                        harness,
                        model,
                        supported: true,
                    });
                }
            } else {
                unsupported.push(Entry {
                    harness,
                    model: "",
                    supported: false,
                });
            }
        }
        supported.extend(unsupported);
        Chooser {
            key,
            label,
            source,
            current_model,
            ended,
            entries: supported,
            selected: 0,
            confirm: None,
        }
    }

    pub fn key(&mut self, k: KeyEvent) -> Outcome {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(spec) = self.confirm.take() {
            return match k.code {
                KeyCode::Enter | KeyCode::Char('y') if !ctrl => Outcome::Switch(spec),
                _ => Outcome::Stay,
            };
        }
        let last = self.entries.len().saturating_sub(1);
        match k.code {
            KeyCode::Esc => Outcome::Close,
            KeyCode::Char('c' | 'e') if ctrl => Outcome::Close,
            KeyCode::Char('?') => Outcome::Help,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                Outcome::Stay
            }
            KeyCode::Char('p') if ctrl => {
                self.selected = self.selected.saturating_sub(1);
                Outcome::Stay
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(last);
                Outcome::Stay
            }
            KeyCode::Char('n') if ctrl => {
                self.selected = (self.selected + 1).min(last);
                Outcome::Stay
            }
            KeyCode::Home | KeyCode::Char('g') if !ctrl => {
                self.selected = 0;
                Outcome::Stay
            }
            KeyCode::End | KeyCode::Char('G') if !ctrl => {
                self.selected = last;
                Outcome::Stay
            }
            KeyCode::Enter if !ctrl => {
                let Some(entry) = self.entries.get(self.selected) else {
                    return Outcome::Stay;
                };
                if entry.supported {
                    self.confirm = Some(entry.spec());
                    Outcome::Stay
                } else {
                    Outcome::Refuse(format!(
                        "Atuin doesn't support {} as a target harness (claude and pi only)",
                        entry.harness
                    ))
                }
            }
            _ => Outcome::Stay,
        }
    }

    /// Return to list mode after a failed switch so the user can retry or
    /// cancel.
    pub fn failed(&mut self) {
        self.confirm = None;
    }

    /// The footer's hints while the chooser is open.
    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.confirm.is_some() {
            vec![("⏎ y", "yes"), ("esc", "no")]
        } else {
            vec![
                ("⏎", "continue"),
                ("↑↓ j k", "choose"),
                ("esc", "cancel"),
                ("?", "keys"),
            ]
        }
    }

    /// The confirmation question shown at the bottom of the box, if armed.
    fn question(&self) -> Option<String> {
        let spec = self.confirm.as_ref()?;
        let detail = if self.ended {
            "Recreates the worktree and opens in a new tab."
        } else {
            "Replaces the running agent in its tab; worktree stays."
        };
        Some(format!("Continue {} in {spec}? {detail}", self.label))
    }

    /// Draw the chooser box over `area`.
    pub fn draw(&self, f: &mut Frame, area: Rect, p: &Palette) {
        crate::backdrop(f, p);
        let text_w = usize::from(popup::LIST_WIDTH.min(area.width).saturating_sub(4)).max(1);
        let foot = self
            .question()
            .map_or_else(Vec::new, |q| crate::preview::wrap(&q, text_w));
        let below = if foot.is_empty() { 0 } else { foot.len() + 1 };
        // Subtitle + blank line + entries + optional confirm + border/padding.
        let height = crate::cells(2 + self.entries.len() + below).saturating_add(4);
        let rect = popup::list_rect(area, height);
        let block = list_block(p, &self.label);
        let inner = popup::open(f, rect, block);
        if inner.height < 2 {
            return;
        }
        let from = harness_spec(
            &self.source,
            (!self.current_model.is_empty()).then_some(self.current_model.as_str()),
        );
        let sub = Line::from(vec![
            Span::styled("from ", Style::new().fg(p.muted)),
            Span::styled(from, Style::new().fg(p.subtle)),
        ]);
        f.render_widget(sub, Rect { height: 1, ..inner });
        let rows = usize::from(inner.height.saturating_sub(2)).saturating_sub(below);
        if rows == 0 {
            return;
        }
        let offset = self.selected.saturating_sub(rows.saturating_sub(1));
        for (n, entry) in self.entries.iter().enumerate().skip(offset).take(rows) {
            let selected = n == self.selected;
            let row = Rect {
                y: inner.y + 2 + crate::cells(n - offset),
                height: 1,
                ..inner
            };
            if selected {
                popup::select(f, p, inner, row);
                let bar = Rect {
                    x: row.x.saturating_sub(1),
                    width: 1,
                    ..row
                };
                f.render_widget(
                    Span::styled("▌", Style::new().fg(p.iris).bg(p.overlay)),
                    bar,
                );
            }
            let bg = if selected { p.overlay } else { p.base };
            let line = if entry.supported {
                let mut h_style = Style::new().fg(p.iris).bg(bg);
                if selected {
                    h_style = h_style.add_modifier(Modifier::BOLD);
                }
                let model = if entry.model.is_empty() {
                    Span::styled("  default", Style::new().fg(p.muted).bg(bg))
                } else {
                    let mut m_style = Style::new().fg(p.text).bg(bg);
                    if selected {
                        m_style = m_style.add_modifier(Modifier::BOLD);
                    }
                    Span::styled(format!("  {}", entry.model), m_style)
                };
                Line::from(vec![
                    Span::styled(format!("{:<6}", entry.harness), h_style),
                    model,
                ])
            } else {
                Line::from(vec![
                    Span::styled(
                        format!("{:<6}", entry.harness),
                        Style::new().fg(p.muted).bg(bg),
                    ),
                    Span::styled("  not supported by Atuin", Style::new().fg(p.muted).bg(bg)),
                ])
            };
            f.render_widget(line, row);
        }
        if !foot.is_empty() {
            let top = inner.bottom().saturating_sub(crate::cells(foot.len()));
            for (i, line) in foot.into_iter().enumerate() {
                let style = Style::new().fg(p.gold).add_modifier(Modifier::BOLD);
                f.render_widget(
                    Span::styled(line, style),
                    Rect {
                        y: top + crate::cells(i),
                        height: 1,
                        ..inner
                    },
                );
            }
        }
    }
}

fn list_block(p: &Palette, label: &str) -> Block<'static> {
    let right = Line::from(vec![
        Span::styled(format!(" {label} "), Style::new().fg(p.muted)),
        Span::styled("─", Style::new().fg(p.iris)),
    ])
    .right_aligned();
    popup::block(p, "Switch harness")
        .padding(Padding::uniform(1))
        .title_top(right)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn claude_task_offers_pi_models_and_marks_agy_unsupported() {
        let mut c = Chooser::new(
            ("s".into(), "w".into()),
            "switchyard/demo".into(),
            "claude".into(),
            "opus".into(),
            false,
        );
        // First entry is pi's default model.
        assert_eq!(c.key(key(KeyCode::Enter)), Outcome::Stay);
        assert!(c.question().unwrap().contains("in pi?"));
        assert!(c.question().unwrap().contains("Replaces the running agent"));
        assert_eq!(c.key(key(KeyCode::Enter)), Outcome::Switch("pi".into()));

        // Last entry is agy, refused with a clear message.
        c.key(key(KeyCode::End));
        match c.key(key(KeyCode::Enter)) {
            Outcome::Refuse(why) => assert!(why.contains("agy") && why.contains("Atuin"), "{why}"),
            other => panic!("expected Refuse for agy, got {other:?}"),
        }
    }

    #[test]
    fn pi_task_offers_claude_models_and_confirms_archived_wording() {
        let mut c = Chooser::new(
            ("s".into(), "w".into()),
            "switchyard/old".into(),
            "pi".into(),
            String::new(),
            true,
        );
        // Move to claude's third entry (`opus`: index 2 after "" and "fable").
        c.key(key(KeyCode::Char('j')));
        c.key(key(KeyCode::Char('j')));
        assert_eq!(c.key(key(KeyCode::Enter)), Outcome::Stay);
        assert!(c.question().unwrap().contains("in claude:opus?"));
        assert!(c.question().unwrap().contains("Recreates the worktree"));
        // Escape cancels confirm back to the list; another Escape closes.
        assert_eq!(c.key(key(KeyCode::Esc)), Outcome::Stay);
        assert!(c.question().is_none());
        assert_eq!(c.key(key(KeyCode::Enter)), Outcome::Stay);
        assert_eq!(
            c.key(key(KeyCode::Char('y'))),
            Outcome::Switch("claude:opus".into())
        );
        assert_eq!(c.key(key(KeyCode::Esc)), Outcome::Close);
    }

    #[test]
    fn draws_list_and_confirmation_cleanly() {
        use ratatui::{Terminal, backend::TestBackend};
        let p = Palette::load();
        let mut c = Chooser::new(
            ("s".into(), "w".into()),
            "switchyard/demo".into(),
            "claude".into(),
            "opus".into(),
            false,
        );
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| c.draw(f, f.area(), &p)).unwrap();
        let text: String = (0..24)
            .map(|y| {
                (0..80)
                    .map(|x| term.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Switch harness"), "{text}");
        assert!(text.contains("from claude:opus"), "{text}");
        assert!(text.contains("pi"), "{text}");
        assert!(text.contains("not supported by Atuin"), "{text}");

        c.key(key(KeyCode::Enter));
        term.draw(|f| c.draw(f, f.area(), &p)).unwrap();
        let confirm_text: String = (0..24)
            .map(|y| {
                (0..80)
                    .map(|x| term.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            confirm_text.contains("Continue switchyard/demo in pi?"),
            "{confirm_text}"
        );
    }
}
