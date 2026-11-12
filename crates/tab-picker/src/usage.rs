//! Read-only Usage view, shared by the float's pastes and standalone feed.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use switchyard_core::usage::{Quota, STALE, Snapshot, Summary, Total};
use switchyard_ui::{Palette, draw};

#[derive(Default)]
pub struct Usage {
    pub snapshot: Option<Summary>,
    pub error: Option<String>,
    pub offset: usize,
}

fn amount(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}b", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}m", n as f64 / 1_000_000.0)
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
    Span::styled(
        text.into(),
        Style::new().fg(color).add_modifier(Modifier::BOLD),
    )
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
                    row.spans
                        .push(Span::styled(std::mem::take(&mut part), s.style));
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

/// A full-width labelled rule that separates profile groups, with an
/// optional right-aligned summary (subtotals) at the end.
fn profile_rule(name: &str, summary: &str, p: &Palette, width: usize) -> Line<'static> {
    let label = if name.is_empty() {
        "OTHER".to_string()
    } else {
        name.to_uppercase()
    };
    let label = format!(" {label} ");
    let summary = if summary.is_empty() {
        String::new()
    } else {
        format!(" {summary} ")
    };
    let mut used = draw::str_width(&label) + 2;
    let tail = if used + draw::str_width(&summary) + 4 <= width {
        used += draw::str_width(&summary) + 2;
        summary
    } else {
        String::new()
    };
    let rest = width.saturating_sub(used);
    let mut spans = vec![
        span("──", p.hl_high),
        bold(label, p.rose),
        span("─".repeat(rest), p.hl_high),
    ];
    if !tail.is_empty() {
        spans.push(span(tail, p.subtle));
        spans.push(span("──", p.hl_high));
    }
    Line::from(spans)
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
        let filled =
            used.map(|n| ((n.clamp(0.0, 100.0) / 100.0) * bar_width as f64).round() as usize);
        let meter = if let Some(n) = filled {
            // Half-cell strokes leave gaps between ticks; a thin dashed track
            // avoids solid blocks merging visually across adjacent quota rows.
            vec![
                span("╸".repeat(n), color),
                span("┄".repeat(bar_width - n), p.hl_med),
            ]
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
            used.map(|n| format!("{n:>4.0}% "))
                .unwrap_or_else(|| "   ?  ".into()),
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
        Line::from(span(
            format!("╭{}╮", "─".repeat(width.saturating_sub(2))),
            border,
        ))
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

/// Pad or truncate (with an ellipsis) to exactly `w` display cells.
fn fit(s: &str, w: usize) -> String {
    let len = draw::str_width(s);
    if len <= w {
        return format!("{s}{}", " ".repeat(w - len));
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = draw::str_width(&c.to_string());
        if used + cw + 1 > w {
            break;
        }
        out.push(c);
        used += cw;
    }
    if w > 0 {
        out.push('…');
        used += 1;
    }
    out + &" ".repeat(w.saturating_sub(used))
}

fn right(s: &str, w: usize) -> String {
    format!("{}{s}", " ".repeat(w.saturating_sub(draw::str_width(s))))
}

fn dollars(cost: f64) -> String {
    if cost > 0.0 && cost < 0.01 {
        "<$0.01".into()
    } else {
        format!("${cost:.2}")
    }
}

/// Unknown prices are never shown as zero: fully unpriced rows read "—" and
/// partially priced rows carry a "+" because the true figure is higher.
fn cost_cell(t: &Total, p: &Palette) -> Span<'static> {
    if t.unpriced == t.requests {
        span("—", p.subtle)
    } else if t.unpriced > 0 {
        span(format!("{}+", dollars(t.cost)), p.gold)
    } else {
        span(dollars(t.cost), p.foam)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Col {
    Model,
    Via,
    Tokens,
    Share,
    Requests,
    Input,
    Output,
    CacheRead,
    CacheWrite,
    Cost,
}

impl Col {
    fn header(self) -> &'static str {
        match self {
            Col::Model => "MODEL",
            Col::Via => "VIA",
            Col::Tokens => "TOKENS",
            Col::Share => "",
            Col::Requests => "REQS",
            Col::Input => "IN",
            Col::Output => "OUT",
            Col::CacheRead => "CACHE R",
            Col::CacheWrite => "CACHE W",
            Col::Cost => "COST",
        }
    }

    fn left(self) -> bool {
        matches!(self, Col::Model | Col::Via | Col::Share)
    }
}

/// Pick the columns that fit, in priority order: the essentials, then the
/// harness, a relative-size meter, and finally the token breakdown.
fn layout(model: usize, via: usize, width: usize) -> Option<Vec<(Col, usize)>> {
    const GAP: usize = 2;
    let total = |cols: &[(Col, usize)]| {
        cols.iter().map(|c| c.1).sum::<usize>() + GAP * cols.len().saturating_sub(1)
    };
    let mut cols = vec![
        (Col::Model, model),
        (Col::Tokens, 7),
        (Col::Requests, 5),
        (Col::Cost, 7),
    ];
    let fixed = total(&cols) - model;
    if fixed + 8 > width {
        return None;
    }
    cols[0].1 = model.min(width - fixed);
    let try_add = |cols: &mut Vec<(Col, usize)>, at: usize, add: &[(Col, usize)]| {
        let mut next = cols.clone();
        for (i, c) in add.iter().enumerate() {
            next.insert(at + i, *c);
        }
        if total(&next) <= width {
            *cols = next;
            true
        } else {
            false
        }
    };
    try_add(&mut cols, 1, &[(Col::Via, via)]);
    let at = cols.iter().position(|c| c.0 == Col::Tokens).unwrap() + 1;
    try_add(&mut cols, at, &[(Col::Share, 10)]);
    let at = cols.iter().position(|c| c.0 == Col::Requests).unwrap() + 1;
    try_add(
        &mut cols,
        at,
        &[
            (Col::Input, 7),
            (Col::Output, 7),
            (Col::CacheRead, 7),
            (Col::CacheWrite, 7),
        ],
    );
    // Give leftover room to the meter, within reason.
    let spare = width.saturating_sub(total(&cols));
    if let Some(c) = cols.iter_mut().find(|c| c.0 == Col::Share) {
        c.1 += spare.min(10);
    }
    Some(cols)
}

/// Session usage as one aligned table per profile, largest models first, so
/// models can be compared at a glance. Context shared by every row (hosts,
/// account = profile) is stated once rather than repeated.
fn session(totals: &[&Total], p: &Palette, width: usize, unfiltered: bool) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut hosts: Vec<String> = Vec::new();
    for t in totals {
        union(&mut hosts, &t.hosts);
    }
    let mut title = vec![
        bold("SESSION USAGE", p.iris),
        span("  rolling 7 days", p.subtle),
    ];
    if !hosts.is_empty() {
        title.push(span(format!(" · on {}", hosts.join(", ")), p.subtle));
    }
    lines.push(Line::from(title));
    if totals.is_empty() {
        lines.push(Line::from(span(
            if unfiltered {
                "No recorded usage in the last 7 days"
            } else {
                "No matching recorded usage · clear the search or change profile"
            },
            p.subtle,
        )));
        return lines;
    }
    let tokens: u64 = totals.iter().map(|t| t.tokens.total()).sum();
    let requests: usize = totals.iter().map(|t| t.requests).sum();
    let unpriced: usize = totals.iter().map(|t| t.unpriced).sum();
    let cost: f64 = totals.iter().map(|t| t.cost).sum();
    let mut summary = vec![
        bold(amount(tokens), p.text),
        span(" tokens · ", p.subtle),
        bold(amount(requests as u64), p.text),
        span(" requests · ", p.subtle),
    ];
    if unpriced == requests {
        summary.push(span("cost unknown", p.subtle));
    } else if unpriced > 0 {
        summary.push(bold(format!("{}+", dollars(cost)), p.gold));
        summary.push(span(
            format!(
                " ({} of {} req priced)",
                amount((requests - unpriced) as u64),
                amount(requests as u64)
            ),
            p.subtle,
        ));
    } else {
        summary.push(bold(dollars(cost), p.foam));
        summary.push(span(" est.", p.subtle));
    }
    lines.push(Line::from(summary));
    lines.push(Line::from(span(
        if unpriced > 0 {
            "API-equivalent estimate, not a bill · — no price data · + partly priced"
        } else {
            "API-equivalent estimate, not a subscription bill"
        },
        p.subtle,
    )));
    lines.push(Line::default());

    let via = |t: &Total| {
        let mut s = format!("{}/{}", t.harness, t.provider);
        if t.account != t.profile {
            s = format!("{} · {s}", t.account);
        }
        let mut own = t.hosts.clone();
        own.sort();
        if own != hosts {
            s = format!("{s} @{}", t.hosts.join(","));
        }
        s
    };
    let model_w = totals
        .iter()
        .map(|t| draw::str_width(&t.model))
        .max()
        .unwrap_or(0)
        .clamp(8, 28);
    let via_w = totals
        .iter()
        .map(|t| draw::str_width(&via(t)))
        .max()
        .unwrap_or(0)
        .clamp(6, 24);
    let cols = layout(model_w, via_w, width);
    let max = totals
        .iter()
        .map(|t| t.tokens.total())
        .max()
        .unwrap_or(0)
        .max(1);

    for (name, mut group) in by_profile(totals, |t| &t.profile) {
        group.sort_by(|a, b| {
            b.tokens
                .total()
                .cmp(&a.tokens.total())
                .then(a.model.cmp(&b.model))
        });
        let sub_tokens: u64 = group.iter().map(|t| t.tokens.total()).sum();
        let sub_requests: usize = group.iter().map(|t| t.requests).sum();
        lines.push(profile_rule(
            &name,
            &format!(
                "{} tokens · {} req",
                amount(sub_tokens),
                amount(sub_requests as u64)
            ),
            p,
            width,
        ));
        let Some(cols) = &cols else {
            // Too narrow for a table: two short lines per model.
            for t in group {
                lines.push(Line::from(bold(&t.model, p.text)));
                lines.push(Line::from(vec![
                    bold(amount(t.tokens.total()), p.foam),
                    span(format!(" tok · {} req · ", t.requests), p.subtle),
                    cost_cell(t, p),
                ]));
            }
            lines.push(Line::default());
            continue;
        };
        let mut header = Vec::new();
        for (i, &(col, w)) in cols.iter().enumerate() {
            if i > 0 {
                header.push(Span::raw("  "));
            }
            let h = col.header();
            header.push(span(
                if col.left() { fit(h, w) } else { right(h, w) },
                p.subtle,
            ));
        }
        lines.push(Line::from(header));
        for t in group {
            let mut row = Vec::new();
            for (i, &(col, w)) in cols.iter().enumerate() {
                if i > 0 {
                    row.push(Span::raw("  "));
                }
                let num = |n: u64| span(right(&amount(n), w), p.subtle);
                match col {
                    Col::Model => row.push(bold(fit(&t.model, w), p.text)),
                    Col::Via => row.push(span(fit(&via(t), w), p.subtle)),
                    Col::Tokens => row.push(bold(right(&amount(t.tokens.total()), w), p.foam)),
                    Col::Share => {
                        let n =
                            ((t.tokens.total() as f64 / max as f64) * w as f64).round() as usize;
                        let n = n.clamp(usize::from(t.tokens.total() > 0), w);
                        row.push(span("━".repeat(n), p.foam));
                        row.push(span("─".repeat(w - n), p.hl_med));
                    }
                    Col::Requests => row.push(span(right(&t.requests.to_string(), w), p.text)),
                    Col::Input => row.push(num(t.tokens.input)),
                    Col::Output => row.push(num(t.tokens.output)),
                    Col::CacheRead => row.push(num(t.tokens.cache_read)),
                    Col::CacheWrite => row.push(num(t.tokens.cache_write)),
                    Col::Cost => {
                        let c = cost_cell(t, p);
                        row.push(Span::styled(right(&c.content, w), c.style));
                    }
                }
            }
            lines.push(Line::from(row));
        }
        lines.push(Line::default());
    }
    lines.pop();
    lines
}

fn union(into: &mut Vec<String>, add: &[String]) {
    for h in add {
        if !into.contains(h) {
            into.push(h.clone());
        }
    }
    into.sort();
}

impl Usage {
    /// Collectors send a summary; a raw snapshot from an older binary is
    /// still aggregated here rather than rejected.
    pub fn paste(&mut self, body: &str) {
        let summary = serde_json::from_str::<Summary>(body)
            .or_else(|_| serde_json::from_str::<Snapshot>(body).map(Snapshot::summary));
        match summary {
            Ok(snapshot) => {
                self.snapshot = Some(snapshot);
                self.error = None;
            }
            Err(_) => {
                self.error = Some("Incompatible usage data; keeping the last snapshot".into())
            }
        }
    }

    fn lines(
        &self,
        p: &Palette,
        now: u64,
        query: &str,
        profile: Option<&str>,
        width: usize,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let secondary = |s: String| Line::from(span(s, p.subtle));
        if let Some(e) = &self.error {
            lines.push(Line::from(bold(format!("! {e}"), p.gold)));
        }
        let Some(snap) = &self.snapshot else {
            lines.push(Line::from(bold("Loading usage…", p.text)));
            lines.push(secondary(
                "Reading subscription caches and session counters".into(),
            ));
            return lines
                .into_iter()
                .flat_map(|line| wrap(line, width))
                .collect();
        };
        let quotas: Vec<_> = snap
            .quotas
            .iter()
            .filter(|q| {
                matches(
                    query,
                    profile,
                    &q.profile,
                    &format!(
                        "{} {} {} {}",
                        q.account,
                        q.provider,
                        q.plan,
                        q.hosts.join(" ")
                    ),
                )
            })
            .collect();
        let totals: Vec<_> = snap
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
                        if needs_attention == 1 {
                            "needs"
                        } else {
                            "need"
                        }
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
            lines.push(profile_rule(&name, "", p, width));
            lines.push(Line::default());
            for chunk in group.chunks(columns) {
                let cards: Vec<_> = chunk
                    .iter()
                    .map(|q| quota_card(q, p, now, card_width))
                    .collect();
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
        if quotas.iter().any(|q| {
            q.windows
                .iter()
                .any(|w| w.reset.is_some_and(|at| at <= now))
        }) {
            lines.push(Line::from(span(
                "↻ Reset due · awaiting collector refresh",
                p.gold,
            )));
            lines.push(Line::default());
        }
        lines.extend(session(
            &totals,
            p,
            width,
            query.is_empty() && profile.is_none(),
        ));
        if !snap.warnings.is_empty() {
            lines.push(Line::default());
            lines.push(Line::from(bold("COVERAGE", p.iris)));
            lines.push(secondary(
                "Configured/active profiles only. Missing logs and unknown prices are not zero usage."
                    .into(),
            ));
            lines.push(secondary(
                "Shared account labels deduplicate copied events; distinct accounts need distinct labels.".into(),
            ));
            lines.push(Line::default());
            lines.push(Line::from(bold(
                format!(
                    "! Partial coverage · {} collector warnings",
                    snap.warnings.len()
                ),
                p.gold,
            )));
            lines.extend(
                snap.warnings
                    .iter()
                    .map(|w| Line::from(span(format!("• {w}"), p.gold))),
            );
        }
        lines
            .into_iter()
            .flat_map(|line| wrap(line, width))
            .collect()
    }

    pub fn draw(
        &mut self,
        f: &mut Frame,
        area: Rect,
        p: &Palette,
        now: u64,
        query: &str,
        profile: Option<&str>,
    ) {
        if area.is_empty() {
            return;
        }
        // Reserve a gutter for the scrollbar so it cannot cover card borders.
        // Layout and wrapping happen before scrolling, in both delivery paths.
        let width = area.width.saturating_sub(2).max(1) as usize;
        let lines = self.lines(p, now, query, profile, width);
        self.offset = self
            .offset
            .min(lines.len().saturating_sub(area.height as usize));
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
            snapshot: Some(Summary {
                fetched: 1,
                ..Summary::default()
            }),
            ..Usage::default()
        };
        let clean = text(&usage, None, 100);
        assert!(clean.contains("STALE"));
        assert!(clean.contains("No quota data"));
        assert!(!clean.contains("COVERAGE"));

        let mut warned = usage;
        warned.snapshot.as_mut().unwrap().warnings = vec!["host: logs unreadable".into()];
        let warned = text(&warned, None, 100);
        assert!(warned.contains("COVERAGE"));
        assert!(warned.contains("not zero usage"));
        assert!(warned.contains("Partial coverage"));
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
        usage.paste(r#"{"fetched":123,"totals":[],"quotas":[],"warnings":[]}"#);
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
            snapshot: Some(Summary {
                fetched: 1000,
                quotas: vec![quota("claude-code", Some(100.0)), quota("codex", None)],
                ..Summary::default()
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
            snapshot: Some(Summary {
                fetched: 1000,
                quotas: vec![quota("codex", None)],
                warnings: vec![
                    "A long collector warning with unicode 主机 and an-unbroken-model-name-that-must-wrap-at-the-edge"
                        .into(),
                ],
                ..Summary::default()
            }),
            offset: usize::MAX,
            ..Usage::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(36, 8)).unwrap();
        terminal
            .draw(|f| usage.draw(f, f.area(), &Palette::load(), 1000, "", None))
            .unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
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
            Line::from(vec![
                span("A readable heading ", p.text),
                bold("100% 主机", p.love),
            ]),
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
