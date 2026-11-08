use crate::apps::{self, App as InstalledApp};
use crate::scan::{self, FileItem};
use crate::size;
use crate::trash::{self, Batch, Entry};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::Sender;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Apps,
    Junk,
    Orphans,
    History,
}

pub const PAGES: [Page; 4] = [Page::Apps, Page::Junk, Page::Orphans, Page::History];

impl Page {
    pub fn title(self) -> &'static str {
        match self {
            Page::Apps => "Applications",
            Page::Junk => "System Junk",
            Page::Orphans => "Orphaned Files",
            Page::History => "Delete History",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AppSort {
    Name,
    Size,
    Installed,
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

pub enum Msg {
    Apps(Vec<InstalledApp>),
    Files(Target, u64, Vec<FileItem>),
}

pub enum Confirm {
    Trash(Target),
    Restore(usize),
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
            quit: false,
            tx,
        };
        app.list_apps();
        // The Junk and Orphans tabs show what they would free before they are opened.
        app.scan(Target::Junk);
        app.scan(Target::Orphans);
        app
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
        }
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
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Enter) {
                match confirm {
                    Confirm::Trash(target) => self.remove_selected(target),
                    Confirm::Restore(index) => self.start(trash::restore(&self.history[index])),
                }
            }
            return;
        }
        if self.help {
            self.help = false;
            return;
        }
        if self.editing_filter {
            let filter = match self.target() {
                Some(target) => &mut self.list_mut(target).filter,
                None => &mut self.filter,
            };
            match key.code {
                KeyCode::Char(c) => filter.push(c),
                KeyCode::Backspace => _ = filter.pop(),
                KeyCode::Esc => {
                    filter.clear();
                    self.editing_filter = false;
                }
                KeyCode::Enter | KeyCode::Down | KeyCode::Up => self.editing_filter = false,
                _ => {}
            }
            match self.target() {
                Some(target) => self.list_mut(target).cursor = 0,
                None => self.app_cursor = 0,
            }
            return;
        }
        self.status.clear();
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Tab => self.show(PAGES[(self.page as usize + 1) % PAGES.len()]),
            KeyCode::BackTab => self.show(PAGES[(self.page as usize + PAGES.len() - 1) % PAGES.len()]),
            KeyCode::Char(c @ '1'..='4') => self.show(PAGES[c as usize - '1' as usize]),
            KeyCode::Char('s') => {
                self.include_system = !self.include_system;
                self.status = format!("System files {}.", if self.include_system { "included" } else { "excluded" });
                self.rescan();
            }
            KeyCode::Char('R') => self.rescan(),
            _ => match self.target() {
                Some(target) => self.file_key(target, key.code),
                None if self.page == Page::Apps => self.apps_key(key.code),
                None => self.history_key(key.code),
            },
        }
    }

    fn rescan(&mut self) {
        match self.target() {
            Some(target) => self.scan(target),
            None if self.page == Page::Apps => self.list_apps(),
            None => self.history = trash::entries(),
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
                let next = match self.app_sort {
                    AppSort::Name => AppSort::Size,
                    AppSort::Size => AppSort::Installed,
                    AppSort::Installed => AppSort::Name,
                };
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
