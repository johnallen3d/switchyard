//! Interactive question card for answering a pending `agy` `ask_question`
//! prompt directly from Switchyard's Inbox.
//!
//! Replaces the reply box while an `agy` agent is blocked on a question:
//! Tab focuses it, arrows / `j` / `k` / `1`–`9` pick options (`Space` toggles
//! checkboxes in multi-select), the trailing `Write-in…` row lets John type a
//! free-text answer, `←`/`→` move between questions in a multi-question prompt,
//! `s` skips (with confirmation), and `Enter` advances or submits.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use switchyard_core::actions::{QuestionAnswer, QuestionResponse};
use switchyard_core::transcript::PromptQuestion;
use switchyard_ui::draw::{cells, str_width};
use switchyard_ui::{Palette, chrome};

use crate::preview;

/// Top soft edge + gap + chip row + bottom soft edge.
const CARD_CHROME_ROWS: u16 = 4;

/// Maximum height the question card claims when focused.
const MAX_CARD_HEIGHT: u16 = 20;

/// What a key pressed in the question card asks the picker to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Key handled inside the card.
    Stay,
    /// Leave the card back to the list, keeping its draft.
    Leave,
    /// Submit all answers (or skip the prompt) to the agent.
    Submit(QuestionResponse),
    /// Leave the card and move the Inbox list by `delta` (`ctrl-j` / `ctrl-k`).
    MoveList(isize),
    /// Scroll the transcript preview above the card (`PgUp`/`PgDn`/`Shift+↑↓`).
    ScrollPreview(isize),
    /// Enter scrollback on the transcript (`ctrl-v`, or `ctrl-u`/`ctrl-d` with key).
    Scrollback(Option<KeyEvent>),
    /// Toggle the Inbox sidebar (`ctrl-s`).
    ToggleSidebar,
    /// Leave the card and arm an archive (`ctrl-x`).
    ArmArchive,
}

/// Per-question selection and write-in draft.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Draft {
    /// Highlighted row: `0..options.len()` are choices, `options.len()` is Write-in.
    cursor: usize,
    /// Checked option indices (for multi-select) or chosen index (for single-select).
    selected: Vec<usize>,
    /// Free-text write-in content.
    write_in: String,
    /// Whether the inline Write-in input currently has text focus.
    writing: bool,
}

impl Draft {
    fn has_answer(&self) -> bool {
        !self.selected.is_empty() || !self.write_in.trim().is_empty()
    }

    fn toggle(&mut self, index: usize) {
        if let Some(pos) = self.selected.iter().position(|&i| i == index) {
            self.selected.remove(pos);
        } else {
            self.selected.push(index);
            self.selected.sort_unstable();
        }
    }
}

/// State for an interactive `agy` question prompt in Inbox.
#[derive(Clone, Debug)]
pub struct QuestionCard {
    pub key: (String, String),
    pub label: String,
    pub questions: Vec<PromptQuestion>,
    step: usize,
    answers: Vec<Draft>,
    scroll: usize,
    hint: Option<String>,
    confirm_skip: bool,
}

impl QuestionCard {
    /// Build a card for `questions` on `(session, window)` named `label`.
    pub fn new(key: (String, String), label: String, questions: Vec<PromptQuestion>) -> Self {
        let answers = vec![Draft::default(); questions.len()];
        Self {
            key,
            label,
            questions,
            step: 0,
            answers,
            scroll: 0,
            hint: None,
            confirm_skip: false,
        }
    }

    /// Whether this card is still for the exact same set of questions.
    pub fn same_questions(&self, questions: &[PromptQuestion]) -> bool {
        self.questions.len() == questions.len()
            && self.questions.iter().zip(questions).all(|(a, b)| {
                a.question == b.question
                    && a.options == b.options
                    && a.multi_select == b.multi_select
            })
    }

    /// Whether the inline Write-in text field is currently taking characters.
    pub fn is_writing(&self) -> bool {
        self.answers.get(self.step).is_some_and(|d| d.writing)
    }

    /// Current 1-based step and total questions, for tests.
    #[cfg(test)]
    pub fn progress(&self) -> (usize, usize) {
        (self.step + 1, self.questions.len())
    }

    /// Paste sanitized single-line text into the active Write-in field.
    pub fn paste(&mut self, text: &str) -> bool {
        let Some(draft) = self.answers.get_mut(self.step) else {
            return false;
        };
        if !draft.writing {
            return false;
        }
        let cleaned = sanitize_inline(text);
        if !cleaned.is_empty() {
            draft.write_in.push_str(&cleaned);
        }
        true
    }

    /// Handle a key event while the question card has focus.
    pub fn key(&mut self, k: KeyEvent, page: isize) -> Outcome {
        self.hint = None;
        if self.confirm_skip {
            self.confirm_skip = false;
            let plain = !k
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
            if k.code == KeyCode::Enter || (plain && matches!(k.code, KeyCode::Char('y' | 'Y'))) {
                return Outcome::Submit(QuestionResponse::Skip);
            }
            return Outcome::Stay;
        }
        if self.is_writing() {
            return self.writing_key(k, page);
        }
        self.option_key(k, page)
    }

    fn writing_key(&mut self, k: KeyEvent, page: isize) -> Outcome {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        match k.code {
            KeyCode::PageUp => return Outcome::ScrollPreview(page),
            KeyCode::PageDown => return Outcome::ScrollPreview(-page),
            KeyCode::Up if shift => return Outcome::ScrollPreview(3),
            KeyCode::Down if shift => return Outcome::ScrollPreview(-3),
            _ => {}
        }
        let Some(draft) = self.answers.get_mut(self.step) else {
            return Outcome::Stay;
        };
        match k.code {
            KeyCode::Esc => {
                draft.writing = false;
            }
            KeyCode::Char('c') if ctrl => {
                if draft.write_in.is_empty() {
                    draft.writing = false;
                } else {
                    draft.write_in.clear();
                }
            }
            KeyCode::Enter => {
                let text = draft.write_in.trim().to_string();
                draft.writing = false;
                if text.is_empty() {
                    draft.write_in.clear();
                    return Outcome::Stay;
                }
                draft.write_in = text;
                let multi = self
                    .questions
                    .get(self.step)
                    .is_some_and(|q| q.multi_select);
                if !multi {
                    draft.selected.clear();
                }
                return self.advance_or_submit();
            }
            KeyCode::Char('u') if ctrl => draft.write_in.clear(),
            KeyCode::Char('w') if ctrl => delete_word(&mut draft.write_in),
            KeyCode::Backspace if alt || ctrl => delete_word(&mut draft.write_in),
            KeyCode::Backspace => {
                draft.write_in.pop();
            }
            KeyCode::Char(c) if !ctrl && !alt && !c.is_control() => {
                draft.write_in.push(c);
            }
            _ => {}
        }
        Outcome::Stay
    }

    fn option_key(&mut self, k: KeyEvent, page: isize) -> Outcome {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        match k.code {
            KeyCode::Char('s') if ctrl => return Outcome::ToggleSidebar,
            KeyCode::Char('j') if ctrl => return Outcome::MoveList(1),
            KeyCode::Char('k') if ctrl => return Outcome::MoveList(-1),
            KeyCode::Char('u' | 'd') if ctrl => return Outcome::Scrollback(Some(k)),
            KeyCode::Char('v') if ctrl => return Outcome::Scrollback(None),
            KeyCode::Char('x') if ctrl => return Outcome::ArmArchive,
            KeyCode::PageUp => return Outcome::ScrollPreview(page),
            KeyCode::PageDown => return Outcome::ScrollPreview(-page),
            KeyCode::Up if shift => return Outcome::ScrollPreview(3),
            KeyCode::Down if shift => return Outcome::ScrollPreview(-3),
            KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab => return Outcome::Leave,
            KeyCode::Char('c') if ctrl => return Outcome::Leave,
            _ => {}
        }

        let Some(q) = self.questions.get(self.step) else {
            return Outcome::Leave;
        };
        let opt_count = q.options.len();
        let multi = q.multi_select;

        match k.code {
            KeyCode::Up => self.move_cursor(-1, opt_count),
            KeyCode::Char('k') if !ctrl && !alt => self.move_cursor(-1, opt_count),
            KeyCode::Char('p') if ctrl => self.move_cursor(-1, opt_count),
            KeyCode::Down => self.move_cursor(1, opt_count),
            KeyCode::Char('j') if !ctrl && !alt => self.move_cursor(1, opt_count),
            KeyCode::Char('n') if ctrl => self.move_cursor(1, opt_count),
            KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') if !ctrl && !alt => {
                self.step_back();
            }
            KeyCode::Right | KeyCode::Char('l') if !ctrl && !alt => self.step_forward(),
            KeyCode::Char('s') if !ctrl && !alt => {
                self.confirm_skip = true;
            }
            KeyCode::Char(' ') if !ctrl && !alt => {
                let draft = &mut self.answers[self.step];
                if draft.cursor == opt_count {
                    draft.writing = true;
                } else if multi {
                    draft.toggle(draft.cursor);
                }
            }
            KeyCode::Char(c @ '1'..='9') if !ctrl && !alt => {
                let idx = (c as usize) - ('1' as usize);
                if idx < opt_count {
                    let draft = &mut self.answers[self.step];
                    draft.cursor = idx;
                    if multi {
                        draft.toggle(idx);
                    } else {
                        draft.selected = vec![idx];
                        draft.write_in.clear();
                        return self.advance_or_submit();
                    }
                } else if idx == opt_count {
                    let draft = &mut self.answers[self.step];
                    draft.cursor = idx;
                    draft.writing = true;
                }
            }
            KeyCode::Enter => {
                let draft = &mut self.answers[self.step];
                if draft.cursor == opt_count {
                    draft.writing = true;
                } else if multi {
                    if !draft.has_answer() {
                        self.hint = Some("space to toggle options, or Enter on Write-in".into());
                        return Outcome::Stay;
                    }
                    return self.advance_or_submit();
                } else {
                    draft.selected = vec![draft.cursor];
                    draft.write_in.clear();
                    return self.advance_or_submit();
                }
            }
            _ => {}
        }
        Outcome::Stay
    }

    fn move_cursor(&mut self, delta: isize, opt_count: usize) {
        if let Some(draft) = self.answers.get_mut(self.step) {
            draft.cursor = draft.cursor.saturating_add_signed(delta).min(opt_count);
        }
    }

    fn step_back(&mut self) {
        if self.step > 0 {
            self.step -= 1;
            self.scroll = 0;
        }
    }

    fn step_forward(&mut self) {
        if self.step + 1 >= self.questions.len() {
            return;
        }
        if self.answers.get(self.step).is_some_and(Draft::has_answer) {
            self.step += 1;
            self.scroll = 0;
        } else {
            self.hint = Some("answer this question first".into());
        }
    }

    fn advance_or_submit(&mut self) -> Outcome {
        if self.step + 1 < self.questions.len() {
            self.step += 1;
            self.scroll = 0;
            return Outcome::Stay;
        }
        let answers = self
            .questions
            .iter()
            .zip(&self.answers)
            .map(|(q, d)| QuestionAnswer {
                option_count: q.options.len(),
                multi_select: q.multi_select,
                selected: d.selected.clone(),
                write_in: (!d.write_in.trim().is_empty()).then(|| d.write_in.trim().to_string()),
            })
            .collect();
        Outcome::Submit(QuestionResponse::Answers { answers })
    }

    /// Footer hints for the current card state.
    pub fn hints(&self, p: &Palette) -> Vec<(&'static str, &'static str, Color)> {
        if self.confirm_skip {
            return vec![
                ("⏎ y", "skip question", p.love),
                ("any key", "cancel", p.subtle),
            ];
        }
        if self.is_writing() {
            let enter = if self.step + 1 < self.questions.len() {
                "next"
            } else {
                "send"
            };
            return vec![
                ("⏎", enter, p.pine),
                ("esc", "back to options", p.subtle),
                ("⌃U", "clear", p.subtle),
            ];
        }
        let multi = self
            .questions
            .get(self.step)
            .is_some_and(|q| q.multi_select);
        let on_write_in = self
            .questions
            .get(self.step)
            .zip(self.answers.get(self.step))
            .is_some_and(|(q, d)| d.cursor == q.options.len());
        let enter_label = if on_write_in {
            "write in"
        } else if self.step + 1 < self.questions.len() {
            "next"
        } else {
            "submit"
        };
        let mut hints = vec![("↑↓ 1-9", "pick", p.subtle)];
        if multi {
            hints.push(("space", "toggle", p.subtle));
        }
        hints.push(("⏎", enter_label, p.pine));
        if self.questions.len() > 1 && self.step > 0 {
            hints.push(("←", "prev", p.subtle));
        }
        hints.push(("s", "skip", p.subtle));
        hints.push(("esc", "leave", p.subtle));
        hints.push(("F1", "keys", p.subtle));
        hints
    }

    /// Preferred total height of the card in `width`, capped to `max_height`.
    pub fn desired_height(&self, width: u16, max_height: u16) -> u16 {
        let Some(q) = self.questions.get(self.step) else {
            return CARD_CHROME_ROWS + 1;
        };
        let inner_w = (width as usize).saturating_sub(4).max(1);
        let header_rows = question_header_lines(q, self.step, self.questions.len(), inner_w).len();
        let draft = &self.answers[self.step];
        let opt_rows: usize = (0..=q.options.len())
            .map(|i| option_row_count(q, draft, i, inner_w))
            .sum();
        // Top/bottom edges + gap + chips + header + blank line + option rows.
        let total = usize::from(CARD_CHROME_ROWS) + header_rows + 1 + opt_rows;
        cells(total).min(MAX_CARD_HEIGHT).min(max_height)
    }

    /// Draw the focused question card inside `card`.
    pub fn draw(
        &mut self,
        f: &mut Frame,
        p: &Palette,
        card: Rect,
        tokens: Option<&str>,
        corners: Option<&chrome::Corners>,
    ) {
        if card.height < CARD_CHROME_ROWS + 2 || card.width < 8 {
            return;
        }
        let Some(q) = self.questions.get(self.step) else {
            return;
        };
        let fill = p.overlay;
        draw_card_shell(f, p, card, fill, p.gold, corners);

        let inner_x = card.x + 2;
        let inner_w = (card.width as usize).saturating_sub(4).max(1);
        let header_lines = question_header_lines(q, self.step, self.questions.len(), inner_w);
        let body_top = card.y + 1;
        let chips_y = card.y + card.height - 2;
        let body_h = usize::from(chips_y.saturating_sub(body_top + 1));

        // Cap header lines so options always get at least 2 rows when possible.
        let max_header = body_h.saturating_sub(2).max(1);
        let header_count = header_lines.len().min(max_header);
        for (idx, text) in header_lines.iter().take(header_count).enumerate() {
            let y = body_top + cells(idx);
            let spans = style_header_line(text, idx == 0, q.multi_select, p, fill);
            f.render_widget(
                Line::from(spans),
                Rect {
                    x: inner_x,
                    y,
                    width: cells(inner_w),
                    height: 1,
                },
            );
        }

        // Leave a 1-row gap below header if there is room.
        let gap = usize::from(body_h > header_count + 1);
        let opt_top = body_top + cells(header_count + gap);
        let opt_h = usize::from(chips_y.saturating_sub(opt_top + 1));

        // Build all option display rows and track where the active cursor lies.
        let draft = &self.answers[self.step];
        let mut rendered: Vec<Line<'static>> = Vec::new();
        let mut cursor_start = 0usize;
        let mut cursor_end = 0usize;
        for i in 0..=q.options.len() {
            let start = rendered.len();
            let lines = render_option(q, draft, i, inner_w, p, fill);
            rendered.extend(lines);
            if i == draft.cursor {
                cursor_start = start;
                cursor_end = rendered.len();
            }
        }

        // Keep the active option scrolled into view.
        if opt_h > 0 {
            if cursor_start < self.scroll {
                self.scroll = cursor_start;
            } else if cursor_end > self.scroll + opt_h {
                self.scroll = cursor_end.saturating_sub(opt_h);
            }
            let max_scroll = rendered.len().saturating_sub(opt_h);
            self.scroll = self.scroll.min(max_scroll);

            for (row_idx, line) in rendered.iter().skip(self.scroll).take(opt_h).enumerate() {
                f.render_widget(
                    line.clone(),
                    Rect {
                        x: inner_x,
                        y: opt_top + cells(row_idx),
                        width: cells(inner_w),
                        height: 1,
                    },
                );
            }
        }

        let more = (
            self.scroll > 0,
            rendered.len().saturating_sub(self.scroll + opt_h) > 0,
        );
        let chips = Rect {
            x: card.x + 2,
            y: chips_y,
            width: card.width.saturating_sub(3),
            height: 1,
        };
        self.draw_chips(f, p, chips, fill, tokens, more);
    }

    fn draw_chips(
        &self,
        f: &mut Frame,
        p: &Palette,
        chips: Rect,
        fill: Color,
        tokens: Option<&str>,
        more: (bool, bool),
    ) {
        let mut left = Vec::new();
        if self.confirm_skip {
            left.push(Span::styled(
                "Skip question? ",
                Style::new()
                    .fg(p.love)
                    .bg(fill)
                    .add_modifier(Modifier::BOLD),
            ));
            left.push(Span::styled(
                "Enter or y to skip · any other key cancels",
                Style::new().fg(p.gold).bg(fill),
            ));
        } else {
            left.push(Span::styled("to ", Style::new().fg(p.muted).bg(fill)));
            left.push(Span::styled(
                self.label.clone(),
                Style::new()
                    .fg(p.text)
                    .bg(fill)
                    .add_modifier(Modifier::BOLD),
            ));
            if self.questions.len() > 1 {
                left.push(Span::styled(
                    format!("  ·  Question {}/{}", self.step + 1, self.questions.len()),
                    Style::new().fg(p.gold).bg(fill),
                ));
            }
            if let Some(hint) = &self.hint {
                left.push(Span::styled(
                    format!("  ·  {hint}"),
                    Style::new().fg(p.gold).bg(fill),
                ));
            }
        }

        let mut right = Vec::new();
        let (has_above, has_below) = more;
        if has_above || has_below {
            let arrows = match (has_above, has_below) {
                (true, true) => "↑↓ more  ",
                (true, false) => "↑ more  ",
                (false, true) => "↓ more  ",
                (false, false) => "",
            };
            right.push(Span::styled(arrows, Style::new().fg(p.gold).bg(fill)));
        }
        if let Some(t) = tokens {
            right.push(Span::styled(
                format!("{t} "),
                Style::new().fg(p.muted).bg(fill),
            ));
        }

        let left_w: usize = left.iter().map(|s| str_width(&s.content)).sum();
        let right_w: usize = right.iter().map(|s| str_width(&s.content)).sum();
        if left_w + right_w < chips.width as usize {
            f.render_widget(Line::from(right).right_aligned(), chips);
        }
        f.render_widget(Line::from(left), chips);
    }
}

/// Draw the unfocused bottom card for an `agy` agent waiting on a question:
/// soft half-block card on `p.surface`, a gold `?` prompt preview, and chips
/// naming the agent and question count.
pub fn draw_unfocused(
    f: &mut Frame,
    p: &Palette,
    card: Rect,
    label: &str,
    questions: &[PromptQuestion],
    tokens: Option<&str>,
    corners: Option<&chrome::Corners>,
) {
    if card.height < CARD_CHROME_ROWS + 1 || card.width < 8 {
        return;
    }
    let fill = p.surface;
    draw_card_shell(f, p, card, fill, p.gold, corners);
    let inner_w = (card.width as usize).saturating_sub(4).max(1);
    let first = questions.first().map_or("", |q| q.question.as_str());
    let prompt_prefix = "Tab to answer";
    let mut spans = vec![
        Span::styled(
            "? ",
            Style::new()
                .fg(p.gold)
                .bg(fill)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            prompt_prefix,
            Style::new()
                .fg(p.gold)
                .bg(fill)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    let used = 2 + str_width(prompt_prefix);
    let room = inner_w.saturating_sub(used + 3);
    if !first.is_empty() && room > 4 {
        spans.push(Span::styled(" · ", Style::new().fg(p.muted).bg(fill)));
        spans.push(Span::styled(
            preview::clip_width(first, room),
            Style::new().fg(p.text).bg(fill),
        ));
    }
    f.render_widget(
        Line::from(spans),
        Rect {
            x: card.x + 2,
            y: card.y + 1,
            width: cells(inner_w),
            height: 1,
        },
    );

    let chips = Rect {
        x: card.x + 2,
        y: card.y + card.height - 2,
        width: card.width.saturating_sub(3),
        height: 1,
    };
    let mut left = vec![
        Span::styled("to ", Style::new().fg(p.muted).bg(fill)),
        Span::styled(label.to_string(), Style::new().fg(p.subtle).bg(fill)),
    ];
    let count_badge = if questions.len() > 1 {
        format!("  ·  {} questions", questions.len())
    } else {
        "  ·  Question".to_string()
    };
    left.push(Span::styled(count_badge, Style::new().fg(p.gold).bg(fill)));

    let mut right = Vec::new();
    if let Some(t) = tokens {
        right.push(Span::styled(
            format!("{t} "),
            Style::new().fg(p.muted).bg(fill),
        ));
    }
    let left_w: usize = left.iter().map(|s| str_width(&s.content)).sum();
    let right_w: usize = right.iter().map(|s| str_width(&s.content)).sum();
    if left_w + right_w < chips.width as usize {
        f.render_widget(Line::from(right).right_aligned(), chips);
    }
    f.render_widget(Line::from(left), chips);
}

/// Draw the soft half-block card shell matching `PromptBox::draw`, with a
/// vertical accent bar on the left edge.
fn draw_card_shell(
    f: &mut Frame,
    p: &Palette,
    card: Rect,
    fill: Color,
    bar: Color,
    corners: Option<&chrome::Corners>,
) {
    let w = card.width as usize;
    let buf = f.buffer_mut();
    buf.set_style(
        Rect {
            y: card.y + 1,
            height: card.height - 2,
            ..card
        },
        Style::new().bg(fill),
    );
    let edge = |l: &str, mid: &str, r: &str| {
        Line::from(vec![
            Span::styled(l.to_string(), Style::new().fg(fill).bg(p.base)),
            Span::styled(mid.repeat(w - 2), Style::new().fg(fill).bg(p.base)),
            Span::styled(r.to_string(), Style::new().fg(fill).bg(p.base)),
        ])
    };
    f.render_widget(edge("▄", "▄", "▄"), Rect { height: 1, ..card });
    let bottom = card.y + card.height - 1;
    f.render_widget(
        edge("▀", "▀", "▀"),
        Rect {
            y: bottom,
            height: 1,
            ..card
        },
    );
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
    let buf = f.buffer_mut();
    for y in card.y + 1..card.y + card.height.saturating_sub(1) {
        buf[(card.x, y)]
            .set_symbol("▌")
            .set_style(Style::new().fg(bar).bg(fill));
    }
}

fn question_header_lines(
    q: &PromptQuestion,
    step: usize,
    total: usize,
    width: usize,
) -> Vec<String> {
    let prefix = if total > 1 {
        format!("? Question {}/{}: ", step + 1, total)
    } else {
        "? ".to_string()
    };
    let suffix = if q.multi_select {
        " (multi-select)"
    } else {
        ""
    };
    let full = format!("{prefix}{}{suffix}", q.question.trim());
    preview::wrap(&full, width.max(1))
}

fn style_header_line(
    line: &str,
    first: bool,
    multi_select: bool,
    p: &Palette,
    bg: Color,
) -> Vec<Span<'static>> {
    let gold = Style::new().fg(p.gold).bg(bg).add_modifier(Modifier::BOLD);
    let text_style = Style::new().fg(p.text).bg(bg).add_modifier(Modifier::BOLD);
    let badge_style = Style::new().fg(p.subtle).bg(bg);
    let mut spans = Vec::new();
    let mut rest = line;
    if first && let Some(after) = rest.strip_prefix("? ") {
        spans.push(Span::styled("? ", gold));
        if let Some((step_tag, tail)) = after.split_once(": ")
            && step_tag.starts_with("Question ")
        {
            spans.push(Span::styled(format!("{step_tag}: "), gold));
            rest = tail;
        } else {
            rest = after;
        }
    }
    if multi_select && let Some(before) = rest.strip_suffix(" (multi-select)") {
        if !before.is_empty() {
            spans.push(Span::styled(before.to_string(), text_style));
        }
        spans.push(Span::styled(" (multi-select)", badge_style));
    } else if !rest.is_empty() {
        spans.push(Span::styled(rest.to_string(), text_style));
    }
    spans
}

fn option_prefix_width(q: &PromptQuestion, index: usize) -> usize {
    // "▸ " (2) + "N. " (digits + 2) + optional "[x] " (4)
    let num = index + 1;
    let digits = if num >= 10 { 2 } else { 1 };
    let checkbox = if q.multi_select && index < q.options.len() {
        4
    } else {
        0
    };
    2 + digits + 2 + checkbox
}

fn option_body_text(q: &PromptQuestion, draft: &Draft, index: usize) -> String {
    if let Some(opt) = q.options.get(index) {
        return opt.clone();
    }
    if draft.writing {
        format!("Write-in: {}▏", draft.write_in)
    } else if !draft.write_in.trim().is_empty() {
        format!("Write-in: ✓ {}", draft.write_in.trim())
    } else {
        "Write-in…".to_string()
    }
}

fn option_row_count(q: &PromptQuestion, draft: &Draft, index: usize, width: usize) -> usize {
    let prefix_w = option_prefix_width(q, index);
    let body_w = width.saturating_sub(prefix_w).max(8);
    preview::wrap(&option_body_text(q, draft, index), body_w)
        .len()
        .max(1)
}

fn render_option(
    q: &PromptQuestion,
    draft: &Draft,
    index: usize,
    width: usize,
    p: &Palette,
    card_bg: Color,
) -> Vec<Line<'static>> {
    let active = index == draft.cursor;
    let is_write_in = index == q.options.len();
    let checked = draft.selected.contains(&index);
    let bg = if active { p.hl_med } else { card_bg };
    let prefix_w = option_prefix_width(q, index);
    let body_w = width.saturating_sub(prefix_w).max(8);
    let wrapped = preview::wrap(&option_body_text(q, draft, index), body_w);
    let indent = " ".repeat(prefix_w);

    wrapped
        .into_iter()
        .enumerate()
        .map(|(row, chunk)| {
            let mut spans = Vec::new();
            if row == 0 {
                spans.push(Span::styled(
                    if active { "▸ " } else { "  " },
                    Style::new().fg(p.gold).bg(bg).add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(
                    format!("{}. ", index + 1),
                    Style::new()
                        .fg(if active { p.gold } else { p.muted })
                        .bg(bg),
                ));
                if q.multi_select && !is_write_in {
                    spans.push(Span::styled(
                        if checked { "[x] " } else { "[ ] " },
                        Style::new()
                            .fg(if checked { p.pine } else { p.muted })
                            .bg(bg)
                            .add_modifier(if checked {
                                Modifier::BOLD
                            } else {
                                Modifier::empty()
                            }),
                    ));
                }
            } else {
                spans.push(Span::styled(indent.clone(), Style::new().bg(bg)));
            }

            if is_write_in {
                push_write_in_spans(&mut spans, &chunk, draft, active, p, bg);
            } else {
                push_option_text_spans(&mut spans, &chunk, active, checked, p, bg);
            }

            let used: usize = spans.iter().map(|s| str_width(&s.content)).sum();
            let pad = width.saturating_sub(used);
            if pad > 0 {
                spans.push(Span::styled(" ".repeat(pad), Style::new().bg(bg)));
            }
            Line::from(spans)
        })
        .collect()
}

fn push_write_in_spans(
    spans: &mut Vec<Span<'static>>,
    chunk: &str,
    draft: &Draft,
    active: bool,
    p: &Palette,
    bg: Color,
) {
    if draft.writing {
        if let Some(after) = chunk.strip_prefix("Write-in: ") {
            spans.push(Span::styled(
                "Write-in: ",
                Style::new().fg(p.gold).bg(bg).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                after.to_string(),
                Style::new().fg(p.text).bg(bg),
            ));
        } else {
            spans.push(Span::styled(
                chunk.to_string(),
                Style::new().fg(p.text).bg(bg),
            ));
        }
    } else if !draft.write_in.trim().is_empty() {
        if let Some(after) = chunk.strip_prefix("Write-in: ") {
            spans.push(Span::styled("Write-in: ", Style::new().fg(p.muted).bg(bg)));
            spans.push(Span::styled(
                after.to_string(),
                Style::new().fg(p.pine).bg(bg).add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                chunk.to_string(),
                Style::new().fg(p.pine).bg(bg),
            ));
        }
    } else {
        spans.push(Span::styled(
            chunk.to_string(),
            Style::new()
                .fg(if active { p.text } else { p.muted })
                .bg(bg)
                .add_modifier(Modifier::ITALIC),
        ));
    }
}

fn push_option_text_spans(
    spans: &mut Vec<Span<'static>>,
    chunk: &str,
    active: bool,
    checked: bool,
    p: &Palette,
    bg: Color,
) {
    let base = Style::new()
        .fg(if active || checked { p.text } else { p.subtle })
        .bg(bg)
        .add_modifier(if active {
            Modifier::BOLD
        } else {
            Modifier::empty()
        });
    if let Some(rest) = chunk.strip_prefix("(Recommended) ") {
        spans.push(Span::styled(
            "(Recommended) ",
            Style::new().fg(p.pine).bg(bg).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(rest.to_string(), base));
    } else if let Some(before) = chunk.strip_suffix(" (Recommended)") {
        spans.push(Span::styled(before.to_string(), base));
        spans.push(Span::styled(
            " (Recommended)",
            Style::new().fg(p.pine).bg(bg).add_modifier(Modifier::BOLD),
        ));
    } else {
        spans.push(Span::styled(chunk.to_string(), base));
    }
}

fn sanitize_inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_space = false;
    for c in text.chars() {
        if matches!(c, '\n' | '\r' | '\t') {
            if !last_space && !out.is_empty() {
                out.push(' ');
                last_space = true;
            }
        } else if !c.is_control() {
            out.push(c);
            last_space = c == ' ';
        }
    }
    out
}

fn delete_word(s: &mut String) {
    while s.ends_with(' ') {
        s.pop();
    }
    while s.chars().last().is_some_and(|c| c != ' ') {
        s.pop();
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    fn plain(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn sample_single() -> Vec<PromptQuestion> {
        vec![PromptQuestion {
            question: "How should an expanded sub-task render?".into(),
            options: vec!["(Recommended) Inline tree".into(), "Separate group".into()],
            multi_select: false,
        }]
    }

    fn sample_multi_step() -> Vec<PromptQuestion> {
        vec![
            PromptQuestion {
                question: "Pick a color".into(),
                options: vec!["Red".into(), "Blue".into()],
                multi_select: false,
            },
            PromptQuestion {
                question: "Pick toppings".into(),
                options: vec!["Cheese".into(), "Olives".into(), "Basil".into()],
                multi_select: true,
            },
        ]
    }

    #[test]
    fn single_select_submits_highlighted_option_on_enter_or_digit() {
        let mut card = QuestionCard::new(("s".into(), "w".into()), "demo".into(), sample_single());
        assert_eq!(card.key(plain(KeyCode::Down), 5), Outcome::Stay);
        assert_eq!(
            card.key(plain(KeyCode::Enter), 5),
            Outcome::Submit(QuestionResponse::Answers {
                answers: vec![QuestionAnswer {
                    option_count: 2,
                    multi_select: false,
                    selected: vec![1],
                    write_in: None,
                }]
            })
        );

        let mut card = QuestionCard::new(("s".into(), "w".into()), "demo".into(), sample_single());
        assert_eq!(
            card.key(plain(KeyCode::Char('1')), 5),
            Outcome::Submit(QuestionResponse::Answers {
                answers: vec![QuestionAnswer {
                    option_count: 2,
                    multi_select: false,
                    selected: vec![0],
                    write_in: None,
                }]
            })
        );
    }

    #[test]
    fn multi_step_supports_back_navigation_multi_select_and_write_in() {
        let mut card =
            QuestionCard::new(("s".into(), "w".into()), "demo".into(), sample_multi_step());
        // Step 1: pick Blue (index 1) -> advances to Step 2.
        assert_eq!(card.key(plain(KeyCode::Char('2')), 5), Outcome::Stay);
        assert_eq!(card.progress(), (2, 2));

        // Step 2: Enter with nothing selected stays and shows hint.
        assert_eq!(card.key(plain(KeyCode::Enter), 5), Outcome::Stay);
        assert!(card.hint.is_some());

        // Toggle Cheese (0) and Basil (2) with digits/space.
        assert_eq!(card.key(plain(KeyCode::Char('1')), 5), Outcome::Stay);
        assert_eq!(card.key(plain(KeyCode::Down), 5), Outcome::Stay);
        assert_eq!(card.key(plain(KeyCode::Down), 5), Outcome::Stay);
        assert_eq!(card.key(plain(KeyCode::Char(' ')), 5), Outcome::Stay);

        // Go back to Step 1 and change to Red (0).
        assert_eq!(card.key(plain(KeyCode::Left), 5), Outcome::Stay);
        assert_eq!(card.progress(), (1, 2));
        assert_eq!(card.key(plain(KeyCode::Char('1')), 5), Outcome::Stay);
        assert_eq!(card.progress(), (2, 2));

        // Add a write-in on Step 2 (index 3 is Write-in -> digit '4') and submit.
        assert_eq!(card.key(plain(KeyCode::Char('4')), 5), Outcome::Stay);
        assert!(card.is_writing());
        assert!(card.paste("extra\ngarlic"));
        assert_eq!(
            card.key(plain(KeyCode::Enter), 5),
            Outcome::Submit(QuestionResponse::Answers {
                answers: vec![
                    QuestionAnswer {
                        option_count: 2,
                        multi_select: false,
                        selected: vec![0],
                        write_in: None,
                    },
                    QuestionAnswer {
                        option_count: 3,
                        multi_select: true,
                        selected: vec![0, 2],
                        write_in: Some("extra garlic".into()),
                    },
                ]
            })
        );
    }

    #[test]
    fn skip_requires_confirmation_and_other_key_cancels() {
        let mut card = QuestionCard::new(("s".into(), "w".into()), "demo".into(), sample_single());
        assert_eq!(card.key(plain(KeyCode::Char('s')), 5), Outcome::Stay);
        assert!(card.confirm_skip);
        assert_eq!(card.key(plain(KeyCode::Char('n')), 5), Outcome::Stay);
        assert!(!card.confirm_skip);

        assert_eq!(card.key(plain(KeyCode::Char('s')), 5), Outcome::Stay);
        assert_eq!(
            card.key(plain(KeyCode::Char('y')), 5),
            Outcome::Submit(QuestionResponse::Skip)
        );
    }

    #[test]
    fn navigation_shortcuts_match_reply_box() {
        let mut card = QuestionCard::new(("s".into(), "w".into()), "demo".into(), sample_single());
        assert_eq!(card.key(ctrl('j'), 5), Outcome::MoveList(1));
        assert_eq!(card.key(ctrl('k'), 5), Outcome::MoveList(-1));
        assert_eq!(card.key(ctrl('s'), 5), Outcome::ToggleSidebar);
        assert_eq!(card.key(ctrl('v'), 5), Outcome::Scrollback(None));
        assert_eq!(card.key(ctrl('u'), 5), Outcome::Scrollback(Some(ctrl('u'))));
        assert_eq!(card.key(ctrl('x'), 5), Outcome::ArmArchive);
        assert_eq!(card.key(plain(KeyCode::Esc), 5), Outcome::Leave);
        assert_eq!(card.key(plain(KeyCode::Tab), 5), Outcome::Leave);
    }

    #[test]
    fn draws_focused_and_unfocused_cards() {
        let p = Palette::load();
        let mut card =
            QuestionCard::new(("s".into(), "w".into()), "my-task".into(), sample_single());
        let mut term = Terminal::new(TestBackend::new(80, 14)).unwrap();
        term.draw(|f| {
            let area = f.area();
            card.draw(f, &p, area, Some("gemini-3.8 · 10k tokens"), None);
        })
        .unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(
            text.contains("How should an expanded sub-task render?"),
            "{text}"
        );
        assert!(text.contains("(Recommended) Inline tree"), "{text}");
        assert!(text.contains("Write-in…"), "{text}");
        assert!(text.contains("to my-task"), "{text}");

        let mut term2 = Terminal::new(TestBackend::new(80, 6)).unwrap();
        term2
            .draw(|f| {
                let area = f.area();
                draw_unfocused(
                    f,
                    &p,
                    area,
                    "my-task",
                    &sample_single(),
                    Some("gemini-3.8"),
                    None,
                );
            })
            .unwrap();
        let text2: String = term2
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text2.contains("Tab to answer"), "{text2}");
        assert!(text2.contains("How should an expanded"), "{text2}");
        assert!(text2.contains("to my-task"), "{text2}");
    }
}
