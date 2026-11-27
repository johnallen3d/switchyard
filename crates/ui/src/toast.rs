//! Floating system-notification-style toasts: shown in the top-right corner
//! of a float so action outcomes and errors never cover the footer's keybind
//! hints, wrap long messages instead of clipping, and auto-dismiss unless
//! still in progress.

use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};
use unicode_width::UnicodeWidthChar;

use crate::Palette;
use crate::draw::{cells, str_width};
use crate::glyphs::SPINNER;

/// How long an informational or success toast stays on screen before
/// auto-dismissing.
pub const INFO_TTL: Duration = Duration::from_secs(5);

/// How long an error toast stays on screen before auto-dismissing.
pub const ERROR_TTL: Duration = Duration::from_secs(8);

/// Maximum outer width of a toast card, including borders and padding.
const MAX_WIDTH: u16 = 54;

/// Minimum outer width of a toast card when the pane has room, wide enough for
/// the title and dismiss hint on their borders without crowding.
const MIN_WIDTH: u16 = 24;

/// Maximum wrapped body lines inside a toast card.
const MAX_LINES: usize = 5;

/// What kind of notification a [`Toast`] shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Completed action or informational notice.
    Info,
    /// Refusal or failed action.
    Error,
    /// Ongoing work (animated spinner, does not auto-dismiss).
    Progress,
}

/// A floating notification card shown in the top-right corner of a float.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
    pub text: String,
    pub kind: Kind,
    pub shown_at: Instant,
}

impl Toast {
    #[must_use]
    pub fn info(text: impl Into<String>) -> Self {
        Self::new(text, Kind::Info, Instant::now())
    }

    #[must_use]
    pub fn error(text: impl Into<String>) -> Self {
        Self::new(text, Kind::Error, Instant::now())
    }

    #[must_use]
    pub fn progress(text: impl Into<String>) -> Self {
        Self::new(text, Kind::Progress, Instant::now())
    }

    #[must_use]
    pub fn from_note(text: impl Into<String>, failed: bool) -> Self {
        if failed {
            Self::error(text)
        } else {
            Self::info(text)
        }
    }

    #[must_use]
    pub fn new(text: impl Into<String>, kind: Kind, shown_at: Instant) -> Self {
        Toast {
            text: text.into(),
            kind,
            shown_at,
        }
    }

    /// How long the toast stays visible before expiring; `None` for progress
    /// toasts, which stay until the operation finishes or replaces them.
    #[must_use]
    pub fn ttl(&self) -> Option<Duration> {
        match self.kind {
            Kind::Info => Some(INFO_TTL),
            Kind::Error => Some(ERROR_TTL),
            Kind::Progress => None,
        }
    }

    /// Whether the toast has outlived its TTL at `now`.
    #[must_use]
    pub fn expired(&self, now: Instant) -> bool {
        self.ttl()
            .is_some_and(|ttl| now.saturating_duration_since(self.shown_at) >= ttl)
    }

    #[must_use]
    pub fn is_error(&self) -> bool {
        self.kind == Kind::Error
    }

    #[must_use]
    pub fn is_progress(&self) -> bool {
        self.kind == Kind::Progress
    }
}

/// Draw `toast` in the top-right corner of `bounds`, leaving the rest of the
/// pane (including the footer) unobstructed. When `dismiss_key` is `Some` and
/// the toast is not in progress, shows a subtle dismiss hint on the bottom
/// border.
pub fn draw(
    f: &mut Frame,
    p: &Palette,
    bounds: Rect,
    toast: &Toast,
    tick: usize,
    dismiss_key: Option<&str>,
) {
    let max_w = MAX_WIDTH.min(bounds.width);
    if max_w < 12 || bounds.height < 3 {
        return;
    }
    let clean = sanitize(&toast.text);
    let natural = clean.lines().map(str_width).max().unwrap_or(0);
    let min_w = MIN_WIDTH.min(max_w);
    let width = cells(natural).saturating_add(4).clamp(min_w, max_w);
    let inner_w = usize::from(width.saturating_sub(4)).max(1);
    let max_lines = MAX_LINES
        .min(usize::from(bounds.height.saturating_sub(2)))
        .max(1);
    let wrapped = wrap_capped(&clean, inner_w, max_lines);
    let height = cells(wrapped.len()).saturating_add(2).min(bounds.height);
    let rect = Rect {
        x: bounds.right().saturating_sub(width),
        y: bounds.y,
        width,
        height,
    };

    let (accent, icon, icon_color, title, title_color) = match toast.kind {
        Kind::Info => (p.pine, "✓", p.foam, "Notice", p.text),
        Kind::Error => (p.love, "✕", p.love, "Error", p.love),
        Kind::Progress => (
            p.iris,
            SPINNER[tick % SPINNER.len()],
            p.iris,
            "Working",
            p.text,
        ),
    };
    let bg = p.surface;
    let border = Style::new().fg(accent).bg(bg);
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border)
        .style(Style::new().bg(bg))
        .padding(Padding::horizontal(1))
        .title_top(Line::from(vec![
            Span::styled("─", border),
            Span::styled(
                format!(" {icon} "),
                Style::new()
                    .fg(icon_color)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{title} "),
                Style::new()
                    .fg(title_color)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    if let Some(key) = dismiss_key.filter(|_| !toast.is_progress()) {
        let full_w = str_width(key) + " dismiss ".len() + 2;
        if usize::from(width.saturating_sub(2)) >= full_w {
            block = block.title_bottom(
                Line::from(vec![
                    Span::styled(
                        format!(" {key}"),
                        Style::new()
                            .fg(p.subtle)
                            .bg(bg)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" dismiss ", Style::new().fg(p.muted).bg(bg)),
                    Span::styled("─", border),
                ])
                .right_aligned(),
            );
        } else if usize::from(width.saturating_sub(2)) >= str_width(key) + 3 {
            block = block.title_bottom(
                Line::from(vec![
                    Span::styled(
                        format!(" {key} "),
                        Style::new()
                            .fg(p.subtle)
                            .bg(bg)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("─", border),
                ])
                .right_aligned(),
            );
        }
    }

    let inner = block.inner(rect);
    f.render_widget(Clear, rect);
    f.render_widget(block, rect);
    let lines: Vec<Line<'static>> = wrapped
        .into_iter()
        .map(|l| Line::styled(l, Style::new().fg(p.text).bg(bg)))
        .collect();
    f.render_widget(
        Paragraph::new(lines).style(Style::new().fg(p.text).bg(bg)),
        inner,
    );
}

/// Normalize control characters that would break cell layout inside the card.
fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\r' => {}
            '\t' => out.push(' '),
            '\n' => out.push('\n'),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Wrap `text` to `width` display columns, capping at `max_lines` with a
/// trailing ellipsis if truncated.
fn wrap_capped(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    let mut lines = wrap(text, width);
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        if let Some(last) = lines.last_mut() {
            *last = clip_with_ellipsis(last, width);
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Word-wrap `text` to `width` display columns, breaking tokens wider than
/// `width` across lines so nothing overflows the box.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for raw_line in text.split('\n') {
        let mut cur = String::new();
        let mut cur_w = 0;
        for word in raw_line.split_whitespace() {
            let word_w = str_width(word);
            if cur.is_empty() {
                push_word(&mut out, &mut cur, &mut cur_w, word, word_w, width);
            } else if cur_w + 1 + word_w <= width {
                cur.push(' ');
                cur.push_str(word);
                cur_w += 1 + word_w;
            } else {
                out.push(std::mem::take(&mut cur));
                cur_w = 0;
                push_word(&mut out, &mut cur, &mut cur_w, word, word_w, width);
            }
        }
        out.push(cur);
    }
    out
}

fn push_word(
    out: &mut Vec<String>,
    cur: &mut String,
    cur_w: &mut usize,
    word: &str,
    word_w: usize,
    width: usize,
) {
    if word_w <= width {
        cur.push_str(word);
        *cur_w = word_w;
        return;
    }
    for c in word.chars() {
        let cw = c.width().unwrap_or(0);
        if *cur_w > 0 && *cur_w + cw > width {
            out.push(std::mem::take(cur));
            *cur_w = 0;
        }
        cur.push(c);
        *cur_w += cw;
    }
}

fn clip_with_ellipsis(line: &str, width: usize) -> String {
    let limit = width.saturating_sub(1);
    let mut kept = String::new();
    let mut used = 0;
    for c in line.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw > limit {
            break;
        }
        kept.push(c);
        used += cw;
    }
    let trimmed = kept.trim_end();
    format!("{trimmed}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(width: u16, height: u16, toast: &Toast, dismiss: Option<&str>) -> Vec<String> {
        let p = Palette::load();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                f.buffer_mut()
                    .set_style(area, Style::new().bg(p.base).fg(p.text));
                draw(f, &p, area, toast, 0, dismiss);
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        (0..height)
            .map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn ttls_expire_info_and_error_but_not_progress() {
        let t0 = Instant::now();
        let info = Toast::new("done", Kind::Info, t0);
        assert!(!info.expired(t0 + Duration::from_secs(4)));
        assert!(info.expired(t0 + INFO_TTL));

        let err = Toast::new("boom", Kind::Error, t0);
        assert!(!err.expired(t0 + INFO_TTL));
        assert!(err.expired(t0 + ERROR_TTL));

        let prog = Toast::new("working…", Kind::Progress, t0);
        assert!(!prog.expired(t0 + Duration::from_secs(3600)));
    }

    #[test]
    fn wraps_long_unbroken_tokens_and_caps_lines() {
        let long = "archived marvin/18-48-58-png-what-does-this-gap-indicate (merged); Removing feature/18-48-58-png-what-does-this-gap-indicate worktree & branch";
        let lines = wrap_capped(long, 30, MAX_LINES);
        assert!(lines.len() <= MAX_LINES);
        for l in &lines {
            assert!(str_width(l) <= 30, "line too wide: {l:?}");
        }
        assert!(lines.join(" ").contains("archived"));

        let huge = (0..20)
            .map(|i| format!("line-{i} with several words"))
            .collect::<Vec<_>>()
            .join(" ");
        let capped = wrap_capped(&huge, 24, 3);
        assert_eq!(capped.len(), 3);
        assert!(capped[2].ends_with('…'), "{capped:?}");
        assert!(str_width(&capped[2]) <= 24);
    }

    #[test]
    fn draws_in_top_right_with_surface_fill_and_dismiss_hint() {
        let p = Palette::load();
        let toast = Toast::info("archived marvin/demo (merged)");
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                f.buffer_mut()
                    .set_style(area, Style::new().bg(p.base).fg(p.text));
                draw(f, &p, area, &toast, 0, Some("esc"));
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let rows: Vec<String> = (0..20)
            .map(|y| (0..100).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        // Top-right corner holds the card; left side and bottom stay untouched.
        assert!(rows[0].contains("✓ Notice"), "{}", rows[0]);
        assert!(
            rows[1].contains("archived marvin/demo (merged)"),
            "{}",
            rows[1]
        );
        assert!(rows[2].contains("esc dismiss"), "{}", rows[2]);
        assert_eq!(buf[(0, 0)].bg, p.base);
        assert_eq!(buf[(99, 0)].bg, p.surface);
        assert_eq!(buf[(99, 1)].bg, p.surface);
        assert_eq!(buf[(99, 2)].bg, p.surface);
        assert_eq!(buf[(99, 19)].bg, p.base);
    }

    #[test]
    fn progress_toast_shows_spinner_and_omits_dismiss_hint() {
        let toast = Toast::progress("Archiving demo on mac…");
        let rows = render(80, 10, &toast, Some("esc"));
        assert!(rows[0].contains("⠋ Working"), "{}", rows[0]);
        assert!(rows[1].contains("Archiving demo on mac…"), "{}", rows[1]);
        assert!(!rows[2].contains("dismiss"), "{}", rows[2]);
    }

    #[test]
    fn error_toast_uses_error_header() {
        let toast = Toast::error("highlight an agent to message it");
        let rows = render(80, 10, &toast, Some("esc"));
        assert!(rows[0].contains("✕ Error"), "{}", rows[0]);
        assert!(rows[1].contains("highlight an agent"), "{}", rows[1]);
    }
}
