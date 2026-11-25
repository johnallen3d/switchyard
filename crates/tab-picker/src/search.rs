//! The Search view (`⌃Q` / `⌘5`): full-text search across Atuin Threads
//! (both the personal `atuin` store and the local-only work `atuin-work`
//! store), mapping matched threads back to Switchyard tasks by agent session
//! ID.
//!
//! Pressing `⌃Q` opens Search with the query box focused. Enter submits the
//! query to Atuin in the background (both stores in parallel when the profile
//! filter is `all`, or only the selected store); results stay labelled by
//! store (`personal` vs `work`) and are never copied across stores. `⇥` or
//! `⌃F` cycles the `--harness` filter (`all`, `claude`, `pi`), `⌃O` cycles
//! the store/profile filter (`all`, `personal`, `work`), and `⌃R` re-runs the
//! current query.
//!
//! On a highlighted result mapped to a Switchyard task: Enter opens it in
//! Inbox (live task) or Archive (ended task), `⌘⏎` or `r` jumps directly to a
//! live task's tab or resumes an ended task, and `⌃Y` copies its store,
//! session ID, Atuin ID, and folder to the clipboard.

use std::sync::mpsc::{self, Receiver, TryRecvError};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use switchyard_core::atuin::{
    self, DEFAULT_SEARCH_LIMIT, SearchHit, SearchResult, Store, StoreError,
};
use switchyard_ui::Palette;
use switchyard_ui::draw::{self, delete_word, highlight, pill, str_width};
use switchyard_ui::glyphs::{ICON_SEARCH, SPINNER};

use crate::model::{Task, Tone, age, badges};
use crate::preview::{clip_width, model_label, wrap_line};

/// Lines each search result takes in the results list (title, store/task meta,
/// matched snippet).
const ROW_LINES: usize = 3;

/// Narrowest body width that splits into the results list on the left and the
/// highlighted match's detail on the right.
const SPLIT_MIN_WIDTH: u16 = 90;

/// Which Atuin harness to filter `--harness` by (`all`, `claude`, or `pi`).
/// Antigravity (`agy`) is not captured by Atuin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HarnessFilter {
    #[default]
    All,
    Claude,
    Pi,
}

impl HarnessFilter {
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            HarnessFilter::All => HarnessFilter::Claude,
            HarnessFilter::Claude => HarnessFilter::Pi,
            HarnessFilter::Pi => HarnessFilter::All,
        }
    }

    /// Switchyard harness name for `atuin::search`, or `None` for all.
    #[must_use]
    pub const fn as_arg(self) -> Option<&'static str> {
        match self {
            HarnessFilter::All => None,
            HarnessFilter::Claude => Some("claude"),
            HarnessFilter::Pi => Some("pi"),
        }
    }

    /// Header chip label when filtered.
    #[must_use]
    pub const fn label(self) -> Option<&'static str> {
        self.as_arg()
    }
}

/// What a key in Search asks of the picker.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Leave the key to the picker (global views, profile filter, help).
    Pass,
    /// Handled inside Search.
    Stay,
    /// Run a background Atuin search with the current query and filters.
    RunSearch,
    /// Focus the mapped task in Inbox (if open) or Archive (if ended).
    FocusTask(usize),
    /// Jump to an open task's tab or resume an ended task directly.
    GoOrResume(usize),
    /// Copy `(clipboard_text, footer_note)`.
    Copy(String, String),
    /// Show a refusal note in the footer.
    Refuse(String),
}

/// Shared read-only context passed into [`Search::draw`].
pub struct DrawCtx<'a> {
    pub palette: &'a Palette,
    pub tasks: &'a [Task],
    pub stores: &'a [Store],
    pub now: u64,
    pub tick: usize,
}

/// Help float section for the Search view.
#[must_use]
pub fn help_section() -> crate::help::Section {
    crate::help::Section::new(
        "Transcript search",
        &[
            (
                "⏎ (typing)",
                "search personal (atuin) and work (atuin-work) stores",
            ),
            (
                "⏎ (results)",
                "open mapped task in Inbox (live) or Archive (ended)",
            ),
            ("⌘⏎ r", "jump to live task's tab, or resume ended task"),
            ("/", "focus the query box to edit"),
            ("esc", "leave the query box for results"),
            ("⇥ ⌃F", "cycle harness filter (all, claude, pi)"),
            ("⌃O", "cycle store filter (all, personal, work)"),
            ("⌃R", "run the search again"),
            ("⌃Y", "copy store, harness, session ID and folder"),
            ("↑↓ j k ⌃P ⌃N", "move through results"),
            ("⌃D ⌃U PgDn PgUp", "page through results"),
            ("⇧↑↓", "scroll match detail beside the list"),
            ("g G Home End", "first, last result"),
            ("⌃Q", "back to the previous view"),
        ],
    )
}

/// The Search view's state.
#[derive(Default)]
pub struct Search {
    /// Text currently in the transcript search bar.
    pub query: String,
    /// Query that produced `hits` (or is currently in flight).
    pub last_query: String,
    /// Whether the search bar has focus for typing.
    pub typing: bool,
    /// `--harness` filter (`all` / `claude` / `pi`).
    pub harness: HarnessFilter,
    /// Combined hits from the queried store(s), sorted by score.
    pub hits: Vec<SearchHit>,
    /// Store errors from the last search.
    pub errors: Vec<StoreError>,
    /// Highlighted hit index in `hits`.
    pub selected: usize,
    /// Index of the first hit shown in the list.
    offset: usize,
    /// Line scroll offset of the right-hand detail pane.
    detail_scroll: usize,
    /// Whether at least one search has been submitted.
    pub has_searched: bool,
    /// Monotonic counter so stale background results are discarded.
    generation: u64,
    /// Active background search `(generation, receiver)`.
    inflight: Option<(u64, Receiver<SearchResult>)>,
}

/// Map an Atuin [`SearchHit`] back to its Switchyard [`Task`] by agent session
/// ID, enforcing store/profile separation (`personal` vs `work`).
///
/// When multiple registry entries share an `agent_session` (for example, an
/// ended task and a newer resumed open task), an open task wins over an ended
/// one, and newer `since` wins ties.
#[must_use]
pub fn match_task(hit: &SearchHit, tasks: &[Task]) -> Option<usize> {
    if hit.session_id.is_empty() {
        return None;
    }
    tasks
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            !t.agent_session.is_empty()
                && t.agent_session == hit.session_id
                && hit.store.matches_profile(&t.profile)
        })
        .max_by_key(|(_, t)| (t.ended.is_none(), t.since))
        .map(|(i, _)| i)
}

impl Search {
    /// Create a fresh Search state, optionally focusing the query box.
    #[must_use]
    pub fn new(typing: bool) -> Self {
        Search {
            typing,
            ..Search::default()
        }
    }

    /// Whether a background Atuin search is currently running.
    #[must_use]
    pub fn loading(&self) -> bool {
        self.inflight.is_some()
    }

    /// Start a background Atuin search for `self.query` across `stores`.
    pub fn start(&mut self, stores: &[Store]) {
        let query = self.query.trim().to_string();
        self.last_query.clone_from(&query);
        self.detail_scroll = 0;
        if query.is_empty() {
            self.generation += 1;
            self.inflight = None;
            self.hits.clear();
            self.errors.clear();
            self.selected = 0;
            self.offset = 0;
            self.has_searched = false;
            return;
        }
        self.has_searched = true;
        self.generation += 1;
        let id = self.generation;
        let stores = stores.to_vec();
        let harness = self.harness.as_arg();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = atuin::search(&stores, &query, harness, DEFAULT_SEARCH_LIMIT);
            let _ = tx.send(result);
        });
        self.inflight = Some((id, rx));
    }

    /// Receive finished background search results, if ready. Returns `true`
    /// when results arrived.
    pub fn poll(&mut self) -> bool {
        let Some((id, rx)) = &self.inflight else {
            return false;
        };
        let id = *id;
        match rx.try_recv() {
            Ok(result) => {
                self.inflight = None;
                if id == self.generation {
                    self.apply_result(result);
                }
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.inflight = None;
                true
            }
        }
    }

    /// Apply a completed search result (also used directly in tests).
    pub fn apply_result(&mut self, result: SearchResult) {
        self.hits = result.hits;
        self.errors = result.errors;
        self.selected = 0;
        self.offset = 0;
        self.detail_scroll = 0;
    }

    /// Paste text into the search box when typing. Returns `true` if consumed.
    pub fn paste(&mut self, text: &str) -> bool {
        if !self.typing {
            return false;
        }
        let cleaned: String = text
            .chars()
            .map(|c| if matches!(c, '\r' | '\n') { ' ' } else { c })
            .collect();
        self.query.push_str(&cleaned);
        true
    }

    /// Handle a key press while in the Search view.
    pub fn key(&mut self, k: KeyEvent, tasks: &[Task], page: usize) -> Outcome {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);

        // Global keys shared across typing and browsing.
        if matches!(k.code, KeyCode::Char('q' | 'a' | 'o') if ctrl) || k.code == KeyCode::F(1) {
            return Outcome::Pass;
        }
        if matches!(k.code, KeyCode::Tab | KeyCode::BackTab)
            || matches!(k.code, KeyCode::Char('f') if ctrl)
        {
            self.harness = self.harness.next();
            return if self.last_query.is_empty() {
                Outcome::Stay
            } else {
                Outcome::RunSearch
            };
        }
        if matches!(k.code, KeyCode::Char('r') if ctrl) {
            return if self.query.trim().is_empty() {
                Outcome::Stay
            } else {
                Outcome::RunSearch
            };
        }

        if self.typing {
            return self.typing_key(k);
        }
        self.browse_key(k, tasks, page)
    }

    fn typing_key(&mut self, k: KeyEvent) -> Outcome {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        match k.code {
            KeyCode::Enter => {
                if !self.query.trim().is_empty() {
                    self.typing = false;
                }
                Outcome::RunSearch
            }
            KeyCode::Esc => {
                self.typing = false;
                Outcome::Stay
            }
            KeyCode::Char('u') if ctrl => {
                self.query.clear();
                Outcome::Stay
            }
            KeyCode::Char('w') if ctrl => {
                delete_word(&mut self.query);
                Outcome::Stay
            }
            KeyCode::Backspace if alt || ctrl => {
                delete_word(&mut self.query);
                Outcome::Stay
            }
            KeyCode::Backspace => {
                self.query.pop();
                Outcome::Stay
            }
            KeyCode::Up if shift => {
                self.detail_scroll = self.detail_scroll.saturating_sub(3);
                Outcome::Stay
            }
            KeyCode::Down if shift => {
                self.detail_scroll = self.detail_scroll.saturating_add(3);
                Outcome::Stay
            }
            KeyCode::Up => {
                self.step(-1);
                Outcome::Stay
            }
            KeyCode::Down => {
                self.step(1);
                Outcome::Stay
            }
            KeyCode::Char('p') if ctrl => {
                self.step(-1);
                Outcome::Stay
            }
            KeyCode::Char('n') if ctrl => {
                self.step(1);
                Outcome::Stay
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                self.query.push(c);
                Outcome::Stay
            }
            _ => Outcome::Stay,
        }
    }

    fn browse_key(&mut self, k: KeyEvent, tasks: &[Task], page: usize) -> Outcome {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let cmd = k.modifiers.contains(KeyModifiers::SUPER);
        let half = isize::try_from((page / 2).max(1)).unwrap_or(1);
        let full = isize::try_from(page.max(1)).unwrap_or(1);
        match k.code {
            KeyCode::Char('?') if !ctrl && !alt => Outcome::Pass,
            KeyCode::Char('/') if !ctrl && !alt => {
                self.typing = true;
                Outcome::Stay
            }
            KeyCode::Char('w') if ctrl => {
                delete_word(&mut self.query);
                self.typing = true;
                Outcome::Stay
            }
            KeyCode::Backspace if alt || ctrl => {
                delete_word(&mut self.query);
                self.typing = true;
                Outcome::Stay
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.typing = true;
                Outcome::Stay
            }
            KeyCode::Up if shift => {
                self.detail_scroll = self.detail_scroll.saturating_sub(3);
                Outcome::Stay
            }
            KeyCode::Down if shift => {
                self.detail_scroll = self.detail_scroll.saturating_add(3);
                Outcome::Stay
            }
            KeyCode::Up => {
                self.step(-1);
                Outcome::Stay
            }
            KeyCode::Down => {
                self.step(1);
                Outcome::Stay
            }
            KeyCode::Char('j' | 'n') if ctrl => {
                self.step(1);
                Outcome::Stay
            }
            KeyCode::Char('k' | 'p') if ctrl => {
                self.step(-1);
                Outcome::Stay
            }
            KeyCode::Char('j') if !ctrl && !alt => {
                self.step(1);
                Outcome::Stay
            }
            KeyCode::Char('k') if !ctrl && !alt => {
                self.step(-1);
                Outcome::Stay
            }
            KeyCode::Char('d') if ctrl => {
                self.step(half);
                Outcome::Stay
            }
            KeyCode::Char('u') if ctrl => {
                self.step(-half);
                Outcome::Stay
            }
            KeyCode::PageDown => {
                self.step(full);
                Outcome::Stay
            }
            KeyCode::PageUp => {
                self.step(-full);
                Outcome::Stay
            }
            KeyCode::Home | KeyCode::Char('g') if !ctrl && !alt => {
                self.select(0);
                Outcome::Stay
            }
            KeyCode::End | KeyCode::Char('G') if !ctrl && !alt => {
                self.select(self.hits.len().saturating_sub(1));
                Outcome::Stay
            }
            KeyCode::Enter if cmd => self.activate_selected(tasks, true),
            KeyCode::Enter => self.activate_selected(tasks, false),
            KeyCode::Char('r') if !ctrl && !alt => self.activate_selected(tasks, true),
            KeyCode::Char('y') if ctrl => self.copy_selected(tasks),
            _ => Outcome::Stay,
        }
    }

    fn step(&mut self, delta: isize) {
        if self.hits.is_empty() {
            return;
        }
        let max = self.hits.len() - 1;
        let next = self.selected.saturating_add_signed(delta).min(max);
        self.select(next);
    }

    fn select(&mut self, index: usize) {
        let clamped = index.min(self.hits.len().saturating_sub(1));
        if clamped != self.selected {
            self.selected = clamped;
            self.detail_scroll = 0;
        }
    }

    fn activate_selected(&self, tasks: &[Task], direct: bool) -> Outcome {
        let Some(hit) = self.hits.get(self.selected) else {
            return Outcome::Stay;
        };
        if let Some(idx) = match_task(hit, tasks) {
            if direct {
                Outcome::GoOrResume(idx)
            } else {
                Outcome::FocusTask(idx)
            }
        } else {
            Outcome::Refuse(format!(
                "thread {} ({}) isn't in Switchyard's task registry · ⌃Y copies its session ID",
                short_id(&hit.session_id),
                hit.store.label()
            ))
        }
    }

    fn copy_selected(&self, tasks: &[Task]) -> Outcome {
        let Some(hit) = self.hits.get(self.selected) else {
            return Outcome::Refuse("highlight a search result to copy its session ID".into());
        };
        let mut lines = vec![
            format!("store: {} ({})", hit.store.label(), hit.store.program()),
            format!("harness: {}", hit.harness),
            format!("session: {}", hit.session_id),
        ];
        if !hit.atuin_id.is_empty() {
            lines.push(format!("atuin_id: {}", hit.atuin_id));
        }
        if !hit.cwd.is_empty() {
            lines.push(format!("cwd: {}", hit.cwd));
        }
        if let Some(t) = match_task(hit, tasks).map(|i| &tasks[i]) {
            lines.push(format!("task: {} ({})", t.label(), t.host));
        }
        let note = format!(
            "copied {} · {} · {}",
            hit.store.label(),
            hit.harness,
            short_id(&hit.session_id)
        );
        Outcome::Copy(lines.join("\n"), note)
    }

    /// Footer hints for the Search view.
    #[must_use]
    pub fn hints(&self, tasks: &[Task]) -> Vec<(&'static str, &'static str)> {
        if self.typing {
            return vec![
                ("⏎", "search"),
                ("⇥", "harness"),
                ("⌃O", "store"),
                ("esc", "results"),
                ("F1", "keys"),
            ];
        }
        let mut hints = Vec::new();
        if let Some(hit) = self.hits.get(self.selected)
            && let Some(idx) = match_task(hit, tasks)
        {
            if tasks[idx].ended.is_none() {
                hints.push(("⏎", "inbox"));
                hints.push(("r", "go"));
            } else {
                hints.push(("⏎", "archive"));
                hints.push(("r", "resume"));
            }
        }
        hints.extend([
            ("/", "query"),
            ("⇥", "harness"),
            ("⌃O", "store"),
            ("?", "keys"),
        ]);
        hints
    }

    /// Draw the Search view's search bar (top line above the rule).
    pub fn draw_search_bar(&self, f: &mut Frame, p: &Palette, area: Rect, tick: usize) {
        let placeholder = "Search transcripts (words, \"exact phrase\") · ⏎ to search";
        let mut right = Vec::new();
        if self.loading() {
            let spin = SPINNER[tick % SPINNER.len()];
            right.push(Span::styled(
                format!("{spin} searching… "),
                Style::new().fg(p.foam),
            ));
        } else if self.has_searched {
            let personal = self
                .hits
                .iter()
                .filter(|h| h.store == Store::Personal)
                .count();
            let work = self.hits.iter().filter(|h| h.store == Store::Work).count();
            let total = self.hits.len();
            let noun = if total == 1 { "hit" } else { "hits" };
            right.push(Span::styled(format!("{total} "), Style::new().fg(p.text)));
            right.push(Span::styled(noun.to_string(), Style::new().fg(p.muted)));
            if personal > 0 && work > 0 {
                right.push(Span::styled(" (", Style::new().fg(p.muted)));
                right.push(Span::styled(
                    format!("{personal} personal"),
                    Style::new().fg(p.subtle),
                ));
                right.push(Span::styled(" · ", Style::new().fg(p.muted)));
                right.push(Span::styled(
                    format!("{work} work"),
                    Style::new().fg(p.gold),
                ));
                right.push(Span::styled(")", Style::new().fg(p.muted)));
            }
            if let Some(warn) = self.partial_warning() {
                right.push(Span::styled(format!(" · {warn}"), Style::new().fg(p.love)));
            }
            right.push(Span::raw(" "));
        }
        let right_w: usize = right.iter().map(|s| str_width(&s.content)).sum();
        let room = usize::from(area.width).saturating_sub(4 + right_w + 1);
        let clipped = if room == 0 {
            String::new()
        } else {
            clip_width(placeholder, room)
        };
        draw::search(f, p, area, &self.query, &clipped, self.typing, self.typing);
        if !right.is_empty() {
            f.render_widget(Line::from(right).right_aligned(), area);
        }
    }

    /// Warning text when some stores succeeded and another failed (ignoring a
    /// missing `atuin-work` binary when personal succeeded).
    fn partial_warning(&self) -> Option<String> {
        if self.hits.is_empty() {
            return None;
        }
        let err = self
            .errors
            .iter()
            .find(|e| !(e.store == Store::Work && e.missing))?;
        Some(format!("{} store failed", err.store.label()))
    }

    /// Draw the Search view body: results list (and detail pane when wide), or
    /// empty/loading/error state.
    pub fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &DrawCtx<'_>) {
        let p = ctx.palette;
        if self.loading() && self.hits.is_empty() {
            let names: Vec<&str> = ctx.stores.iter().map(|s| s.label()).collect();
            let what = format!("Searching {} transcripts…", names.join(" and "));
            draw::centered(
                f,
                area,
                vec![
                    Line::styled(
                        SPINNER[ctx.tick % SPINNER.len()].to_string(),
                        Style::new().fg(p.foam),
                    ),
                    Line::from(""),
                    Line::styled(what, Style::new().fg(p.subtle)),
                ],
            );
            return;
        }
        if !self.has_searched {
            draw_welcome(f, area, p, ctx.stores);
            return;
        }
        if self.hits.is_empty() {
            self.draw_empty(f, area, p, ctx.stores);
            return;
        }
        if area.width >= SPLIT_MIN_WIDTH {
            let left_w = (area.width * 46 / 100).clamp(38, 66);
            let [list, gap, detail] = Layout::horizontal([
                Constraint::Length(left_w),
                Constraint::Length(3),
                Constraint::Min(0),
            ])
            .areas(area);
            self.draw_list(f, list, p, ctx.tasks, ctx.now);
            let buf = f.buffer_mut();
            for y in gap.y..gap.y + gap.height {
                buf[(gap.x + 1, y)]
                    .set_symbol("│")
                    .set_style(Style::new().fg(p.hl_med));
            }
            self.draw_detail(f, detail, p, ctx.tasks, ctx.now);
        } else {
            self.draw_list(f, area, p, ctx.tasks, ctx.now);
        }
    }

    fn draw_empty(&self, f: &mut Frame, area: Rect, p: &Palette, stores: &[Store]) {
        let fatal: Vec<&StoreError> = self
            .errors
            .iter()
            .filter(|e| stores.len() == 1 || !(e.store == Store::Work && e.missing))
            .collect();
        if !fatal.is_empty() {
            let mut lines = vec![
                Line::styled("✗", Style::new().fg(p.love)),
                Line::from(""),
                Line::styled("Transcript search failed", Style::new().fg(p.text)),
            ];
            for err in fatal {
                lines.push(Line::styled(
                    format!("{}: {}", err.store.label(), err.message),
                    Style::new().fg(p.muted),
                ));
            }
            draw::centered(f, area, lines);
            return;
        }
        draw::centered(
            f,
            area,
            vec![
                Line::styled(ICON_SEARCH, Style::new().fg(p.hl_high)),
                Line::from(""),
                Line::from(vec![
                    Span::styled("No transcripts match ", Style::new().fg(p.subtle)),
                    Span::styled(format!("“{}”", self.last_query), Style::new().fg(p.text)),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled("/", Style::new().fg(p.subtle).bg(p.overlay)),
                    Span::styled(" edit query   ", Style::new().fg(p.muted)),
                    Span::styled("⌃O", Style::new().fg(p.subtle).bg(p.overlay)),
                    Span::styled(" store   ", Style::new().fg(p.muted)),
                    Span::styled("⇥", Style::new().fg(p.subtle).bg(p.overlay)),
                    Span::styled(" harness", Style::new().fg(p.muted)),
                ]),
            ],
        );
    }

    fn draw_list(&mut self, f: &mut Frame, area: Rect, p: &Palette, tasks: &[Task], now: u64) {
        let height = area.height as usize;
        if height == 0 {
            return;
        }
        let visible_rows = (height / ROW_LINES).max(1);
        self.selected = self.selected.min(self.hits.len().saturating_sub(1));
        self.offset = draw::keep_in_view(self.offset, self.selected, visible_rows, false);
        let overflow = self.hits.len() > visible_rows;
        let list = Rect {
            width: area.width.saturating_sub(if overflow { 2 } else { 0 }),
            ..area
        };
        let mut row_y = 0;
        for (idx, hit) in self.hits.iter().enumerate().skip(self.offset) {
            if row_y >= height {
                break;
            }
            let row_h = ROW_LINES.min(height - row_y);
            let row_rect = Rect {
                y: area.y + crate::cells(row_y),
                height: crate::cells(row_h),
                ..list
            };
            let task = match_task(hit, tasks).map(|t_idx| &tasks[t_idx]);
            draw_hit_row(f, row_rect, p, hit, task, idx == self.selected, now);
            row_y += row_h;
        }
        if overflow {
            draw::scrollbar(
                f,
                p,
                area,
                self.offset * ROW_LINES,
                self.hits.len() * ROW_LINES,
            );
        }
    }

    fn draw_detail(&mut self, f: &mut Frame, area: Rect, p: &Palette, tasks: &[Task], now: u64) {
        let Some(hit) = self.hits.get(self.selected) else {
            return;
        };
        let task = match_task(hit, tasks).map(|idx| &tasks[idx]);
        let text_w = usize::from(area.width.saturating_sub(2)).max(1);
        let lines = detail_lines(hit, task, p, text_w, now);
        let h = area.height as usize;
        let max_scroll = lines.len().saturating_sub(h);
        self.detail_scroll = self.detail_scroll.min(max_scroll);
        let text_area = Rect {
            width: area
                .width
                .saturating_sub(if lines.len() > h { 2 } else { 0 }),
            ..area
        };
        let shown: Vec<Line> = lines
            .iter()
            .skip(self.detail_scroll)
            .take(h)
            .cloned()
            .collect();
        f.render_widget(Paragraph::new(shown), text_area);
        if lines.len() > h {
            draw::scrollbar(f, p, area, self.detail_scroll, lines.len());
        }
    }
}

/// Convert UTF-8 byte ranges inside `text` into sorted char indices suitable
/// for [`switchyard_ui::draw::highlight`].
#[must_use]
pub fn byte_ranges_to_char_indices(text: &str, ranges: &[(usize, usize)]) -> Vec<usize> {
    if ranges.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (char_idx, (byte_idx, _)) in text.char_indices().enumerate() {
        if ranges.iter().any(|&(a, b)| byte_idx >= a && byte_idx < b) {
            out.push(char_idx);
        }
    }
    out
}

fn task_glyph_and_label(task: Option<&Task>, p: &Palette) -> (&'static str, Color, &'static str) {
    match task {
        Some(t) if t.ended.is_none() => match t.state.as_str() {
            "blocked" => ("●", p.gold, "waiting"),
            "error" => ("✗", p.love, "failed"),
            "done" => ("✓", p.pine, "done"),
            "working" => ("●", p.foam, "working"),
            _ => ("●", p.pine, "live"),
        },
        Some(t) => match t.ended.as_deref() {
            Some("merged") => ("✓", p.iris, "merged"),
            Some("archived" | "force-archived" | "discarded" | "force-discarded") => {
                ("▢", p.subtle, "archived")
            }
            Some("tab closed") => ("×", p.muted, "closed"),
            _ => ("·", p.muted, "ended"),
        },
        None => ("○", p.muted, "no task"),
    }
}

fn display_title<'a>(
    hit: &'a SearchHit,
    task: Option<&'a Task>,
) -> (&'a str, &'a [(usize, usize)]) {
    if !hit.title.is_empty() {
        return (&hit.title, &hit.title_matches);
    }
    if let Some(t) = task {
        return (t.shown_name(), &[]);
    }
    if !hit.first_prompt.is_empty() {
        return (hit.first_prompt.lines().next().unwrap_or(""), &[]);
    }
    (&hit.session_id, &[])
}

fn draw_welcome(f: &mut Frame, area: Rect, p: &Palette, stores: &[Store]) {
    let store_desc = match stores {
        [Store::Personal] => "personal (atuin)",
        [Store::Work] => "work (atuin-work)",
        _ => "personal (atuin) and work (atuin-work)",
    };
    draw::centered(
        f,
        area,
        vec![
            Line::styled(ICON_SEARCH, Style::new().fg(p.iris)),
            Line::from(""),
            Line::styled(
                "Search across tasks' transcripts",
                Style::new().fg(p.text).add_modifier(Modifier::BOLD),
            ),
            Line::styled(
                format!("Searches {store_desc} stores"),
                Style::new().fg(p.subtle),
            ),
            Line::styled(
                "Files changed, commands run, and prompts",
                Style::new().fg(p.muted),
            ),
            Line::from(""),
            draw::key_hint(p, "⏎", " search (quote words for an exact phrase)   "),
            Line::from(vec![
                Span::styled("⇥", Style::new().fg(p.subtle).bg(p.overlay)),
                Span::styled(" harness   ", Style::new().fg(p.muted)),
                Span::styled("⌃O", Style::new().fg(p.subtle).bg(p.overlay)),
                Span::styled(" store (personal / work)", Style::new().fg(p.muted)),
            ]),
            Line::from(""),
            Line::styled(
                "Claude and Pi sessions are captured; agy and notes aren't",
                Style::new().fg(p.muted),
            ),
        ],
    );
}

struct RowLook<'a> {
    edge: &'a Span<'static>,
    glyph: &'static str,
    glyph_color: Color,
    status_word: &'static str,
    selected: bool,
    bg: Color,
    hl: Style,
    now: u64,
}

fn draw_hit_row(
    f: &mut Frame,
    row_rect: Rect,
    p: &Palette,
    hit: &SearchHit,
    task: Option<&Task>,
    selected: bool,
    now: u64,
) {
    let bg = if selected { p.overlay } else { p.base };
    let edge = if selected {
        Span::styled("▎", Style::new().fg(p.iris).bg(bg))
    } else {
        Span::styled(" ", Style::new().bg(bg))
    };
    f.buffer_mut().set_style(row_rect, Style::new().bg(bg));
    let (glyph, glyph_color, status_word) = task_glyph_and_label(task, p);
    let look = RowLook {
        edge: &edge,
        glyph,
        glyph_color,
        status_word,
        selected,
        bg,
        hl: Style::new()
            .fg(p.rose)
            .bg(bg)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        now,
    };
    draw_hit_title(
        f,
        Rect {
            height: 1,
            ..row_rect
        },
        p,
        hit,
        task,
        &look,
    );
    if row_rect.height < 2 {
        return;
    }
    draw_hit_meta(
        f,
        Rect {
            y: row_rect.y + 1,
            height: 1,
            ..row_rect
        },
        p,
        hit,
        task,
        &look,
    );
    if row_rect.height < 3 {
        return;
    }
    draw_hit_snippet(
        f,
        Rect {
            y: row_rect.y + 2,
            height: 1,
            ..row_rect
        },
        p,
        hit,
        &look,
    );
}

fn draw_hit_title(
    f: &mut Frame,
    line_rect: Rect,
    p: &Palette,
    hit: &SearchHit,
    task: Option<&Task>,
    look: &RowLook<'_>,
) {
    let when = if hit.updated_at > 0 {
        format!("{} ", age(look.now.saturating_sub(hit.updated_at)))
    } else {
        String::new()
    };
    let (title_text, title_ranges) = display_title(hit, task);
    let title_hits = byte_ranges_to_char_indices(title_text, title_ranges);
    let mut title_style = Style::new().fg(p.text).bg(look.bg);
    if look.selected {
        title_style = title_style.add_modifier(Modifier::BOLD);
    }
    let room = usize::from(line_rect.width).saturating_sub(3 + str_width(&when) + 1);
    let mut spans = vec![
        look.edge.clone(),
        Span::styled(
            format!("{} ", look.glyph),
            Style::new().fg(look.glyph_color).bg(look.bg),
        ),
    ];
    spans.extend(highlight(
        title_text,
        &title_hits,
        room,
        false,
        title_style,
        look.hl,
    ));
    f.render_widget(Line::from(spans), line_rect);
    f.render_widget(
        Line::from(Span::styled(when, Style::new().fg(p.muted).bg(look.bg))).right_aligned(),
        line_rect,
    );
}

fn draw_hit_meta(
    f: &mut Frame,
    line_rect: Rect,
    p: &Palette,
    hit: &SearchHit,
    task: Option<&Task>,
    look: &RowLook<'_>,
) {
    let width = usize::from(line_rect.width);
    let store_fg = match hit.store {
        Store::Work => p.gold,
        Store::Personal => p.foam,
    };
    let mut spans = vec![
        look.edge.clone(),
        Span::styled("  ", Style::new().bg(look.bg)),
    ];
    spans.extend(pill(
        vec![Span::styled(
            hit.store.label().to_string(),
            Style::new().fg(store_fg),
        )],
        p.surface,
        look.bg,
    ));
    let mut used: usize = spans.iter().map(|s| str_width(&s.content)).sum();
    let mut push_text = |text: &str, fg: Color| {
        let rem = width.saturating_sub(used + 1);
        if rem == 0 {
            return;
        }
        let clipped = clip_width(text, rem);
        used += str_width(&clipped);
        spans.push(Span::styled(clipped, Style::new().fg(fg).bg(look.bg)));
    };
    push_text(" ", p.muted);
    if let Some(t) = task {
        push_text(&t.label(), p.iris);
    } else {
        let proj = crate::folder_project(&hit.cwd);
        if !proj.is_empty() {
            push_text(&proj, p.subtle);
        }
        if !hit.git_branch.is_empty() {
            push_text(&format!(" ({})", hit.git_branch), p.muted);
        }
    }
    if !hit.harness.is_empty() {
        push_text(&format!(" · {}", hit.harness), p.muted);
    }
    push_text(&format!(" · {}", look.status_word), look.glyph_color);
    f.render_widget(Line::from(spans), line_rect);
}

fn draw_hit_snippet(
    f: &mut Frame,
    line_rect: Rect,
    p: &Palette,
    hit: &SearchHit,
    look: &RowLook<'_>,
) {
    let (snip, snip_ranges) = if hit.snippet.is_empty() {
        (hit.first_prompt.as_str(), &[][..])
    } else {
        (hit.snippet.as_str(), hit.snippet_matches.as_slice())
    };
    let one_line = snip.replace(['\r', '\n'], " ");
    let snip_hits = if hit.snippet.is_empty() {
        Vec::new()
    } else {
        byte_ranges_to_char_indices(&one_line, snip_ranges)
    };
    let room = usize::from(line_rect.width).saturating_sub(4);
    let snip_style = Style::new()
        .fg(if look.selected { p.subtle } else { p.muted })
        .bg(look.bg);
    let mut spans = vec![
        look.edge.clone(),
        Span::styled("  ", Style::new().bg(look.bg)),
    ];
    spans.extend(highlight(
        &one_line, &snip_hits, room, false, snip_style, look.hl,
    ));
    f.render_widget(Line::from(spans), line_rect);
}

/// Build the wrapped lines for the right-hand detail pane of `hit`.
fn detail_lines(
    hit: &SearchHit,
    task: Option<&Task>,
    p: &Palette,
    width: usize,
    now: u64,
) -> Vec<Line<'static>> {
    let hl = Style::new()
        .fg(p.rose)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let mut lines = Vec::new();
    detail_title_and_task(&mut lines, hit, task, p, width, hl);
    detail_agent_meta(&mut lines, hit, task, p, width, now);
    detail_body_lines(&mut lines, hit, p, width, hl);
    lines
}

fn detail_title_and_task(
    lines: &mut Vec<Line<'static>>,
    hit: &SearchHit,
    task: Option<&Task>,
    p: &Palette,
    width: usize,
    hl: Style,
) {
    let bold = Style::new().fg(p.text).add_modifier(Modifier::BOLD);
    let (title_text, title_ranges) = display_title(hit, task);
    let title_hits = byte_ranges_to_char_indices(title_text, title_ranges);
    let title_line = Line::from(highlight(
        title_text,
        &title_hits,
        usize::MAX,
        false,
        bold,
        hl,
    ));
    lines.extend(wrap_line(title_line, width));
    lines.push(Line::default());

    let store_fg = match hit.store {
        Store::Work => p.gold,
        Store::Personal => p.foam,
    };
    let mut store_spans = vec![Span::styled("store    ", Style::new().fg(p.muted))];
    store_spans.extend(pill(
        vec![Span::styled(
            format!("{} · {}", hit.store.label(), hit.store.program()),
            Style::new().fg(store_fg),
        )],
        p.surface,
        p.base,
    ));
    lines.push(Line::from(store_spans));

    let (glyph, glyph_color, status_word) = task_glyph_and_label(task, p);
    if let Some(t) = task {
        let mut task_spans = vec![
            Span::styled("task     ", Style::new().fg(p.muted)),
            Span::styled(format!("{glyph} "), Style::new().fg(glyph_color)),
            Span::styled(t.label(), Style::new().fg(p.iris)),
            Span::styled(format!(" · {status_word}"), Style::new().fg(glyph_color)),
        ];
        if !t.host.is_empty() {
            task_spans.push(Span::styled(
                format!(" · {}", t.host),
                Style::new().fg(p.subtle),
            ));
        }
        lines.extend(wrap_line(Line::from(task_spans), width));
        if !t.links.is_empty() {
            let mut link_spans = vec![Span::styled("items    ", Style::new().fg(p.muted))];
            for (text, tone) in badges(&t.links, width.saturating_sub(10)) {
                let fg = match tone {
                    Tone::Ref => p.subtle,
                    Tone::Muted => p.muted,
                    Tone::Merged => p.iris,
                    Tone::Pass => p.pine,
                    Tone::Fail => p.love,
                    Tone::Pending => p.gold,
                };
                link_spans.push(Span::styled(text, Style::new().fg(fg)));
            }
            lines.push(Line::from(link_spans));
        }
    } else {
        lines.push(Line::from(vec![
            Span::styled("task     ", Style::new().fg(p.muted)),
            Span::styled(
                "not in Switchyard's task registry",
                Style::new().fg(p.muted),
            ),
        ]));
    }
}

fn detail_agent_meta(
    lines: &mut Vec<Line<'static>>,
    hit: &SearchHit,
    task: Option<&Task>,
    p: &Palette,
    width: usize,
    now: u64,
) {
    let mut meta_parts = Vec::new();
    if !hit.harness.is_empty() {
        meta_parts.push(hit.harness.clone());
    }
    if !hit.model.is_empty() {
        meta_parts.push(model_label(&hit.model).to_string());
    }
    if hit.message_count > 0 {
        meta_parts.push(format!("{} msgs", hit.message_count));
    }
    if hit.updated_at > 0 {
        meta_parts.push(format!("{} ago", age(now.saturating_sub(hit.updated_at))));
    }
    if !meta_parts.is_empty() {
        lines.extend(wrap_line(
            Line::from(vec![
                Span::styled("agent    ", Style::new().fg(p.muted)),
                Span::styled(meta_parts.join(" · "), Style::new().fg(p.subtle)),
            ]),
            width,
        ));
    }
    let branch = if hit.git_branch.is_empty() {
        task.map_or("", |t| t.branch.as_str())
    } else {
        hit.git_branch.as_str()
    };
    if !branch.is_empty() {
        lines.extend(wrap_line(
            Line::from(vec![
                Span::styled("branch   ", Style::new().fg(p.muted)),
                Span::styled(branch.to_string(), Style::new().fg(p.subtle)),
            ]),
            width,
        ));
    }
    if !hit.cwd.is_empty() {
        lines.extend(wrap_line(
            Line::from(vec![
                Span::styled("folder   ", Style::new().fg(p.muted)),
                Span::styled(
                    switchyard_core::folders::home_relative(&hit.cwd),
                    Style::new().fg(p.muted),
                ),
            ]),
            width,
        ));
    }
    if !hit.session_id.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("session  ", Style::new().fg(p.muted)),
            Span::styled(
                clip_width(&hit.session_id, width.saturating_sub(9)),
                Style::new().fg(p.muted),
            ),
        ]));
    }
}

fn detail_body_lines(
    lines: &mut Vec<Line<'static>>,
    hit: &SearchHit,
    p: &Palette,
    width: usize,
    hl: Style,
) {
    lines.push(Line::default());
    lines.push(Line::styled("─".repeat(width), Style::new().fg(p.overlay)));

    if !hit.snippet.is_empty() {
        lines.push(Line::default());
        let heading = match hit.message_index {
            Some(i) => format!("Match (message #{i})"),
            None => "Match".to_string(),
        };
        lines.push(Line::styled(
            heading,
            Style::new().fg(p.iris).add_modifier(Modifier::BOLD),
        ));
        let snip_hits = byte_ranges_to_char_indices(&hit.snippet, &hit.snippet_matches);
        let snip_line = Line::from(highlight(
            &hit.snippet,
            &snip_hits,
            usize::MAX,
            false,
            Style::new().fg(p.text),
            hl,
        ));
        lines.extend(wrap_line(snip_line, width));
    }

    if !hit.first_prompt.is_empty() {
        lines.push(Line::default());
        lines.push(Line::styled(
            "Prompt",
            Style::new().fg(p.subtle).add_modifier(Modifier::BOLD),
        ));
        for raw in hit.first_prompt.lines().take(8) {
            lines.extend(wrap_line(
                Line::styled(raw.to_string(), Style::new().fg(p.subtle)),
                width,
            ));
        }
    }

    if !hit.last_reply.is_empty() {
        lines.push(Line::default());
        lines.push(Line::styled(
            "Last reply",
            Style::new().fg(p.subtle).add_modifier(Modifier::BOLD),
        ));
        for raw in hit.last_reply.lines().take(12) {
            lines.extend(wrap_line(
                Line::styled(raw.to_string(), Style::new().fg(p.muted)),
                width,
            ));
        }
    }
}

fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn sample_task(
        name: &str,
        profile: &str,
        agent_session: &str,
        ended: Option<&str>,
        since: u64,
    ) -> Task {
        Task {
            host: "m4-mbp".into(),
            session_id: "s1".into(),
            window_id: format!("w-{name}"),
            project: "switchyard".into(),
            repo: "~/dev/src/playground/switchyard".into(),
            worktree: format!("~/dev/src/playground/switchyard/.worktrees/{name}"),
            branch: format!("feature/{name}"),
            name: name.into(),
            title: String::new(),
            profile: profile.into(),
            harness: "claude".into(),
            model: "opus".into(),
            agent_session: agent_session.into(),
            dirty: 0,
            dirty_files: Vec::new(),
            state: "done".into(),
            msg: String::new(),
            since,
            ended: ended.map(String::from),
            left: false,
            links: Vec::new(),
            merged: String::new(),
            pinned: false,
            snoozed_until: 0,
            wake_at: 0,
            tab: None,
            parent_window: String::new(),
        }
    }

    fn sample_hit(store: Store, session_id: &str, title: &str, snippet: &str) -> SearchHit {
        SearchHit {
            store,
            harness: "claude".into(),
            session_id: session_id.into(),
            atuin_id: "01a20464386f71c7".into(),
            cwd: "/Users/john/dev/src/playground/switchyard/.worktrees/demo".into(),
            git_branch: "feature/demo".into(),
            model: "claude-opus-5-5".into(),
            started_at: 100,
            updated_at: 200,
            message_count: 42,
            title: title.into(),
            title_matches: Vec::new(),
            snippet: snippet.into(),
            snippet_matches: Vec::new(),
            first_prompt: "fix fed.rs".into(),
            last_reply: "all done".into(),
            message_index: Some(12),
            score: 7.5,
        }
    }

    #[test]
    fn match_task_isolates_personal_and_work_stores_and_prefers_open() {
        let tasks = vec![
            sample_task("old-ended", "personal", "sess-1", Some("archived"), 100),
            sample_task("live-open", "personal", "sess-1", None, 200),
            sample_task("work-task", "work", "sess-1", None, 300),
        ];
        let personal_hit = sample_hit(Store::Personal, "sess-1", "t", "s");
        assert_eq!(match_task(&personal_hit, &tasks), Some(1));

        let work_hit = sample_hit(Store::Work, "sess-1", "t", "s");
        assert_eq!(match_task(&work_hit, &tasks), Some(2));

        let unmatched = sample_hit(Store::Personal, "sess-missing", "t", "s");
        assert_eq!(match_task(&unmatched, &tasks), None);
    }

    #[test]
    fn byte_ranges_convert_across_multibyte_chars() {
        // `…` is 3 UTF-8 bytes (0..3), then `fed.rs` is bytes 3..9 (chars 1..7).
        let text = "…fed.rs";
        assert_eq!(
            byte_ranges_to_char_indices(text, &[(3, 6), (7, 9)]),
            vec![1, 2, 3, 5, 6]
        );
    }

    #[test]
    fn typing_submit_navigation_harness_cycle_and_actions() {
        let tasks = vec![
            sample_task("live-task", "personal", "sess-live", None, 200),
            sample_task("ended-task", "work", "sess-ended", Some("archived"), 100),
        ];
        let mut s = Search {
            typing: true,
            ..Search::default()
        };
        for c in "fed.rs".chars() {
            assert_eq!(s.key(key(KeyCode::Char(c)), &tasks, 10), Outcome::Stay);
        }
        assert_eq!(s.query, "fed.rs");
        assert_eq!(s.key(key(KeyCode::Enter), &tasks, 10), Outcome::RunSearch);
        assert!(!s.typing);

        s.has_searched = true;
        s.last_query = "fed.rs".into();
        s.apply_result(SearchResult {
            hits: vec![
                sample_hit(Store::Personal, "sess-live", "Live hit", "touched fed.rs"),
                sample_hit(Store::Work, "sess-ended", "Work hit", "read fed.rs"),
                sample_hit(Store::Personal, "sess-untracked", "Old hit", "fed.rs"),
            ],
            errors: Vec::new(),
        });

        // Enter on open task focuses it in Inbox; `r` jumps directly.
        assert_eq!(
            s.key(key(KeyCode::Enter), &tasks, 10),
            Outcome::FocusTask(0)
        );
        assert_eq!(
            s.key(key(KeyCode::Char('r')), &tasks, 10),
            Outcome::GoOrResume(0)
        );
        // Move down to work ended task.
        assert_eq!(s.key(key(KeyCode::Char('j')), &tasks, 10), Outcome::Stay);
        assert_eq!(s.selected, 1);
        assert_eq!(
            s.key(key(KeyCode::Enter), &tasks, 10),
            Outcome::FocusTask(1)
        );
        assert_eq!(
            s.key(key(KeyCode::Char('r')), &tasks, 10),
            Outcome::GoOrResume(1)
        );
        // Move down to untracked thread.
        assert_eq!(s.key(key(KeyCode::Down), &tasks, 10), Outcome::Stay);
        assert!(matches!(
            s.key(key(KeyCode::Enter), &tasks, 10),
            Outcome::Refuse(_)
        ));
        // ctrl-y copies session info even on untracked thread.
        assert!(matches!(s.key(ctrl('y'), &tasks, 10), Outcome::Copy(_, _)));

        // Tab cycles harness filter and re-runs active search.
        assert_eq!(s.key(key(KeyCode::Tab), &tasks, 10), Outcome::RunSearch);
        assert_eq!(s.harness, HarnessFilter::Claude);
        assert_eq!(s.key(ctrl('f'), &tasks, 10), Outcome::RunSearch);
        assert_eq!(s.harness, HarnessFilter::Pi);
        assert_eq!(s.key(key(KeyCode::Tab), &tasks, 10), Outcome::RunSearch);
        assert_eq!(s.harness, HarnessFilter::All);

        // `/` returns focus to query box.
        assert_eq!(s.key(key(KeyCode::Char('/')), &tasks, 10), Outcome::Stay);
        assert!(s.typing);
    }

    #[test]
    fn draws_split_view_with_store_labels_and_mapped_task() {
        let p = Palette::load();
        let tasks = vec![sample_task(
            "fix-fed",
            "work",
            "sess-1",
            Some("archived"),
            100,
        )];
        let mut s = Search {
            query: "fed.rs".into(),
            last_query: "fed.rs".into(),
            has_searched: true,
            ..Search::default()
        };
        let mut hit = sample_hit(Store::Work, "sess-1", "Fix fed.rs", "edited crates/fed.rs");
        hit.snippet_matches = vec![(14, 20)];
        s.apply_result(SearchResult {
            hits: vec![hit],
            errors: Vec::new(),
        });
        let mut term = Terminal::new(TestBackend::new(110, 24)).unwrap();
        term.draw(|f| {
            let area = f.area();
            let ctx = DrawCtx {
                palette: &p,
                tasks: &tasks,
                stores: &Store::ALL,
                now: 500,
                tick: 0,
            };
            s.draw(f, area, &ctx);
        })
        .unwrap();
        let screen: String = (0..24)
            .map(|y| {
                (0..110)
                    .map(|x| term.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("work"), "{screen}");
        assert!(screen.contains("atuin-work"), "{screen}");
        assert!(screen.contains("switchyard/fix-fed"), "{screen}");
        assert!(screen.contains("edited crates/fed.rs"), "{screen}");
        assert!(screen.contains("Match (message #12)"), "{screen}");
    }
}
