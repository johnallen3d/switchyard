//! Read-only Usage view, shared by the float's pastes and standalone feed.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use switchyard_core::usage::{Quota, STALE, Snapshot, Total};
use switchyard_ui::{Palette, draw};

#[derive(Default)]
pub struct Usage {
    pub snapshot: Option<Snapshot>,
    pub error: Option<String>,
    pub offset: usize,
    totals: Vec<Total>,
}

fn amount(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.2}b", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.2}m", n as f64 / 1_000_000.0)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

fn duration(secs: u64) -> String {
    if secs < 60 {
        return "<1m".into();
    }
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
        && query
            .to_lowercase()
            .split_whitespace()
            .all(|q| text.to_lowercase().contains(q))
}

fn span(text: impl Into<String>, color: Color) -> Span<'static> {
    Span::styled(text.into(), Style::new().fg(color))
}

fn bold(text: impl Into<String>, color: Color) -> Span<'static> {
    Span::styled(text.into(), Style::new().fg(color).add_modifier(Modifier::BOLD))
}

/// Word-wrap without flattening spans: numbers, warnings, and bars retain their
/// contrast after wrapping, including long model names and collector errors.
fn wrap(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut row = Line::default().style(line.style);
    let mut used = 0;
    for s in line.spans {
        for word in s.content.split_inclusive(char::is_whitespace) {
            let len = draw::str_width(word.trim_end());
            if used > 0 && used + len > width {
                rows.push(row);
                row = Line::default().style(line.style);
                used = 0;
            }
            let mut part = String::new();
            for c in word.chars() {
                let len = draw::str_width(&c.to_string());
                if used + len > width {
                    row.spans.push(Span::styled(std::mem::take(&mut part), s.style));
                    rows.push(row);
                    row = Line::default().style(line.style);
                    used = 0;
                }
                // Whitespace at a wrap boundary should not create a blank row.
                if used == 0 && c.is_whitespace() {
                    continue;
                }
                part.push(c);
                used += len;
            }
            if !part.is_empty() {
                row.spans.push(Span::styled(part, s.style));
            }
        }
    }
    if used > 0 || rows.is_empty() {
        rows.push(row);
    }
    rows
}

/// Split rows into profile groups (personal, work, …) in a stable order so the
/// two accounts never interleave. Unlabelled rows sort last.
fn by_profile<'a, T>(rows: &[&'a T], profile: impl Fn(&T) -> &str) -> Vec<(String, Vec<&'a T>)> {
    let mut groups: Vec<(String, Vec<&'a T>)> = Vec::new();
    for &row in rows {
        let name = profile(row);
        match groups.iter_mut().find(|(n, _)| n == name) {
            Some((_, items)) => items.push(row),
            None => groups.push((name.to_string(), vec![row])),
        }
    }
    groups.sort_by(|(a, _), (b, _)| (a.is_empty(), a).cmp(&(b.is_empty(), b)));
    groups
}

/// A full-width labelled rule that separates profile groups.
fn profile_rule(name: &str, p: &Palette, width: usize) -> Line<'static> {
    let label = if name.is_empty() {
        "OTHER".to_string()
    } else {
        name.to_uppercase()
    };
    let label = format!(" {label} ");
    let rest = width.saturating_sub(draw::str_width(&label) + 2);
    Line::from(vec![
        span("──", p.hl_high),
        bold(label, p.rose),
        span("─".repeat(rest), p.hl_high),
    ])
}

fn stale(q: &Quota, now: u64) -> bool {
    q.fetched == 0 || now.saturating_sub(q.fetched) > STALE
}

fn attention(q: &Quota, now: u64) -> bool {
    stale(q, now)
        || q.status != "ok"
        || q.windows
            .iter()
            .any(|w| w.status != "ok" || w.used.is_some_and(|n| n >= 90.0))
}

/// Each card is a small quota table. RESET IN applies to every window, so the
/// values need no repeated prose. Unknown and expired resets are never zero.
fn quota_card(q: &Quota, p: &Palette, now: u64, width: usize) -> Vec<Line<'static>> {
    let inner = width.saturating_sub(4).max(1);
    let state = if stale(q, now) {
        "STALE".into()
    } else if q.status != "ok" {
        q.status.replace('_', " ")
    } else if q.windows.iter().any(|w| w.used.is_some_and(|n| n >= 100.0)) {
        "At limit".into()
    } else if q.windows.iter().any(|w| w.used.is_some_and(|n| n >= 90.0)) {
        "Near limit".into()
    } else {
        String::new()
    };
    let color = if q.windows.iter().any(|w| w.used.is_some_and(|n| n >= 100.0)) {
        p.love
    } else {
        p.gold
    };
    let mut title = Line::from(bold(&q.provider, p.text));
    if !state.is_empty() {
        title.spans.push(bold(format!(" · {state}"), color));
    }
    let mut body = Vec::new();
    let title_in_border = title.width() + 5 <= width;
    if !title_in_border {
        body.push(title.clone());
    }
    let meta = [&q.plan, &q.hosts.join(", ")]
        .into_iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" · ");
    body.push(Line::from(vec![
        bold(&q.account, p.iris),
        span(
            if meta.is_empty() {
                String::new()
            } else {
                format!(" · {meta}")
            },
            p.subtle,
        ),
    ]));
    // The compact layout still leaves room for a meter at the grid breakpoint.
    let bar_width = inner.saturating_sub(24).clamp(4, 12);
    let left_width = 5 + bar_width + 6;
    let reset_width = inner.saturating_sub(left_width).max(8);
    body.push(Line::from(vec![
        span(format!("{:<left_width$}", "WINDOW / USED"), p.subtle),
        span(format!("{:>reset_width$}", "RESET IN"), p.subtle),
    ]));
    for w in &q.windows {
        let used = w.used.filter(|n| n.is_finite());
        let color = match used {
            Some(n) if n >= 100.0 => p.love,
            Some(n) if n >= 90.0 => p.gold,
            _ if w.status != "ok" => p.gold,
            _ => p.foam,
        };
        let filled = used.map(|n| ((n.clamp(0.0, 100.0) / 100.0) * bar_width as f64).round() as usize);
        let meter = if let Some(n) = filled {
            // Half-cell strokes leave gaps between ticks; a thin dashed track
            // avoids solid blocks merging visually across adjacent quota rows.
            vec![span("╸".repeat(n), color), span("┄".repeat(bar_width - n), p.hl_med)]
        } else {
            vec![span("·".repeat(bar_width), p.subtle)]
        };
        let reset = match w.reset {
            Some(at) if at > now => duration(at - now),
            Some(_) => "due ↻".into(),
            None => "unknown".into(),
        };
        let mut row = vec![span(format!("{:<4} ", w.label), p.text)];
        row.extend(meter);
        row.push(bold(
            used.map(|n| format!("{n:>4.0}% ")).unwrap_or_else(|| "   ?  ".into()),
            color,
        ));
        row.push(span(
            format!("{reset:>reset_width$}"),
            if w.reset.is_some_and(|at| at <= now) {
                p.gold
            } else {
                p.text
            },
        ));
        body.push(Line::from(row));
        if w.status != "ok" && w.status != q.status {
            body.push(Line::from(span(
                format!("{}: {}", w.label, w.status.replace('_', " ")),
                p.gold,
            )));
        }
    }
    if q.windows.is_empty() {
        body.push(Line::from(span("No quota windows reported", p.gold)));
    }
    if let Some(credits) = &q.credits {
        body.push(Line::from(span(format!("Credits: {credits}"), p.subtle)));
    }
    let border = if attention(q, now) { p.gold } else { p.hl_med };
    let top = if title_in_border {
        let remaining = width - title.width() - 5;
        let mut spans = vec![span("╭─ ", border)];
        spans.extend(title.spans);
        spans.push(span(format!(" {}╮", "─".repeat(remaining)), border));
        Line::from(spans)
    } else {
        Line::from(span(format!("╭{}╮", "─".repeat(width.saturating_sub(2))), border))
    };
    let mut rows = vec![top];
    // Fill only the cells between the border glyphs; a background on the
    // glyph cells themselves bleeds past the thin vertical rules.
    let fill = Style::new().bg(p.surface);
    for line in body.into_iter().flat_map(|line| wrap(line, inner)) {
        let padding = inner.saturating_sub(line.width());
        let mut spans = vec![span("│", border), Span::styled(" ", fill)];
        spans.extend(line.spans.into_iter().map(|s| {
            let style = fill.patch(s.style);
            s.style(style)
        }));
        spans.push(Span::styled(" ".repeat(padding + 1), fill));
        spans.push(span("│", border));
        rows.push(Line::from(spans));
    }
    rows.push(Line::from(span(
        format!("╰{}╯", "─".repeat(width.saturating_sub(2))),
        border,
    )));
    rows
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

    fn lines(&self, p: &Palette, now: u64, query: &str, profile: Option<&str>, width: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let secondary = |s: String| Line::from(span(s, p.subtle));
        if let Some(e) = &self.error {
            lines.push(Line::from(bold(format!("! {e}"), p.gold)));
        }
        let Some(snap) = &self.snapshot else {
            lines.push(Line::from(bold("Loading usage…", p.text)));
            lines.push(secondary("Reading subscription caches and session counters".into()));
            return lines.into_iter().flat_map(|line| wrap(line, width)).collect();
        };
        let quotas: Vec<_> = snap
            .quotas
            .iter()
            .filter(|q| {
                matches(
                    query,
                    profile,
                    &q.profile,
                    &format!("{} {} {} {}", q.account, q.provider, q.plan, q.hosts.join(" ")),
                )
            })
            .collect();
        let totals: Vec<_> = self
            .totals
            .iter()
            .filter(|t| {
                matches(
                    query,
                    profile,
                    &t.profile,
                    &format!(
                        "{} {} {} {} {}",
                        t.account,
                        t.harness,
                        t.provider,
                        t.model,
                        t.hosts.join(" ")
                    ),
                )
            })
            .collect();
        let age = now.saturating_sub(snap.fetched);
        let needs_attention = quotas.iter().filter(|q| attention(q, now)).count();
        lines.push(Line::from(vec![
            bold("SUBSCRIPTIONS", p.iris),
            span(format!("  {} sources", quotas.len()), p.subtle),
            bold(
                if needs_attention > 0 {
                    format!(
                        "  · {needs_attention} {} attention",
                        if needs_attention == 1 { "needs" } else { "need" }
                    )
                } else {
                    String::new()
                },
                p.gold,
            ),
        ]));
        lines.push(Line::from(vec![
            span(format!("Updated {} ago", duration(age)), p.subtle),
            bold(if age > STALE { " · STALE" } else { "" }, p.gold),
            span(" · account limits, not summed across hosts", p.subtle),
        ]));
        lines.push(Line::default());
        // Two cards only when both can hold aligned meters and reset columns.
        let columns = if width >= 68 { 2 } else { 1 };
        let card_width = (width.saturating_sub((columns - 1) * 2) / columns).max(5);
        for (name, group) in by_profile(&quotas, |q| &q.profile) {
            lines.push(profile_rule(&name, p, width));
            lines.push(Line::default());
            for chunk in group.chunks(columns) {
                let cards: Vec<_> = chunk.iter().map(|q| quota_card(q, p, now, card_width)).collect();
                let height = cards.iter().map(Vec::len).max().unwrap_or(0);
                for row in 0..height {
                    let mut spans = Vec::new();
                    for (column, card) in cards.iter().enumerate() {
                        if column > 0 {
                            spans.push(Span::raw("  "));
                        }
                        if let Some(line) = card.get(row) {
                            spans.extend(line.spans.iter().cloned());
                        } else {
                            spans.push(Span::raw(" ".repeat(card_width)));
                        }
                    }
                    lines.push(Line::from(spans));
                }
                lines.push(Line::default());
            }
        }
        if quotas.is_empty() {
            lines.push(secondary(if query.is_empty() && profile.is_none() {
                "No quota data · configure usage.quotas for each account".into()
            } else {
                "No matching quota data · clear the search or change profile".into()
            }));
            lines.push(Line::default());
        }
        if quotas
            .iter()
            .any(|q| q.windows.iter().any(|w| w.reset.is_some_and(|at| at <= now)))
        {
            lines.push(Line::from(span("↻ Reset due · awaiting collector refresh", p.gold)));
            lines.push(Line::default());
        }
        lines.push(Line::from(vec![
            bold("SESSION USAGE", p.iris),
            span("  / rolling 7 days", p.subtle),
        ]));
        lines.push(secondary(
            "API-equivalent USD estimates · not subscription bills".into(),
        ));
        let tokens: u64 = totals.iter().map(|t| t.tokens.total()).sum();
        let requests: u64 = totals.iter().map(|t| t.requests as u64).sum();
        let unpriced: u64 = totals.iter().map(|t| t.unpriced as u64).sum();
        let cost: f64 = totals.iter().map(|t| t.cost).sum();
        if !totals.is_empty() {
            let estimate = if unpriced == requests {
                "cost unknown".into()
            } else {
                format!("≈ ${cost:.2}{}", if unpriced > 0 { " known portion" } else { "" })
            };
            lines.push(Line::from(vec![
                bold(amount(tokens), p.text),
                span(" tokens   ", p.subtle),
                bold(amount(requests), p.text),
                span(" requests   ", p.subtle),
                bold(estimate, p.foam),
            ]));
            if unpriced > 0 {
                lines.push(Line::from(span(
                    format!("{unpriced} unpriced requests · excluded from the estimate"),
                    p.gold,
                )));
            }
        }
        lines.push(Line::default());
        for (name, group) in by_profile(&totals, |t| &t.profile) {
            lines.push(profile_rule(&name, p, width));
            lines.push(Line::default());
            for t in group {
                let cost = if t.unpriced == t.requests {
                    "cost unknown".into()
                } else if t.unpriced > 0 {
                    format!("≈ ${:.4} known portion · {} unpriced requests", t.cost, t.unpriced)
                } else {
                    format!("≈ ${:.4}", t.cost)
                };
                lines.push(Line::from(bold(&t.model, p.text)));
                lines.push(secondary(format!(
                    "{} · {} / {} · {}",
                    t.account,
                    t.harness,
                    t.provider,
                    t.hosts.join(", ")
                )));
                lines.push(Line::from(vec![
                    bold(amount(t.tokens.total()), p.foam),
                    span(" tokens   ", p.subtle),
                    bold(t.requests.to_string(), p.text),
                    span(" requests   ", p.subtle),
                    span(cost, if t.unpriced > 0 { p.gold } else { p.foam }),
                ]));
                lines.push(secondary(format!(
                    "In {} · Out {} · Cache read {} / write {}",
                    amount(t.tokens.input),
                    amount(t.tokens.output),
                    amount(t.tokens.cache_read),
                    amount(t.tokens.cache_write)
                )));
                lines.push(Line::default());
            }
        }
        if totals.is_empty() {
            lines.push(secondary("No matching recorded usage in the last 7 days".into()));
            lines.push(Line::default());
        }
        lines.push(Line::from(bold("COVERAGE", p.iris)));
        lines.push(secondary(
            "Configured/active profiles only. Missing logs and unknown prices are not zero usage.".into(),
        ));
        lines.push(secondary(
            "Shared account labels deduplicate copied events; distinct accounts need distinct labels.".into(),
        ));
        if !snap.warnings.is_empty() {
            lines.push(Line::default());
            lines.push(Line::from(bold(
                format!("! Partial coverage · {} collector warnings", snap.warnings.len()),
                p.gold,
            )));
            lines.extend(snap.warnings.iter().map(|w| Line::from(span(format!("• {w}"), p.gold))));
        }
        lines.into_iter().flat_map(|line| wrap(line, width)).collect()
    }

    pub fn draw(&mut self, f: &mut Frame, area: Rect, p: &Palette, now: u64, query: &str, profile: Option<&str>) {
        if area.is_empty() {
            return;
        }
        // Reserve a gutter for the scrollbar so it cannot cover card borders.
        // Layout and wrapping happen before scrolling, in both delivery paths.
        let width = area.width.saturating_sub(2).max(1) as usize;
        let lines = self.lines(p, now, query, profile, width);
        self.offset = self.offset.min(lines.len().saturating_sub(area.height as usize));
        let overflow = lines.len() > area.height as usize;
        let body = Rect {
            width: width as u16,
            ..area
        };
        let shown: Vec<_> = lines
            .iter()
            .skip(self.offset)
            .take(area.height as usize)
            .cloned()
            .collect();
        f.render_widget(Paragraph::new(shown), body);
        if overflow {
            draw::scrollbar(f, p, area, self.offset, lines.len());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn text(usage: &Usage, profile: Option<&str>, width: usize) -> String {
        usage
            .lines(&Palette::load(), 1000, "", profile, width)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn stale_and_missing_cost_are_not_zero() {
        let usage = Usage {
            snapshot: Some(Snapshot {
                fetched: 1,
                ..Snapshot::default()
            }),
            ..Usage::default()
        };
        let text = text(&usage, None, 100);
        assert!(text.contains("STALE"));
        assert!(text.contains("not zero usage"));
        assert!(text.contains("No quota data"));
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
        assert!(text(&usage, Some("work"), 100).contains("cost unknown"));
        assert!(!text(&usage, Some("work"), 100).contains("$0.0000"));
        assert!(!text(&usage, Some("personal"), 100).contains("work-login"));
    }

    #[test]
    fn invalid_delivery_keeps_previous_data() {
        let mut usage = Usage::default();
        usage.paste(r#"{"fetched":123,"samples":[],"quotas":[],"warnings":[]}"#);
        usage.paste("broken");
        assert_eq!(usage.snapshot.unwrap().fetched, 123);
        assert!(usage.error.is_some());
    }

    fn quota(provider: &str, used: Option<f64>) -> Quota {
        Quota {
            account: "personal".into(),
            profile: "personal".into(),
            provider: provider.into(),
            plan: "Plus".into(),
            fetched: 1000,
            status: "ok".into(),
            credits: None,
            hosts: vec!["mac".into()],
            windows: vec![switchyard_core::usage::Window {
                label: "5h".into(),
                used,
                reset: Some(4600),
                status: "ok".into(),
            }],
        }
    }

    #[test]
    fn responsive_cards_keep_values_and_warning_styles() {
        let p = Palette::load();
        let usage = Usage {
            snapshot: Some(Snapshot {
                fetched: 1000,
                quotas: vec![quota("claude-code", Some(100.0)), quota("codex", None)],
                ..Snapshot::default()
            }),
            ..Usage::default()
        };
        let wide = usage.lines(&p, 1000, "", None, 100);
        assert!(
            wide.iter()
                .any(|l| l.to_string().contains("claude-code") && l.to_string().contains("codex"))
        );
        assert!(wide.iter().any(|l| {
            l.spans
                .iter()
                .any(|s| s.content.contains("100%") && s.style.fg == Some(p.love))
        }));
        assert!(text(&usage, None, 100).contains("At limit"));
        assert!(text(&usage, None, 100).contains('?'));
        for width in [1, 20, 40, 67, 68, 78, 100] {
            let lines = usage.lines(&p, 1000, "", None, width);
            assert!(lines.iter().all(|l| l.width() <= width), "width {width}");
            if width >= 40 {
                assert!(lines.iter().any(|l| l.to_string().contains("100%")));
            }
        }
    }

    #[test]
    fn scrolling_reaches_wrapped_warnings_and_clamps_after_filtering() {
        let mut usage = Usage {
            snapshot: Some(Snapshot {
                fetched: 1000,
                quotas: vec![quota("codex", None)],
                warnings: vec![
                    "A long collector warning with unicode 主机 and an-unbroken-model-name-that-must-wrap-at-the-edge"
                        .into(),
                ],
                ..Snapshot::default()
            }),
            offset: usize::MAX,
            ..Usage::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(36, 8)).unwrap();
        terminal
            .draw(|f| usage.draw(f, f.area(), &Palette::load(), 1000, "", None))
            .unwrap();
        let rendered: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
        assert!(rendered.contains("at-the-edge"));
        assert!(usage.offset < usize::MAX);
        terminal
            .draw(|f| usage.draw(f, f.area(), &Palette::load(), 1000, "missing", None))
            .unwrap();
    }

    #[test]
    fn wrapping_preserves_unicode_and_semantic_colors() {
        let p = Palette::load();
        let lines = wrap(
            Line::from(vec![span("A readable heading ", p.text), bold("100% 主机", p.love)]),
            12,
        );
        assert!(lines.iter().all(|l| l.width() <= 12));
        assert!(lines.iter().any(|l| {
            l.spans
                .iter()
                .any(|s| s.content.contains("100%") && s.style.fg == Some(p.love))
        }));
    }
}
