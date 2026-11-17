//! The Work view's right side: the highlighted item's description and
//! comments (core's `item_view`), and for a pull request its diff and
//! commits (core's `changes::pull_request`), which ctrl-l cycles through
//! (see `Lens`). ctrl-e gives it the whole view.
//!
//! Each item is fetched once the highlight has rested on it for `SETTLE`,
//! in the background, and kept until ctrl-r lists the project again; its
//! diff and commits only once a lens asks for them. The pane starts at the
//! top of what it shows and stays where John scrolled it until the item
//! or the lens changes.
//!
//! ctrl-v moves the keys into it as scrollback (see scrollback.rs): a line
//! cursor, search and copy, with `]` and `[` between a diff's files or a
//! thread's comments, and Enter on a file in the diff's list going to its
//! patch.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use serde_json::Value;
use switchyard_core::item_view::Thread;
use switchyard_ui::Palette;
use switchyard_ui::draw::{self, str_width};

use crate::changes::{self, Changes};
use crate::model::Link;
use crate::preview::{self, Pane};
use crate::scrollback::{self, Nav, Scrollback};

/// How long the highlight rests on an item before it is fetched, so
/// stepping through the list doesn't start a fetch a row.
const SETTLE: Duration = Duration::from_millis(200);

/// What the right side shows of the highlighted item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lens {
    #[default]
    About,
    Diff,
    Commits,
}

impl Lens {
    const ALL: [Lens; 3] = [Lens::About, Lens::Diff, Lens::Commits];

    /// The one ctrl-l moves to.
    pub fn next(self) -> Lens {
        match self {
            Lens::About => Lens::Diff,
            Lens::Diff => Lens::Commits,
            Lens::Commits => Lens::About,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Lens::About => "Description",
            Lens::Diff => "Diff",
            Lens::Commits => "Commits",
        }
    }

    fn changes(self) -> changes::Lens {
        match self {
            Lens::Commits => changes::Lens::Commits,
            _ => changes::Lens::Diff,
        }
    }
}

/// A fetch's result, for the item at a URL.
enum Got {
    Thread(Result<Thread, String>),
    Changes(Box<Value>),
}

/// What has been fetched of one item, and what has been asked for.
#[derive(Default)]
struct Entry {
    thread: Option<Result<Thread, String>>,
    changes: Option<Changes>,
    asked_thread: bool,
    asked_changes: bool,
}

pub struct Detail {
    lens: Lens,
    /// The right side has the whole view (ctrl-e).
    pub expanded: bool,
    cache: HashMap<String, Entry>,
    tx: Sender<(String, Got)>,
    rx: Receiver<(String, Got)>,
    /// The highlighted item's URL, and since when.
    shown: Option<(String, Instant)>,
    /// The first line shown, for the item and lens it was scrolled on.
    top: (usize, Option<(String, Lens)>),
    /// The text's area as last drawn: its width wraps the lines
    /// scrollback moves through, its height pages.
    text: Rect,
    /// Drawn this frame, so scrollback has lines to move through.
    pub visible: bool,
    /// ctrl-v (or ctrl-u or ctrl-d, the key in it) gave it the view:
    /// enter scrollback once it is drawn, then the key.
    pub want_scroll: bool,
    /// The key to give scrollback once `want_scroll` enters it.
    pub want_key: Option<ratatui::crossterm::event::KeyEvent>,
    /// The description's lines as last drawn: URL, width, lines and the
    /// comments' headers as sections.
    drawn: Option<(String, u16, Vec<Line<'static>>, Nav)>,
}

impl Default for Detail {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Detail {
            lens: Lens::About,
            expanded: false,
            cache: HashMap::new(),
            tx,
            rx,
            shown: None,
            top: (0, None),
            text: Rect::default(),
            visible: false,
            want_scroll: false,
            want_key: None,
            drawn: None,
        }
    }
}

/// Whether a link is to a pull request, which has a diff and commits.
fn is_pr(link: &Link) -> bool {
    link.kind == "pr" && link.system == "github"
}

impl Detail {
    /// The lens shown for `link`: the chosen one for a pull request, else
    /// its description.
    pub fn lens_for(&self, link: &Link) -> Lens {
        if is_pr(link) { self.lens } else { Lens::About }
    }

    /// ctrl-l: the next lens, for a pull request; why not, for others.
    ///
    /// # Errors
    ///
    /// Fails, saying why, when `link` is not a pull request.
    pub fn cycle(&mut self, link: &Link) -> Result<Lens, String> {
        if !is_pr(link) {
            return Err("only a pull request has a diff and commits".into());
        }
        self.lens = self.lens.next();
        Ok(self.lens)
    }

    /// Forget what was fetched, so it is fetched again.
    pub fn forget(&mut self) {
        self.cache.clear();
        self.drawn = None;
    }

    /// Take finished fetches, and start what the highlighted item (`link`,
    /// read under `profile`) needs once the highlight has settled.
    pub fn poll(&mut self, link: Option<&Link>, profile: &str) {
        while let Ok((url, got)) = self.rx.try_recv() {
            let e = self.cache.entry(url.clone()).or_default();
            match got {
                Got::Thread(t) => {
                    e.thread = Some(t);
                    if self.drawn.as_ref().is_some_and(|d| d.0 == url) {
                        self.drawn = None;
                    }
                }
                Got::Changes(v) => e.changes = Some(changes::from_value(&v)),
            }
        }
        let Some(link) = link else {
            self.shown = None;
            return;
        };
        if self.shown.as_ref().is_none_or(|(u, _)| *u != link.url) {
            self.shown = Some((link.url.clone(), Instant::now()));
        }
        if self
            .shown
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() < SETTLE)
        {
            return;
        }
        let want_changes = self.lens_for(link) != Lens::About;
        let e = self.cache.entry(link.url.clone()).or_default();
        if !e.asked_thread {
            e.asked_thread = true;
            let (tx, l, profile) = (self.tx.clone(), link.clone(), profile.to_string());
            std::thread::spawn(move || {
                let t = crate::standalone::item_thread(&l, &profile);
                let _ = tx.send((l.url, Got::Thread(t)));
            });
        }
        if want_changes && !e.asked_changes {
            e.asked_changes = true;
            let (tx, l, profile) = (self.tx.clone(), link.clone(), profile.to_string());
            std::thread::spawn(move || {
                let v = switchyard_core::changes::pull_request(&l.reference, &profile);
                let _ = tx.send((l.url, Got::Changes(Box::new(v))));
            });
        }
    }

    /// Whether the highlighted item's shown lens is waiting on a fetch,
    /// for the spinner's quicker redraws.
    pub fn busy(&self, link: Option<&Link>) -> bool {
        let Some(link) = link else { return false };
        let Some(e) = self.cache.get(&link.url) else {
            return true;
        };
        match self.lens_for(link) {
            Lens::About => e.thread.is_none(),
            _ => e.changes.is_none(),
        }
    }

    /// Scroll by `delta` lines (positive: further down); `draw` keeps it
    /// within what is shown.
    pub fn scroll(&mut self, delta: isize) {
        self.top.0 = self.top.0.saturating_add_signed(delta);
    }

    /// A page of the text, less two lines for context.
    pub fn page(&self) -> isize {
        isize::try_from(self.text.height.saturating_sub(2).max(1)).unwrap_or(1)
    }

    /// The first line shown.
    pub fn top(&self) -> usize {
        self.top.0
    }

    /// Show from `line` (where scrollback left it).
    pub fn set_top(&mut self, line: usize) {
        self.top.0 = line;
    }

    /// The text's area as last drawn.
    pub fn text_area(&self) -> Rect {
        self.text
    }

    /// What scrollback over `link` is for: another item or lens leaves it.
    pub fn pane(&self, link: &Link) -> Pane {
        Pane {
            target: "work".into(),
            session: self.lens_for(link).name().into(),
            window: link.url.clone(),
        }
    }

    /// The shown lines as text, with their sections, for scrollback; None
    /// until they have been drawn and fetched.
    pub fn text(&mut self, link: &Link, p: &Palette, now: u64) -> Option<(Vec<String>, Nav)> {
        let width = self.text.width;
        if !self.visible || width == 0 {
            return None;
        }
        let (lines, nav) = self.lines(link, width, p, now).ok()?;
        Some((lines.iter().map(scrollback::line_text).collect(), nav))
    }

    /// `link`'s lines for its lens, wrapped to `width`, and their sections;
    /// else what to say instead.
    fn lines(
        &mut self,
        link: &Link,
        width: u16,
        p: &Palette,
        now: u64,
    ) -> Result<(&[Line<'static>], Nav), String> {
        let lens = self.lens_for(link);
        let reading = || format!("Reading {}…", link.short());
        let Some(e) = self.cache.get_mut(&link.url) else {
            return Err(reading());
        };
        if lens != Lens::About {
            let c = e.changes.as_mut().ok_or("Reading the changes…")?;
            // Draw them first, so their sections are the ones for `width`;
            // the second call is the cached lines.
            c.lines(lens.changes(), width, now, p)?;
            let nav = c.nav();
            return Ok((c.lines(lens.changes(), width, now, p)?, nav));
        }
        let thread = match &e.thread {
            None => return Err(reading()),
            Some(Err(err)) => return Err(err.clone()),
            Some(Ok(t)) => t,
        };
        if self
            .drawn
            .as_ref()
            .is_none_or(|(u, w, _, _)| *u != link.url || *w != width)
        {
            let lines = crate::items::reader_lines(thread, link, usize::from(width), p);
            // Each comment opens with a `── who` header.
            let marks = lines
                .iter()
                .enumerate()
                .filter(|(_, l)| scrollback::line_text(l).starts_with("── "))
                .map(|(i, _)| i)
                .collect();
            let nav = Nav {
                marks,
                jumps: Vec::new(),
                what: "comments",
            };
            self.drawn = Some((link.url.clone(), width, lines, nav));
        }
        let (_, _, lines, nav) = self.drawn.as_ref().ok_or("not drawn")?;
        Ok((lines.as_slice(), nav.clone()))
    }

    /// Scroll to the top, or as far down as it goes.
    pub fn edge(&mut self, end: bool) {
        self.top.0 = if end { usize::MAX } else { 0 };
    }

    /// The right side for `link`: a line naming the lenses (the one shown
    /// lit; only the description for other than a pull request) and what
    /// the diff or commits come to, then its text with a scrollbar, under
    /// scrollback's cursor while `sb` is on.
    pub fn draw(
        &mut self,
        f: &mut Frame,
        area: Rect,
        p: &Palette,
        link: Option<&Link>,
        now: u64,
        sb: Option<&mut Scrollback>,
    ) {
        let muted = |text: &str| vec![Line::styled(text.to_string(), Style::new().fg(p.muted))];
        if area.height < 3 {
            return;
        }
        let head = Rect { height: 1, ..area };
        let screen = Rect {
            y: area.y + 2,
            height: area.height - 2,
            ..area
        };
        let Some(link) = link else {
            return draw::centered(f, screen, muted("Highlight an item to see it here"));
        };
        self.visible = true;
        let lens = self.lens_for(link);
        let key = (link.url.clone(), lens);
        if self.top.1.as_ref() != Some(&key) {
            self.top = (0, Some(key));
        }
        // Leave a column for the scrollbar.
        let text = Rect {
            width: screen.width.saturating_sub(2),
            ..screen
        };
        self.text = text;
        f.render_widget(self.head_line(link, lens, head.width, p), head);
        let lines = match self.lines(link, text.width, p, now) {
            Err(why) => return draw::centered(f, screen, muted(&why)),
            Ok((lines, _)) => lines.to_vec(),
        };
        if let Some(sb) = sb {
            crate::draw_scrollback(f, p, screen, text, &lines, sb);
            self.top.0 = sb.top;
            return;
        }
        let h = text.height as usize;
        let top = self.top.0.min(lines.len().saturating_sub(h));
        self.top.0 = top;
        let end = lines.len().min(top + h);
        f.render_widget(Paragraph::new(lines[top..end].to_vec()), text);
        if lines.len() > h {
            draw::scrollbar(f, p, screen, top, lines.len());
        }
    }

    /// The lenses, the one shown lit, then what the diff or commits come
    /// to, in `width`.
    fn head_line(&self, link: &Link, lens: Lens, width: u16, p: &Palette) -> Line<'static> {
        let lenses: &[Lens] = if is_pr(link) {
            &Lens::ALL
        } else {
            &[Lens::About]
        };
        let mut spans = vec![Span::raw(" ")];
        for &l in lenses {
            let style = if l == lens {
                Style::new().fg(p.iris).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(p.muted)
            };
            spans.push(Span::styled(l.name(), style));
            spans.push(Span::raw("  "));
        }
        let summary = self
            .cache
            .get(&link.url)
            .and_then(|e| e.changes.as_ref())
            .filter(|_| lens != Lens::About)
            .and_then(|c| c.summary(lens.changes()));
        if let Some(sum) = summary {
            let used: usize = spans.iter().map(|s| str_width(&s.content)).sum();
            let room = (width as usize).saturating_sub(used);
            spans.push(Span::styled(
                preview::clip_width(&sum, room),
                Style::new().fg(p.subtle),
            ));
        }
        Line::from(spans)
    }
}

#[cfg(test)]
impl Detail {
    /// Have `url`'s changes as if fetched.
    pub fn put_changes(&mut self, url: &str, v: &Value) {
        let e = self.cache.entry(url.to_string()).or_default();
        e.asked_thread = true;
        e.asked_changes = true;
        e.changes = Some(changes::from_value(v));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(kind: &str, system: &str) -> Link {
        Link {
            kind: kind.into(),
            system: system.into(),
            reference: "o/r#5".into(),
            url: "https://github.com/o/r/pull/5".into(),
            state: String::new(),
            checks: String::new(),
            title: String::new(),
            column: String::new(),
            assignee: String::new(),
        }
    }

    #[test]
    fn only_a_pull_request_cycles_through_its_diff_and_commits() {
        let mut d = Detail::default();
        let pr = link("pr", "github");
        let issue = link("issue", "github");
        assert!(d.cycle(&issue).is_err());
        assert_eq!(d.cycle(&pr), Ok(Lens::Diff));
        // An issue shows its description whatever the lens.
        assert_eq!(d.lens_for(&issue), Lens::About);
        assert_eq!(d.cycle(&pr), Ok(Lens::Commits));
        assert_eq!(d.cycle(&pr), Ok(Lens::About));
    }

    #[test]
    fn nothing_is_fetched_until_the_highlight_settles() {
        let mut d = Detail::default();
        let pr = link("pr", "github");
        d.poll(Some(&pr), "");
        assert!(d.cache.is_empty());
        assert!(d.busy(Some(&pr)));
    }
}
