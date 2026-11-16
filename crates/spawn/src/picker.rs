//! The new-session picker (see main.rs for the modes). It lists the catalog's
//! projects and the zoxide folders of one host, and reports a pick in the
//! terminal title: a plain session in the highlighted folder or project
//! (kind `session`), or agent work in a catalog project or Git folder (kind `agent`) with
//! a name, harness, optional work item and optional prompt. Titles are
//! `rex-new:session/HOST/PATH` and
//! `rex-new:agent/HOST/PATH/NAME/HARNESS[/PROMPT_REF[/WORK_ITEM]]`, the
//! fields hex encoded; `PROMPT_REF` is empty when only a work item follows.
//!
//! In a catalog project, the work item field lists the project's open cards
//! or issues (core's `items`) once it has focus: ↓ or typing shows the
//! matches over the list, Enter (the float's action or standalone) takes
//! the highlighted one's URL as the work item, Tab fills the field with it
//! and Escape hides them. Nothing listed, as when the CLI is missing or
//! unauthenticated, leaves the field free-form.
//!
//! ctrl-o chooses the host (the folders' host, and where agent work runs,
//! where `auto` picks the least busy); ctrl-s switches the kind. A session
//! on `auto` runs on the host the float is on.

use std::collections::{HashMap, HashSet};
use std::io;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Clear;
use ratatui::{DefaultTerminal, Frame};
use ratatui_textarea::TextArea;
use switchyard_core::commands::{self, SlashCommand};
use switchyard_core::folders::{self, Folder};
use switchyard_core::hosts::{AUTO, CONFIG, Project, catalog, compatible, dev_hosts, pool};
use switchyard_core::items::{self, OpenItem, OpenItems};
use switchyard_core::launch::{WORK_ITEM_COMMAND, launch};
use switchyard_core::naming::{HARNESSES, branch_and_slug, name_char, name_problem};
use switchyard_core::prompts::prune_prompts;
use switchyard_core::rex::{local_label, log, select};
use switchyard_ui::Palette;
use switchyard_ui::chrome;
use switchyard_ui::draw::{self, cells, delete_word, highlight, pill, str_width};
use switchyard_ui::fuzzy::score_fields;
use switchyard_ui::glyphs::{ICON_SEARCH, SPINNER, tick};
use switchyard_ui::prompt_box::{self, PromptBox, place_cursor, text_field, text_key};
use switchyard_ui::protocol::{Title, clear_title, hex};

const ICON_REPO: &str = "\u{f0ea0}"; // nf-md-source_repository
const ICON_GROUP: &str = "\u{f024b}"; // nf-md-folder_multiple
const ICON_FOLDER: &str = "\u{f024b}"; // nf-md-folder

/// What the picker prepares: a plain session in a folder, or agent work.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Session,
    Agent,
}

impl Kind {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "session" => Some(Kind::Session),
            "agent" => Some(Kind::Agent),
            _ => None,
        }
    }
}

/// Where agent work would run, for its slash commands: (host, profile,
/// harness, folder).
type Probe = (String, String, String, String);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Project,
    Name,
    Item,
    Prompt,
}

/// Text rows the prompt box grows between; longer prompts scroll to their end.
const PROMPT_MIN_ROWS: u16 = 3;
const PROMPT_MAX_ROWS: u16 = 6;
/// The prompt box around its text: soft top and bottom edges, a gap and the
/// chip row.
const PROMPT_CHROME_ROWS: u16 = prompt_box::CHROME_ROWS;
/// The most rows the work item menu shows at once.
const ITEM_MENU_ROWS: u16 = 8;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Item {
    Group(usize),
    Project(usize),
    /// The header of the listed host's folders.
    Folders,
    Folder(usize),
}

fn selectable(i: &Item) -> bool {
    matches!(i, Item::Project(_) | Item::Folder(_))
}

/// Name and path (or folder display) hits for a row's fields.
type Hits = (Vec<usize>, Vec<usize>);

/// A host's folders, once listed.
enum Load {
    Waiting,
    Ready(Vec<Folder>),
    Failed(String),
}

fn keep_best(best: &mut Option<(u32, Item)>, score: u32, item: Item) {
    if best.is_none_or(|(b, _)| score > b) {
        *best = Some((score, item));
    }
}

/// The listed host's folder rows (a header first, unless the list is ready
/// and has none to show), skipping folders that are catalog projects, which
/// have their own rows. While filtering, only the matching folders.
fn folder_rows(
    load: Option<&Load>,
    catalog: &HashSet<String>,
    matcher: &mut Matcher,
    pattern: &Pattern,
    filtering: bool,
    hits: &mut HashMap<Item, Hits>,
    best: &mut Option<(u32, Item)>,
) -> Vec<Item> {
    let mut rows = Vec::new();
    if let Some(Load::Ready(list)) = load {
        for (j, f) in list.iter().enumerate() {
            if catalog.contains(&f.display) {
                continue;
            }
            if filtering {
                let Some((score, mut h)) = score_fields(matcher, pattern, &[&f.display]) else {
                    continue;
                };
                hits.insert(Item::Folder(j), (h.swap_remove(0), Vec::new()));
                keep_best(best, score, Item::Folder(j));
            }
            rows.push(Item::Folder(j));
        }
    }
    if !rows.is_empty() || !matches!(load, Some(Load::Ready(_))) {
        rows.insert(0, Item::Folders);
    }
    rows
}

/// How a list row is drawn: its background, the edge at its left (iris on
/// the focused pick), and the style of the query's hits.
struct RowLook {
    edge: Span<'static>,
    bg: Color,
    hl: Style,
}

/// What a key asked the event loop to do once it has been handled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Request {
    /// A standalone Enter: launch the pick.
    Launch,
    /// ctrl-g: hand the prompt to `$EDITOR`.
    Edit,
}

/// The work item menu: whether it was asked for (↓ or typing in the field),
/// its matches for the field's text (item index and title hits), the
/// highlighted match, the text they are for, and where the form's fields
/// were drawn, within what.
#[derive(Default)]
struct ItemMenu {
    open: bool,
    hits: Vec<(usize, Vec<usize>)>,
    selected: usize,
    query: Option<String>,
    card: Option<(Rect, Rect)>,
}

struct App {
    p: Palette,
    kind: Kind,
    projects: Vec<Project>,
    groups: Vec<String>,
    /// Host chip order: `auto` first, then this host, the config host, and
    /// the dev hosts.
    hosts: Vec<String>,
    host: usize,
    /// This host's label, where a float runs and `session` on `auto` opens.
    here: String,
    /// The config host's label and zoxide folders, passed in by the float (or
    /// read here when run from a shell there).
    home: String,
    home_folders: String,
    loads: HashMap<String, Load>,
    tx: Sender<(String, Result<String, String>)>,
    rx: Receiver<(String, Result<String, String>)>,
    harness: usize,
    focus: Focus,
    query: String,
    /// The name, work item and prompt fields: readline-style editors (see
    /// `text_key`).
    name: TextArea<'static>,
    item: TextArea<'static>,
    /// The prompt, kept in its prompt file (see `PromptBox`).
    prompt: PromptBox,
    /// Slash commands by where the agent would run (`Probe`), None while
    /// being fetched; the probe the prompt has the commands of; and the
    /// fetches' results.
    commands: HashMap<Probe, Option<Vec<SlashCommand>>>,
    commands_for: Option<Probe>,
    commands_tx: Sender<(Probe, Result<Vec<SlashCommand>, String>)>,
    commands_rx: Receiver<(Probe, Result<Vec<SlashCommand>, String>)>,
    /// Where the prompt box was last drawn, and within what, for its menu.
    prompt_card: Option<(Rect, Rect)>,
    /// Open work items by catalog project path, None while being listed
    /// (empty when they couldn't be), and the listings' results.
    open_items: HashMap<String, Option<OpenItems>>,
    items_tx: Sender<(String, Result<OpenItems, String>)>,
    items_rx: Receiver<(String, Result<OpenItems, String>)>,
    item_menu: ItemMenu,
    matcher: Matcher,
    rows: Vec<Item>,
    hits: HashMap<Item, Hits>,
    selected: Option<usize>,
    offset: usize,
    list_area: Rect,
    title: Title,
    /// Smooth prompt-box corners, when the terminal does kitty graphics.
    corners: Option<chrome::Corners>,
    /// Whether the terminal lost focus since the mode was last entered.
    away: bool,
    /// Whether Escape was pressed once with a prompt, so the next one closes.
    confirm_close: bool,
    /// What the last key asked the event loop to do (see `Request`).
    request: Option<Request>,
    /// The terminal's cell size, to send the corner images again.
    cell: Option<chrome::CellSize>,
    /// Run from a shell rather than in the config's float: Enter reaches the
    /// picker, which opens the pick itself (see `standalone_launch`).
    standalone: bool,
    /// The last standalone launch's progress or failure (and whether it
    /// failed), shown in the footer until the next key.
    note: Option<(String, bool)>,
    /// Printed once the picker has closed: a launch that couldn't show its tab.
    exit_note: Option<String>,
    started: Instant,
}

impl App {
    fn new(
        kind: Kind,
        projects: Vec<Project>,
        home: String,
        home_folders: String,
        dev: Vec<String>,
    ) -> Self {
        let mut groups: Vec<String> = Vec::new();
        for p in &projects {
            if !groups.contains(&p.profile) {
                groups.push(p.profile.clone());
            }
        }
        let here = local_label();
        let mut hosts = vec![AUTO.to_string()];
        for h in [here.clone(), home.clone()].into_iter().chain(dev) {
            if !h.is_empty() && !hosts.contains(&h) {
                hosts.push(h);
            }
        }
        let (tx, rx) = mpsc::channel();
        let (commands_tx, commands_rx) = mpsc::channel();
        let (items_tx, items_rx) = mpsc::channel();
        let mut app = App {
            p: Palette::load(),
            kind,
            projects,
            groups,
            hosts,
            host: 0,
            here,
            home,
            home_folders,
            loads: HashMap::new(),
            tx,
            rx,
            harness: 0,
            focus: Focus::Project,
            query: String::new(),
            name: text_field(),
            item: text_field(),
            prompt: PromptBox::new(),
            commands: HashMap::new(),
            commands_for: None,
            commands_tx,
            commands_rx,
            prompt_card: None,
            open_items: HashMap::new(),
            items_tx,
            items_rx,
            item_menu: ItemMenu::default(),
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            rows: Vec::new(),
            hits: HashMap::new(),
            selected: None,
            offset: 0,
            list_area: Rect::default(),
            title: Title::new("rex-new"),
            corners: None,
            away: false,
            confirm_close: false,
            cell: None,
            standalone: false,
            request: None,
            note: None,
            exit_note: None,
            started: Instant::now(),
        };
        app.load_listed();
        app.refilter();
        app
    }

    /// The host whose folders are listed: the host chip's, or this host
    /// under `auto`.
    fn listed(&self) -> &str {
        match self.hosts[self.host].as_str() {
            AUTO => &self.here,
            host => host,
        }
    }

    /// List the listed host's folders in the background, once per host.
    fn load_listed(&mut self) {
        let host = self.listed().to_string();
        if self.loads.contains_key(&host) {
            return;
        }
        self.loads.insert(host.clone(), Load::Waiting);
        let (tx, here, home, home_folders) = (
            self.tx.clone(),
            self.here.clone(),
            self.home.clone(),
            self.home_folders.clone(),
        );
        std::thread::spawn(move || {
            let r = folders::list(&host, &here, &home, &home_folders);
            let _ = tx.send((host, r));
        });
    }

    /// Where the highlighted agent work would run, for its slash commands:
    /// host (on `auto`, this one, where the catalog's config also is),
    /// profile, harness and folder.
    fn probe(&self) -> Option<Probe> {
        let dir = self.target_path()?;
        let profile = match self.project() {
            Some(p) => p.profile.clone(),
            None if dir.contains("/dev/src/amfaro") => "work".into(),
            None => "personal".into(),
        };
        let host = match self.agent_host() {
            AUTO => self.here.clone(),
            h => h.to_string(),
        };
        Some((host, profile, HARNESSES[self.harness].to_string(), dir))
    }

    /// Give the prompt the slash commands of where the work would run,
    /// fetching them in the background once the prompt has focus there.
    fn sync_commands(&mut self) {
        while let Ok((probe, got)) = self.commands_rx.try_recv() {
            let list = got.unwrap_or_else(|e| {
                log(&format!(
                    "slash commands for {} on {}: {e}",
                    probe.2, probe.0
                ));
                Vec::new()
            });
            self.commands.insert(probe, Some(list));
            // Take them now if they are for the current probe.
            self.commands_for = None;
        }
        let probe = if self.kind == Kind::Agent {
            self.probe()
        } else {
            None
        };
        if probe == self.commands_for {
            return;
        }
        let Some(probe) = probe else {
            self.prompt.set_commands(Vec::new(), true);
            self.commands_for = None;
            return;
        };
        let anywhere = commands::anywhere(&probe.2);
        match self.commands.get(&probe) {
            Some(Some(list)) => {
                self.prompt.set_commands(list.clone(), anywhere);
                self.commands_for = Some(probe);
            }
            None if self.focus == Focus::Prompt => {
                self.prompt.set_commands(Vec::new(), anywhere);
                self.commands.insert(probe.clone(), None);
                let tx = self.commands_tx.clone();
                std::thread::spawn(move || {
                    let got = commands::fetch(&probe.0, &probe.1, &probe.2, &probe.3);
                    let _ = tx.send((probe, got));
                });
            }
            // Still being fetched, or not wanted until the prompt has focus.
            Some(None) | None => self.prompt.set_commands(Vec::new(), anywhere),
        }
    }

    /// List the highlighted project's open work items in the background
    /// once the work item field has focus there, and match them against the
    /// field's text.
    fn sync_items(&mut self) {
        while let Ok((path, got)) = self.items_rx.try_recv() {
            let list = got.unwrap_or_else(|e| {
                log(&format!("open work items for {path}: {e}"));
                OpenItems {
                    noun: "items",
                    items: Vec::new(),
                }
            });
            self.open_items.insert(path, Some(list));
        }
        if self.kind == Kind::Agent
            && self.focus == Focus::Item
            && let Some(project) = self.project()
            && !self.open_items.contains_key(&project.path)
        {
            let project = project.clone();
            self.open_items.insert(project.path.clone(), None);
            let tx = self.items_tx.clone();
            std::thread::spawn(move || {
                let got = items::list_open(&project);
                let _ = tx.send((project.path, got));
            });
        }
        self.match_items();
    }

    /// Whether the highlighted project's open items are being listed, for
    /// the spinner.
    fn listing_items(&self) -> bool {
        self.project()
            .is_some_and(|p| matches!(self.open_items.get(&p.path), Some(None)))
    }

    /// The highlighted project's open work items, once listed.
    fn item_list(&self) -> Option<&OpenItems> {
        self.project()
            .and_then(|p| self.open_items.get(&p.path))
            .and_then(Option::as_ref)
    }

    /// Match the listed items against the field's text, by number and title:
    /// all of them, in order, for an empty field; none once the field holds
    /// one of their URLs (chosen) or the menu isn't showing.
    fn match_items(&mut self) {
        let query = self.typed_item();
        if self.item_menu.query.as_ref() != Some(&query) {
            self.item_menu.selected = 0;
            self.item_menu.query = Some(query.clone());
        }
        // Leaving the field closes the menu, so tabbing back through it
        // never picks an item by accident.
        if self.focus != Focus::Item {
            self.item_menu.open = false;
        }
        let showing = self.kind == Kind::Agent && self.focus == Focus::Item && self.item_menu.open;
        let list = match self.item_list() {
            Some(list) if showing && !list.items.iter().any(|i| i.url == query) => {
                list.items.clone()
            }
            _ => Vec::new(),
        };
        let query = query.trim_start_matches('#');
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let mut scored: Vec<(u32, usize, Vec<usize>)> = Vec::new();
        for (i, item) in list.iter().enumerate() {
            if query.is_empty() {
                scored.push((0, i, Vec::new()));
                continue;
            }
            let number = item.number.to_string();
            if let Some((score, mut hits)) =
                score_fields(&mut self.matcher, &pattern, &[&number, &item.title])
            {
                scored.push((score, i, hits.swap_remove(1)));
            }
        }
        // Best first, in listed order among equals.
        scored.sort_by_key(|s| std::cmp::Reverse(s.0));
        self.item_menu.hits = scored.into_iter().map(|(_, i, h)| (i, h)).collect();
        self.item_menu.selected = self
            .item_menu
            .selected
            .min(self.item_menu.hits.len().saturating_sub(1));
    }

    /// The item highlighted in the showing work item menu.
    fn chosen_item(&self) -> Option<&OpenItem> {
        let (i, _) = self.item_menu.hits.get(self.item_menu.selected)?;
        self.item_list()?.items.get(*i)
    }

    /// Take a key for the work item menu: ↓ opens it when there are items;
    /// while it shows, ↑/↓ (ctrl-p/n) move, Tab fills the field with the
    /// highlighted item and Escape hides it. False for any other key.
    fn item_menu_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        self.match_items();
        if self.item_menu.hits.is_empty() {
            if k.code != KeyCode::Down || self.item_menu.open {
                return false;
            }
            self.item_menu.open = true;
            self.match_items();
            return !self.item_menu.hits.is_empty();
        }
        let count = self.item_menu.hits.len();
        let step = |at: usize, by: isize| draw::cycle(at, by, count);
        match k.code {
            KeyCode::Down => self.item_menu.selected = step(self.item_menu.selected, 1),
            KeyCode::Char('n') if ctrl => {
                self.item_menu.selected = step(self.item_menu.selected, 1);
            }
            KeyCode::Up => self.item_menu.selected = step(self.item_menu.selected, -1),
            KeyCode::Char('p') if ctrl => {
                self.item_menu.selected = step(self.item_menu.selected, -1);
            }
            KeyCode::Tab => {
                if let Some(url) = self.chosen_item().map(|i| i.url.clone()) {
                    self.item.clear();
                    self.item.insert_str(&url);
                }
                self.item_menu.open = false;
            }
            KeyCode::Esc => self.item_menu.open = false,
            _ => return false,
        }
        self.match_items();
        true
    }

    fn receive(&mut self) {
        let mut changed = false;
        while let Ok((host, r)) = self.rx.try_recv() {
            let load = match r {
                Ok(text) => Load::Ready(folders::parse(&text)),
                Err(e) => Load::Failed(e),
            };
            changed |= host == self.listed();
            self.loads.insert(host, load);
        }
        if changed {
            self.refilter();
        }
    }

    fn waiting(&self) -> bool {
        matches!(self.loads.get(self.listed()), Some(Load::Waiting) | None)
    }

    /// ctrl-o: the next host chip the highlighted pick may use. Leaving
    /// `auto` skips the host it already shows, so the first press changes
    /// where the work would run.
    fn next_host(&mut self) {
        let n = self.hosts.len().max(1);
        let from_auto = self.chosen() == AUTO;
        let shown = self.auto_host();
        let start = if from_auto { 0 } else { self.host };
        let next = (1..=n).map(|step| (start + step) % n).find(|&i| {
            let h = self.hosts[i].as_str();
            self.allowed(h) && !(from_auto && shown.as_deref() == Some(h))
        });
        self.host = next.unwrap_or(0);
        self.host_changed();
    }

    /// Whether the host chip may be `host` for the highlighted pick: a
    /// catalog project only moves to hosts of its profile (see
    /// `hosts::compatible`); `auto`, folders and nothing are unrestricted.
    fn allowed(&self, host: &str) -> bool {
        host == AUTO
            || self.project().is_none_or(|project| {
                compatible(project)
                    .iter()
                    .any(|h| self.host_label(h) == host)
            })
    }

    /// The host chip as it applies to the highlighted pick: `auto` when the
    /// chosen host is not one the pick may use.
    fn chosen(&self) -> &str {
        let host = self.hosts[self.host].as_str();
        if self.allowed(host) { host } else { AUTO }
    }

    /// A pool entry as its host label: the config host by its own name.
    fn host_label(&self, host: &str) -> String {
        match host {
            CONFIG if !self.home.is_empty() => self.home.clone(),
            CONFIG => "Mac".to_string(),
            h => h.to_string(),
        }
    }

    /// The one host `auto` would use for the highlighted pick, if it is
    /// settled rather than the least busy of a pool.
    fn auto_host(&self) -> Option<String> {
        if self.kind == Kind::Session {
            return Some(self.here.clone());
        }
        if self.folder().is_some() {
            return Some(self.listed().to_string());
        }
        match self.project() {
            Some(project) => match pool(project).as_slice() {
                [h] => Some(self.host_label(h)),
                _ => None,
            },
            None => Some(self.listed().to_string()),
        }
    }

    /// The host chip moved: list its folders and refilter.
    fn host_changed(&mut self) {
        self.load_listed();
        self.refilter();
    }

    fn highlighted(&self) -> Option<Item> {
        self.selected.and_then(|r| self.rows.get(r)).copied()
    }

    fn project(&self) -> Option<&Project> {
        match self.highlighted() {
            Some(Item::Project(i)) => self.projects.get(i),
            _ => None,
        }
    }

    fn folder_at(&self, j: usize) -> Option<&Folder> {
        match self.loads.get(self.listed()) {
            Some(Load::Ready(list)) => list.get(j),
            _ => None,
        }
    }

    fn folder(&self) -> Option<&Folder> {
        match self.highlighted() {
            Some(Item::Folder(j)) => self.folder_at(j),
            _ => None,
        }
    }

    /// The highlighted project's or folder's path, as the session or agent
    /// work uses it (a catalog path may start with ~/).
    fn target_path(&self) -> Option<String> {
        self.project()
            .map(|p| p.path.clone())
            .or_else(|| self.folder().map(|f| f.path.clone()))
    }

    fn refilter(&mut self) {
        let prev = self.highlighted();
        self.rows.clear();
        self.hits.clear();
        let pattern = Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart);
        let filtering = !self.query.trim().is_empty();
        let mut best: Option<(u32, Item)> = None;
        for g in 0..self.groups.len() {
            let mut matched = Vec::new();
            for i in 0..self.projects.len() {
                if self.projects[i].profile != self.groups[g] {
                    continue;
                }
                if !filtering {
                    matched.push(i);
                    continue;
                }
                // Match projects by name only; every catalog path shares a
                // prefix, so path hits would match most of the list.
                let name = self.projects[i].name.clone();
                if let Some((score, mut f)) = score_fields(&mut self.matcher, &pattern, &[&name]) {
                    self.hits
                        .insert(Item::Project(i), (f.swap_remove(0), Vec::new()));
                    keep_best(&mut best, score, Item::Project(i));
                    matched.push(i);
                }
            }
            if matched.is_empty() {
                continue;
            }
            self.rows.push(Item::Group(g));
            self.rows.extend(matched.into_iter().map(Item::Project));
        }
        let host = self.listed().to_string();
        let catalog: HashSet<String> = self
            .projects
            .iter()
            .map(|p| folders::home_relative(&p.path))
            .collect();
        let rows = folder_rows(
            self.loads.get(&host),
            &catalog,
            &mut self.matcher,
            &pattern,
            filtering,
            &mut self.hits,
            &mut best,
        );
        self.rows.extend(rows);
        let want = if filtering {
            best.map(|(_, i)| i)
        } else {
            prev
        };
        self.selected = want
            .and_then(|w| self.rows.iter().position(|i| *i == w))
            .or_else(|| self.rows.iter().position(selectable));
        self.offset = 0;
    }

    fn step(&mut self, delta: isize) {
        let Some(at) = self.selected else { return };
        self.selected = Some(draw::step(&self.rows, at, delta, selectable));
    }

    /// As `step`, but past either end of the list it wraps to the other.
    fn wrap_step(&mut self, delta: isize) {
        let Some(at) = self.selected else { return };
        self.selected = Some(draw::step_wrapping(&self.rows, at, delta, selectable));
    }

    fn next_focus(&mut self, forward: bool) {
        // A session has no name or prompt.
        if self.kind == Kind::Session {
            self.focus = Focus::Project;
            return;
        }
        // Top to bottom: project, prompt, name, work item.
        self.focus = match (self.focus, forward) {
            (Focus::Project, true) | (Focus::Name, false) => Focus::Prompt,
            (Focus::Prompt, true) | (Focus::Item, false) => Focus::Name,
            (Focus::Name, true) | (Focus::Project, false) => Focus::Item,
            (Focus::Item, true) | (Focus::Prompt, false) => Focus::Project,
        };
    }

    fn toggle_kind(&mut self) {
        self.kind = match self.kind {
            Kind::Agent => Kind::Session,
            Kind::Session => Kind::Agent,
        };
        if self.kind == Kind::Session {
            self.focus = Focus::Project;
        }
    }

    /// Catalog work may use its auto pool; a folder only exists on the
    /// host whose folders are listed.
    fn agent_host(&self) -> &str {
        if self.folder().is_some() {
            self.listed()
        } else {
            self.chosen()
        }
    }

    /// The work name as typed.
    fn name_text(&self) -> String {
        self.name.lines().concat()
    }

    /// The work item as typed.
    fn typed_item(&self) -> String {
        self.item.lines().concat().trim().to_string()
    }

    /// The work item as sent: the work item menu's highlighted item while
    /// it shows (so Enter takes it), else the field's text.
    fn item_text(&self) -> String {
        match self.chosen_item() {
            Some(item) => item.url.clone(),
            None => self.typed_item(),
        }
    }

    /// The prompt as sent: surrounding blank space dropped.
    fn prompt_text(&self) -> String {
        self.prompt.text()
    }

    /// What a blank name is named from on Enter (see core's `autoname`):
    /// the work item, else the prompt; None when the work must be named.
    fn names_from(&self) -> Option<&'static str> {
        if !self.name_text().is_empty() {
            None
        } else if !self.item_text().is_empty() {
            Some("work item")
        } else if !self.prompt_text().is_empty() {
            Some("prompt")
        } else {
            None
        }
    }

    /// Whether closing would throw away typed text (only agent work keeps a
    /// prompt and work item).
    fn has_draft(&self) -> bool {
        self.kind == Kind::Agent && !(self.prompt_text().is_empty() && self.item_text().is_empty())
    }

    /// What Enter would do, as the config's title protocol expects; empty
    /// while the form is incomplete or the prompt couldn't be saved.
    fn pick(&self) -> String {
        let Some(path) = self.target_path() else {
            return String::new();
        };
        match self.kind {
            Kind::Session => format!("session/{}/{}", hex(self.listed()), hex(&path)),
            Kind::Agent => {
                let name = self.name_text();
                // A blank name is fine when launch can name the work.
                if name_problem(&name).is_some() && self.names_from().is_none() {
                    return String::new();
                }
                let mut pick = format!(
                    "agent/{}/{}/{}/{}",
                    hex(self.agent_host()),
                    hex(&path),
                    hex(&name),
                    HARNESSES[self.harness]
                );
                let Some(prompt_ref) = self.prompt.saved_ref() else {
                    return String::new();
                };
                let item = self.item_text();
                if !item.is_empty() {
                    pick = format!("{pick}/{}/{}", hex(prompt_ref), hex(&item));
                } else if !prompt_ref.is_empty() {
                    pick = format!("{pick}/{}", hex(prompt_ref));
                }
                pick
            }
        }
    }

    /// Keep the prompt file in step with the field.
    fn save_prompt(&mut self) {
        self.prompt.save();
    }

    /// Switching away from Rex drops the float's mode, and without it Enter
    /// reaches the picker instead of the config's action. Enter the mode
    /// again once the terminal has focus back.
    fn rearm_mode(&mut self) {
        if !std::mem::take(&mut self.away) || self.standalone {
            return;
        }
        let child = Command::new("rex")
            .args(["do", "client.mode.enter", "name=new"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match child {
            Ok(mut child) => {
                std::thread::spawn(move || child.wait());
            }
            Err(e) => log(&format!("could not re-enter the new-session mode: {e}")),
        }
    }

    fn paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        match self.focus {
            Focus::Project => {
                self.query.extend(text.chars().filter(|c| !c.is_control()));
                self.refilter();
            }
            Focus::Name => {
                self.name
                    .insert_str(text.chars().filter_map(name_char).collect::<String>());
            }
            Focus::Item => {
                self.item
                    .insert_str(text.chars().filter(|&c| item_char(c)).collect::<String>());
                self.item_menu.open = true;
            }
            Focus::Prompt => self.prompt.paste(&text),
        }
    }

    fn report(&mut self) -> io::Result<()> {
        if self.kind == Kind::Agent {
            self.save_prompt();
        }
        let pick = self.pick();
        self.title.report(pick)
    }

    fn key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        self.note = None;
        if !(k.code == KeyCode::Esc || (ctrl && k.code == KeyCode::Char('c'))) {
            self.confirm_close = false;
        }
        // The slash-command menu, while open, has Tab, the arrows and Escape.
        if self.focus == Focus::Prompt && self.prompt.menu_key(k) {
            return true;
        }
        if self.focus == Focus::Item && self.item_menu_key(k) {
            return true;
        }
        // The prompt keeps ctrl-n/p/j/k as readline's line keys; elsewhere
        // they move through the list, like the tab picker. The name keeps
        // ctrl-k for deleting to the end.
        let list_key = match self.focus {
            Focus::Project => matches!(k.code, KeyCode::Char('n' | 'p' | 'j' | 'k')),
            Focus::Name | Focus::Item => matches!(k.code, KeyCode::Char('n' | 'p' | 'j')),
            Focus::Prompt => false,
        };
        match k.code {
            // Standalone, plain Enter acts from any field, as the config's
            // mode makes it do in the float.
            KeyCode::Enter if self.standalone && !alt && !shift && !ctrl => {
                self.request = Some(Request::Launch);
                return true;
            }
            // ctrl-g hands the prompt to $EDITOR, as in other harnesses.
            KeyCode::Char('g') if ctrl && self.focus == Focus::Prompt => {
                self.request = Some(Request::Edit);
                return true;
            }
            // As in a shell, ctrl-c in the prompt throws its text away (ctrl-z
            // brings it back); with nothing to clear, it closes the picker.
            KeyCode::Char('c')
                if ctrl && self.focus == Focus::Prompt && !self.prompt.is_empty() =>
            {
                self.prompt.clear();
            }
            // Escape (or ctrl-c outside a non-empty prompt) closes, but a
            // prompt would be lost, so then it takes a second press.
            KeyCode::Esc | KeyCode::Char('c') if k.code == KeyCode::Esc || ctrl => {
                if !self.has_draft() || std::mem::take(&mut self.confirm_close) {
                    return false;
                }
                self.confirm_close = true;
            }
            KeyCode::Tab => self.next_focus(true),
            KeyCode::BackTab => self.next_focus(false),
            // Overrides for the defaults: host starts at auto, harness at claude.
            KeyCode::Char('o') if ctrl => self.next_host(),
            KeyCode::Char('s') if ctrl => self.toggle_kind(),
            KeyCode::Char('t') if ctrl && self.kind == Kind::Agent => {
                self.harness = (self.harness + 1) % HARNESSES.len();
            }
            KeyCode::Char('p') if ctrl && list_key => self.step(-1),
            KeyCode::Char('n') if ctrl && list_key => self.step(1),
            // ctrl-j and ctrl-k wrap past either end of the list.
            KeyCode::Char('k') if ctrl && list_key => self.wrap_step(-1),
            KeyCode::Char('j') if ctrl && list_key => self.wrap_step(1),
            _ => self.field_key(k),
        }
        true
    }

    /// A key the focused field has rather than the whole form.
    fn field_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let page = isize::try_from(self.list_area.height.saturating_sub(2))
            .unwrap_or(isize::MAX)
            .max(1);
        match self.focus {
            Focus::Project => match k.code {
                KeyCode::Char('u') if ctrl => {
                    self.query.clear();
                    self.refilter();
                }
                KeyCode::Char('w') if ctrl => {
                    delete_word(&mut self.query);
                    self.refilter();
                }
                KeyCode::Backspace if alt || ctrl => {
                    delete_word(&mut self.query);
                    self.refilter();
                }
                KeyCode::Backspace => {
                    self.query.pop();
                    self.refilter();
                }
                KeyCode::Char(c) if !ctrl && !alt => {
                    self.query.push(c);
                    self.refilter();
                }
                KeyCode::Up => self.step(-1),
                KeyCode::Down => self.step(1),
                KeyCode::PageUp => self.step(-page),
                KeyCode::PageDown => self.step(page),
                _ => {}
            },
            Focus::Name => match k.code {
                KeyCode::Up => self.focus = Focus::Prompt,
                KeyCode::Down => self.focus = Focus::Item,
                KeyCode::Enter => {}
                KeyCode::Char(c) if !ctrl && !alt => {
                    if let Some(c) = name_char(c) {
                        self.name.insert_char(c);
                    }
                }
                _ => text_key(&mut self.name, k),
            },
            Focus::Item => match k.code {
                KeyCode::Up => self.focus = Focus::Name,
                KeyCode::Enter => {}
                KeyCode::Char(c) if !ctrl && !alt => {
                    if item_char(c) {
                        self.item.insert_char(c);
                        self.item_menu.open = true;
                    }
                }
                _ => {
                    let before = self.typed_item();
                    text_key(&mut self.item, k);
                    self.item_menu.open |= self.typed_item() != before;
                }
            },
            // Rex's mode takes plain Enter to act, so a newline is alt- or
            // shift-Enter (or arrives in a paste).
            Focus::Prompt => match k.code {
                KeyCode::Up if self.prompt.field.screen_cursor().row == 0 => {
                    self.focus = Focus::Project;
                }
                _ => self.prompt.key(k),
            },
        }
    }

    // ------------------------------------------------------------ drawing

    fn draw(&mut self, f: &mut Frame) {
        let agent = self.kind == Kind::Agent;
        let title = if agent { "Agent work" } else { "New session" };
        let inner = draw::frame(f, &self.p, title, Vec::new(), "", Margin::new(2, 1));
        self.prompt_card = None;
        self.item_menu.card = None;
        // Everything but the list (at least 3 rows) and the prompt's text is
        // fixed; the prompt box grows only into what the list can spare.
        let spare = inner.height.saturating_sub(11 + 3 + PROMPT_CHROME_ROWS);
        let prompt_rows = if agent {
            self.prompt
                .rows(inner.width, PROMPT_MIN_ROWS, PROMPT_MAX_ROWS)
                .min(spare.max(1))
        } else {
            0
        };
        let form_rows = if agent {
            3 + prompt_rows + PROMPT_CHROME_ROWS
        } else {
            2
        };
        let [search, rule1, _, body, _, rule2, _, form, _, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(form_rows),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(inner);
        draw::rule(f, &self.p, rule1);
        draw::rule(f, &self.p, rule2);
        self.draw_search(f, search);
        self.draw_list(f, body);
        if agent {
            self.draw_form(f, form, prompt_rows);
        } else {
            self.draw_session(f, form);
        }
        self.draw_footer(f, footer);
        if let Some((card, bounds)) = self.prompt_card {
            self.prompt.draw_menu(f, &self.p, card, bounds);
        }
        if let Some((fields, bounds)) = self.item_menu.card {
            self.draw_item_menu(f, fields, bounds);
        }
    }

    /// The work item menu, over the list (`bounds`) up to the form's fields:
    /// the matching items, number and title, the highlighted one picked out.
    fn draw_item_menu(&self, f: &mut Frame, fields: Rect, bounds: Rect) {
        let Some(list) = self.item_list() else { return };
        let p = &self.p;
        let room = fields.y.saturating_sub(bounds.y);
        let rows = cells(self.item_menu.hits.len())
            .min(ITEM_MENU_ROWS)
            .min(room);
        if rows == 0 || fields.width < 12 {
            return;
        }
        let area = Rect {
            x: fields.x,
            y: fields.y - rows,
            width: fields.width,
            height: rows,
        };
        f.render_widget(Clear, area);
        let first = self.item_menu.selected.saturating_sub(rows as usize - 1);
        let w = area.width as usize;
        let number_w = self
            .item_menu
            .hits
            .iter()
            .map(|(i, _)| list.items[*i].number.to_string().len())
            .max()
            .unwrap_or(1)
            + 1;
        for (n, (i, hits)) in self
            .item_menu
            .hits
            .iter()
            .enumerate()
            .skip(first)
            .take(rows as usize)
        {
            let item = &list.items[*i];
            let selected = n == self.item_menu.selected;
            let bg = if selected { p.hl_med } else { p.surface };
            let mut spans = vec![
                Span::styled(
                    if selected { "▎" } else { " " },
                    Style::new().fg(p.iris).bg(bg),
                ),
                Span::styled(" ", Style::new().bg(bg)),
                Span::styled(
                    format!("{:>number_w$}  ", format!("#{}", item.number)),
                    Style::new()
                        .fg(if selected { p.foam } else { p.subtle })
                        .bg(bg),
                ),
            ];
            let used: usize = spans.iter().map(|s| str_width(&s.content)).sum();
            let plain = if selected {
                Style::new().fg(p.text).bg(bg).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(p.text).bg(bg)
            };
            let hl = Style::new()
                .fg(p.rose)
                .bg(bg)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            spans.extend(highlight(
                &item.title,
                hits,
                w.saturating_sub(used + 1),
                false,
                plain,
                hl,
            ));
            let used: usize = spans.iter().map(|s| str_width(&s.content)).sum();
            spans.push(Span::styled(
                " ".repeat(w.saturating_sub(used)),
                Style::new().bg(bg),
            ));
            let row = Rect {
                y: area.y + cells(n - first),
                height: 1,
                ..area
            };
            f.render_widget(Line::from(spans), row);
        }
    }

    fn draw_search(&mut self, f: &mut Frame, area: Rect) {
        let p = &self.p;
        let focused = self.focus == Focus::Project;
        let typing = !self.query.is_empty();
        draw::search(
            f,
            p,
            area,
            &self.query,
            "Search projects and folders",
            focused,
            focused,
        );
        let total = self.projects.len();
        let shown = self
            .rows
            .iter()
            .filter(|r| matches!(r, Item::Project(_)))
            .count();
        let mut right = Vec::new();
        if typing {
            right.push(Span::styled(shown.to_string(), Style::new().fg(p.text)));
            right.push(Span::styled(
                format!(" of {total} projects  "),
                Style::new().fg(p.muted),
            ));
        } else {
            right.push(Span::styled(
                format!("{total} projects  "),
                Style::new().fg(p.muted),
            ));
        }
        right.extend(pill(
            vec![Span::styled(
                self.listed().to_string(),
                Style::new().fg(p.foam),
            )],
            p.overlay,
            p.base,
        ));
        right.push(Span::raw(" "));
        f.render_widget(Line::from(right).right_aligned(), area);
    }

    fn draw_list(&mut self, f: &mut Frame, area: Rect) {
        self.list_area = area;
        let h = area.height as usize;
        if self.rows.is_empty() {
            self.draw_empty(f, area);
            return;
        }
        if let Some(sel) = self.selected {
            let header_above = sel > 0 && !selectable(&self.rows[sel - 1]);
            self.offset = draw::keep_in_view(self.offset, sel, h, header_above);
        }
        let overflow = self.rows.len() > h && h > 0;
        let row_area = Rect {
            width: area.width.saturating_sub(if overflow { 2 } else { 0 }),
            ..area
        };
        for (i, row) in self.rows.iter().enumerate().skip(self.offset).take(h) {
            let r = Rect {
                y: area.y + cells(i - self.offset),
                height: 1,
                ..row_area
            };
            let last_in_group = !self.rows.get(i + 1).is_some_and(selectable);
            self.draw_row(f, r, row, self.selected == Some(i), last_in_group);
        }
        if overflow {
            draw::scrollbar(f, &self.p, area, self.offset, self.rows.len());
        }
    }

    fn draw_empty(&self, f: &mut Frame, area: Rect) {
        let p = &self.p;
        let lines = if self.query.trim().is_empty() {
            vec![
                Line::styled(ICON_REPO, Style::new().fg(p.hl_high)),
                Line::from(""),
                Line::styled(
                    format!("No projects or folders on {}", self.listed()),
                    Style::new().fg(p.subtle),
                ),
                Line::styled(
                    "Add some to agent-projects/projects.toml",
                    Style::new().fg(p.muted),
                ),
            ]
        } else {
            vec![
                Line::styled(ICON_SEARCH, Style::new().fg(p.hl_high)),
                Line::from(""),
                Line::from(vec![
                    Span::styled("Nothing matches ", Style::new().fg(p.subtle)),
                    Span::styled(format!("“{}”", self.query), Style::new().fg(p.text)),
                ]),
                draw::key_hint(p, "⌃U", " clears the search"),
            ]
        };
        draw::centered(f, area, lines);
    }

    fn draw_row(&self, f: &mut Frame, row: Rect, item: &Item, selected: bool, last: bool) {
        let p = &self.p;
        let width = usize::from(row.width);
        // The list keeps showing its pick while another field has focus, but
        // only the focused list gets the iris edge.
        let active = selected && self.focus == Focus::Project;
        let bg = if selected { p.overlay } else { p.base };
        f.buffer_mut().set_style(row, Style::new().bg(bg));
        let edge = if active {
            Span::styled("▎", Style::new().fg(p.iris).bg(bg))
        } else {
            Span::styled(" ", Style::new().bg(bg))
        };
        let hl = Style::new()
            .fg(p.rose)
            .bg(bg)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        // Folders are not agent work's to run in: muted when agent work is
        // the kind.
        let folder_ok = self.kind == Kind::Session;
        match item {
            Item::Group(index) => {
                let group = &self.groups[*index];
                let count = self.projects.iter().filter(|x| &x.profile == group).count();
                let spans = vec![
                    edge,
                    Span::styled(ICON_GROUP, Style::new().fg(p.subtle).bg(bg)),
                    Span::styled("  ", Style::new().bg(bg)),
                    Span::styled(
                        group.clone(),
                        Style::new().fg(p.text).bg(bg).add_modifier(Modifier::BOLD),
                    ),
                ];
                f.render_widget(Line::from(spans), row);
                let text = format!("{count} project{}", if count == 1 { "" } else { "s" });
                let mut right = pill(
                    vec![Span::styled(text, Style::new().fg(p.subtle))],
                    p.overlay,
                    bg,
                );
                right.push(Span::styled(" ", Style::new().bg(bg)));
                f.render_widget(Line::from(right).right_aligned(), row);
            }
            Item::Folders => self.draw_folders_row(f, row),
            Item::Project(_) => {
                self.draw_project_row(f, row, item, selected, last, RowLook { edge, bg, hl });
            }
            Item::Folder(j) => {
                let Some(folder) = self.folder_at(*j) else {
                    return;
                };
                let hits = self.hits.get(item).map(|h| h.0.clone()).unwrap_or_default();
                let on = if folder_ok { p.subtle } else { p.muted };
                let text = if selected && folder_ok {
                    Style::new().fg(p.text).bg(bg).add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(on).bg(bg)
                };
                let mut spans = vec![
                    edge,
                    Span::styled(" ", Style::new().bg(bg)),
                    Span::styled(
                        ICON_FOLDER,
                        Style::new()
                            .fg(if selected { p.iris } else { p.muted })
                            .bg(bg),
                    ),
                    Span::styled("  ", Style::new().bg(bg)),
                ];
                let lead: usize = spans.iter().map(|s| str_width(&s.content)).sum();
                let room = width.saturating_sub(lead + 1);
                spans.extend(highlight(&folder.display, &hits, room, true, text, hl));
                f.render_widget(Line::from(spans), row);
            }
        }
    }

    /// The header over a host's folders: where they are listed from and how
    /// the listing went.
    fn draw_folders_row(&self, f: &mut Frame, row: Rect) {
        let p = &self.p;
        let (text, color) = match self.loads.get(self.listed()) {
            Some(Load::Ready(_)) => {
                let n = self
                    .rows
                    .iter()
                    .filter(|i| matches!(i, Item::Folder(_)))
                    .count();
                (
                    format!("{n} folder{}", if n == 1 { "" } else { "s" }),
                    p.muted,
                )
            }
            Some(Load::Failed(err)) => (err.clone(), p.love),
            _ => (
                format!("{} listing…", SPINNER[tick(self.started) % SPINNER.len()]),
                p.foam,
            ),
        };
        let spans = vec![
            Span::styled(" ", Style::new().bg(p.base)),
            Span::styled(ICON_FOLDER, Style::new().fg(p.subtle)),
            Span::styled(
                format!("  Folders on {}", self.listed()),
                Style::new().fg(p.text).add_modifier(Modifier::BOLD),
            ),
        ];
        f.render_widget(Line::from(spans), row);
        let mut right = vec![Span::styled(text, Style::new().fg(color))];
        right.push(Span::styled(" ", Style::new().bg(p.base)));
        f.render_widget(Line::from(right).right_aligned(), row);
    }

    /// A catalog project `item` under its group, with the query's hits.
    fn draw_project_row(
        &self,
        f: &mut Frame,
        row: Rect,
        item: &Item,
        selected: bool,
        last: bool,
        look: RowLook,
    ) {
        let Item::Project(index) = item else {
            return;
        };
        let RowLook { edge, bg, hl } = look;
        let p = &self.p;
        let width = usize::from(row.width);
        let project = &self.projects[*index];
        let (name_hits, path_hits) = self.hits.get(item).cloned().unwrap_or_default();
        let guide = if last { "╰─ " } else { "├─ " };
        let lead = 1 + 1 + 3 + 2;
        let path_room = (width.saturating_sub(lead) / 2).min(48);
        let path_w = str_width(&project.path).min(path_room);
        let name_room = width.saturating_sub(lead + path_w + 3);
        let name_style = if selected {
            Style::new().fg(p.text).bg(bg).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(p.text).bg(bg)
        };
        let mut spans = vec![
            edge,
            Span::styled(" ", Style::new().bg(bg)),
            Span::styled(guide, Style::new().fg(p.hl_med).bg(bg)),
            Span::styled(
                ICON_REPO,
                Style::new()
                    .fg(if selected { p.iris } else { p.muted })
                    .bg(bg),
            ),
            Span::styled(" ", Style::new().bg(bg)),
        ];
        spans.extend(highlight(
            &project.name,
            &name_hits,
            name_room,
            false,
            name_style,
            hl,
        ));
        f.render_widget(Line::from(spans), row);
        let path_style = Style::new()
            .fg(if selected { p.subtle } else { p.muted })
            .bg(bg);
        let mut path = highlight(&project.path, &path_hits, path_w, true, path_style, hl);
        path.push(Span::styled(" ", Style::new().bg(bg)));
        f.render_widget(Line::from(path).right_aligned(), row);
    }

    fn draw_form(&mut self, f: &mut Frame, area: Rect, prompt_rows: u16) {
        // The prompt leads, so Tab from the project reaches it first; the name
        // and work item follow it.
        let [card, _, name_row, item_row]: [Rect; 4] = Layout::vertical([
            Constraint::Length(prompt_rows + PROMPT_CHROME_ROWS),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);
        let p = self.p;
        let label_w = 11u16;
        let focused = self.focus == Focus::Name;

        // Name, with the branch and worktree it becomes.
        let mut spans = vec![
            Span::styled(if focused { "▎" } else { " " }, Style::new().fg(p.iris)),
            Span::styled(
                format!("{:<width$}", "Name", width = label_w as usize - 1),
                if focused {
                    Style::new().fg(p.iris).add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(p.subtle)
                },
            ),
        ];
        let name = self.name_text();
        if name.is_empty() && !focused {
            spans.push(Span::styled("—", Style::new().fg(p.muted)));
        }
        f.render_widget(Line::from(spans), name_row);
        // Right-aligned hint, shortened or dropped so it never covers the name.
        let room =
            (name_row.width as usize).saturating_sub(label_w as usize + str_width(&name) + 2);
        let (candidates, color) = match name_problem(&name) {
            Some(problem) if !name.is_empty() => (vec![format!("{problem} ")], p.love),
            Some(_) => match self.names_from() {
                Some(from) => (
                    vec![format!("named on ⏎ from the {from} "), "named on ⏎ ".into()],
                    p.muted,
                ),
                None => (vec![], p.muted),
            },
            None => {
                let (branch, slug) = branch_and_slug(&name);
                (
                    vec![
                        format!("{branch} · .worktrees/{slug} "),
                        format!("{branch} "),
                    ],
                    p.muted,
                )
            }
        };
        let hint = candidates.into_iter().find(|h| str_width(h) <= room);
        let hint_w = cells(hint.as_deref().map_or(0, str_width));
        if let Some(hint) = hint {
            f.render_widget(
                Line::from(Span::styled(hint, Style::new().fg(color))).right_aligned(),
                name_row,
            );
        }
        if focused || !name.is_empty() {
            let field = Rect {
                x: name_row.x + label_w,
                width: name_row.width.saturating_sub(label_w + hint_w),
                ..name_row
            };
            self.name.set_style(Style::new().fg(p.text).bg(p.base));
            self.name.set_placeholder_text("api-work, fix/login …");
            self.name
                .set_placeholder_style(Style::new().fg(p.muted).bg(p.base));
            f.render_widget(&self.name, field);
            if focused {
                place_cursor(f, &self.name, field);
            }
        }

        self.draw_item(f, item_row, label_w);
        if self.focus == Focus::Item && !self.item_menu.hits.is_empty() {
            // The menu opens right above the work item field.
            self.item_menu.card = Some((item_row, self.list_area));
        }
        self.draw_prompt(f, card, prompt_rows);
    }

    /// The optional work item, with what it starts as on the right.
    fn draw_item(&mut self, f: &mut Frame, row: Rect, label_w: u16) {
        let p = self.p;
        let focused = self.focus == Focus::Item;
        let mut spans = vec![
            Span::styled(if focused { "▎" } else { " " }, Style::new().fg(p.iris)),
            Span::styled(
                format!("{:<width$}", "Work item", width = label_w as usize - 1),
                if focused {
                    Style::new().fg(p.iris).add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(p.subtle)
                },
            ),
        ];
        let item = self.typed_item();
        if item.is_empty() && !focused {
            spans.push(Span::styled("—", Style::new().fg(p.muted)));
        }
        f.render_widget(Line::from(spans), row);
        // While focused: the open items being listed, how many there are to
        // choose from, or the highlighted one Enter would take. Otherwise,
        // the catalog says whether a project is work; a folder's profile is
        // settled at launch, so it gets no hint.
        let listing = self.listing_items();
        let open = self.item_list().map_or(0, |l| l.items.len());
        let (hints, color): (Vec<String>, Color) = match (self.chosen_item(), self.project()) {
            (Some(chosen), _) => (
                vec![
                    format!("⏎ #{} · ⇥ fill ", chosen.number),
                    format!("#{} ", chosen.number),
                ],
                p.foam,
            ),
            (None, _) if focused && listing => (
                vec![format!(
                    "{} listing open items ",
                    SPINNER[tick(self.started) % SPINNER.len()]
                )],
                p.muted,
            ),
            (None, _) if focused && item.is_empty() && open > 0 => {
                let noun = self.item_list().map_or("items", |l| l.noun);
                (vec![format!("↓ {open} open {noun} "), "↓ ".into()], p.muted)
            }
            (None, Some(project)) if !item.is_empty() && project.profile == "work" => {
                (vec![format!("→ {WORK_ITEM_COMMAND} ")], p.gold)
            }
            _ => (Vec::new(), p.muted),
        };
        let room = (row.width as usize).saturating_sub(label_w as usize + str_width(&item) + 2);
        let hint = hints.into_iter().find(|h| str_width(h) <= room);
        let hint_w = cells(hint.as_deref().map_or(0, str_width));
        if let Some(hint) = hint {
            f.render_widget(
                Line::from(Span::styled(hint, Style::new().fg(color))).right_aligned(),
                row,
            );
        }
        if focused || !item.is_empty() {
            let field = Rect {
                x: row.x + label_w,
                width: row.width.saturating_sub(label_w + hint_w),
                ..row
            };
            self.item.set_style(Style::new().fg(p.text).bg(p.base));
            self.item
                .set_placeholder_text("optional: #42, ENG-123, an issue or card URL");
            self.item
                .set_placeholder_style(Style::new().fg(p.muted).bg(p.base));
            f.render_widget(&self.item, field);
            if focused {
                place_cursor(f, &self.item, field);
            }
        }
    }

    /// A plain session's form: the folder it opens and the chips.
    fn draw_session(&self, f: &mut Frame, area: Rect) {
        let p = &self.p;
        let [line, chips] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
        let spans = match self.target_path() {
            Some(path) => vec![
                Span::styled("  Folder  ", Style::new().fg(p.subtle)),
                Span::styled(folders::home_relative(&path), Style::new().fg(p.text)),
                Span::styled(format!("  on {}", self.listed()), Style::new().fg(p.muted)),
            ],
            None => vec![Span::styled(
                "  Pick a project or folder",
                Style::new().fg(p.muted),
            )],
        };
        f.render_widget(Line::from(spans), line);
        let (left, right) = self.chips(p.base);
        let left_w: usize = left.iter().map(|s| str_width(&s.content)).sum();
        let right_w: usize = right.iter().map(|s| str_width(&s.content)).sum();
        if left_w + right_w < chips.width as usize {
            f.render_widget(Line::from(right).right_aligned(), chips);
        }
        f.render_widget(Line::from(left), chips);
    }

    /// The prompt box: a filled card with soft half-block edges holding the
    /// prompt, wrapped and scrolled so its end (the cursor) stays in view,
    /// over the chips row.
    fn draw_prompt(&mut self, f: &mut Frame, card: Rect, rows: u16) {
        let p = self.p;
        let focused = self.focus == Focus::Prompt;
        let fill = if focused { p.overlay } else { p.surface };
        self.prompt
            .set_placeholder("What should the agent do? (optional)");
        if focused {
            self.prompt_card = Some((card, f.area()));
        }
        let Some((chips_area, skip)) =
            self.prompt
                .draw(f, &p, card, rows, focused, self.corners.as_ref())
        else {
            return;
        };
        let (left, mut right) = self.chips(fill);
        if skip > 0 {
            right.insert(
                0,
                Span::styled(format!("+{skip} lines above  "), Style::new().fg(p.muted)),
            );
        }
        let left_w: usize = left.iter().map(|s| str_width(&s.content)).sum();
        let right_w: usize = right.iter().map(|s| str_width(&s.content)).sum();
        if left_w + right_w < chips_area.width as usize {
            f.render_widget(Line::from(right).right_aligned(), chips_area);
        }
        f.render_widget(Line::from(left), chips_area);
    }

    /// The chips for the form: where the work runs (changed defaults in gold),
    /// the harness (agent work), the project's profile, and the button on the
    /// right, all on `fill`.
    fn chips(&self, fill: Color) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
        let p = self.p;
        let chip_bg = p.hl_med;
        let changed = |on: bool| Style::new().fg(if on { p.gold } else { p.text });
        let host = self.chosen();
        let host_text = match (host, self.project()) {
            (AUTO, _) if self.kind == Kind::Session => self.here.clone(),
            (AUTO, Some(project)) => {
                let hosts: Vec<String> = pool(project).iter().map(|h| self.host_label(h)).collect();
                if hosts.len() == 1 {
                    hosts[0].clone()
                } else {
                    format!("least busy of {}", hosts.join(", "))
                }
            }
            (AUTO, None) => self.listed().to_string(),
            (h, _) => h.to_string(),
        };
        let mut left = pill(
            vec![
                Span::styled("\u{f048b} ", Style::new().fg(p.iris)),
                Span::styled(host_text, changed(host != AUTO)),
            ],
            chip_bg,
            fill,
        );
        if self.kind == Kind::Agent {
            left.push(Span::raw(" "));
            left.extend(pill(
                vec![Span::styled(
                    HARNESSES[self.harness].to_string(),
                    changed(self.harness != 0),
                )],
                chip_bg,
                fill,
            ));
        }
        if let Some(project) = self.project() {
            let color = if project.profile == "work" {
                p.gold
            } else {
                p.foam
            };
            left.push(Span::raw(" "));
            left.extend(pill(
                vec![Span::styled(
                    project.profile.clone(),
                    Style::new().fg(color),
                )],
                chip_bg,
                fill,
            ));
        }
        let ready = !self.pick().is_empty();
        let (send_fill, send_text) = if ready {
            (p.pine, p.text)
        } else {
            (chip_bg, p.muted)
        };
        let label = if self.kind == Kind::Agent {
            "⏎ spawn"
        } else {
            "⏎ open"
        };
        let right = pill(
            vec![Span::styled(
                label,
                Style::new().fg(send_text).add_modifier(Modifier::BOLD),
            )],
            send_fill,
            fill,
        );
        (left, right)
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        let p = &self.p;
        if let Some((msg, failed)) = &self.note {
            let spans = vec![
                Span::raw(" "),
                Span::styled(
                    msg.clone(),
                    Style::new().fg(if *failed { p.love } else { p.pine }),
                ),
            ];
            f.render_widget(Line::from(spans), area);
            return;
        }
        let ready = !self.pick().is_empty();
        let mut hints: Vec<(&str, &str, Color)> = Vec::new();
        if self.confirm_close {
            hints.push(("esc", "again to discard the prompt", p.love));
        } else {
            match self.kind {
                Kind::Agent => {
                    let next = match self.focus {
                        Focus::Project => "prompt",
                        Focus::Prompt => "name",
                        Focus::Name => "work item",
                        Focus::Item if !self.item_menu.hits.is_empty() => "fill item",
                        Focus::Item => "project",
                    };
                    hints.push(("⇥", next, p.subtle));
                    let spawn_label = if ready {
                        "spawn"
                    } else if !self.prompt.saved_ok {
                        "spawn (prompt not saved)"
                    } else if self.target_path().is_none() {
                        "spawn (pick a folder)"
                    } else {
                        "spawn (name it, or add a prompt)"
                    };
                    hints.push(("⏎", spawn_label, if ready { p.pine } else { p.muted }));
                    if self.focus == Focus::Prompt {
                        hints.push(("⌥⏎", "newline", p.subtle));
                        hints.push(("⌃G", "editor", p.subtle));
                    }
                    hints.push(("⌃T", "harness", p.subtle));
                    hints.push(("⌃S", "plain session", p.subtle));
                }
                Kind::Session => {
                    let open_label = if ready {
                        "open"
                    } else {
                        "open (pick a folder)"
                    };
                    hints.push(("⏎", open_label, if ready { p.pine } else { p.muted }));
                    hints.push(("⌃S", "agent work", p.subtle));
                }
            }
            hints.push(("⌃O", "host", p.subtle));
            let esc = if self.focus == Focus::Item && !self.item_menu.hits.is_empty() {
                "hide items"
            } else {
                "close"
            };
            hints.push(("esc", esc, p.subtle));
        }
        let (spans, _) = draw::hints(p, &hints, area.width as usize);
        f.render_widget(Line::from(spans), area);
    }
}

/// Whether `c` can be part of a work item: one ref, so no spaces.
fn item_char(c: char) -> bool {
    !c.is_whitespace() && !c.is_control()
}

/// Run the picker. `home` and `home_folders` are the config host's label and
/// zoxide folders when the float passes them in; standalone, the picker
/// lists this host's own.
pub fn run_picker(
    kind: Kind,
    home: String,
    home_folders: String,
    standalone: bool,
) -> io::Result<()> {
    prune_prompts();
    let mut app = App::new(
        kind,
        catalog().map_err(io::Error::other)?,
        home,
        home_folders,
        dev_hosts(),
    );
    app.standalone = standalone;
    let mut term = ratatui::init();
    // Raw mode is on and nothing has read stdin yet: ask about graphics now.
    app.cell = chrome::detect(Duration::from_millis(300));
    if let Some(cell) = app.cell {
        app.corners =
            chrome::Corners::new(cell, &[app.p.overlay, app.p.surface], &mut io::stdout()).ok();
    }
    // No mouse capture, so the terminal's own selection and copy work. Kitty
    // key reporting makes shift-Enter arrive as one crossterm can decode;
    // without it Rex sends xterm's `CSI 27;2;13~`, which crossterm drops.
    let _ = execute!(
        io::stdout(),
        EnableBracketedPaste,
        EnableFocusChange,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );
    let result = picker_loop(&mut term, &mut app);
    let _ = execute!(
        io::stdout(),
        PopKeyboardEnhancementFlags,
        DisableBracketedPaste,
        DisableFocusChange
    );
    if let Some(corners) = &app.corners {
        let _ = corners.delete(&mut io::stdout());
    }
    ratatui::restore();
    if result.is_ok() {
        // Closed from the keyboard, not by a launch (which closes the float
        // and may still need the prompt file): drop the prompt and the mode.
        app.prompt.discard();
        if app.standalone {
            // Leave the shell's title as it was before the picks.
            clear_title();
            if let Some(note) = &app.exit_note {
                eprintln!("switchyard: {note}");
            }
        } else {
            let _ = Command::new("rex")
                .args(["do", "client.mode.exit_all"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
    result
}

/// The picker run from a shell on the config host, which is where launches
/// run (the task registry lives there and it reaches every Rex host).
pub fn run_standalone(kind: Kind) -> io::Result<()> {
    if !cfg!(target_os = "macos") {
        return Err(io::Error::other(
            "spawning runs on the Mac; run switchyard new there, or press ctrl+cmd+n in Rex",
        ));
    }
    // Act as the config's action does: rex CLI calls name their own targets
    // rather than inheriting this shell's Rex session.
    for var in ["REX_SERVER", "REX_CLIENT", "REX_SESSION", "REX_BLOCK"] {
        // SAFETY: still single-threaded.
        unsafe { std::env::remove_var(var) };
    }
    // This host is the config host, so it lists its own folders.
    run_picker(kind, local_label(), String::new(), true)
}

/// Enter's pick from a shell: the chosen project or folder's session, or
/// agent work, opened here as the config's actions do. Ok(true) when it
/// launched and the picker should close; failures stay in the footer.
fn standalone_launch(term: &mut DefaultTerminal, app: &mut App) -> io::Result<bool> {
    if app.kind == Kind::Session {
        return standalone_open(term, app);
    }
    app.save_prompt();
    let Some(path) = app.target_path() else {
        app.note = Some(("pick a project or Git folder first".into(), true));
        return Ok(false);
    };
    let name = app.name_text();
    let names_from = app.names_from();
    if let Some(problem) = name_problem(&name).filter(|_| names_from.is_none()) {
        app.note = Some((problem.into(), true));
        return Ok(false);
    }
    let prompt_ref = match app.prompt.saved_ref() {
        Some("") => None,
        Some(r) => Some(r.to_string()),
        None => {
            app.note = Some(("the prompt couldn't be saved".into(), true));
            return Ok(false);
        }
    };
    let item = app.item_text();
    let host = app.agent_host().to_string();
    let label = folders::home_relative(&path);
    app.note = Some((
        match names_from {
            Some(from) => format!("Naming from the {from} and spawning in {label} on {host}…"),
            None => format!("Spawning {label}/{name} on {host}…"),
        },
        false,
    ));
    term.draw(|f| app.draw(f))?;
    match launch(
        &host,
        &path,
        &name,
        HARNESSES[app.harness],
        prompt_ref.as_deref(),
        Some(item.as_str()).filter(|i| !i.is_empty()),
        None,
    ) {
        // Launched: close even if the tab can't be shown, so a second Enter
        // doesn't spawn the work twice.
        Ok((host, session, window)) => {
            app.exit_note = select(&host, &session, &window).err();
            Ok(true)
        }
        Err(e) => {
            log(&format!("standalone launch of {label}/{name} failed: {e}"));
            app.note = Some((e, true));
            Ok(false)
        }
    }
}

/// Open the highlighted folder's session on the chosen host, as the config's
/// session action does. Ok(true) when it opened and the picker should close.
fn standalone_open(term: &mut DefaultTerminal, app: &mut App) -> io::Result<bool> {
    let Some(path) = app.target_path() else {
        app.note = Some(("pick a project or folder first".into(), true));
        return Ok(false);
    };
    let host = app.listed().to_string();
    app.note = Some((
        format!("Opening {} on {host}…", folders::home_relative(&path)),
        false,
    ));
    term.draw(|f| app.draw(f))?;
    match folders::open(&host, &path) {
        Ok(session) => {
            app.exit_note = select(&host, &session, "").err();
            Ok(true)
        }
        Err(e) => {
            log(&format!(
                "standalone session in {path} on {host} failed: {e}"
            ));
            app.note = Some((e, true));
            Ok(false)
        }
    }
}

fn picker_loop(term: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    loop {
        app.receive();
        app.sync_commands();
        app.sync_items();
        term.draw(|f| app.draw(f))?;
        app.report()?;
        let wait = if app.waiting() || app.listing_items() {
            Duration::from_millis(90)
        } else {
            Duration::from_millis(250)
        };
        if !event::poll(wait)? {
            continue;
        }
        match event::read()? {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                if !app.key(k) {
                    return Ok(());
                }
                match app.request.take() {
                    Some(Request::Launch) => {
                        if standalone_launch(term, app)? {
                            return Ok(());
                        }
                    }
                    Some(Request::Edit) => edit_prompt(term, app)?,
                    None => {}
                }
            }
            Event::Paste(text) => app.paste(&text),
            Event::FocusLost => app.away = true,
            Event::FocusGained => app.rearm_mode(),
            _ => {}
        }
    }
}

/// Edit the prompt in `$VISUAL` or `$EDITOR` (else nvim or vi), taking the
/// float over until it exits, then put the saved text back in the field
/// (ctrl-z undoes that). An editor that fails, such as nvim's `:cq`, keeps
/// the prompt as it was.
fn edit_prompt(term: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    let before = app.prompt.field.lines().join("\n");
    let standalone = app.standalone;
    let fills = [app.p.overlay, app.p.surface];
    let edited =
        prompt_box::edit_in_editor(term, &app.prompt, &mut app.corners, app.cell, fills, || {
            // The mode would take the editor's Enter as the action.
            if !standalone {
                let _ = Command::new("rex")
                    .args(["do", "client.mode.exit_all"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            let _ = execute!(io::stdout(), DisableFocusChange);
        });
    let _ = execute!(io::stdout(), EnableFocusChange);
    app.away = true;
    app.rearm_mode();
    if let Some(text) = edited?
        && text != before
    {
        app.prompt.clear();
        app.paste(&text);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let mut app = App::new(
            Kind::Agent,
            vec![Project {
                profile: "personal".into(),
                path: "~/catalog/repo".into(),
                clone_url: "unused".into(),
                name: "repo".into(),
                hosts: vec![],
                tracker: String::new(),
            }],
            "config-host".into(),
            String::new(),
            vec!["folder-host".into()],
        );
        // Supply deterministic folders; no prompts, tasks or tabs are written.
        app.here = "picker-host".into();
        app.loads.insert(
            "picker-host".into(),
            Load::Ready(folders::parse("/repos/local")),
        );
        app.loads.insert(
            "folder-host".into(),
            Load::Ready(folders::parse("/repos/remote")),
        );
        app.name.insert_str("folder-work");
        app.refilter();
        app
    }

    #[test]
    fn folder_pick_reports_path_and_its_host_under_auto() {
        let mut app = app();
        app.query = "local".into();
        app.refilter();
        assert_eq!(
            app.pick(),
            format!(
                "agent/{}/{}/{}/{}",
                hex("picker-host"),
                hex("/repos/local"),
                hex("folder-work"),
                HARNESSES[0]
            )
        );
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            app.request.is_none(),
            "float still dispatches Enter through Lua"
        );
        app.standalone = true;
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.request, Some(Request::Launch));
    }

    #[test]
    fn remote_folder_and_prompt_keep_the_selected_host() {
        let mut app = app();
        app.host = app.hosts.iter().position(|h| h == "folder-host").unwrap();
        app.query = "remote".into();
        app.refilter();
        app.prompt.field.insert_str("do the work");
        app.prompt.file_ref = "picker-host:test-prompt".into();
        assert_eq!(
            app.pick(),
            format!(
                "agent/{}/{}/{}/{}/{}",
                hex("folder-host"),
                hex("/repos/remote"),
                hex("folder-work"),
                HARNESSES[0],
                hex(&app.prompt.file_ref)
            )
        );
        app.prompt.saved_ok = false;
        assert_eq!(app.pick(), "");
    }

    #[test]
    fn work_item_follows_the_prompt_ref_which_may_be_empty() {
        let mut app = app();
        app.query = "local".into();
        app.refilter();
        let base = format!(
            "agent/{}/{}/{}/{}",
            hex("picker-host"),
            hex("/repos/local"),
            hex("folder-work"),
            HARNESSES[0]
        );
        app.focus = Focus::Item;
        app.paste(" #42 x");
        assert_eq!(app.item_text(), "#42x", "spaces never reach the item");
        app.item.clear();
        app.item.insert_str("#42");
        assert_eq!(app.pick(), format!("{base}//{}", hex("#42")));
        assert!(app.has_draft());
        app.prompt.field.insert_str("do the work");
        app.prompt.file_ref = "picker-host:test-prompt".into();
        assert_eq!(
            app.pick(),
            format!("{base}/{}/{}", hex(&app.prompt.file_ref), hex("#42"))
        );
        app.focus = Focus::Name;
        app.next_focus(true);
        assert!(app.focus == Focus::Item);
    }

    #[test]
    fn tab_runs_project_prompt_name_item() {
        let mut app = app();
        app.query = "local".into();
        app.refilter();
        assert!(app.focus == Focus::Project);
        app.next_focus(true);
        assert!(app.focus == Focus::Prompt, "one Tab reaches the prompt");
        app.next_focus(true);
        assert!(app.focus == Focus::Name);
        app.next_focus(true);
        assert!(app.focus == Focus::Item);
        app.next_focus(true);
        assert!(app.focus == Focus::Project);
        app.next_focus(false);
        assert!(app.focus == Focus::Item);
        app.next_focus(false);
        assert!(app.focus == Focus::Name);
        app.next_focus(false);
        assert!(app.focus == Focus::Prompt);
        app.next_focus(false);
        assert!(app.focus == Focus::Project);
    }

    #[test]
    fn blank_name_is_named_on_launch_from_the_item_or_prompt() {
        let mut app = app();
        app.query = "local".into();
        app.refilter();
        app.name.clear();
        assert_eq!(app.names_from(), None);
        assert!(app.pick().is_empty(), "nothing to name it from");
        let base = format!(
            "agent/{}/{}//{}",
            hex("picker-host"),
            hex("/repos/local"),
            HARNESSES[0]
        );
        app.prompt.field.insert_str("do the work");
        app.prompt.file_ref = "picker-host:test-prompt".into();
        assert_eq!(app.names_from(), Some("prompt"));
        assert_eq!(app.pick(), format!("{base}/{}", hex(&app.prompt.file_ref)));
        app.item.insert_str("#42");
        assert_eq!(app.names_from(), Some("work item"));
        assert_eq!(
            app.pick(),
            format!("{base}/{}/{}", hex(&app.prompt.file_ref), hex("#42"))
        );
        app.name.insert_str("typed");
        assert_eq!(app.names_from(), None, "a typed name wins");
    }

    fn key(app: &mut App, code: KeyCode) {
        app.key(KeyEvent::new(code, KeyModifiers::NONE));
        app.sync_items();
    }

    #[test]
    fn open_items_are_chosen_from_the_work_item_field() {
        let mut app = app();
        app.query = "repo".into();
        app.refilter();
        app.name.clear();
        // Listed already, so no CLI runs.
        let card = |n: u64, title: &str| OpenItem {
            number: n,
            title: title.into(),
            url: format!("https://app.fizzy.do/1/cards/{n}"),
            ..OpenItem::default()
        };
        app.open_items.insert(
            "~/catalog/repo".into(),
            Some(OpenItems {
                noun: "cards",
                items: vec![
                    card(1158, "Spawn from a card"),
                    card(1157, "Work item titles"),
                ],
            }),
        );
        app.focus = Focus::Item;
        app.sync_items();
        assert!(
            app.item_menu.hits.is_empty(),
            "the menu waits to be asked for"
        );
        assert!(
            app.pick().is_empty(),
            "nothing chosen, nothing to name it from"
        );
        key(&mut app, KeyCode::Down);
        assert_eq!(app.item_menu.hits.len(), 2);
        key(&mut app, KeyCode::Down);
        let base = format!(
            "agent/{}/{}//{}",
            hex(AUTO),
            hex("~/catalog/repo"),
            HARNESSES[0]
        );
        let url = "https://app.fizzy.do/1/cards/1157";
        assert_eq!(
            app.pick(),
            format!("{base}//{}", hex(url)),
            "Enter takes the highlighted item"
        );
        // Typing filters by title or number; Tab fills the field and closes.
        app.paste("spawn");
        app.sync_items();
        assert_eq!(
            app.item_menu.hits.iter().map(|h| h.0).collect::<Vec<_>>(),
            [0]
        );
        key(&mut app, KeyCode::Tab);
        assert_eq!(app.typed_item(), "https://app.fizzy.do/1/cards/1158");
        assert!(app.item_menu.hits.is_empty() && app.focus == Focus::Item);
        // A ref matching nothing is sent as typed.
        app.item.clear();
        app.paste("ENG-12");
        app.sync_items();
        assert_eq!(app.item_menu.hits, [] as [(usize, Vec<usize>); 0]);
        assert_eq!(app.item_text(), "ENG-12");
        // Escape hides the menu rather than closing; leaving the field closes it.
        app.item.clear();
        key(&mut app, KeyCode::Down);
        assert_ne!(app.item_menu.hits, [] as [(usize, Vec<usize>); 0]);
        assert!(app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        app.sync_items();
        assert_eq!(app.item_menu.hits, [] as [(usize, Vec<usize>); 0]);
        key(&mut app, KeyCode::Down);
        app.focus = Focus::Prompt;
        app.sync_items();
        app.focus = Focus::Item;
        app.sync_items();
        assert!(
            app.item_menu.hits.is_empty(),
            "tabbing back never reopens it"
        );
    }

    #[test]
    fn unlisted_items_leave_the_field_free_form() {
        let mut app = app();
        app.query = "repo".into();
        app.refilter();
        app.open_items.insert(
            "~/catalog/repo".into(),
            Some(OpenItems {
                noun: "issues",
                items: Vec::new(),
            }),
        );
        app.focus = Focus::Item;
        key(&mut app, KeyCode::Down);
        assert!(
            app.focus == Focus::Item,
            "the work item is the last field, so ↓ stays put"
        );
        key(&mut app, KeyCode::Up);
        assert!(app.focus == Focus::Name, "↑ still walks up the form");
    }

    #[test]
    fn catalog_pick_keeps_auto_and_folder_still_requires_name() {
        let mut app = app();
        app.query = "repo".into();
        app.refilter();
        assert_eq!(app.agent_host(), AUTO);
        assert!(app.pick().starts_with(&format!("agent/{}/", hex(AUTO))));
        app.query = "local".into();
        app.refilter();
        app.name.clear();
        assert_eq!(app.pick(), "");
        app.kind = Kind::Session;
        assert_eq!(
            app.pick(),
            format!("session/{}/{}", hex("picker-host"), hex("/repos/local"))
        );
    }
}
