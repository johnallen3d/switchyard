//! Read-only Usage view, shared by the float's pastes and standalone feed.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use switchyard_core::usage::{STALE, Snapshot, Total};
use switchyard_ui::{Palette, draw};

#[derive(Default)]
pub struct Usage {
    pub snapshot: Option<Snapshot>,
    pub error: Option<String>,
    pub offset: usize,
    totals: Vec<Total>,
}

fn amount(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.2}m", n as f64 / 1_000_000.0)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

fn duration(secs: u64) -> String {
    if secs >= 86400 {
        format!("{}d {}h", secs / 86400, secs % 86400 / 3600)
    } else if secs >= 3600 {
        format!("{}h {}m", secs / 3600, secs % 3600 / 60)
    } else {
        format!("{}m", secs.div_ceil(60))
    }
}

fn matches(query: &str, profile: Option<&str>, row_profile: &str, text: &str) -> bool {
    profile.is_none_or(|p| p == row_profile)
        && query.to_lowercase().split_whitespace().all(|q| text.to_lowercase().contains(q))
}

impl Usage {
    pub fn paste(&mut self, body: &str) {
        match serde_json::from_str::<Snapshot>(body) {
            Ok(mut snapshot) => {
                snapshot.dedup();
                self.totals = snapshot.totals();
                self.snapshot = Some(snapshot);
                self.error = None;
            }
            Err(_) => self.error = Some("Incompatible usage data; keeping the last snapshot".into()),
        }
    }

    fn lines(&self, p: &Palette, now: u64, query: &str, profile: Option<&str>) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let heading = |s: String| Line::styled(s, Style::new().fg(p.iris).add_modifier(Modifier::BOLD));
        let muted = |s: String| Line::styled(s, Style::new().fg(p.muted));
        let warning = |s: String| Line::styled(s, Style::new().fg(p.gold));
        if let Some(e) = &self.error {
            lines.push(warning(e.clone()));
        }
        let Some(snap) = &self.snapshot else {
            lines.push(muted("Loading subscription caches and session counters…".into()));
            return lines;
        };
        let age = now.saturating_sub(snap.fetched);
        lines.push(muted(format!(
            "Updated {} ago{} · accounts pooled across hosts",
            duration(age),
            if age > STALE { " · STALE" } else { "" }
        )));
        lines.push(heading("Subscription limits · account snapshots, not added across hosts".into()));
        let mut count = 0;
        for q in &snap.quotas {
            let title = format!("{} · {} {} · {}", q.account, q.provider, q.plan, q.hosts.join(", "));
            if !matches(query, profile, &q.profile, &title) {
                continue;
            }
            count += 1;
            let stale = q.fetched == 0 || now.saturating_sub(q.fetched) > STALE;
            let state = format!("{}{}", q.status, if stale { " · STALE" } else { "" });
            lines.push(Line::styled(
                format!("  {title} · {state}"),
                Style::new().fg(if stale || q.status != "ok" { p.gold } else { p.text }),
            ));
            for w in &q.windows {
                let used = w.used.map(|n| format!("{n:.0}% used")).unwrap_or_else(|| "usage unknown".into());
                let reset = match w.reset {
                    Some(at) if at > now => format!("resets in {}", duration(at - now)),
                    Some(_) => "reset due · awaiting collector refresh".into(),
                    None => "reset unknown".into(),
                };
                lines.push(Line::styled(
                    format!("    {:>4}  {used} · {reset} · {}", w.label, w.status),
                    Style::new().fg(if w.used.is_some_and(|n| n >= 90.0) || w.status != "ok" {
                        p.gold
                    } else {
                        p.subtle
                    }),
                ));
            }
            if q.windows.is_empty() {
                lines.push(muted("    No quota windows reported".into()));
            }
            if let Some(credits) = &q.credits {
                lines.push(muted(format!("    Credits: {credits}")));
            }
        }
        if count == 0 {
            lines.push(muted("  No matching quota data · configure usage.quotas for each account".into()));
        }
        lines.push(Line::default());
        lines.push(heading(
            "Session usage · rolling 7 days · API-equivalent USD estimates, not subscription bills".into(),
        ));
        let mut count = 0;
        for t in &self.totals {
            let title =
                format!("{} · {} / {} · {} · {}", t.account, t.harness, t.provider, t.model, t.hosts.join(", "));
            if !matches(query, profile, &t.profile, &title) {
                continue;
            }
            count += 1;
            let cost = if t.unpriced == t.requests {
                "cost unknown (no recorded price or configured rate)".into()
            } else if t.unpriced > 0 {
                format!("≈ ${:.4} known portion · {} unpriced requests", t.cost, t.unpriced)
            } else {
                format!("≈ ${:.4}", t.cost)
            };
            lines.push(Line::styled(format!("  {title}"), Style::new().fg(p.text)));
            lines.push(Line::styled(
                format!("    {} tokens · {} requests · {cost}", amount(t.tokens.total()), t.requests),
                Style::new().fg(p.foam),
            ));
            lines.push(muted(format!(
                "    in {} · out {} · cache read {} · cache write {}",
                amount(t.tokens.input),
                amount(t.tokens.output),
                amount(t.tokens.cache_read),
                amount(t.tokens.cache_write)
            )));
        }
        if count == 0 {
            lines.push(muted("  No matching recorded usage in the last 7 days".into()));
        }
        lines.push(Line::default());
        lines.push(muted(
            "Coverage: configured/active profiles only; missing logs and unknown prices are not zero usage.".into(),
        ));
        lines.push(muted(
            "Shared account labels deduplicate copied events; distinct accounts must use distinct labels.".into(),
        ));
        if !snap.warnings.is_empty() {
            lines.push(heading("Partial coverage / collector warnings".into()));
            lines.extend(snap.warnings.iter().map(|w| warning(format!("  {w}"))));
        }
        lines
    }

    pub fn draw(&mut self, f: &mut Frame, area: Rect, p: &Palette, now: u64, query: &str, profile: Option<&str>) {
        // Wrap before applying the scroll offset, including warnings and long
        // model names. In a narrow float, unknown prices must not be clipped.
        let width = area.width.saturating_sub(2).max(1) as usize;
        let lines: Vec<_> = self
            .lines(p, now, query, profile)
            .into_iter()
            .flat_map(|line| {
                let style = line.style;
                let mut wrapped = Vec::new();
                let mut part = String::new();
                let mut used = 0;
                for c in line.to_string().chars() {
                    let w = draw::str_width(&c.to_string());
                    if used + w > width && !part.is_empty() {
                        wrapped.push(Line::styled(std::mem::take(&mut part), style));
                        used = 0;
                    }
                    part.push(c);
                    used += w;
                }
                wrapped.push(Line::styled(part, style));
                wrapped
            })
            .collect();
        self.offset = self.offset.min(lines.len().saturating_sub(area.height as usize));
        let overflow = lines.len() > area.height as usize;
        let body = Rect { width: area.width.saturating_sub(if overflow { 2 } else { 0 }), ..area };
        let shown: Vec<_> = lines.iter().skip(self.offset).take(area.height as usize).cloned().collect();
        f.render_widget(Paragraph::new(shown), body);
        if overflow {
            draw::scrollbar(f, p, area, self.offset, lines.len());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_and_missing_cost_are_not_zero() {
        let usage = Usage { snapshot: Some(Snapshot { fetched: 1, ..Snapshot::default() }), ..Usage::default() };
        let text = usage
            .lines(&Palette::load(), 1000, "", None)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("STALE"));
        assert!(text.contains("not zero usage"));
        assert!(text.contains("No matching quota data"));
    }

    #[test]
    fn unknown_cost_is_visible_and_profile_filter_is_applied() {
        let mut usage = Usage::default();
        usage.paste(
            &serde_json::json!({"fetched":1000,"quotas":[],"warnings":[],"samples":[{
                "id":"request","harness":"pi","account":"work-login","profile":"work","model":"model",
                "provider":"subscription","at":999,"tokens":{"input":100,"output":10,"cache_read":20,"cache_write":0},
                "cost":null,"hosts":["mac"]
            }]})
            .to_string(),
        );
        let text = |profile| {
            usage
                .lines(&Palette::load(), 1000, "", profile)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(text(Some("work")).contains("cost unknown"));
        assert!(!text(Some("work")).contains("$0.0000"));
        assert!(!text(Some("personal")).contains("work-login"));
    }

    #[test]
    fn invalid_delivery_keeps_previous_data() {
        let mut usage = Usage::default();
        usage.paste(r#"{"fetched":123,"samples":[],"quotas":[],"warnings":[]}"#);
        usage.paste("broken");
        assert_eq!(usage.snapshot.unwrap().fetched, 123);
        assert!(usage.error.is_some());
    }
}
