use crate::apps::{self, App as InstalledApp};
use crate::brew::{self, Package};
use crate::scan::{self, FileItem};
use crate::size;
use crate::trash::{self, Batch, Entry};
use crate::update;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{SendError, Sender};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Apps,
    Junk,
    Orphans,
    History,
    Brew(View),
    Settings,
}

/// The Homebrew page's views, nested under it in the sidebar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Installed,
    Outdated,
    Search,
    Services,
    Taps,
    Maintenance,
}

/// Sidebar order, which tab walks.
pub const PAGES: [Page; 11] = [
    Page::Apps,
    Page::Junk,
    Page::Orphans,
    Page::History,
    Page::Brew(View::Installed),
    Page::Brew(View::Outdated),
    Page::Brew(View::Search),
    Page::Brew(View::Services),
    Page::Brew(View::Taps),
    Page::Brew(View::Maintenance),
    Page::Settings,
];

impl Page {
    pub fn title(self) -> &'static str {
        match self {
            Page::Apps => "Applications",
            Page::Junk => "System Junk",
            Page::Orphans => "Orphaned Files",
            Page::History => "Delete History",
            Page::Brew(View::Installed) => "Installed",
            Page::Brew(View::Outdated) => "Outdated",
            Page::Brew(View::Search) => "Search",
            Page::Brew(View::Services) => "Services",
            Page::Brew(View::Taps) => "Taps",
            Page::Brew(View::Maintenance) => "Maintenance",
            Page::Settings => "Settings",
        }
    }
}

pub const CHORES: [Chore; 5] = [Chore::UpgradeAll, Chore::Cleanup, Chore::Autoremove, Chore::Update, Chore::Doctor];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Chore {
    UpgradeAll,
    Cleanup,
    Autoremove,
    Update,
    Doctor,
}

impl Chore {
    pub fn args(self) -> &'static [&'static str] {
        match self {
            Chore::UpgradeAll => &["upgrade"],
            Chore::Cleanup => &["cleanup", "--prune=all"],
            Chore::Autoremove => &["autoremove"],
            Chore::Update => &["update"],
            Chore::Doctor => &["doctor"],
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Chore::UpgradeAll => "Upgrade everything",
            Chore::Cleanup => "Clean up",
            Chore::Autoremove => "Remove unused dependencies",
            Chore::Update => "Update Homebrew",
            Chore::Doctor => "Check for problems",
        }
    }

    pub fn about(self) -> &'static str {
        match self {
            Chore::UpgradeAll => "Upgrades every outdated formula and cask that is not pinned.",
            Chore::Cleanup => "Deletes old versions, downloads, caches and the dependencies nothing needs, for good.",
            Chore::Autoremove => "Uninstalls the dependencies no installed formula needs any more.",
            Chore::Update => "Fetches the newest formula and cask list. Changes nothing installed.",
            Chore::Doctor => "Lists problems with the Homebrew install and changes nothing. It ends as failed when it finds any.",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum AppSort {
    #[default]
    Name,
    Size,
    Installed,
}

impl AppSort {
    fn next(self) -> Self {
        match self {
            AppSort::Name => AppSort::Size,
            AppSort::Size => AppSort::Installed,
            AppSort::Installed => AppSort::Name,
        }
    }
}

/// The three file lists: an app's leftovers, junk and orphans.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Leftovers,
    Junk,
    Orphans,
}

/// A row of a file list: a group header over a run of items with one category and owner, or one item.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    Group(Range<usize>),
    Item(usize),
}

#[derive(Default)]
pub struct List {
    /// None until the first scan finishes. Sorted, so each group is one run.
    pub items: Option<Vec<FileItem>>,
    /// Index into `rows()`.
    pub cursor: usize,
    /// Groups shown open, by category and `system`.
    unfolded: HashSet<(&'static str, bool)>,
    /// Shows only items whose path contains it, their groups open.
    pub filter: String,
    pub scanning: bool,
    /// Bumped by every scan, so a slow earlier scan cannot overwrite a newer one.
    generation: u64,
}

impl List {
    pub fn selected(&self) -> impl Iterator<Item = &FileItem> {
        self.items.iter().flatten().filter(|item| item.selected)
    }

    pub fn selected_bytes(&self) -> u64 {
        self.selected().map(|item| item.size).sum()
    }

    /// Disk use of every item found, selected or not: (the app bundle, everything else).
    /// An item inside another item's folder is already in that folder's size.
    pub fn total_bytes(&self) -> (u64, u64) {
        let items = self.items.as_deref().unwrap_or_default();
        let paths: HashSet<&Path> = items.iter().map(|item| item.path.as_path()).collect();
        let (mut app, mut rest) = (0, 0);
        for item in items {
            if item.path.ancestors().skip(1).any(|ancestor| paths.contains(ancestor)) {
                continue;
            }
            match item.category {
                "Application" => app += item.size,
                _ => rest += item.size,
            }
        }
        (app, rest)
    }

    /// One header per group with a match, followed by the group's matching items when it is open.
    pub fn rows(&self) -> Vec<Row> {
        let items = self.items.as_deref().unwrap_or_default();
        let mut rows = Vec::new();
        let mut start = 0;
        while start < items.len() {
            let key = (items[start].category, items[start].system);
            let end = start + items[start..].iter().take_while(|item| (item.category, item.system) == key).count();
            let shown: Vec<usize> = (start..end).filter(|&index| matches(&self.filter, &items[index])).collect();
            if !shown.is_empty() {
                rows.push(Row::Group(start..end));
            }
            if !self.filter.is_empty() || self.unfolded.contains(&key) {
                rows.extend(shown.into_iter().map(Row::Item));
            }
            start = end;
        }
        rows
    }

    /// Opens the groups only partly selected: the ones that need a look before deleting.
    fn open_groups_to_review(&mut self) {
        let (mut with_selected, mut with_unselected) = (HashSet::new(), HashSet::new());
        for item in self.items.iter().flatten() {
            let groups = if item.selected { &mut with_selected } else { &mut with_unselected };
            groups.insert((item.category, item.system));
        }
        self.unfolded = with_selected.intersection(&with_unselected).copied().collect();
    }
}

pub fn matches(filter: &str, item: &FileItem) -> bool {
    filter.is_empty() || item.path.to_string_lossy().to_lowercase().contains(&filter.to_lowercase())
}

/// The deepest folder holding every item of a group.
pub fn folder(group: &[FileItem]) -> PathBuf {
    let mut folder = group[0].path.parent().unwrap_or(Path::new("/")).to_path_buf();
    for item in group {
        while (!item.path.starts_with(&folder) || item.path == folder) && folder.pop() {}
    }
    folder
}

/// What the Homebrew views show. A `None` list is still loading.
#[derive(Default)]
pub struct Brew {
    pub packages: Option<Vec<Package>>,
    pub services: Option<Vec<brew::Service>>,
    pub taps: Option<Vec<brew::Tap>>,
    pub results: Option<Vec<Package>>,
    pub maintenance: Option<brew::Maintenance>,
    pub searching: bool,
    pub measuring: bool,
    /// Why the installed packages could not be listed, such as brew missing.
    pub error: Option<String>,
    /// Filters Installed and Outdated by name.
    pub filter: String,
    pub query: String,
    pub new_tap: String,
    pub sort: AppSort,
    /// Picked packages, by kind and full name, so picks survive a reload. Only the picks among the rows shown count.
    pub picked: HashSet<(bool, String)>,
    pub cursors: [usize; 6],
    /// Bumped by every search, so a slow earlier search cannot overwrite a newer one.
    search_generation: u64,
    /// Bumped by every dry run and reload, so an estimate of the state before a brew command is dropped.
    measure_generation: u64,
}

/// brew commands to run in the terminal after the user confirms them.
pub struct Plan {
    pub title: String,
    pub commands: Vec<brew::Args>,
    pub note: String,
    /// Removes something: drawn in red.
    pub removes: bool,
}

pub enum Msg {
    Apps(Vec<InstalledApp>),
    Files(Target, u64, Vec<FileItem>),
    Status(String),
    Packages(Result<Vec<Package>, String>),
    /// Sizes measured after `Packages`, by package key.
    Sizes(Vec<Package>),
    Services(Result<Vec<brew::Service>, String>),
    Taps(Result<Vec<brew::Tap>, String>),
    Results(u64, Result<Vec<Package>, String>),
    Maintenance(u64, Result<brew::Maintenance, String>),
}

pub enum Confirm {
    Trash(Target),
    Restore(usize),
    Brew(Plan),
}

pub struct App {
    pub page: Page,
    /// None while the first listing runs.
    pub apps: Option<Vec<InstalledApp>>,
    pub app_cursor: usize,
    pub filter: String,
    pub editing_filter: bool,
    pub help: bool,
    pub app_sort: AppSort,
    /// The app whose leftovers are shown.
    pub opened: Option<InstalledApp>,
    pub leftovers: List,
    pub junk: List,
    pub orphans: List,
    pub history: Vec<Entry>,
    pub history_cursor: usize,
    pub confirm: Option<Confirm>,
    pub status: String,
    /// Also scan /Library, /etc and /var/log.
    pub include_system: bool,
    /// The home volume's size and free bytes.
    pub disk: Option<(u64, u64)>,
    /// Moves that hit a permission error; the main loop runs them through sudo, then calls `settle`.
    pub admin: Option<Batch>,
    pub brew: Brew,
    /// Confirmed brew commands; the main loop runs them in the terminal, then calls `brew_finished`.
    pub brew_run: Option<Vec<brew::Args>>,
    /// One of `update::CHANNELS`.
    pub update_channel: &'static str,
    pub quit: bool,
    tx: Sender<Msg>,
}

impl App {
    pub fn new(tx: Sender<Msg>) -> Self {
        let mut app = App {
            page: Page::Apps,
            apps: None,
            app_cursor: 0,
            filter: String::new(),
            editing_filter: false,
            help: false,
            app_sort: AppSort::Name,
            opened: None,
            leftovers: List::default(),
            junk: List::default(),
            orphans: List::default(),
            history: trash::entries(),
            history_cursor: 0,
            confirm: None,
            status: String::new(),
            include_system: false,
            disk: size::disk_space(),
            admin: None,
            brew: Brew::default(),
            brew_run: None,
            update_channel: update::channel(),
            quit: false,
            tx,
        };
        app.list_apps();
        // The Junk and Orphans tabs show what they would free before they are opened.
        app.scan(Target::Junk);
        app.scan(Target::Orphans);
        // So do the Homebrew entries: outdated, running services, taps.
        app.reload_brew();
        if update::enabled() {
            app.update_at_start();
        }
        app
    }

    /// The daily check: the status line hears from it only when it installed a release or failed.
    fn update_at_start(&self) {
        let channel = self.update_channel;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let status = match update::install_latest(channel) {
                Ok(Some(version)) => format!("Updated to v{version}. Restart cleaner to use it."),
                Ok(None) => return Ok(()),
                Err(error) => format!("Update failed: {error}"),
            };
            tx.send(Msg::Status(status))
        });
    }

    fn measure_brew(&mut self) {
        self.brew.measuring = true;
        self.brew.measure_generation += 1;
        let generation = self.brew.measure_generation;
        let tx = self.tx.clone();
        std::thread::spawn(move || tx.send(Msg::Maintenance(generation, brew::maintenance())));
    }

    fn search_brew(&mut self) {
        let query = self.brew.query.trim().to_owned();
        if query.is_empty() {
            return;
        }
        self.brew.search_generation += 1;
        self.brew.searching = true;
        self.brew.results = None;
        let generation = self.brew.search_generation;
        let tx = self.tx.clone();
        std::thread::spawn(move || tx.send(Msg::Results(generation, brew::search(&query))));
    }

    /// Runs a quick brew command that needs no terminal, such as a pin, and reloads after it.
    fn brew_quietly(&mut self, args: &[&str], done: String) {
        self.status = format!("brew {}...", args.join(" "));
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let status = brew::output(&args).map_or_else(|error| error, |_| done);
            list_brew(&tx, Some(status))
        });
    }

    /// After confirmed commands ran in the terminal: what changed is unknown, so everything reloads.
    pub fn brew_finished(&mut self, commands: &[brew::Args], succeeded: bool) {
        let ran = commands.iter().map(|args| format!("brew {}", args.join(" "))).collect::<Vec<_>>().join(", ");
        self.status = if succeeded { format!("Done: {ran}.") } else { format!("Failed: {ran}.") };
        self.brew.picked.clear();
        self.reload_brew();
        self.disk = size::disk_space();
    }

    /// Lists everything again and drops the Maintenance estimate, which a brew command may have changed.
    fn reload_brew(&mut self) {
        self.brew.error = None;
        self.brew.maintenance = None;
        self.brew.measuring = false;
        self.brew.measure_generation += 1;
        let tx = self.tx.clone();
        std::thread::spawn(move || list_brew(&tx, None));
        if self.page == Page::Brew(View::Maintenance) {
            self.measure_brew();
        }
    }

    fn list_apps(&self) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let mut apps = apps::installed();
            tx.send(Msg::Apps(apps.clone()))?;
            let paths: Vec<_> = apps.iter().map(|app| app.path.clone()).collect();
            for (app, (size, _)) in apps.iter_mut().zip(size::sizes(&paths)) {
                app.size = Some(size);
            }
            tx.send(Msg::Apps(apps))
        });
    }

    pub fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Apps(apps) => self.keep_cursor(|app| app.apps = Some(apps)),
            Msg::Files(target, generation, items) => {
                let list = self.list_mut(target);
                if list.generation == generation {
                    list.items = Some(items);
                    list.scanning = false;
                    list.cursor = 0;
                    list.open_groups_to_review();
                }
            }
            Msg::Status(status) => self.status = status,
            Msg::Packages(Ok(mut packages)) => {
                // Sizes carry over until the new list is measured, so a size sort does not reshuffle under the cursor.
                copy_sizes(&mut packages, self.brew.packages.as_deref().unwrap_or_default());
                self.brew.packages = Some(packages);
            }
            Msg::Packages(Err(error)) => {
                self.brew.packages = None;
                self.brew.error = Some(error);
            }
            // Only sizes: a list measured before a pin must not undo the pin mark.
            Msg::Sizes(measured) => {
                if let Some(packages) = &mut self.brew.packages {
                    copy_sizes(packages, &measured);
                }
            }
            Msg::Services(services) => self.brew.services = Some(services.unwrap_or_else(|error| self.failed(error))),
            Msg::Taps(taps) => self.brew.taps = Some(taps.unwrap_or_else(|error| self.failed(error))),
            Msg::Results(generation, results) if generation == self.brew.search_generation => {
                self.brew.searching = false;
                self.brew.cursors[View::Search as usize] = 0;
                self.brew.results = Some(results.unwrap_or_else(|error| self.failed(error)));
            }
            Msg::Maintenance(generation, maintenance) if generation == self.brew.measure_generation => {
                self.brew.measuring = false;
                match maintenance {
                    Ok(maintenance) => self.brew.maintenance = Some(maintenance),
                    Err(error) => self.status = error,
                }
            }
            Msg::Results(..) | Msg::Maintenance(..) => {}
        }
    }

    /// Shows a background brew error on the status line and leaves its list empty.
    /// Without brew every Homebrew view says so already, and Apps must not.
    fn failed<T>(&mut self, error: String) -> Vec<T> {
        if brew::program().is_some() {
            self.status = error;
        }
        Vec::new()
    }

    pub fn list(&self, target: Target) -> &List {
        match target {
            Target::Leftovers => &self.leftovers,
            Target::Junk => &self.junk,
            Target::Orphans => &self.orphans,
        }
    }

    fn list_mut(&mut self, target: Target) -> &mut List {
        match target {
            Target::Leftovers => &mut self.leftovers,
            Target::Junk => &mut self.junk,
            Target::Orphans => &mut self.orphans,
        }
    }

    /// The file list the main panel shows, if any.
    pub fn target(&self) -> Option<Target> {
        match self.page {
            Page::Apps if self.opened.is_some() => Some(Target::Leftovers),
            Page::Junk => Some(Target::Junk),
            Page::Orphans => Some(Target::Orphans),
            _ => None,
        }
    }

    pub fn visible_apps(&self) -> Vec<&InstalledApp> {
        let filter = self.filter.to_lowercase();
        let mut apps: Vec<_> = self.apps.iter().flatten().filter(|app| app.name.to_lowercase().contains(&filter)).collect();
        sort_apps(&mut apps, self.app_sort);
        apps
    }

    /// The search results, or the installed packages filtered and sorted: the ones you asked for first, then dependencies.
    pub fn package_rows(&self, view: View) -> Vec<&Package> {
        if view == View::Search {
            return self.brew.results.iter().flatten().collect();
        }
        let filter = self.brew.filter.to_lowercase();
        let outdated_only = view == View::Outdated;
        let mut packages: Vec<&Package> = self.brew.packages.iter().flatten().filter(|package| (!outdated_only || package.newer.is_some()) && package.name.to_lowercase().contains(&filter)).collect();
        match self.brew.sort {
            AppSort::Name => {}
            AppSort::Size => packages.sort_by_key(|package| std::cmp::Reverse(package.size)),
            AppSort::Installed => packages.sort_by_key(|package| std::cmp::Reverse(package.installed_at)),
        }
        packages.sort_by_key(|package| !package.on_request);
        packages
    }

    /// Rows of a Homebrew view, for the cursor to stay inside.
    fn brew_rows(&self, view: View) -> usize {
        match view {
            View::Installed | View::Outdated | View::Search => self.package_rows(view).len(),
            View::Services => self.brew.services.as_ref().map_or(0, Vec::len),
            View::Taps => self.brew.taps.as_ref().map_or(0, Vec::len),
            View::Maintenance => CHORES.len(),
        }
    }

    /// Whether a search result is installed, by the live list rather than the search's own snapshot.
    pub fn is_installed(&self, found: &Package) -> bool {
        self.brew.packages.iter().flatten().any(|package| package.key() == found.key())
    }

    pub fn is_picked(&self, package: &Package) -> bool {
        self.brew.picked.contains(&package.key())
    }

    /// The picked rows of a package view, or the one under the cursor when none is picked.
    fn targets(&self, view: View) -> Vec<&Package> {
        let rows = self.package_rows(view);
        let picked: Vec<&Package> = rows.iter().copied().filter(|package| self.is_picked(package)).collect();
        if !picked.is_empty() {
            return picked;
        }
        rows.get(self.brew_cursor(view)).copied().into_iter().collect()
    }

    pub fn package_at_cursor(&self, view: View) -> Option<&Package> {
        self.package_rows(view).get(self.brew_cursor(view)).copied()
    }

    /// The listing error a view shows in place of its rows: Installed and Outdated, or every view once brew is missing.
    pub fn brew_error(&self, view: View) -> Option<&str> {
        if matches!(view, View::Installed | View::Outdated) || brew::program().is_none() { self.brew.error.as_deref() } else { None }
    }

    /// Runs `change` and moves the cursor to wherever the app it was on ended up.
    fn keep_cursor(&mut self, change: impl FnOnce(&mut Self)) {
        let current = self.visible_apps().get(self.app_cursor).map(|app| app.path.clone());
        change(self);
        let position = self.visible_apps().iter().position(|app| Some(&app.path) == current.as_ref());
        self.app_cursor = position.unwrap_or(0);
    }

    fn scan(&mut self, target: Target) {
        let include_system = self.include_system;
        let opened = self.opened.clone();
        let installed = self.apps.clone().unwrap_or_default();
        let list = self.list_mut(target);
        list.generation += 1;
        list.scanning = true;
        let generation = list.generation;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let items = match (target, opened) {
                (Target::Leftovers, Some(app)) => scan::leftovers(&app, &installed, include_system),
                (Target::Leftovers, None) => vec![],
                (Target::Junk, _) => scan::junk(include_system),
                // Lists apps itself, so it does not wait for the Applications page.
                (Target::Orphans, _) => scan::orphans(&apps::installed(), include_system),
            };
            tx.send(Msg::Files(target, generation, items))
        });
    }

    fn show(&mut self, page: Page) {
        // Picks belong to the view they were made in: a search pick must not join an uninstall in Installed.
        if page != self.page {
            self.brew.picked.clear();
        }
        self.page = page;
        if let Some(target) = self.target()
            && self.list(target).items.is_none()
            && !self.list(target).scanning
        {
            self.scan(target);
        }
        if page == Page::History {
            self.history = trash::entries();
        }
        if page == Page::Brew(View::Maintenance) && self.brew.maintenance.is_none() && !self.brew.measuring {
            self.measure_brew();
        }
    }

    /// The text being typed: a file list's filter, the app or package filter, the search or a new tap.
    fn typed_text(&mut self) -> &mut String {
        match (self.page, self.target()) {
            (_, Some(target)) => &mut self.list_mut(target).filter,
            (Page::Brew(View::Search), _) => &mut self.brew.query,
            (Page::Brew(View::Taps), _) => &mut self.brew.new_tap,
            (Page::Brew(_), _) => &mut self.brew.filter,
            _ => &mut self.filter,
        }
    }

    pub fn key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if let Some(confirm) = self.confirm.take() {
            // enter also opens a brew popup, so a held or doubled enter must not run brew.
            if key.code == KeyCode::Char('y') || (key.code == KeyCode::Enter && !matches!(confirm, Confirm::Brew(_))) {
                match confirm {
                    Confirm::Trash(target) => self.remove_selected(target),
                    Confirm::Restore(index) => self.start(trash::restore(&self.history[index])),
                    Confirm::Brew(plan) => self.brew_run = Some(plan.commands),
                }
            }
            return;
        }
        if self.help {
            self.help = false;
            return;
        }
        if self.editing_filter {
            let text = self.typed_text();
            match key.code {
                KeyCode::Char(c) => text.push(c),
                KeyCode::Backspace => _ = text.pop(),
                KeyCode::Esc => {
                    text.clear();
                    self.editing_filter = false;
                }
                KeyCode::Enter | KeyCode::Down | KeyCode::Up => self.editing_filter = false,
                _ => {}
            }
            match (self.page, self.target()) {
                (_, Some(target)) => self.list_mut(target).cursor = 0,
                (Page::Brew(View::Search), _) if key.code == KeyCode::Enter => self.search_brew(),
                (Page::Brew(View::Taps), _) if key.code == KeyCode::Enter => self.add_tap(),
                (Page::Brew(view), _) => self.brew.cursors[view as usize] = 0,
                _ => self.app_cursor = 0,
            }
            return;
        }
        self.status.clear();
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Tab => self.show(PAGES[(self.page_index() + 1) % PAGES.len()]),
            KeyCode::BackTab => self.show(PAGES[(self.page_index() + PAGES.len() - 1) % PAGES.len()]),
            KeyCode::Char(c @ '1'..='5') => self.show(PAGES[c as usize - '1' as usize]),
            KeyCode::Char('6') => self.show(Page::Settings),
            KeyCode::Char('s') if !matches!(self.page, Page::Brew(_) | Page::Settings) => {
                self.include_system = !self.include_system;
                self.status = format!("System files {}.", if self.include_system { "included" } else { "excluded" });
                self.rescan();
            }
            KeyCode::Char('R') => self.rescan(),
            _ => match (self.page, self.target()) {
                (_, Some(target)) => self.file_key(target, key.code),
                (Page::Apps, _) => self.apps_key(key.code),
                // The view shows an error in place of its rows, so no key may act on the hidden rows.
                (Page::Brew(view), _) if self.brew_error(view).is_some() => {}
                (Page::Brew(View::Services), _) => self.service_key(key.code),
                (Page::Brew(View::Taps), _) => self.tap_key(key.code),
                (Page::Brew(View::Maintenance), _) => self.chore_key(key.code),
                (Page::Brew(view), _) => self.package_key(view, key.code),
                (Page::Settings, _) => self.settings_key(key.code),
                _ => self.history_key(key.code),
            },
        }
    }

    fn page_index(&self) -> usize {
        PAGES.iter().position(|page| *page == self.page).expect("every page is in PAGES")
    }

    fn rescan(&mut self) {
        match (self.page, self.target()) {
            (_, Some(target)) => self.scan(target),
            (Page::Apps, _) => self.list_apps(),
            (Page::Brew(_), _) => self.reload_brew(),
            _ => self.history = trash::entries(),
        }
        // Other lists scanned with the old setting rescan when next shown.
        for target in [Target::Junk, Target::Orphans] {
            if Some(target) != self.target() && !self.list(target).scanning {
                self.list_mut(target).items = None;
            }
        }
    }

    fn apps_key(&mut self, code: KeyCode) {
        let count = self.visible_apps().len();
        match code {
            KeyCode::Char('/') => self.editing_filter = true,
            KeyCode::Char('S') => {
                let next = self.app_sort.next();
                self.keep_cursor(|app| app.app_sort = next);
            }
            KeyCode::Esc => self.filter.clear(),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                let Some(app) = self.visible_apps().get(self.app_cursor).map(|app| (*app).clone()) else { return };
                self.opened = Some(app);
                self.leftovers.items = None;
                self.leftovers.filter.clear();
                self.scan(Target::Leftovers);
            }
            _ => self.app_cursor = step(self.app_cursor, count, code),
        }
    }

    fn file_key(&mut self, target: Target, code: KeyCode) {
        let leftovers = target == Target::Leftovers;
        let list = self.list_mut(target);
        let deletable = !list.scanning && list.selected().next().is_some();
        let rows = list.rows();
        // The group header at or above the cursor, and whether the cursor is on it.
        let header = rows.get(..=list.cursor).and_then(|above| above.iter().rposition(|row| matches!(row, Row::Group(_))));
        let on_header = header == Some(list.cursor);
        let open = header.is_some_and(|header| matches!(rows.get(header + 1), Some(Row::Item(_))));
        let back = match code {
            KeyCode::Esc | KeyCode::Backspace if !list.filter.is_empty() => {
                list.filter.clear();
                list.cursor = header.unwrap_or(0);
                return;
            }
            KeyCode::Esc | KeyCode::Backspace => true,
            KeyCode::Left | KeyCode::Char('h') => on_header && !open,
            _ => false,
        };
        if back {
            if leftovers {
                self.opened = None;
            }
            return;
        }
        let Some(items) = &mut list.items else { return };
        let fold = match code {
            KeyCode::Enter => true,
            KeyCode::Right | KeyCode::Char('l') => on_header && !open,
            KeyCode::Left | KeyCode::Char('h') => open,
            _ => false,
        };
        if fold {
            let Some(Row::Group(range)) = header.map(|header| &rows[header]) else { return };
            let key = (items[range.start].category, items[range.start].system);
            if !list.unfolded.remove(&key) {
                list.unfolded.insert(key);
            }
            list.cursor = header.unwrap_or(0);
            return;
        }
        match code {
            KeyCode::Char('/') => self.editing_filter = true,
            KeyCode::Char(' ') => {
                match rows.get(list.cursor) {
                    Some(Row::Item(index)) => items[*index].selected = !items[*index].selected,
                    Some(Row::Group(range)) => {
                        let shown: Vec<usize> = range.clone().filter(|&index| matches(&list.filter, &items[index])).collect();
                        let select = !shown.iter().all(|&index| items[index].selected);
                        for index in shown {
                            items[index].selected = select;
                        }
                    }
                    None => {}
                }
                list.cursor = step(list.cursor, rows.len(), KeyCode::Down);
            }
            KeyCode::Char('a') => {
                let select = !items.iter().filter(|item| item.auto_selectable && matches(&list.filter, item)).all(|item| item.selected);
                for item in items.iter_mut().filter(|item| matches(&list.filter, item)) {
                    item.selected = select && item.auto_selectable;
                }
            }
            KeyCode::Char('o') => {
                let path = match rows.get(list.cursor) {
                    Some(Row::Item(index)) => items[*index].path.clone(),
                    Some(Row::Group(range)) => folder(&items[range.clone()]),
                    None => return,
                };
                let _ = Command::new("open").arg("-R").arg(path).spawn();
            }
            KeyCode::Char('d') | KeyCode::Delete if deletable => self.confirm = Some(Confirm::Trash(target)),
            _ => list.cursor = step(list.cursor, rows.len(), code),
        }
    }

    fn history_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('u') if self.history.get(self.history_cursor).is_some_and(|entry| !trash::restorable(entry).is_empty()) => {
                self.confirm = Some(Confirm::Restore(self.history_cursor));
            }
            KeyCode::Char('u') => self.status = "Nothing in this entry can be restored.".into(),
            _ => self.history_cursor = step(self.history_cursor, self.history.len(), code),
        }
    }

    fn settings_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Enter | KeyCode::Char(' ') => self.switch_channel(),
            KeyCode::Char('u') => self.check_now(),
            _ => {}
        }
    }

    fn switch_channel(&mut self) {
        let current = update::CHANNELS.iter().position(|channel| *channel == self.update_channel).expect("update_channel is one of CHANNELS");
        let next = update::CHANNELS[(current + 1) % update::CHANNELS.len()];
        if let Err(error) = update::save_channel(next) {
            self.status = error;
            return;
        }
        self.update_channel = next;
        if next == "off" {
            self.status = "Channel: off. cleaner no longer checks for updates at start.".into();
            return;
        }
        self.check_now();
    }

    /// `off` only stops the check at start, so a check now with it follows stable releases.
    fn check_now(&mut self) {
        let channel = self.update_channel;
        if !update::enabled() {
            self.status = format!("Channel: {channel}. Only release builds update themselves.");
            return;
        }
        self.status = format!("Channel: {channel}. Checking...");
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let status = match update::install_now(channel, &|_| ()) {
                Ok(Some(version)) => format!("Updated to v{version}. Restart cleaner to use it."),
                Ok(None) => format!("cleaner v{} is up to date.", update::CURRENT),
                Err(error) => format!("Update failed: {error}"),
            };
            tx.send(Msg::Status(status))
        });
    }

    /// The cursor of a Homebrew view, inside its rows even after the list shrank.
    pub fn brew_cursor(&self, view: View) -> usize {
        self.brew.cursors[view as usize].min(self.brew_rows(view).saturating_sub(1))
    }

    fn step_brew(&mut self, view: View, code: KeyCode) {
        self.brew.cursors[view as usize] = step(self.brew_cursor(view), self.brew_rows(view), code);
    }

    fn package_key(&mut self, view: View, code: KeyCode) {
        let installed_view = view != View::Search;
        match code {
            KeyCode::Char('/') => self.editing_filter = true,
            KeyCode::Char(' ') => {
                let Some(key) = self.package_at_cursor(view).map(Package::key) else { return };
                if !self.brew.picked.remove(&key) {
                    self.brew.picked.insert(key);
                }
                self.step_brew(view, KeyCode::Down);
            }
            KeyCode::Char('a') => {
                let keys: Vec<(bool, String)> = self.package_rows(view).iter().map(|package| package.key()).collect();
                if keys.iter().all(|key| self.brew.picked.contains(key)) {
                    self.brew.picked.retain(|key| !keys.contains(key));
                } else {
                    self.brew.picked.extend(keys);
                }
            }
            KeyCode::Char('o') => {
                let homepage = self.package_at_cursor(view).map(|package| package.homepage.clone()).unwrap_or_default();
                self.open_web(&homepage);
            }
            KeyCode::Esc if installed_view && !self.brew.filter.is_empty() => self.brew.filter.clear(),
            KeyCode::Esc => self.brew.picked.clear(),
            KeyCode::Char('i') if !installed_view => self.install(),
            KeyCode::Char('u') if installed_view => self.upgrade(view),
            KeyCode::Char(c @ ('d' | 'z')) if installed_view => self.uninstall(view, c == 'z'),
            KeyCode::Char('p') if installed_view => self.pin(view),
            KeyCode::Char('S') if installed_view => self.brew.sort = self.brew.sort.next(),
            _ => self.step_brew(view, code),
        }
    }

    fn install(&mut self) {
        let targets: Vec<&Package> = self.targets(View::Search).into_iter().filter(|package| !self.is_installed(package)).collect();
        if targets.is_empty() {
            self.status = "Nothing to install.".into();
            return;
        }
        let title = format!("Install {}?", what(&targets));
        let commands = brew::per_kind(&["install"], &["install"], &targets);
        self.confirm = Some(Confirm::Brew(Plan { title, commands, note: "Formulae bring their dependencies; a cask may ask for your password.".into(), removes: false }));
    }

    fn upgrade(&mut self, view: View) {
        let targets: Vec<&Package> = self.targets(view).into_iter().filter(|package| package.newer.is_some() && !package.pinned).collect();
        if targets.is_empty() {
            self.status = "Nothing to upgrade: up to date or pinned.".into();
            return;
        }
        let title = format!("Upgrade {}?", what(&targets));
        let commands = brew::per_kind(&["upgrade"], &["upgrade"], &targets);
        self.confirm = Some(Confirm::Brew(Plan { title, commands, note: "A running app may need to quit first.".into(), removes: false }));
    }

    fn uninstall(&mut self, view: View, zap: bool) {
        let picked = self.targets(view);
        if picked.is_empty() {
            return;
        }
        let (targets, kept) = brew::removable(self.brew.packages.as_deref().unwrap_or_default(), picked);
        if targets.is_empty() {
            self.status = format!("Kept {}: pinned or needed by other packages.", names(&kept));
            return;
        }
        if zap && !targets.iter().any(|package| package.cask) {
            self.status = if kept.iter().any(|package| package.cask) {
                format!("Kept {}: pinned or needed by other packages.", names(&kept))
            } else {
                "Only casks leave files to zap; d uninstalls a formula.".into()
            };
            return;
        }
        let (verb, cask_args, note) = if zap {
            ("Zap", &["uninstall", "--zap"][..], "Zap also removes the settings, caches and logs the casks keep in ~/Library. brew deletes most of it for good.")
        } else {
            ("Uninstall", &["uninstall"][..], "brew deletes for good, not to the Trash, and also uninstalls the dependencies nothing else needs.")
        };
        let mut note = note.to_owned();
        if !kept.is_empty() {
            note += &format!(" Leaves out {}: pinned or needed by other packages.", names(&kept));
        }
        let plan = Plan { title: format!("{verb} {}?", what(&targets)), commands: brew::per_kind(&["uninstall"], cask_args, &targets), note, removes: true };
        self.confirm = Some(Confirm::Brew(plan));
    }

    fn pin(&mut self, view: View) {
        let Some(package) = self.package_at_cursor(view) else { return };
        let (verb, done) = if package.pinned { ("unpin", "Unpinned") } else { ("pin", "Pinned") };
        let (kind, name) = (if package.cask { "--cask" } else { "--formula" }, package.full_name.clone());
        self.brew_quietly(&[verb, kind, "--", &name], format!("{done} {name}."));
    }

    fn service_key(&mut self, code: KeyCode) {
        let cursor = self.brew_cursor(View::Services);
        // Only `none` is unloaded: `start` refuses a loaded service, even one that stopped or failed.
        let service = self.brew.services.iter().flatten().nth(cursor).map(|service| (service.name.clone(), service.status != "none"));
        match (code, service) {
            (KeyCode::Enter, Some((name, true))) => self.brew_quietly(&["services", "stop", "--", &name], format!("Stopped {name}.")),
            (KeyCode::Enter, Some((name, false))) => self.brew_quietly(&["services", "start", "--", &name], format!("Started {name}.")),
            (KeyCode::Char('r'), Some((name, _))) => self.brew_quietly(&["services", "restart", "--", &name], format!("Restarted {name}.")),
            _ => self.step_brew(View::Services, code),
        }
    }

    fn tap_key(&mut self, code: KeyCode) {
        let cursor = self.brew_cursor(View::Taps);
        let tap = self.brew.taps.iter().flatten().nth(cursor).map(|tap| (tap.name.clone(), tap.remote.clone()));
        match (code, tap) {
            (KeyCode::Char('a'), _) => self.editing_filter = true,
            (KeyCode::Char('d'), Some((name, _))) => {
                let commands = vec![["untap", "--", &name].map(str::to_owned).to_vec()];
                let plan = Plan { title: format!("Remove the tap {name}?"), commands, note: "brew refuses while a package from it is installed.".into(), removes: true };
                self.confirm = Some(Confirm::Brew(plan));
            }
            (KeyCode::Char('o'), Some((_, remote))) => self.open_web(&remote),
            _ => self.step_brew(View::Taps, code),
        }
    }

    /// Asks before tapping the `user/repo` typed in the Taps view.
    fn add_tap(&mut self) {
        let name = std::mem::take(&mut self.brew.new_tap).trim().to_owned();
        if name.is_empty() {
            return;
        }
        if !brew::is_tap_name(&name) {
            self.status = format!("\"{name}\" is not a tap name: type it as user/repo.");
            return;
        }
        let note = "A tap is someone else's repository: what you install from it runs their code.".into();
        self.confirm = Some(Confirm::Brew(Plan { title: format!("Add the tap {name}?"), commands: vec![vec!["tap".into(), name]], note, removes: false }));
    }

    fn chore_key(&mut self, code: KeyCode) {
        let chore = CHORES[self.brew_cursor(View::Maintenance)];
        match code {
            KeyCode::Enter => {
                let commands = vec![chore.args().iter().map(|arg| arg.to_string()).collect()];
                let removes = matches!(chore, Chore::Cleanup | Chore::Autoremove);
                self.confirm = Some(Confirm::Brew(Plan { title: format!("{}?", chore.title()), commands, note: chore.about().into(), removes }));
            }
            _ => self.step_brew(View::Maintenance, code),
        }
    }

    /// Package and tap pages come from third-party taps, so only web links open: never a file or an app.
    fn open_web(&mut self, url: &str) {
        if !url.starts_with("https://") && !url.starts_with("http://") {
            self.status = "No web page to open.".into();
            return;
        }
        let _ = Command::new("open").arg(url).spawn();
    }

    fn remove_selected(&mut self, target: Target) {
        let origin = match (target, &self.opened) {
            (Target::Leftovers, Some(app)) => app.name.clone(),
            (Target::Orphans, _) => "Orphaned Files".into(),
            _ => "System Junk".into(),
        };
        if let (Target::Leftovers, Some(app)) = (target, &self.opened) {
            stop_agents(self.leftovers.selected());
            if self.leftovers.selected().any(|item| item.path == app.path) {
                reset_privacy(app);
            }
        }
        let batch = trash::move_to_trash(self.list(target).selected(), &origin);
        self.start(batch);
    }

    fn start(&mut self, batch: Batch) {
        if batch.pending.is_empty() {
            self.settle(batch);
        } else {
            self.admin = Some(batch);
        }
    }

    pub fn settle(&mut self, batch: Batch) {
        self.status = trash::finish(batch);
        for target in [Target::Leftovers, Target::Junk, Target::Orphans] {
            let list = self.list_mut(target);
            if let Some(items) = &mut list.items {
                items.retain(|item| std::fs::symlink_metadata(&item.path).is_ok());
                list.cursor = list.cursor.min(list.rows().len().saturating_sub(1));
            }
        }
        if let Some(apps) = &mut self.apps {
            apps.retain(|app| !scan::gone(&app.path));
        }
        if self.opened.as_ref().is_some_and(|app| scan::gone(&app.path)) {
            self.opened = None;
        }
        self.disk = size::disk_space();
        self.history = trash::entries();
        self.history_cursor = self.history_cursor.min(self.history.len().saturating_sub(1));
    }
}

/// User apps first, then system apps; inside each group by `sort`, apps listed by name.
fn sort_apps(apps: &mut [&InstalledApp], sort: AppSort) {
    match sort {
        AppSort::Name => {}
        // Reverse puts unmeasured apps (None) last.
        AppSort::Size => apps.sort_by_key(|app| std::cmp::Reverse(app.size)),
        AppSort::Installed => apps.sort_by_key(|app| std::cmp::Reverse(app.installed)),
    }
    apps.sort_by_key(|app| app.is_system);
}

/// Lists packages, services and taps, then sends the status, so a key pressed once it shows acts on the new lists.
/// Sizes follow.
fn list_brew(tx: &Sender<Msg>, status: Option<String>) -> Result<(), SendError<Msg>> {
    let (packages, services, taps) = std::thread::scope(|scope| {
        let services = scope.spawn(brew::services);
        let taps = scope.spawn(brew::taps);
        (brew::installed(), services.join().expect("brew services thread panicked"), taps.join().expect("brew taps thread panicked"))
    });
    tx.send(Msg::Packages(packages.clone()))?;
    tx.send(Msg::Services(services))?;
    tx.send(Msg::Taps(taps))?;
    if let Some(status) = status {
        tx.send(Msg::Status(status))?;
    }
    let Ok(mut packages) = packages else { return Ok(()) };
    brew::measure(&mut packages);
    tx.send(Msg::Sizes(packages))
}

fn copy_sizes(packages: &mut [Package], from: &[Package]) {
    for package in packages {
        if let Some(size) = from.iter().find(|other| other.key() == package.key()).and_then(|other| other.size) {
            package.size = Some(size);
        }
    }
}

/// `ffmpeg`, or `3 packages`.
fn what(packages: &[&Package]) -> String {
    match packages {
        [package] => package.name.clone(),
        _ => trash::plural(packages.len(), "package"),
    }
}

/// Up to three names, then how many more.
fn names(packages: &[&Package]) -> String {
    let shown: Vec<&str> = packages.iter().take(3).map(|package| package.name.as_str()).collect();
    match packages.len() {
        0..=3 => shown.join(", "),
        count => format!("{} and {} more", shown.join(", "), count - 3),
    }
}

fn step(cursor: usize, count: usize, code: KeyCode) -> usize {
    let last = count.saturating_sub(1);
    match code {
        KeyCode::Down | KeyCode::Char('j') => (cursor + 1).min(last),
        KeyCode::Up | KeyCode::Char('k') => cursor.saturating_sub(1),
        KeyCode::PageDown => (cursor + 20).min(last),
        KeyCode::PageUp => cursor.saturating_sub(20),
        KeyCode::Home | KeyCode::Char('g') => 0,
        KeyCode::End | KeyCode::Char('G') => last,
        _ => cursor.min(last),
    }
}

/// Unloads the app's own launch agents before their plists go, so they stop running now.
fn stop_agents<'a>(items: impl Iterator<Item = &'a FileItem>) {
    let agents = scan::home().join("Library/LaunchAgents");
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    for item in items.filter(|item| item.path.parent() == Some(agents.as_path())) {
        if let Some(label) = scan::launchd_label(&item.path) {
            let _ = Command::new("/bin/launchctl").args(["bootout", &format!("gui/{uid}/{label}")]).output();
        }
    }
}

/// Clears the camera, microphone and other privacy grants the app held.
fn reset_privacy(app: &InstalledApp) {
    for id in app.bundle_id.iter().chain(&app.helper_ids) {
        let _ = Command::new("/usr/bin/tccutil").args(["reset", "All", id]).output();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_stays_inside_the_list() {
        assert_eq!(step(0, 3, KeyCode::Up), 0);
        assert_eq!(step(2, 3, KeyCode::Down), 2);
        assert_eq!(step(1, 0, KeyCode::Down), 0);
        assert_eq!(step(5, 30, KeyCode::PageDown), 25);
        assert_eq!(step(5, 3, KeyCode::Char('x')), 2);
    }

    #[test]
    fn total_counts_a_nested_item_once_and_the_bundle_apart() {
        let item = |path: &str, category, size| FileItem { path: path.into(), category, system: false, size, approximate: false, selected: false, auto_selectable: true, why: "" };
        let items = vec![
            item("/Applications/Zed.app", "Application", 100),
            item("/u/Library/Application Support/Zed", "Application Support", 50),
            item("/u/Library/Application Support/Zed/node", "Application Support", 20),
            item("/u/Library/Caches/dev.zed.Zed", "Caches", 7),
        ];
        let list = List { items: Some(items), ..List::default() };
        assert_eq!(list.total_bytes(), (100, 57));
    }

    #[test]
    fn rows_fold_runs_of_one_category_and_owner() {
        let item = |path: &str, category, system| FileItem { path: path.into(), category, system, size: 1, approximate: false, selected: false, auto_selectable: true, why: "" };
        let mut items = vec![
            item("/u/Library/Caches/a", "Caches", false),
            item("/u/Library/Caches/b/c", "Caches", false),
            item("/Library/Caches/a", "Caches", true),
            item("/u/Library/Developer/Xcode/DerivedData", "Developer", false),
            item("/u/Library/Developer/Xcode", "Developer", false),
        ];
        items[3].selected = true;
        assert_eq!(folder(&items[0..2]), PathBuf::from("/u/Library/Caches"));
        assert_eq!(folder(&items[3..5]), PathBuf::from("/u/Library/Developer"));
        let mut list = List { items: Some(items), ..List::default() };
        list.open_groups_to_review();
        assert_eq!(list.rows(), [Row::Group(0..2), Row::Group(2..3), Row::Group(3..5), Row::Item(3), Row::Item(4)]);
        list.unfolded = HashSet::from([("Caches", true)]);
        assert_eq!(list.rows(), [Row::Group(0..2), Row::Group(2..3), Row::Item(2), Row::Group(3..5)]);
        list.filter = "B/C".into();
        assert_eq!(list.rows(), [Row::Group(0..2), Row::Item(1)]);
    }

    #[test]
    fn apps_sort_inside_user_and_system_groups() {
        let app = |name: &str, is_system, size, installed| InstalledApp {
            path: format!("/Applications/{name}.app").into(),
            bundle_id: None,
            name: name.into(),
            bundle_name: None,
            executable: None,
            helper_ids: vec![],
            is_system,
            version: None,
            size,
            installed,
        };
        let all = [app("Books", true, Some(900), 5), app("Zed", false, Some(10), 3), app("Arc", false, None, 9), app("Blender", false, Some(500), 1)];
        let names = |sort| {
            let mut apps: Vec<_> = all.iter().collect();
            sort_apps(&mut apps, sort);
            apps.iter().map(|app| app.name.as_str()).collect::<Vec<_>>()
        };
        assert_eq!(names(AppSort::Name), ["Zed", "Arc", "Blender", "Books"]);
        assert_eq!(names(AppSort::Size), ["Blender", "Zed", "Arc", "Books"]);
        assert_eq!(names(AppSort::Installed), ["Arc", "Zed", "Blender", "Books"]);
    }
}
