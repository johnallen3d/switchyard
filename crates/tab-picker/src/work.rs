//! The Work view: one catalog project's open work items, the open cards on
//! its Fizzy board or the open issues in its GitHub repo (see core's
//! `items`), to start a session from.
//!
//! It opens on the highlighted task's project, when that is a catalog
//! project, else on the last one shown. Tab opens a chooser over the
//! catalog (type to filter, Enter to switch); ctrl-r lists the project
//! again. Each project is listed once, in the background, and kept.
//!
//! Items no open task links to come first. Those an open task links to
//! follow, dimmed, under "With a task", each naming its task. Typing
//! searches titles and numbers, as in the other views.
//!
//! On the highlighted item: Enter starts a session from it (auto host,
//! claude, named from the item; see core's `launch`), in the background,
//! or on a dimmed item jumps to its task; ctrl-v reads it in place (the
//! work items float's reader, see items.rs); ctrl-b opens it in the
//! browser. This holds the view's state and keys; `standalone_key` runs
//! what they ask for (see `Outcome`).

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear};
use switchyard_core::hosts::Project;
use switchyard_core::items::{OpenItem, OpenItems};
use switchyard_core::links::{Tracker, parse_ref};
use switchyard_ui::Palette;
use switchyard_ui::draw::{delete_word, highlight, keep_in_view, str_width};
use switchyard_ui::fuzzy::score_fields;
use switchyard_ui::glyphs::SPINNER;

use crate::model::{Link, Task};

/// A project's listing.
#[derive(Clone, Debug, PartialEq)]
pub enum Listing {
    Loading,
    Ready(OpenItems),
    Failed(String),
}

/// The open task an item is linked to, for its row and for Enter to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Linked {
    pub label: String,
    pub host: String,
    pub session: String,
    pub window: String,
}

/// A row: an item of the shown listing (its index there), the task linked
/// to it, if any, and the title's search hits.
#[derive(Clone, Debug)]
pub struct Row {
    pub index: usize,
    pub linked: Option<Linked>,
    pub hits: Vec<usize>,
}

/// A line of the drawn list.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ListLine {
    Row(usize),
    Gap,
    Heading,
}

/// The project chooser Tab opens: a filter over the catalog.
#[derive(Default)]
pub struct Chooser {
    pub query: String,
    pub selected: usize,
}

/// What a key asks of the picker.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Not a Work key: the list's own (search, views, help).
    Pass,
    /// Handled here.
    Stay,
    /// Start a session from `url` in `project` (its catalog path).
    Launch {
        path: String,
        url: String,
        name: String,
    },
    /// Show the linked task's tab.
    Go(Linked),
    /// Read the item in place.
    Read(Link),
    /// Open the URL in the browser here.
    Open(String),
    /// Why the key does nothing here, for the footer.
    Refuse(String),
}

pub struct Work {
    /// The catalog, read the first time the view opens.
    pub projects: Vec<Project>,
    /// Why the catalog couldn't be read.
    pub catalog_error: Option<String>,
    catalog_read: bool,
    /// The shown project, an index into `projects`.
    pub project: usize,
    lists: HashMap<String, Listing>,
    tx: Sender<(String, Result<OpenItems, String>)>,
    rx: Receiver<(String, Result<OpenItems, String>)>,
    pub rows: Vec<Row>,
    pub selected: usize,
    offset: usize,
    pub chooser: Option<Chooser>,
    /// Items a launch is starting from, by URL, until a task links them.
    pub starting: HashSet<String>,
}

impl Default for Work {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Work {
            projects: Vec::new(),
            catalog_error: None,
            catalog_read: false,
            project: 0,
            lists: HashMap::new(),
            tx,
            rx,
            rows: Vec::new(),
            selected: 0,
            offset: 0,
            chooser: None,
            starting: HashSet::new(),
        }
    }
}

/// An item's link fields (kind, system, ref) from its URL, as a task's
/// links have them.
fn link_key(url: &str) -> Option<(String, String, String)> {
    let l = parse_ref(url, &Tracker::default())?;
    let s = |k: &str| l[k].as_str().unwrap_or("").to_string();
    Some((s("kind"), s("system"), s("ref")))
}

/// An item as a link, for the reader.
fn item_link(item: &OpenItem) -> Link {
    let (kind, system, reference) = link_key(&item.url).unwrap_or_default();
    Link {
        kind,
        system,
        reference,
        url: item.url.clone(),
        state: String::new(),
        checks: String::new(),
        title: item.title.clone(),
        column: String::new(),
        assignee: String::new(),
    }
}

impl Work {
    /// Read the catalog the first time, and show `project` (a catalog
    /// project's name) if given and in it; list the shown project when it
    /// hasn't been.
    pub fn open(&mut self, project: Option<&str>) {
        if !self.catalog_read {
            self.catalog_read = true;
            match switchyard_core::hosts::catalog() {
                Ok(p) => self.projects = p,
                Err(e) => self.catalog_error = Some(e),
            }
        }
        if let Some(i) = project.and_then(|name| self.projects.iter().position(|p| p.name == name))
        {
            self.show(i);
        }
        self.load(false);
    }

    /// Show project `i`, from its first item.
    fn show(&mut self, i: usize) {
        if i != self.project {
            self.project = i;
            // The rows index the last project's items.
            self.rows.clear();
            self.selected = 0;
            self.offset = 0;
        }
    }

    pub fn current(&self) -> Option<&Project> {
        self.projects.get(self.project)
    }

    /// The shown project's listing.
    pub fn listing(&self) -> Option<&Listing> {
        self.lists.get(&self.current()?.name)
    }

    fn items(&self) -> &[OpenItem] {
        match self.listing() {
            Some(Listing::Ready(l)) => &l.items,
            _ => &[],
        }
    }

    /// List the shown project in the background, unless it has been and
    /// not `again`.
    pub fn load(&mut self, again: bool) {
        let Some(project) = self.current().cloned() else {
            return;
        };
        if !again && self.lists.contains_key(&project.name) {
            return;
        }
        if matches!(self.lists.get(&project.name), Some(Listing::Loading)) {
            return;
        }
        self.lists.insert(project.name.clone(), Listing::Loading);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let got = switchyard_core::items::list_open(&project);
            let _ = tx.send((project.name, got));
        });
    }

    /// Take finished listings; whether any came.
    pub fn receive(&mut self) -> bool {
        let mut any = false;
        while let Ok((name, got)) = self.rx.try_recv() {
            if let Err(e) = &got {
                switchyard_core::rex::log(&format!("work items for {name}: {e}"));
            }
            self.lists.insert(
                name,
                match got {
                    Ok(items) => Listing::Ready(items),
                    Err(e) => Listing::Failed(e),
                },
            );
            any = true;
        }
        any
    }

    /// Whether a listing is under way, for the spinner.
    pub fn loading(&self) -> bool {
        matches!(self.listing(), Some(Listing::Loading))
    }

    /// Rebuild the rows for the search and the open tasks' links, keeping
    /// the highlighted item.
    pub fn refilter(&mut self, query: &str, tasks: &[Task]) {
        let keep = self.selected_item().map(|i| i.url.clone());
        let mut linked: HashMap<(String, String, String), Linked> = HashMap::new();
        let mut linked_urls: HashMap<String, Linked> = HashMap::new();
        for t in tasks.iter().filter(|t| t.ended.is_none()) {
            let to = Linked {
                label: t.label(),
                host: t.host.clone(),
                session: t.session_id.clone(),
                window: t.window_id.clone(),
            };
            for l in &t.links {
                linked
                    .entry((l.kind.clone(), l.system.clone(), l.reference.clone()))
                    .or_insert_with(|| to.clone());
                if !l.url.is_empty() {
                    linked_urls
                        .entry(l.url.clone())
                        .or_insert_with(|| to.clone());
                }
            }
        }
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let filtering = !query.trim().is_empty();
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut scored: Vec<(bool, u32, Row)> = Vec::new();
        for (index, item) in self.items().iter().enumerate() {
            let to = linked_urls
                .get(&item.url)
                .cloned()
                .or_else(|| link_key(&item.url).and_then(|k| linked.get(&k).cloned()));
            let (score, hits) = if filtering {
                let number = format!("#{}", item.number);
                match score_fields(&mut matcher, &pattern, &[&item.title, &number]) {
                    Some((score, mut hits)) => (score, hits.swap_remove(0)),
                    None => continue,
                }
            } else {
                (0, Vec::new())
            };
            scored.push((
                to.is_some(),
                score,
                Row {
                    index,
                    linked: to,
                    hits,
                },
            ));
        }
        // A launch's item counts as started until a task links it.
        let started: Vec<String> = scored
            .iter()
            .filter(|(linked, _, _)| *linked)
            .map(|(_, _, r)| self.items()[r.index].url.clone())
            .collect();
        for url in started {
            self.starting.remove(&url);
        }
        // Free items first, then those with a task; best match first in
        // each while searching, else the tracker's order (stable).
        scored.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        self.rows = scored.into_iter().map(|(_, _, r)| r).collect();
        self.selected = if filtering {
            0
        } else {
            keep.and_then(|url| {
                self.rows
                    .iter()
                    .position(|r| self.items()[r.index].url == url)
            })
            .unwrap_or(self.selected)
            .min(self.rows.len().saturating_sub(1))
        };
    }

    pub fn selected_row(&self) -> Option<&Row> {
        self.rows.get(self.selected)
    }

    pub fn selected_item(&self) -> Option<&OpenItem> {
        self.items().get(self.selected_row()?.index)
    }

    /// The catalog projects matching the chooser's filter, best first, as
    /// indices into `projects`.
    pub fn chosen(&self) -> Vec<usize> {
        let query = self.chooser.as_ref().map_or("", |c| c.query.as_str());
        if query.trim().is_empty() {
            return (0..self.projects.len()).collect();
        }
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut scored: Vec<(u32, usize)> = self
            .projects
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                score_fields(&mut matcher, &pattern, &[&p.name, &p.path]).map(|(s, _)| (s, i))
            })
            .collect();
        scored.sort_by_key(|a| std::cmp::Reverse(a.0));
        scored.into_iter().map(|(_, i)| i).collect()
    }

    /// A key in the view, `page` rows a page.
    pub fn key(&mut self, k: KeyEvent, page: usize) -> Outcome {
        if self.chooser.is_some() {
            return self.chooser_key(k);
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let last = self.rows.len().saturating_sub(1);
        let page = page.max(1);
        match k.code {
            KeyCode::Tab | KeyCode::BackTab => {
                if self.projects.is_empty() {
                    let why = self
                        .catalog_error
                        .clone()
                        .unwrap_or_else(|| "the catalog has no projects".into());
                    return Outcome::Refuse(why);
                }
                self.chooser = Some(Chooser::default());
                // Start on the shown project.
                if let Some(c) = &mut self.chooser {
                    c.selected = self.project;
                }
                Outcome::Stay
            }
            KeyCode::Char('r') if ctrl => {
                self.load(true);
                Outcome::Stay
            }
            KeyCode::Up => self.step(self.selected.saturating_sub(1)),
            KeyCode::Char('p' | 'k') if ctrl => self.step(self.selected.saturating_sub(1)),
            KeyCode::Down => self.step((self.selected + 1).min(last)),
            KeyCode::Char('n' | 'j') if ctrl => self.step((self.selected + 1).min(last)),
            KeyCode::PageUp => self.step(self.selected.saturating_sub(page)),
            KeyCode::PageDown => self.step((self.selected + page).min(last)),
            KeyCode::Home => self.step(0),
            KeyCode::End => self.step(last),
            KeyCode::Enter => self.enter(),
            KeyCode::Char('v') if ctrl => match self.selected_item() {
                Some(item) => Outcome::Read(item_link(item)),
                None => Outcome::Stay,
            },
            KeyCode::Char('b') if ctrl => match self.selected_item() {
                Some(item) => Outcome::Open(item.url.clone()),
                None => Outcome::Stay,
            },
            _ => Outcome::Pass,
        }
    }

    /// Enter: jump to the item's task, or start one from it.
    fn enter(&self) -> Outcome {
        let (Some(row), Some(item), Some(project)) =
            (self.selected_row(), self.selected_item(), self.current())
        else {
            return Outcome::Stay;
        };
        if let Some(to) = &row.linked {
            return Outcome::Go(to.clone());
        }
        if self.starting.contains(&item.url) {
            return Outcome::Refuse(format!("#{} is already starting", item.number));
        }
        Outcome::Launch {
            path: project.path.clone(),
            url: item.url.clone(),
            name: format!("#{} {}", item.number, item.title),
        }
    }

    fn step(&mut self, to: usize) -> Outcome {
        self.selected = to;
        Outcome::Stay
    }

    fn chooser_key(&mut self, k: KeyEvent) -> Outcome {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let chosen = self.chosen();
        let Some(c) = &mut self.chooser else {
            return Outcome::Stay;
        };
        // The highlighted entry's place among the matches.
        let at = chosen.iter().position(|&i| i == c.selected).unwrap_or(0);
        let last = chosen.len().saturating_sub(1);
        let mut refilter = false;
        match k.code {
            KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab => self.chooser = None,
            KeyCode::Char('c' | 'g') if ctrl => self.chooser = None,
            KeyCode::Enter => {
                self.chooser = None;
                if let Some(&i) = chosen.get(at) {
                    self.show(i);
                    self.load(false);
                }
            }
            KeyCode::Up => c.selected = chosen.get(at.saturating_sub(1)).copied().unwrap_or(0),
            KeyCode::Char('p' | 'k') if ctrl => {
                c.selected = chosen.get(at.saturating_sub(1)).copied().unwrap_or(0);
            }
            KeyCode::Down => c.selected = chosen.get((at + 1).min(last)).copied().unwrap_or(0),
            KeyCode::Char('n' | 'j') if ctrl => {
                c.selected = chosen.get((at + 1).min(last)).copied().unwrap_or(0);
            }
            KeyCode::Char('u') if ctrl => {
                c.query.clear();
                refilter = true;
            }
            KeyCode::Char('w') if ctrl => {
                delete_word(&mut c.query);
                refilter = true;
            }
            KeyCode::Backspace if alt || ctrl => {
                delete_word(&mut c.query);
                refilter = true;
            }
            KeyCode::Backspace => {
                c.query.pop();
                refilter = true;
            }
            KeyCode::Char(ch) if !ctrl && !alt => {
                c.query.push(ch);
                refilter = true;
            }
            _ => {}
        }
        // Typing highlights the best match.
        if refilter {
            let best = self.chosen().first().copied();
            if let (Some(c), Some(best)) = (&mut self.chooser, best) {
                c.selected = best;
            }
        }
        Outcome::Stay
    }

    /// The footer's hints.
    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.chooser.is_some() {
            return vec![("⏎", "show"), ("esc", "back")];
        }
        let mut h = Vec::new();
        match self.selected_row() {
            Some(r) if r.linked.is_some() => h.push(("⏎", "go to its task")),
            Some(_) => h.push(("⏎", "start a session")),
            None => {}
        }
        if self.selected_row().is_some() {
            h.extend([("⌃V", "read"), ("⌃B", "open")]);
        }
        h.extend([("⇥", "project"), ("?", "keys")]);
        h
    }

    /// The view: a line naming the project and what it lists, then the
    /// rows, free items first and those with a task dimmed under a
    /// heading; the project chooser over them while it's open.
    pub fn draw(&mut self, f: &mut Frame, area: Rect, p: &Palette, tick: usize) {
        if area.height < 3 {
            return;
        }
        let head = Rect { height: 1, ..area };
        let list = Rect {
            y: area.y + 2,
            height: area.height - 2,
            ..area
        };
        f.render_widget(self.head_line(p, tick), head);
        self.draw_rows(f, list, p, tick);
        if self.chooser.is_some() {
            self.draw_chooser(f, area, p);
        }
    }

    fn head_line(&self, p: &Palette, tick: usize) -> Line<'static> {
        let Some(project) = self.current() else {
            let why = self
                .catalog_error
                .clone()
                .unwrap_or_else(|| "The catalog has no projects".into());
            return Line::styled(format!(" {why}"), Style::new().fg(p.love));
        };
        let mut spans = vec![
            Span::styled(" ", Style::new()),
            Span::styled(
                project.name.clone(),
                Style::new().fg(p.iris).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ▾", Style::new().fg(p.muted)),
        ];
        let muted = Style::new().fg(p.muted);
        match self.listing() {
            None | Some(Listing::Loading) => spans.push(Span::styled(
                format!(
                    "   {} listing open work items",
                    SPINNER[tick % SPINNER.len()]
                ),
                muted,
            )),
            Some(Listing::Failed(_)) => {}
            Some(Listing::Ready(l)) => {
                let free = self.rows.iter().filter(|r| r.linked.is_none()).count();
                let taken = self.rows.len() - free;
                spans.push(Span::styled(
                    format!("   {} open {}", l.items.len(), l.noun),
                    muted,
                ));
                if taken > 0 {
                    spans.push(Span::styled(format!(" · {taken} with a task"), muted));
                }
                if self.rows.len() < l.items.len() {
                    spans.push(Span::styled(
                        format!(" · {} match", self.rows.len()),
                        Style::new().fg(p.text),
                    ));
                }
            }
        }
        Line::from(spans)
    }

    fn draw_rows(&mut self, f: &mut Frame, area: Rect, p: &Palette, tick: usize) {
        let muted = Style::new().fg(p.muted);
        let message = match self.listing() {
            _ if self.current().is_none() => Some(String::new()),
            Some(Listing::Failed(e)) => Some(format!("Couldn't list them: {e}")),
            Some(Listing::Ready(l)) if l.items.is_empty() => Some(format!("No open {}", l.noun)),
            Some(Listing::Ready(_)) if self.rows.is_empty() => Some("Nothing matches".into()),
            Some(Listing::Ready(_)) => None,
            _ => Some(String::new()),
        };
        if let Some(msg) = message {
            f.render_widget(Line::styled(format!(" {msg}"), muted), area);
            return;
        }
        // Lines: each row, with a gap and a heading before the first with
        // a task.
        let first_taken = self.rows.iter().position(|r| r.linked.is_some());
        let mut lines: Vec<ListLine> = Vec::new();
        for i in 0..self.rows.len() {
            if Some(i) == first_taken {
                if i > 0 {
                    lines.push(ListLine::Gap);
                }
                lines.push(ListLine::Heading);
            }
            lines.push(ListLine::Row(i));
        }
        let at = lines
            .iter()
            .position(|l| *l == ListLine::Row(self.selected))
            .unwrap_or(0);
        let height = area.height as usize;
        let header_above = at > 0 && lines[at - 1] == ListLine::Heading;
        self.offset = keep_in_view(self.offset, at, height, header_above)
            .min(lines.len().saturating_sub(height));
        let width = area.width as usize;
        for (n, line) in lines.iter().enumerate().skip(self.offset).take(height) {
            let r = Rect {
                y: area.y + crate::cells(n - self.offset),
                height: 1,
                ..area
            };
            match line {
                ListLine::Heading => f.render_widget(
                    Line::styled(
                        " With a task",
                        Style::new().fg(p.subtle).add_modifier(Modifier::BOLD),
                    ),
                    r,
                ),
                ListLine::Gap => {}
                ListLine::Row(i) => self.draw_row(f, r, p, *i, width, tick),
            }
        }
    }

    fn draw_row(&self, f: &mut Frame, r: Rect, p: &Palette, i: usize, width: usize, tick: usize) {
        let row = &self.rows[i];
        let item = &self.items()[row.index];
        let selected = i == self.selected;
        let bg = if selected { p.overlay } else { p.base };
        f.buffer_mut().set_style(r, Style::new().bg(bg));
        let taken = row.linked.is_some();
        let starting = !taken && self.starting.contains(&item.url);
        let right = match (&row.linked, starting) {
            (Some(to), _) => {
                Span::styled(format!("→ {} ", to.label), Style::new().fg(p.muted).bg(bg))
            }
            (None, true) => Span::styled(
                format!("{} starting ", SPINNER[tick % SPINNER.len()]),
                Style::new().fg(p.gold).bg(bg),
            ),
            (None, false) => Span::raw(""),
        };
        let edge = if selected {
            Span::styled("▎", Style::new().fg(p.iris).bg(bg))
        } else {
            Span::styled(" ", Style::new().bg(bg))
        };
        let number = format!("#{:<5}", item.number);
        let room = width.saturating_sub(str_width(&number) + 3 + str_width(&right.content) + 1);
        let fg = if taken { p.muted } else { p.text };
        let mut base = Style::new().fg(fg).bg(bg);
        if selected {
            base = base.add_modifier(Modifier::BOLD);
        }
        let hl = Style::new().fg(p.gold).bg(bg).add_modifier(Modifier::BOLD);
        let mut spans = vec![
            edge,
            Span::styled(format!("{number} "), Style::new().fg(p.muted).bg(bg)),
        ];
        let title = if item.title.is_empty() {
            item.url.as_str()
        } else {
            item.title.as_str()
        };
        spans.extend(highlight(title, &row.hits, room, false, base, hl));
        f.render_widget(Line::from(spans), r);
        f.render_widget(Line::from(right).right_aligned(), r);
    }

    /// The chooser: a box over the view with the filter and the matching
    /// projects.
    fn draw_chooser(&self, f: &mut Frame, area: Rect, p: &Palette) {
        let Some(c) = &self.chooser else { return };
        let chosen = self.chosen();
        let width = self
            .projects
            .iter()
            .map(|p| str_width(&p.name) + 6)
            .chain([40])
            .max()
            .unwrap_or(40)
            .min(area.width as usize);
        let height = (chosen.len() + 4).min(area.height as usize).max(4);
        let r = crate::centered(area, crate::cells(width), crate::cells(height));
        f.render_widget(Clear, r);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(p.iris))
            .style(Style::new().bg(p.base))
            .title(Span::styled(" Project ", Style::new().fg(p.text)));
        let inner = block.inner(r);
        f.render_widget(block, r);
        if inner.height < 2 {
            return;
        }
        let query = if c.query.is_empty() {
            Line::from(vec![
                Span::styled(" ", Style::new()),
                Span::styled("type to filter", Style::new().fg(p.muted)),
            ])
        } else {
            Line::from(vec![
                Span::styled(" ", Style::new()),
                Span::styled(c.query.clone(), Style::new().fg(p.text)),
                Span::styled("▏", Style::new().fg(p.iris)),
            ])
        };
        f.render_widget(query, Rect { height: 1, ..inner });
        let rows = inner.height as usize - 1;
        let at = chosen.iter().position(|&i| i == c.selected).unwrap_or(0);
        let offset = at.saturating_sub(rows.saturating_sub(1));
        for (n, &i) in chosen.iter().enumerate().skip(offset).take(rows) {
            let selected = i == c.selected;
            let bg = if selected { p.overlay } else { p.base };
            let row = Rect {
                y: inner.y + 1 + crate::cells(n - offset),
                height: 1,
                ..inner
            };
            f.buffer_mut().set_style(row, Style::new().bg(bg));
            let edge = if selected {
                Span::styled("▎", Style::new().fg(p.iris).bg(bg))
            } else {
                Span::styled(" ", Style::new().bg(bg))
            };
            let mut style = Style::new().fg(p.text).bg(bg);
            if i == self.project {
                style = style.fg(p.iris);
            }
            if selected {
                style = style.add_modifier(Modifier::BOLD);
            }
            f.render_widget(
                Line::from(vec![
                    edge,
                    Span::styled(self.projects[i].name.clone(), style),
                ]),
                row,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(name: &str) -> Project {
        Project {
            profile: "personal".into(),
            path: format!("~/dev/{name}"),
            clone_url: String::new(),
            name: name.into(),
            hosts: Vec::new(),
            tracker: String::new(),
        }
    }

    fn card(n: u64, title: &str) -> OpenItem {
        OpenItem {
            number: n,
            title: title.into(),
            url: format!("https://app.fizzy.do/1/cards/{n}"),
        }
    }

    fn task(name: &str, url: &str, ended: bool) -> Task {
        let json = serde_json::json!([{
            "host": "mac", "session_id": "s", "window_id": format!("w-{name}"),
            "project": "sy", "name": name, "profile": "personal",
            "ended_at": if ended { serde_json::Value::from(1) } else { serde_json::Value::Null },
            "ended_reason": if ended { "archived" } else { "" },
            "links": [{"kind": "card", "system": "fizzy", "ref": url.rsplit('/').next().unwrap(), "url": url}],
        }]);
        crate::model::parse_tasks(&json.to_string())
            .unwrap()
            .remove(0)
    }

    fn work() -> Work {
        let mut w = Work {
            projects: vec![project("sy"), project("other")],
            catalog_read: true,
            ..Work::default()
        };
        w.lists.insert(
            "sy".into(),
            Listing::Ready(OpenItems {
                noun: "cards",
                items: vec![card(3, "Third"), card(2, "Second"), card(1, "First")],
            }),
        );
        w
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn items_with_a_task_follow_the_free_ones_and_enter_goes_to_the_task() {
        let mut w = work();
        let tasks = vec![
            task("on-two", "https://app.fizzy.do/1/cards/2", false),
            // An ended task's item is free again.
            task("on-one", "https://app.fizzy.do/1/cards/1", true),
        ];
        w.refilter("", &tasks);
        let order: Vec<u64> = w.rows.iter().map(|r| w.items()[r.index].number).collect();
        assert_eq!(order, [3, 1, 2]);
        assert!(w.rows[2].linked.is_some() && w.rows[1].linked.is_none());
        w.selected = 2;
        match w.key(key(KeyCode::Enter), 10) {
            Outcome::Go(to) => assert_eq!(to.label, "sy/on-two"),
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn enter_on_a_free_item_launches_it_once() {
        let mut w = work();
        w.refilter("", &[]);
        let Outcome::Launch { path, url, .. } = w.key(key(KeyCode::Enter), 10) else {
            panic!("no launch");
        };
        assert_eq!(
            (path.as_str(), url.as_str()),
            ("~/dev/sy", "https://app.fizzy.do/1/cards/3")
        );
        w.starting.insert(url.clone());
        assert!(matches!(w.key(key(KeyCode::Enter), 10), Outcome::Refuse(_)));
        // Once a task links it, it is no longer starting.
        let tasks = vec![task("t", &url, false)];
        w.refilter("", &tasks);
        assert!(w.starting.is_empty());
    }

    #[test]
    fn search_matches_titles_and_numbers_and_keeps_the_highlight() {
        let mut w = work();
        w.refilter("sec", &[]);
        assert_eq!(w.rows.len(), 1);
        assert_eq!(w.selected_item().unwrap().number, 2);
        w.refilter("#1", &[]);
        assert_eq!(w.selected_item().unwrap().number, 1);
        w.refilter("", &[]);
        // Back to every item, still on #1.
        assert_eq!(w.rows.len(), 3);
        assert_eq!(w.selected_item().unwrap().number, 1);
    }

    #[test]
    fn tab_chooses_another_project() {
        let mut w = work();
        w.refilter("", &[]);
        assert_eq!(w.key(key(KeyCode::Tab), 10), Outcome::Stay);
        assert!(w.chooser.is_some());
        for c in "oth".chars() {
            w.key(key(KeyCode::Char(c)), 10);
        }
        assert_eq!(w.chosen(), [1]);
        w.key(key(KeyCode::Enter), 10);
        assert!(w.chooser.is_none());
        assert_eq!(w.current().unwrap().name, "other");
        // Listing it has started, and the last project's rows are gone.
        assert!(w.lists.contains_key("other"));
        assert!(w.rows.is_empty() && w.selected_item().is_none());
    }

    #[test]
    fn letters_and_view_keys_pass_to_the_list() {
        let mut w = work();
        w.refilter("", &[]);
        assert_eq!(w.key(key(KeyCode::Char('x')), 10), Outcome::Pass);
        let ctrl_l = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(w.key(ctrl_l, 10), Outcome::Pass);
        let ctrl_v = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL);
        match w.key(ctrl_v, 10) {
            Outcome::Read(l) => assert_eq!(
                (l.kind.as_str(), l.system.as_str(), l.reference.as_str()),
                ("card", "fizzy", "3")
            ),
            o => panic!("{o:?}"),
        }
    }
}
