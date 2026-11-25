//! The ctrl-h move-host float: move a live or ended personal task to another
//! compatible pool host (or `auto` for the least busy), carrying its branch,
//! uncommitted changes, and synced Atuin thread.
//!
//! Lists `auto` (when more than one other compatible host exists) followed by
//! each compatible host other than the task's current one, with cached load
//! and busy-agent metrics when switchyard-server has them. Enter asks for
//! confirmation (naming what happens to uncommitted files and the tab); Enter
//! or `y` runs it, anything else goes back. Escape, ctrl-c or ctrl-h closes
//! the float.

use std::collections::HashMap;
use std::hash::BuildHasher;

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Padding};
use switchyard_core::hosts::{AUTO, Probe};
use switchyard_ui::Palette;
use switchyard_ui::popup;

/// One selectable target host in the move-host chooser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub target: String,
    pub detail: String,
}

/// What a key asks of the picker.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Close,
    Help,
    /// Move the task to this target (`HOST` or `"auto"`).
    Move(String),
}

pub struct Chooser {
    /// The task's tab, as (`session_id`, `window_id`), which stays highlighted.
    pub key: (String, String),
    pub label: String,
    pub source_host: String,
    pub dirty: u64,
    pub ended: bool,
    entries: Vec<Entry>,
    selected: usize,
    /// When Some, asking whether to move to that target host.
    confirm: Option<String>,
}

/// Metadata about the task being moved, shown in the chooser header and
/// confirmation prompt.
pub struct Source {
    pub key: (String, String),
    pub label: String,
    pub host: String,
    pub dirty: u64,
    pub ended: bool,
}

impl Chooser {
    /// Build a chooser over `hosts` (compatible hosts other than `source.host`),
    /// prepending `auto` when there are multiple choices and annotating each
    /// host with cached `probes` when available.
    pub fn from_hosts<S: BuildHasher>(
        source: Source,
        hosts: &[String],
        local: &str,
        probes: &HashMap<String, Probe, S>,
    ) -> Self {
        let mut entries = Vec::new();
        if hosts.len() > 1 {
            entries.push(Entry {
                target: AUTO.to_string(),
                detail: "least busy".to_string(),
            });
        }
        for host in hosts {
            let detail = if let Some(p) = probes.get(host) {
                let s = if p.agents == 1 { "" } else { "s" };
                format!(
                    "load {:.2}/cpu · {:.0} GiB · {} busy agent{s}",
                    p.load, p.mem_gib, p.agents
                )
            } else if host == local {
                "local".to_string()
            } else {
                String::new()
            };
            entries.push(Entry {
                target: host.clone(),
                detail,
            });
        }
        Chooser {
            key: source.key,
            label: source.label,
            source_host: source.host,
            dirty: source.dirty,
            ended: source.ended,
            entries,
            selected: 0,
            confirm: None,
        }
    }

    pub fn key(&mut self, k: KeyEvent) -> Outcome {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(target) = self.confirm.take() {
            return match k.code {
                KeyCode::Enter | KeyCode::Char('y') if !ctrl => Outcome::Move(target),
                _ => Outcome::Stay,
            };
        }
        let last = self.entries.len().saturating_sub(1);
        match k.code {
            KeyCode::Esc => Outcome::Close,
            KeyCode::Char('c' | 'h') if ctrl => Outcome::Close,
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
                if let Some(entry) = self.entries.get(self.selected) {
                    self.confirm = Some(entry.target.clone());
                }
                Outcome::Stay
            }
            _ => Outcome::Stay,
        }
    }

    /// Return to list mode after a failed move so the user can retry or
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
                ("⏎", "move"),
                ("↑↓ j k", "choose"),
                ("esc", "cancel"),
                ("?", "keys"),
            ]
        }
    }

    /// The confirmation question shown at the bottom of the box, if armed.
    fn question(&self) -> Option<String> {
        let target = self.confirm.as_ref()?;
        let dest = if target == AUTO {
            "the least busy host".to_string()
        } else {
            target.clone()
        };
        let detail = if self.ended {
            "Moves branch and Atuin thread, opens a resumed tab there."
        } else if self.dirty > 0 {
            let s = if self.dirty == 1 { "" } else { "s" };
            return Some(format!(
                "Move {} to {dest}? Carries {} uncommitted file{s}, moves branch and Atuin thread, closes this tab.",
                self.label, self.dirty
            ));
        } else {
            "Moves branch and Atuin thread, opens tab there, closes this one."
        };
        Some(format!("Move {} to {dest}? {detail}", self.label))
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
        let mut sub_spans = vec![
            Span::styled("from ", Style::new().fg(p.muted)),
            Span::styled(self.source_host.clone(), Style::new().fg(p.subtle)),
        ];
        if self.dirty > 0 {
            let s = if self.dirty == 1 { "" } else { "s" };
            sub_spans.push(Span::styled(
                format!("  · {} uncommitted file{s} (carried over)", self.dirty),
                Style::new().fg(p.gold),
            ));
        }
        f.render_widget(Line::from(sub_spans), Rect { height: 1, ..inner });
        let rows = usize::from(inner.height.saturating_sub(2)).saturating_sub(below);
        if rows == 0 {
            return;
        }
        let name_col = self
            .entries
            .iter()
            .map(|e| e.target.len())
            .max()
            .unwrap_or(4)
            .max(4);
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
            let mut h_style = Style::new().fg(p.iris).bg(bg);
            if selected {
                h_style = h_style.add_modifier(Modifier::BOLD);
            }
            let mut spans = vec![Span::styled(
                format!("{:<name_col$}", entry.target),
                h_style,
            )];
            if !entry.detail.is_empty() {
                let d_style = Style::new()
                    .fg(if selected { p.text } else { p.muted })
                    .bg(bg);
                spans.push(Span::styled(format!("  {}", entry.detail), d_style));
            }
            f.render_widget(Line::from(spans), row);
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
    popup::block(p, "Move host")
        .padding(Padding::uniform(1))
        .title_top(right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use switchyard_core::hosts::Metrics;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn offers_auto_when_multiple_targets_exist_and_confirms_dirty_wording() {
        let mut probes = HashMap::new();
        probes.insert(
            "omarchy".into(),
            Probe::new(
                "omarchy",
                Metrics {
                    load: 0.25,
                    mem_gib: 24.0,
                },
                1,
                true,
            ),
        );
        let mut c = Chooser::from_hosts(
            Source {
                key: ("s".into(), "w".into()),
                label: "switchyard/demo".into(),
                host: "mac".into(),
                dirty: 3,
                ended: false,
            },
            &["omarchy".into(), "drummer".into()],
            "mac",
            &probes,
        );
        // First entry is auto when >= 2 hosts.
        assert_eq!(c.key(key(KeyCode::Enter)), Outcome::Stay);
        let q = c.question().unwrap();
        assert!(q.contains("the least busy host"), "{q}");
        assert!(q.contains("3 uncommitted files"), "{q}");
        // Escape returns to list.
        assert_eq!(c.key(key(KeyCode::Esc)), Outcome::Stay);
        assert!(c.question().is_none());
        // Move to omarchy and confirm with y.
        c.key(key(KeyCode::Char('j')));
        assert_eq!(c.key(key(KeyCode::Enter)), Outcome::Stay);
        assert_eq!(
            c.key(key(KeyCode::Char('y'))),
            Outcome::Move("omarchy".into())
        );
    }

    #[test]
    fn omits_auto_for_single_target_and_draws_cleanly() {
        use ratatui::{Terminal, backend::TestBackend};
        let p = Palette::load();
        let probes: HashMap<String, Probe> = HashMap::new();
        let mut c = Chooser::from_hosts(
            Source {
                key: ("s".into(), "w".into()),
                label: "switchyard/old".into(),
                host: "omarchy".into(),
                dirty: 0,
                ended: true,
            },
            &["mac".into()],
            "mac",
            &probes,
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
        assert!(text.contains("Move host"), "{text}");
        assert!(text.contains("from omarchy"), "{text}");
        assert!(text.contains("mac"), "{text}");
        assert!(text.contains("local"), "{text}");
        assert!(!text.contains("least busy"), "{text}");

        c.key(key(KeyCode::Enter));
        assert!(c.question().unwrap().contains("resumed tab"));
        assert_eq!(c.key(key(KeyCode::Enter)), Outcome::Move("mac".into()));
    }
}
