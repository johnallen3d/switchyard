//! The Work view: one catalog project's open work items, the open cards on
//! its Fizzy board or the open issues in its GitHub repo (see core's
//! `items`), to start a session from.
//!
//! It opens on the highlighted task's project, when that is a catalog
//! project, else on the last one shown. Tab opens a chooser over the
//! catalog (type to filter, Enter to switch); ctrl-r lists the project
//! again. ctrl-f switches between the work items and the project's GitHub
//! repo's open pull requests, whose rows show their author, draft and review
//! state (see `Kind`). Each project's list of each is fetched once, in the
//! background, and kept.
//!
//! Items no open task links to come first. Those an open task links to
//! follow, dimmed and marked (`TASK_MARK`), under "With a task", each
//! naming its task. Each row shows what the listing gave besides the title:
//! labels, assignee and age. Typing searches titles and numbers, as in the
//! other views.
//!
//! The Inbox's ctrl-b float sends an item here with Enter: the view opens
//! on its task's project, listing pull requests for a PR, and highlights
//! the item once it lists (see `focus_on`); the footer says when it isn't
//! among the open ones.
//!
//! On the highlighted item: Enter starts a session from it (auto host,
//! claude, named from the item; see core's `launch`), in the background,
//! switching to Inbox, which highlights the session once it lists, or on
//! a dimmed item jumps to its task; ctrl-b opens it in the browser. On a
//! pull request that isn't a draft, ctrl-g arms its approval, as ctrl-x
//! arms an archive elsewhere: Enter then approves it (`gh pr review
//! --approve`) and any other key disarms it. ctrl-m arms a squash-merge
//! the same way, on a pull request that is approved with no failing or
//! running checks (see core's `merge_blocker`); Enter asks GitHub again
//! before merging (see core's `item_actions`). A row says what stands in
//! the way: failing or running checks, or changes requested. ctrl-t
//! reviews a pull request in tuicr, in a float over the picker (`tuicr
//! pr`, whose `:submit` sends the review to GitHub).
//!
//! With room, the view splits: the list on the left and the highlighted
//! item on the right, its description and comments, and for a pull
//! request its diff and commits with ctrl-l (see detail.rs). ctrl-s gives
//! the right side the whole view, where the arrows scroll it; shift and
//! the arrows scroll it beside the list. ctrl-v moves the keys into it as
//! scrollback, giving it the whole view first when there is no room
//! beside the list; ctrl-u and ctrl-d do too, half a page up or down.
//! This holds the view's state and keys; `standalone_key` runs
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
use switchyard_core::hosts::Project;
use switchyard_core::item_actions::{Action, Item, merge_blocker};
use switchyard_core::items::{OpenItem, OpenItems};
use switchyard_core::links::{Tracker, parse_ref};
use switchyard_ui::Palette;
use switchyard_ui::draw::{delete_word, highlight, keep_in_view, str_width};
use switchyard_ui::fuzzy::score_fields;
use switchyard_ui::glyphs::SPINNER;
use switchyard_ui::popup;

use crate::detail::Detail;
use crate::model::{Link, Task, age, now};
use crate::scrollback::Scrollback;

/// Marks an item an open task links to, on its row and the head line's
/// count.
const TASK_MARK: &str = "●";
/// The most labels a row shows, and the widest an assignee or label is
/// drawn.
const MAX_LABELS: usize = 2;
const MAX_NAME: usize = 12;
const MAX_LABEL: usize = 16;
/// The narrowest a title is squeezed to before a row drops metadata.
const MIN_TITLE: usize = 32;
/// The narrowest labels are cut to before a row drops them.
const MIN_LABELS: usize = 6;
/// The list's width range when the view splits (two fifths of it).
const LIST_MIN: u16 = 40;
const LIST_MAX: u16 = 72;

/// What the view lists for a project: its work items, or its pull
/// requests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Kind {
    #[default]
    Items,
    Prs,
}

impl Kind {
    fn other(self) -> Self {
        match self {
            Kind::Items => Kind::Prs,
            Kind::Prs => Kind::Items,
        }
    }
}

/// A listing's key: the project's name and what it lists.
type Key = (String, Kind);

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
    Heading(&'static str),
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
    /// Move the keys into the highlighted item's side, as scrollback,
    /// then the key that asked (ctrl-u or ctrl-d) in it.
    Scroll(Option<KeyEvent>),
    /// Open the URL in the browser here.
    Open(String),
    /// ctrl-g or ctrl-m armed an action: the footer says what Enter will
    /// do.
    Arm,
    /// Approve or squash-merge (`action`) the pull request at `url`, named
    /// `name` (`#57`).
    Act {
        action: Action,
        item: Item,
        url: String,
        name: String,
    },
    /// Review the pull request at `url`, named `name` (`#57`), in tuicr
    /// under `profile`'s account, in a float.
    Review {
        profile: String,
        url: String,
        name: String,
    },
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
    /// Whether the view lists work items or pull requests.
    pub kind: Kind,
    lists: HashMap<Key, Listing>,
    tx: Sender<(Key, Result<OpenItems, String>)>,
    rx: Receiver<(Key, Result<OpenItems, String>)>,
    pub rows: Vec<Row>,
    pub selected: usize,
    offset: usize,
    pub chooser: Option<Chooser>,
    /// Items a launch is starting from, by URL, until a task links them.
    pub starting: HashSet<String>,
    /// The highlighted item's side (see detail.rs).
    pub detail: Detail,
    /// What ctrl-g or ctrl-m armed (approve or squash-merge) and on which
    /// pull request, by URL, until the next key.
    pub armed: Option<(Action, String)>,
    /// The item to highlight once its listing comes (see `focus_on`).
    focus: Option<Link>,
    /// Whether the pending focus has relisted, once, on not finding its
    /// item in a listing that may predate it.
    focus_relisted: bool,
    /// The last item to highlight that wasn't listed, for the footer.
    pub missed: Option<Link>,
}

impl Default for Work {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Work {
            projects: Vec::new(),
            catalog_error: None,
            catalog_read: false,
            project: 0,
            kind: Kind::Items,
            lists: HashMap::new(),
            tx,
            rx,
            rows: Vec::new(),
            selected: 0,
            offset: 0,
            chooser: None,
            starting: HashSet::new(),
            detail: Detail::default(),
            armed: None,
            focus: None,
            focus_relisted: false,
            missed: None,
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
                Ok(mut p) => {
                    // By name, for the chooser.
                    p.sort_by_cached_key(|p| p.name.to_lowercase());
                    self.projects = p;
                }
                Err(e) => self.catalog_error = Some(e),
            }
        }
        if let Some(i) = project.and_then(|name| self.projects.iter().position(|p| p.name == name))
        {
            self.show(i);
        }
        self.load(false);
    }

    /// Highlight `link` once the shown listing has it, listing pull
    /// requests for a PR and work items otherwise. Call before `open`,
    /// which lists the project.
    pub fn focus_on(&mut self, link: Link) {
        let kind = if link.kind == "pr" {
            Kind::Prs
        } else {
            Kind::Items
        };
        if kind != self.kind {
            self.kind = kind;
            self.reset();
        }
        self.missed = None;
        self.focus = Some(link);
        self.focus_relisted = false;
    }

    /// Highlight the pending focus if the listing has come: on its row, or
    /// noting it as missed when it isn't one. Listings last the picker's
    /// life, so a miss relists once first: the item may be newer.
    fn take_focus(&mut self) {
        let Some(link) = &self.focus else {
            return;
        };
        match self.listing() {
            Some(Listing::Ready(_)) => {}
            Some(Listing::Failed(_)) => {
                self.focus = None;
                return;
            }
            _ => return,
        }
        let key = (
            link.kind.clone(),
            link.system.clone(),
            link.reference.clone(),
        );
        let found = self.rows.iter().position(|r| {
            let item = &self.items()[r.index];
            (!link.url.is_empty() && item.url == link.url)
                || link_key(&item.url) == Some(key.clone())
        });
        if found.is_none() && !self.focus_relisted {
            self.focus_relisted = true;
            self.load(true);
            return;
        }
        let link = self.focus.take();
        match found {
            Some(i) => self.selected = i,
            None => self.missed = link,
        }
    }

    /// Show project `i`, from its first item.
    fn show(&mut self, i: usize) {
        if i != self.project {
            self.project = i;
            self.reset();
        }
    }

    /// Forget the rows, which index the last listing's items.
    fn reset(&mut self) {
        self.rows.clear();
        self.selected = 0;
        self.offset = 0;
    }

    /// Switch between work items and pull requests, listing the other when
    /// it hasn't been.
    fn switch_kind(&mut self) {
        self.kind = self.kind.other();
        self.reset();
        self.load(false);
    }

    fn key_of(&self, project: &Project) -> Key {
        (project.name.clone(), self.kind)
    }

    pub fn current(&self) -> Option<&Project> {
        self.projects.get(self.project)
    }

    /// The shown project's listing.
    pub fn listing(&self) -> Option<&Listing> {
        self.lists.get(&self.key_of(self.current()?))
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
        let key = self.key_of(&project);
        if !again && self.lists.contains_key(&key) {
            return;
        }
        if matches!(self.lists.get(&key), Some(Listing::Loading)) {
            return;
        }
        self.lists.insert(key.clone(), Listing::Loading);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let got = match key.1 {
                Kind::Items => switchyard_core::items::list_open(&project),
                Kind::Prs => switchyard_core::items::list_prs(&project),
            };
            let _ = tx.send((key, got));
        });
    }

    /// Take finished listings; whether any came.
    pub fn receive(&mut self) -> bool {
        let mut any = false;
        while let Ok((key, got)) = self.rx.try_recv() {
            if let Err(e) = &got {
                let what = match key.1 {
                    Kind::Items => "work items",
                    Kind::Prs => "pull requests",
                };
                switchyard_core::rex::log(&format!("{what} for {}: {e}", key.0));
            }
            self.lists.insert(
                key,
                match got {
                    Ok(items) => Listing::Ready(items),
                    Err(e) => Listing::Failed(e),
                },
            );
            any = true;
        }
        any
    }

    /// Whether a listing or the highlighted item's fetch is under way,
    /// for the spinner.
    pub fn loading(&self) -> bool {
        matches!(self.listing(), Some(Listing::Loading))
            || self.detail.busy(self.selected_link().as_ref())
    }

    /// Take the right side's fetches and start what the highlighted item
    /// needs (see detail.rs).
    pub fn poll_detail(&mut self) {
        let link = self.selected_link();
        let profile = self
            .current()
            .map(|p| p.profile.clone())
            .unwrap_or_default();
        self.detail.poll(link.as_ref(), &profile);
    }

    /// The highlighted item as a link.
    pub fn selected_link(&self) -> Option<Link> {
        self.selected_item().map(item_link)
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
        // Items with a task first, then the free ones; best match first in
        // each while searching, else the tracker's order (stable).
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
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
        self.take_focus();
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
        // Enter runs what's armed on a pull request; any other key but
        // the one that armed it disarms it.
        let armed = self.armed.take();
        if self.chooser.is_some() {
            return self.chooser_key(k);
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
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
                self.detail.forget();
                Outcome::Stay
            }
            KeyCode::Char('l') if ctrl => match self.selected_link() {
                Some(l) => match self.detail.cycle(&l) {
                    Ok(_) => Outcome::Stay,
                    Err(why) => Outcome::Refuse(why),
                },
                None => Outcome::Stay,
            },
            KeyCode::Char('s') if ctrl => {
                self.detail.expanded = !self.detail.expanded;
                Outcome::Stay
            }
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
                if shift || self.detail.expanded =>
            {
                let page = self.detail.page();
                self.detail.scroll(match k.code {
                    KeyCode::Up => -1,
                    KeyCode::Down => 1,
                    KeyCode::PageUp => -page,
                    _ => page,
                });
                Outcome::Stay
            }
            KeyCode::Home | KeyCode::End if self.detail.expanded => {
                self.detail.edge(k.code == KeyCode::End);
                Outcome::Stay
            }
            KeyCode::Char('f') if ctrl => {
                self.switch_kind();
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
            KeyCode::Enter if armed.is_some() => self.act(armed.as_ref()),
            KeyCode::Enter => self.enter(),
            KeyCode::Char('v') if ctrl => match self.selected_item() {
                Some(_) => Outcome::Scroll(None),
                None => Outcome::Stay,
            },
            // ctrl-u and ctrl-d scroll the item half a page, in scrollback;
            // search finds a row faster than paging the list.
            KeyCode::Char('u' | 'd') if ctrl => match self.selected_item() {
                Some(_) => Outcome::Scroll(Some(k)),
                None => Outcome::Stay,
            },
            KeyCode::Char('b') if ctrl => match self.selected_item() {
                Some(item) => Outcome::Open(item.url.clone()),
                None => Outcome::Stay,
            },
            KeyCode::Char('g') if ctrl => self.arm(Action::Approve),
            KeyCode::Char('t') if ctrl => self.review(),
            KeyCode::Char('m') if ctrl => self.arm(Action::Merge),
            // Searching brings back the list it searches.
            KeyCode::Char(_) if !ctrl => {
                self.detail.expanded = false;
                Outcome::Pass
            }
            _ => Outcome::Pass,
        }
    }

    /// ctrl-t: review the highlighted pull request in tuicr.
    fn review(&self) -> Outcome {
        if self.kind != Kind::Prs {
            return Outcome::Refuse("ctrl-t reviews pull requests; ctrl-f lists them".into());
        }
        let (Some(pr), Some(project)) = (self.selected_item(), self.current()) else {
            return Outcome::Stay;
        };
        Outcome::Review {
            profile: project.profile.clone(),
            url: pr.url.clone(),
            name: format!("#{}", pr.number),
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

    /// The highlighted pull request, when `action` (approve or
    /// squash-merge) can be taken on it: as an item, with its URL and `#N`;
    /// else why not. A merge waits for an approval and passing checks, as
    /// the listing last saw them.
    fn actionable(&self, action: Action) -> Result<(Item, String, String), Outcome> {
        if self.kind != Kind::Prs {
            let key = if action == Action::Merge {
                "ctrl-m squash-merges"
            } else {
                "ctrl-g approves"
            };
            return Err(Outcome::Refuse(format!(
                "{key} pull requests; ctrl-f lists them"
            )));
        }
        let pr = self.selected_item().ok_or(Outcome::Stay)?;
        let name = format!("#{}", pr.number);
        if pr.draft {
            return Err(Outcome::Refuse(format!("{name} is a draft")));
        }
        if action == Action::Merge
            && let Some(why) = merge_blocker("OPEN", pr.draft, &pr.review, &pr.checks)
        {
            return Err(Outcome::Refuse(format!("{name} {why}")));
        }
        let item = link_key(&pr.url)
            .and_then(|(kind, system, reference)| {
                Item::from_link(&kind, &system, &reference, &pr.url, "open")
            })
            .filter(|i| matches!(i, Item::Pr { .. }))
            .ok_or_else(|| Outcome::Refuse(format!("{name} isn't a GitHub pull request")))?;
        Ok((item, pr.url.clone(), name))
    }

    /// ctrl-g or ctrl-m: arm `action` on the highlighted pull request,
    /// for Enter.
    fn arm(&mut self, action: Action) -> Outcome {
        match self.actionable(action) {
            Ok((_, url, _)) => {
                self.armed = Some((action, url));
                Outcome::Arm
            }
            Err(why) => why,
        }
    }

    /// Enter on an armed action: take it on the highlighted pull request
    /// if it is the one armed (the highlight can't move while armed).
    fn act(&self, armed: Option<&(Action, String)>) -> Outcome {
        let Some((action, armed_url)) = armed else {
            return Outcome::Stay;
        };
        match self.actionable(*action) {
            Ok((item, url, name)) if *armed_url == url => Outcome::Act {
                action: *action,
                item,
                url,
                name,
            },
            Ok(_) => Outcome::Stay,
            Err(why) => why,
        }
    }

    /// The armed action and its pull request, `#N` and its title, for the
    /// footer.
    pub fn armed_pr(&self) -> Option<(Action, String, String)> {
        let pr = self.selected_item()?;
        let (action, url) = self.armed.as_ref()?;
        (*url == pr.url).then(|| (*action, format!("#{}", pr.number), pr.title.clone()))
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
            h.extend([("⌃V", "scroll"), ("⌃B", "open")]);
        }
        if self.kind == Kind::Prs && self.selected_item().is_some_and(|i| !i.draft) {
            h.push(("⌃G", "approve"));
        }
        if self.kind == Kind::Prs && self.actionable(Action::Merge).is_ok() {
            h.push(("⌃M", "squash-merge"));
        }
        if self.kind == Kind::Prs && self.selected_item().is_some() {
            h.push(("⌃T", "review"));
        }
        if let Some(l) = self.selected_link().filter(|l| l.kind == "pr") {
            h.push(("⌃L", self.detail.lens_for(&l).next().name()));
        }
        h.push((
            "⌃S",
            if self.detail.expanded {
                "list"
            } else {
                "expand"
            },
        ));
        h.push(match self.kind {
            Kind::Items => ("⌃F", "PRs"),
            Kind::Prs => ("⌃F", "work items"),
        });
        h.extend([("⇥", "project"), ("?", "keys")]);
        h
    }

    /// The view: the list, with the highlighted item beside it when
    /// `split` (there is room) or over it when expanded (see detail.rs);
    /// the project chooser over them while it's open.
    pub fn draw(
        &mut self,
        f: &mut Frame,
        area: Rect,
        p: &Palette,
        tick: usize,
        split: bool,
        sb: Option<&mut Scrollback>,
    ) {
        self.detail.visible = false;
        if area.height < 4 {
            return;
        }
        let link = self.selected_link();
        let body = Rect {
            height: area.height - 1,
            ..area
        };
        // Expanded, with nothing highlighted (listing, or no match), the
        // list says why.
        if self.detail.expanded && link.is_some() {
            self.detail.draw(f, body, p, link.as_ref(), now(), sb);
        } else if split {
            let w = (area.width * 2 / 5).clamp(LIST_MIN, LIST_MAX);
            let list = Rect { width: w, ..area };
            let gap = Rect {
                x: area.x + w,
                width: 3,
                ..body
            };
            let side = Rect {
                x: gap.x + gap.width,
                width: area.width.saturating_sub(w + gap.width),
                ..body
            };
            self.draw_list(f, list, p, tick);
            let buf = f.buffer_mut();
            for y in gap.y..gap.y + gap.height {
                buf[(gap.x + 1, y)]
                    .set_symbol("│")
                    .set_style(Style::new().fg(p.hl_med));
            }
            self.detail.draw(f, side, p, link.as_ref(), now(), sb);
        } else {
            self.draw_list(f, area, p, tick);
        }
        if self.chooser.is_some() {
            self.draw_chooser(f, area, p);
        }
    }

    /// The list: a line naming the project and what it lists, then the
    /// rows, those with a task dimmed under a heading first and the free
    /// ones under another, and a blank line above the footer.
    fn draw_list(&mut self, f: &mut Frame, area: Rect, p: &Palette, tick: usize) {
        let head = Rect { height: 1, ..area };
        let list = Rect {
            y: area.y + 2,
            height: area.height - 3,
            ..area
        };
        f.render_widget(self.head_line(p, tick), head);
        self.draw_rows(f, list, p, tick);
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
                    "   {} listing open {}",
                    SPINNER[tick % SPINNER.len()],
                    match self.kind {
                        Kind::Items => "work items",
                        Kind::Prs => "pull requests",
                    }
                ),
                muted,
            )),
            Some(Listing::Failed(_)) => {}
            Some(Listing::Ready(l)) => {
                let free = self.rows.iter().filter(|r| r.linked.is_none()).count();
                let taken = self.rows.len() - free;
                // "cards" or "issues", singular for one.
                let noun = match l.items.len() {
                    1 => l.noun.strip_suffix('s').unwrap_or(l.noun),
                    _ => l.noun,
                };
                spans.push(Span::styled(
                    format!("   {} open {noun}", l.items.len()),
                    muted,
                ));
                if taken > 0 {
                    spans.push(Span::styled(" · ", muted));
                    spans.push(Span::styled(TASK_MARK, Style::new().fg(p.foam)));
                    spans.push(Span::styled(format!(" {taken} with a task"), muted));
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
        // Lines: each row, with a heading over those with a task and, when
        // there are any, a gap and a heading over the free ones after them.
        let first_free = self.rows.iter().position(|r| r.linked.is_none());
        let mut lines: Vec<ListLine> = Vec::new();
        if self.rows.first().is_some_and(|r| r.linked.is_some()) {
            lines.push(ListLine::Heading(" With a task"));
        }
        for i in 0..self.rows.len() {
            if Some(i) == first_free && i > 0 {
                lines.push(ListLine::Gap);
                lines.push(ListLine::Heading(" Without a task"));
            }
            lines.push(ListLine::Row(i));
        }
        let at = lines
            .iter()
            .position(|l| *l == ListLine::Row(self.selected))
            .unwrap_or(0);
        let height = area.height as usize;
        let header_above = at > 0 && matches!(lines[at - 1], ListLine::Heading(_));
        self.offset = keep_in_view(self.offset, at, height, header_above)
            .min(lines.len().saturating_sub(height));
        let width = area.width as usize;
        let names = self.name_width();
        for (n, line) in lines.iter().enumerate().skip(self.offset).take(height) {
            let r = Rect {
                y: area.y + crate::cells(n - self.offset),
                height: 1,
                ..area
            };
            match line {
                ListLine::Heading(title) => f.render_widget(
                    Line::styled(
                        *title,
                        Style::new().fg(p.subtle).add_modifier(Modifier::BOLD),
                    ),
                    r,
                ),
                ListLine::Gap => {}
                ListLine::Row(i) => self.draw_row(f, r, p, *i, (width, names), tick),
            }
        }
    }

    /// How wide the people column is: the widest shown author or
    /// assignee, so ages line up.
    fn name_width(&self) -> usize {
        self.items()
            .iter()
            .map(|i| str_width(&person(i)))
            .max()
            .unwrap_or(0)
    }

    /// A row: the selection edge, the task mark, the number and the title,
    /// then on the right its task or launch, labels, assignee and age. The
    /// highlighted title is brightest, free ones next and those with a
    /// task dimmest.
    fn draw_row(
        &self,
        f: &mut Frame,
        r: Rect,
        p: &Palette,
        i: usize,
        (width, names): (usize, usize),
        tick: usize,
    ) {
        let row = &self.rows[i];
        let item = &self.items()[row.index];
        let selected = i == self.selected;
        let bg = if selected { p.overlay } else { p.base };
        f.buffer_mut().set_style(r, Style::new().bg(bg));
        let taken = row.linked.is_some();
        let starting = !taken && self.starting.contains(&item.url);
        let on = |fg| Style::new().fg(fg).bg(bg);
        let edge = if selected {
            Span::styled("▎", on(p.iris))
        } else {
            Span::styled(" ", on(p.base))
        };
        let mark = if taken {
            Span::styled(format!("{TASK_MARK} "), on(p.foam))
        } else {
            Span::styled("  ", on(p.base))
        };
        let number = format!("#{:<5} ", item.number);
        let lead = 1 + 2 + str_width(&number);
        // What the right shows, widest first to drop when the title needs
        // the room: labels, then the assignee.
        let task = match (&row.linked, starting) {
            (Some(to), _) => Some(Span::styled(format!("→ {}", to.label), on(p.foam))),
            (None, true) => Some(Span::styled(
                format!("{} starting", SPINNER[tick % SPINNER.len()]),
                on(p.gold),
            )),
            (None, false) => None,
        };
        let state = pr_state(item, p);
        let labels = labels(item);
        let who = format!("{:>names$}", person(item));
        let when = item
            .created
            .map(|c| age(now().saturating_sub(c)))
            .unwrap_or_default();
        // Each part takes its width and a two-cell gap (between parts, or
        // from the title); the line ends one cell in.
        let mut parts: Vec<Span<'static>> = task.into_iter().collect();
        if let Some((text, fg)) = state {
            parts.push(Span::styled(text, on(fg)));
        }
        let mut used = 1 + parts
            .iter()
            .map(|s| str_width(&s.content) + 2)
            .sum::<usize>();
        // Labels take what the assignee and age leave, cut to fit.
        let after = if names > 0 { names + 2 } else { 0 } + 6;
        let label_room = width.saturating_sub(lead + MIN_TITLE + used + 2 + after);
        let mut add = |parts: &mut Vec<Span<'static>>, text: String, fg, also: usize| {
            let cells = str_width(&text) + 2;
            if lead + MIN_TITLE + used + cells + also <= width {
                used += cells;
                parts.push(Span::styled(text, on(fg)));
            }
        };
        if !labels.is_empty() && label_room >= MIN_LABELS {
            add(&mut parts, clip(&labels, label_room), p.pine, after);
        }
        if names > 0 {
            add(&mut parts, who, p.subtle, 6);
        }
        add(&mut parts, format!("{when:>4}"), p.muted, 0);
        let mut right: Vec<Span<'static>> = Vec::new();
        for part in parts {
            right.push(part);
            right.push(Span::styled("  ", on(p.base)));
        }
        right.pop();
        right.push(Span::styled(" ", on(p.base)));
        let room = width.saturating_sub(lead + used);
        let fg = if selected {
            p.text
        } else if taken {
            p.muted
        } else {
            p.subtle
        };
        let mut base = on(fg);
        if selected {
            base = base.add_modifier(Modifier::BOLD);
        }
        let hl = on(p.gold).add_modifier(Modifier::BOLD);
        let mut spans = vec![edge, mark, Span::styled(number, on(p.muted))];
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
        crate::backdrop(f, p);
        let height = crate::cells(chosen.len() + 3).max(4);
        let r = popup::list_rect(area, height);
        let inner = popup::open(f, r, popup::block(p, "Project"));
        if inner.height < 2 {
            return;
        }
        let query = if c.query.is_empty() {
            Line::styled("type to filter", Style::new().fg(p.muted))
        } else {
            Line::from(vec![
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
            let row = Rect {
                y: inner.y + 1 + crate::cells(n - offset),
                height: 1,
                ..inner
            };
            if selected {
                popup::select(f, p, inner, row);
            }
            let bg = if selected { p.overlay } else { p.base };
            let mut style = Style::new().fg(p.text).bg(bg);
            if i == self.project {
                style = style.fg(p.iris);
            }
            if selected {
                style = style.add_modifier(Modifier::BOLD);
            }
            f.render_widget(Span::styled(self.projects[i].name.clone(), style), row);
        }
    }
}

/// Up to `MAX_LABELS` of an item's labels, each cut to `MAX_LABEL`, and
/// how many more.
fn labels(item: &OpenItem) -> String {
    let mut shown: Vec<String> = item
        .labels
        .iter()
        .take(MAX_LABELS)
        .map(|l| clip(l, MAX_LABEL))
        .collect();
    if item.labels.len() > MAX_LABELS {
        shown.push(format!("+{}", item.labels.len() - MAX_LABELS));
    }
    shown.join(" ")
}

/// Who a row names: a pull request's author, else whom the item is
/// assigned to.
fn person(item: &OpenItem) -> String {
    if item.author.is_empty() {
        assignee(item)
    } else {
        clip(&item.author, MAX_NAME)
    }
}

/// A pull request's state worth a word on its row, and its colour, what
/// most stands in the way of merging it first: draft, failing checks,
/// changes requested, running checks; else approved (ready to merge) or
/// nothing.
fn pr_state(item: &OpenItem, p: &Palette) -> Option<(String, ratatui::style::Color)> {
    if item.draft {
        return Some(("draft".into(), p.muted));
    }
    match (item.review.as_str(), item.checks.as_str()) {
        (_, "fail") => Some(("checks failing".into(), p.love)),
        ("CHANGES_REQUESTED", _) => Some(("changes requested".into(), p.love)),
        (_, "pending") => Some(("checks running".into(), p.muted)),
        ("APPROVED", _) => Some(("approved".into(), p.foam)),
        _ => None,
    }
}

/// Whom an item is assigned to, briefly: the first assignee's first name
/// (or login), cut to `MAX_NAME`, and how many more.
fn assignee(item: &OpenItem) -> String {
    let Some(first) = item.assignees.first() else {
        return String::new();
    };
    let name = clip(first.split_whitespace().next().unwrap_or(first), MAX_NAME);
    match item.assignees.len() {
        1 => name,
        n => format!("{name}+{}", n - 1),
    }
}

/// `s` cut to `max` cells, with an ellipsis when cut.
fn clip(s: &str, max: usize) -> String {
    if str_width(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    for c in s.chars() {
        if str_width(&out) + str_width(&c.to_string()) >= max {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
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
            ..OpenItem::default()
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
            ("sy".into(), Kind::Items),
            Listing::Ready(OpenItems {
                noun: "cards",
                items: vec![card(3, "Third"), card(2, "Second"), card(1, "First")],
            }),
        );
        w
    }

    #[test]
    fn a_pull_requests_diff_shows_beside_the_list_and_scrollback_jumps_between_files() {
        use ratatui::{Terminal, backend::TestBackend};
        use std::fmt::Write as _;
        let url = "https://github.com/o/r/pull/5";
        let mut w = work();
        w.kind = Kind::Prs;
        w.lists.insert(
            ("sy".into(), Kind::Prs),
            Listing::Ready(OpenItems {
                noun: "pull requests",
                items: vec![OpenItem {
                    number: 5,
                    title: "Five".into(),
                    url: url.into(),
                    ..OpenItem::default()
                }],
            }),
        );
        w.refilter("", &[]);
        let mut patch = String::new();
        for f in ["one.rs", "two.rs"] {
            let _ = write!(patch, "diff --git a/{f} b/{f}\n@@ -1 +1,30 @@\n");
            for i in 0..30 {
                let _ = writeln!(patch, "+{f} line {i}");
            }
        }
        w.detail.put_changes(
            url,
            &serde_json::json!({
                "base": "main", "branch": "feature/x", "dirty": 0, "commits": [],
                "files": [
                    {"path": "one.rs", "added": 30, "removed": 0},
                    {"path": "two.rs", "added": 30, "removed": 0}
                ],
                "patch": patch, "truncated": false,
            }),
        );
        let ctrl_l = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(w.key(ctrl_l, 10), Outcome::Stay);
        let p = Palette::load();
        let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
        let mut screen = |w: &mut Work, sb: Option<&mut Scrollback>| {
            terminal
                .draw(|f| w.draw(f, f.area(), &p, 0, true, sb))
                .unwrap();
            let buf = terminal.backend().buffer();
            (0..buf.area.height)
                .map(|y| {
                    (0..buf.area.width)
                        .map(|x| buf[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
        };
        let rows = screen(&mut w, None);
        assert!(rows[0].contains("Description  Diff  Commits"), "{rows:#?}");
        assert!(
            rows.iter().any(|r| r.contains("#5") && r.contains("+30")),
            "{rows:#?}"
        );
        let link = w.selected_link().unwrap();
        let (lines, nav) = w.detail.text(&link, &p, 0).unwrap();
        assert_eq!(nav.marks.len(), 2);
        let mut sb = Scrollback::new(w.detail.pane(&link), lines.len(), 10);
        sb.cursor = 0;
        sb.nav = nav;
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        // Enter on two.rs in the file list goes to its patch.
        sb.key(key(KeyCode::Down), &lines, 10);
        assert_eq!(sb.key(enter, &lines, 10), crate::scrollback::Outcome::Stay);
        let rows = screen(&mut w, Some(&mut sb));
        assert!(rows[2].contains("two.rs"), "{rows:#?}");
        // And `[` back to one.rs's.
        sb.key(key(KeyCode::Char('[')), &lines, 10);
        let rows = screen(&mut w, Some(&mut sb));
        assert!(
            rows[2].contains("one.rs") && !rows[2].contains('+'),
            "{rows:#?}"
        );
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn a_focused_item_is_highlighted_once_listed_or_noted_as_missed() {
        let link = |kind: &str, url: &str| Link {
            kind: kind.into(),
            system: if kind == "pr" { "github" } else { "fizzy" }.into(),
            reference: url.rsplit('/').next().unwrap().into(),
            url: url.into(),
            ..Link::default()
        };
        // A card waits for its listing, then gets the highlight.
        let mut w = work();
        let ready = w.lists.remove(&("sy".into(), Kind::Items)).unwrap();
        w.lists.insert(("sy".into(), Kind::Items), Listing::Loading);
        w.focus_on(link("card", "https://app.fizzy.do/1/cards/1"));
        w.refilter("", &[]);
        assert_eq!((w.selected, w.focus.is_some()), (0, true));
        w.lists.insert(("sy".into(), Kind::Items), ready);
        w.refilter("", &[]);
        assert_eq!(w.selected_item().unwrap().number, 1);
        assert!(w.focus.is_none() && w.missed.is_none());
        // A PR switches to pull requests; one not listed relists once,
        // then is missed.
        w.focus_on(link("pr", "https://github.com/o/r/pull/9"));
        assert_eq!(w.kind, Kind::Prs);
        let none = || {
            Listing::Ready(OpenItems {
                noun: "pull requests",
                items: Vec::new(),
            })
        };
        w.lists.insert(("sy".into(), Kind::Prs), none());
        w.refilter("", &[]);
        assert!(w.missed.is_none() && w.focus.is_some());
        assert!(matches!(w.listing(), Some(Listing::Loading)));
        w.lists.insert(("sy".into(), Kind::Prs), none());
        w.refilter("", &[]);
        assert_eq!(
            w.missed.as_ref().map(|l| l.url.as_str()),
            Some("https://github.com/o/r/pull/9")
        );
    }

    #[test]
    fn items_with_a_task_lead_the_free_ones_and_enter_goes_to_the_task() {
        let mut w = work();
        let tasks = vec![
            task("on-two", "https://app.fizzy.do/1/cards/2", false),
            // An ended task's item is free again.
            task("on-one", "https://app.fizzy.do/1/cards/1", true),
        ];
        w.refilter("", &tasks);
        let order: Vec<u64> = w.rows.iter().map(|r| w.items()[r.index].number).collect();
        assert_eq!(order, [2, 3, 1]);
        assert!(w.rows[0].linked.is_some() && w.rows[1].linked.is_none());
        w.selected = 0;
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
        assert!(w.lists.contains_key(&("other".into(), Kind::Items)));
        assert!(w.rows.is_empty() && w.selected_item().is_none());
    }

    #[test]
    fn ctrl_f_switches_to_pull_requests_and_back() {
        let mut w = work();
        w.lists.insert(
            ("sy".into(), Kind::Prs),
            Listing::Ready(OpenItems {
                noun: "pull requests",
                items: vec![OpenItem {
                    number: 8,
                    title: "A PR".into(),
                    url: "https://github.com/o/r/pull/8".into(),
                    author: "octo".into(),
                    ..OpenItem::default()
                }],
            }),
        );
        w.refilter("", &[]);
        let ctrl_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL);
        assert_eq!(w.key(ctrl_f, 10), Outcome::Stay);
        assert_eq!(w.kind, Kind::Prs);
        w.refilter("", &[]);
        let item = w.selected_item().unwrap();
        assert_eq!((item.number, person(item).as_str()), (8, "octo"));
        // A task the PR is linked to (as its derived link) takes it.
        let json = serde_json::json!([{
            "host": "mac", "session_id": "s", "window_id": "w", "project": "sy",
            "name": "t", "profile": "personal",
            "links": [{"kind": "pr", "system": "github", "ref": "o/r#8", "url": ""}],
        }]);
        let tasks = crate::model::parse_tasks(&json.to_string()).unwrap();
        w.refilter("", &tasks);
        assert!(matches!(w.key(key(KeyCode::Enter), 10), Outcome::Go(_)));
        w.key(ctrl_f, 10);
        w.refilter("", &[]);
        assert_eq!(w.selected_item().unwrap().number, 3);
    }

    #[test]
    fn ctrl_g_arms_an_approval_and_enter_approves() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let ctrl_g = || KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL);
        let mut w = work();
        w.refilter("", &[]);
        // Work items aren't approved.
        assert!(matches!(w.key(ctrl_g(), 10), Outcome::Refuse(_)));
        w.kind = Kind::Prs;
        let pr = |n: u64, draft: bool| OpenItem {
            number: n,
            title: format!("PR {n}"),
            url: format!("https://github.com/o/r/pull/{n}"),
            draft,
            ..OpenItem::default()
        };
        w.lists.insert(
            ("sy".into(), Kind::Prs),
            Listing::Ready(OpenItems {
                noun: "pull requests",
                items: vec![pr(5, false), pr(4, true)],
            }),
        );
        w.refilter("", &[]);
        assert_eq!(w.key(ctrl_g(), 10), Outcome::Arm);
        assert_eq!(
            w.armed_pr(),
            Some((Action::Approve, "#5".into(), "PR 5".into()))
        );
        // Any other key disarms it, and Enter goes back to launching.
        w.key(key(KeyCode::Home), 10);
        assert_eq!(w.armed_pr(), None);
        assert!(matches!(
            w.key(key(KeyCode::Enter), 10),
            Outcome::Launch { .. }
        ));
        // ctrl-g again keeps it armed; Enter approves.
        w.key(ctrl_g(), 10);
        assert_eq!(w.key(ctrl_g(), 10), Outcome::Arm);
        assert_eq!(
            w.key(key(KeyCode::Enter), 10),
            Outcome::Act {
                action: Action::Approve,
                item: Item::Pr {
                    repo: "o/r".into(),
                    number: "5".into(),
                    draft: false,
                },
                url: "https://github.com/o/r/pull/5".into(),
                name: "#5".into(),
            }
        );
        assert_eq!(w.armed, None);
        // Moving disarms; a draft can't be armed.
        w.key(ctrl_g(), 10);
        w.key(key(KeyCode::Down), 10);
        assert_eq!(w.armed_pr(), None);
        assert_eq!(w.key(ctrl_g(), 10), Outcome::Refuse("#4 is a draft".into()));
    }

    #[test]
    fn ctrl_m_squash_merges_only_an_approved_pr_with_passing_checks() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let ctrl_m = || KeyEvent::new(KeyCode::Char('m'), KeyModifiers::CONTROL);
        let mut w = work();
        w.refilter("", &[]);
        assert!(matches!(w.key(ctrl_m(), 10), Outcome::Refuse(_)));
        w.kind = Kind::Prs;
        let pr = |n: u64, review: &str, checks: &str| OpenItem {
            number: n,
            title: format!("PR {n}"),
            url: format!("https://github.com/o/r/pull/{n}"),
            review: review.into(),
            checks: checks.into(),
            ..OpenItem::default()
        };
        w.lists.insert(
            ("sy".into(), Kind::Prs),
            Listing::Ready(OpenItems {
                noun: "pull requests",
                items: vec![
                    pr(5, "APPROVED", "pass"),
                    pr(4, "", "pass"),
                    pr(3, "APPROVED", "fail"),
                    pr(2, "APPROVED", "pending"),
                ],
            }),
        );
        w.refilter("", &[]);
        assert!(w.hints().contains(&("⌃M", "squash-merge")));
        assert_eq!(w.key(ctrl_m(), 10), Outcome::Arm);
        assert_eq!(
            w.armed_pr(),
            Some((Action::Merge, "#5".into(), "PR 5".into()))
        );
        assert_eq!(
            w.key(key(KeyCode::Enter), 10),
            Outcome::Act {
                action: Action::Merge,
                item: Item::Pr {
                    repo: "o/r".into(),
                    number: "5".into(),
                    draft: false,
                },
                url: "https://github.com/o/r/pull/5".into(),
                name: "#5".into(),
            }
        );
        let refused = |w: &mut Work| match w.key(ctrl_m(), 10) {
            Outcome::Refuse(why) => why,
            o => panic!("{o:?}"),
        };
        w.key(key(KeyCode::Down), 10);
        assert!(!w.hints().contains(&("⌃M", "squash-merge")));
        assert_eq!(refused(&mut w), "#4 isn't approved yet");
        w.key(key(KeyCode::Down), 10);
        assert_eq!(refused(&mut w), "#3 has failing checks");
        w.key(key(KeyCode::Down), 10);
        assert_eq!(refused(&mut w), "#2 has checks still running");
        assert_eq!(w.armed, None);
    }

    #[test]
    fn letters_and_view_keys_pass_to_the_list() {
        let mut w = work();
        w.refilter("", &[]);
        assert_eq!(w.key(key(KeyCode::Char('x')), 10), Outcome::Pass);
        let ctrl_o = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL);
        assert_eq!(w.key(ctrl_o, 10), Outcome::Pass);
        let ctrl_v = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL);
        assert_eq!(w.key(ctrl_v, 10), Outcome::Scroll(None));
        // ctrl-u and ctrl-d scroll the item, not the list.
        let ctrl_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(w.key(ctrl_d, 10), Outcome::Scroll(Some(ctrl_d)));
        let ctrl_u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(w.key(ctrl_u, 10), Outcome::Scroll(Some(ctrl_u)));
    }

    #[test]
    fn rows_name_a_few_labels_and_the_first_assignee_briefly() {
        let item = OpenItem {
            labels: vec!["enhancement".into(), "rust".into(), "schema".into()],
            assignees: vec!["John Allen".into(), "octo".into()],
            ..card(1, "t")
        };
        assert_eq!(labels(&item), "enhancement rust +1");
        assert_eq!(assignee(&item), "John+1");
        assert_eq!(assignee(&card(2, "t")), "");
        assert_eq!(clip("jasmineHunderfeld", MAX_NAME), "jasmineHund…");
        assert_eq!(clip("short", MAX_NAME), "short");
    }
}
