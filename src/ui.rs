use crate::app::{self, App, AppSort, CHORES, Chore, Confirm, Page, Row, Target, View};
use crate::brew::{self, Package};
use crate::scan::{FileItem, format_bytes};
use crate::trash::{self, plural};
use crate::update;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, HighlightSpacing, List, ListItem, ListState, Padding, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const ACCENT: Color = Color::Green;
const DANGER: Color = Color::LightRed;
const DIM: Style = Style::new().fg(Color::DarkGray);
const KEY: Style = Style::new().fg(Color::Cyan);
const BOLD: Style = Style::new().add_modifier(Modifier::BOLD);
const SYSTEM: Style = Style::new().fg(Color::Yellow);
const BADGE: Style = Style::new().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD);
const CURSOR: Style = Style::new().bg(Color::Indexed(236));
/// Columns a `cursor_list` row loses: the cursor mark, a gap and the scrollbar.
const LIST_CHROME: u16 = 4;
/// Text on the cursor bar, which stays dark on light terminal themes too.
const CURSOR_TEXT: Color = Color::Indexed(255);
const SIDEBAR_WIDTH: u16 = 22;
/// The Homebrew detail pane, shown when the main panel is at least `DETAIL_FROM` wide.
const DETAIL_WIDTH: u16 = 40;
const DETAIL_FROM: u16 = 100;
const VERSION_WIDTH: usize = 20;

pub fn draw(frame: &mut Frame, app: &App) {
    let [body, bottom_rule, footer] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1), Constraint::Length(1)]).areas(frame.area());
    let [sidebar, sidebar_rule, main] = Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Length(1), Constraint::Fill(1)]).areas(body);
    draw_sidebar(frame, app, sidebar);
    vertical_rule(frame, sidebar_rule);
    match (app.page, app.target()) {
        (_, Some(target)) => draw_files(frame, app, target, main),
        (Page::Apps, _) => draw_apps(frame, app, main),
        (Page::Brew(view), _) => draw_brew(frame, app, view, main),
        (Page::Settings, _) => draw_settings(frame, app, main),
        _ => draw_history(frame, app, main),
    }
    rule(frame, bottom_rule);
    draw_footer(frame, app, footer);
    if let Some(confirm) = &app.confirm {
        draw_confirm(frame, app, confirm);
    }
    if app.help {
        draw_help(frame, app);
    }
}

/// The pages with what each holds, the Homebrew views nested under Homebrew, and the disk gauge at the bottom.
fn draw_sidebar(frame: &mut Frame, app: &App, area: Rect) {
    let [pages_area, disk_area] = Layout::vertical([Constraint::Fill(1), Constraint::Length(2)]).areas(area);
    let width = area.width as usize;
    let summary = |target: Target| {
        let list = app.list(target);
        match &list.items {
            _ if list.scanning => spinner().to_string(),
            // Orphans start with nothing selected: their count says more than "0 B".
            Some(items) if list.selected_bytes() == 0 => items.len().to_string(),
            Some(_) => format_bytes(list.selected_bytes()),
            None => String::new(),
        }
    };
    let pages = [
        (Page::Apps, "Apps", app.apps.as_ref().map_or(spinner().to_string(), |apps| apps.len().to_string())),
        (Page::Junk, "Junk", summary(Target::Junk)),
        (Page::Orphans, "Orphans", summary(Target::Orphans)),
        (Page::History, "History", app.history.len().to_string()),
    ];
    let mut lines = vec![Line::styled(" Cleaner", BOLD), Line::raw("")];
    for (index, (page, name, count)) in pages.into_iter().enumerate() {
        let (style, count_style) = if page == app.page { (BADGE, BADGE) } else { (Style::new(), DIM) };
        lines.push(sidebar_row(&format!("  {} {name}", index + 1), &count, width, style, count_style));
    }
    let style = if matches!(app.page, Page::Brew(_)) { BADGE } else { Style::new() };
    lines.push(sidebar_row("  5 Homebrew", "", width, style, style));
    let brew = &app.brew;
    let count = |count: Option<usize>| count.map(|count| count.to_string()).unwrap_or_default();
    let outdated = brew.packages.as_ref().map(|packages| packages.iter().filter(|package| package.newer.is_some()).count());
    let installed = if brew.packages.is_none() && brew.error.is_none() { spinner().to_string() } else { count(brew.packages.as_ref().map(Vec::len)) };
    let views = [
        (View::Installed, installed),
        (View::Outdated, count(outdated.filter(|outdated| *outdated > 0))),
        (View::Search, count(brew.results.as_ref().map(Vec::len))),
        (View::Services, brew.services.as_ref().map(|services| format!("{}/{}", services.iter().filter(|service| service.status == "started").count(), services.len())).unwrap_or_default()),
        (View::Taps, count(brew.taps.as_ref().map(Vec::len))),
        (View::Maintenance, brew.maintenance.as_ref().and_then(|maintenance| maintenance.cleanup.clone()).unwrap_or_default()),
    ];
    for (view, count) in views {
        let count_style = if view == View::Outdated { Style::new().fg(ACCENT) } else { DIM };
        let page = Page::Brew(view);
        let (marker, style) = if page == app.page { ("▌ ", Style::new().fg(ACCENT)) } else { ("  ", Style::new()) };
        lines.push(sidebar_row(&format!("  {marker}{}", page.title()), &count, width, style, count_style));
    }
    let style = if app.page == Page::Settings { BADGE } else { Style::new() };
    lines.push(sidebar_row("  6 Settings", "", width, style, style));
    frame.render_widget(Paragraph::new(lines), pages_area);
    let Some((total, free)) = app.disk.filter(|(total, _)| *total > 0) else { return };
    let used = 1.0 - free as f64 / total as f64;
    let gauge_width = width.saturating_sub(7);
    let filled = ((used * gauge_width as f64).round() as usize).min(gauge_width);
    let color = if used > 0.9 { DANGER } else { ACCENT };
    let gauge = Line::from(vec![Span::styled(" disk ", DIM), Span::styled("━".repeat(filled), Style::new().fg(color)), Span::styled("━".repeat(gauge_width - filled), DIM)]);
    frame.render_widget(Paragraph::new(vec![gauge, Line::styled(format!(" {} free", format_bytes(free)), DIM)]), disk_area);
}

/// `  2 Junk      30 GB `: the label on the left, its count against the right edge.
fn sidebar_row(label: &str, count: &str, width: usize, style: Style, count_style: Style) -> Line<'static> {
    let label = truncate(label, width.saturating_sub(count.chars().count() + 2));
    let gap = width.saturating_sub(label.chars().count() + count.chars().count() + 1);
    Line::from(vec![Span::styled(label, style), Span::styled(" ".repeat(gap), style), Span::styled(format!("{count} "), count_style)])
}

fn sort_label(sort: AppSort) -> &'static str {
    match sort {
        AppSort::Name => "name",
        AppSort::Size => "size ▾",
        AppSort::Installed => "install date ▾",
    }
}

fn draw_apps(frame: &mut Frame, app: &App, area: Rect) {
    let [head, gap, columns, list_area] = Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let apps = app.visible_apps();
    let user_count = apps.iter().filter(|installed| !installed.is_system).count();
    let left = vec![Span::styled(" Applications", BOLD), Span::styled(format!("  {user_count} user · {} system", apps.len() - user_count), DIM)];
    spread(frame, head, left, vec![Span::styled("sort ", DIM), Span::styled(sort_label(app.app_sort), KEY), Span::raw("   "), filter_span(app, &app.filter)]);
    rule(frame, gap);
    if app.apps.is_none() {
        placeholder(frame, list_area, &format!("{} Looking for apps...", spinner()));
        return;
    }
    if apps.is_empty() {
        placeholder(frame, list_area, "No app name matches the filter.");
        return;
    }
    let width = list_area.width.saturating_sub(LIST_CHROME) as usize;
    let header = app_row(Span::raw("NAME"), ["VERSION", "INSTALLED", "SIZE", "SHARE"], None, width);
    frame.render_widget(Paragraph::new(header.patch_style(DIM)), Rect { x: columns.x + 2, width: columns.width.saturating_sub(2), ..columns });
    let largest = apps.iter().filter_map(|installed| installed.size).max().unwrap_or(0).max(1);
    let total = apps.iter().filter_map(|installed| installed.size).sum::<u64>().max(1);
    let rows: Vec<ListItem> = apps
        .iter()
        .enumerate()
        .map(|(index, installed)| {
            let name = Span::styled(installed.name.clone(), if installed.is_system { DIM } else { Style::new() });
            let version = installed.version.clone().unwrap_or_default();
            let date: String = trash::local_time(installed.installed, "").chars().take(10).collect();
            let line = match installed.size {
                Some(size) => {
                    let share = format!("{:.1}%", size as f64 * 100.0 / total as f64);
                    app_row(name, [&version, &date, &format_bytes(size), &share], Some(size as f64 / largest as f64), width)
                }
                None => app_row(name, [&version, &date, "…", ""], None, width),
            };
            ListItem::new(if index == app.app_cursor { on_cursor(line) } else { line })
        })
        .collect();
    cursor_list(frame, list_area, rows, app.app_cursor);
}

/// Name, version, install date, size, share bar and percent; narrow terminals drop the date and percent.
fn app_row(name: Span<'static>, [version, installed, size, share]: [&str; 4], fraction: Option<f64>, width: usize) -> Line<'static> {
    let wide = width >= 96;
    let bar_width = if wide { 20 } else { 10 };
    let fixed = 18 + 9 + 3 + bar_width + if wide { 14 + 7 } else { 0 };
    let name_width = width.saturating_sub(fixed).max(1);
    let mut spans = vec![Span::styled(pad(&name.content, name_width), name.style), Span::styled(pad(version, 18), DIM)];
    if wide {
        spans.push(Span::styled(pad(installed, 14), DIM));
    }
    spans.extend([Span::raw(format!("{:>9}", truncate(size, 9))), Span::raw("   "), Span::styled(bar(fraction.unwrap_or(0.0), bar_width), Style::new().fg(ACCENT))]);
    if wide {
        spans.push(Span::styled(format!("{:>7}", truncate(share, 7)), DIM));
    }
    Line::from(spans)
}

fn draw_files(frame: &mut Frame, app: &App, target: Target, area: Rect) {
    let [head, gap, list_area, rule_area, detail_area, _, action_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1), Constraint::Length(1), Constraint::Length(1), Constraint::Length(1)]).areas(area);
    let list = app.list(target);
    let items = list.items.as_deref().unwrap_or_default();
    let left = match (target, &app.opened) {
        (Target::Leftovers, Some(opened)) => {
            let about: Vec<&str> = opened.version.iter().chain(&opened.bundle_id).map(String::as_str).collect();
            vec![Span::styled(" Apps › ", DIM), Span::styled(opened.name.clone(), BOLD), Span::styled(format!(" {}", about.join(" · ")), DIM)]
        }
        _ => vec![Span::styled(format!(" {}", app.page.title()), BOLD)],
    };
    let mut right = match target {
        _ if list.scanning => vec![Span::styled("scanning   ", DIM)],
        Target::Leftovers => app_total(list),
        _ => vec![Span::styled(format!("{} found   ", items.len()), DIM)],
    };
    right.push(filter_span(app, &list.filter));
    spread(frame, head, left, right);
    rule(frame, gap);
    rule(frame, rule_area);
    if list.scanning {
        placeholder(frame, list_area, &format!("{} Scanning...", spinner()));
        return;
    }
    let rows = list.rows();
    if rows.is_empty() {
        placeholder(frame, list_area, if items.is_empty() { "Nothing found." } else { "No path matches the filter." });
        return;
    }
    let width = list_area.width.saturating_sub(LIST_CHROME) as usize;
    let bar_width = if width >= 96 { 24 } else { 10 };
    let category_width = items.iter().map(|item| item.category.len()).max().unwrap_or(0).min(width / 4);
    // One scale for every bar: the biggest group.
    let mut largest = 1;
    for row in &rows {
        if let Row::Group(range) = row {
            largest = largest.max(items[range.clone()].iter().map(|item| item.size).sum());
        }
    }
    let cursor = list.cursor.min(rows.len() - 1);
    let mut lines = Vec::new();
    let mut folder = PathBuf::new();
    for (index, row) in rows.iter().enumerate() {
        let line = match row {
            Row::Group(range) => {
                folder = app::folder(&items[range.clone()]);
                let open = matches!(rows.get(index + 1), Some(Row::Item(_)));
                group_row(&items[range.clone()], &folder, open, category_width, largest, bar_width, width)
            }
            Row::Item(item) => file_row(&items[*item], &folder, largest, bar_width, width),
        };
        lines.push(ListItem::new(if index == cursor { on_cursor(line) } else { line }));
    }
    frame.render_widget(Paragraph::new(detail_line(items, &rows[cursor], detail_area.width as usize)), detail_area);
    cursor_list(frame, list_area, lines, cursor);
    let (left, right) = action_bar(list, target);
    spread(frame, action_area, left, right);
}

/// `~/Library/Caches/pro.betteraudio   matched by bundle id · 37 KB · modified 2026-11-01 14:30`
fn detail_line(items: &[FileItem], row: &Row, width: usize) -> Line<'static> {
    let (path, about) = match row {
        Row::Group(range) => {
            let group = &items[range.clone()];
            (app::folder(group), format!("   {} · {}", plural(group.len(), "item"), format_bytes(group.iter().map(|item| item.size).sum())))
        }
        Row::Item(index) => {
            let item = &items[*index];
            let mut about = vec![format_bytes(item.size)];
            if !item.why.is_empty() {
                about.insert(0, format!("matched by {}", item.why));
            }
            if let Some(modified) = std::fs::symlink_metadata(&item.path).ok().and_then(|meta| meta.modified().ok()) {
                let secs = modified.duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs() as i64);
                about.push(format!("modified {}", trash::local_time(secs, "")));
            }
            (item.path.clone(), format!("   {}", about.join(" · ")))
        }
    };
    let about = truncate(&about, width.saturating_sub(12));
    let path = truncate_left(&tilde(&path.to_string_lossy()), width.saturating_sub(about.chars().count() + 1));
    let (parent, name) = path.rsplit_once('/').map_or((String::new(), path.clone()), |(parent, name)| (format!("{parent}/"), name.to_owned()));
    Line::from(vec![Span::styled(format!(" {parent}"), DIM), Span::raw(name), Span::styled(about, DIM)])
}

/// `takes 1.7 GB · app 56 MB + 204 leftovers, 1.6 GB`: everything found for the app, selected or not.
fn app_total(list: &app::List) -> Vec<Span<'static>> {
    let items = list.items.as_deref().unwrap_or_default();
    let (bundle, rest) = list.total_bytes();
    let approximate = if items.iter().any(|item| item.approximate) { "~" } else { "" };
    let leftovers = items.iter().filter(|item| item.category != "Application").count();
    let app_part = if leftovers < items.len() { format!("app {} + ", format_bytes(bundle)) } else { String::new() };
    vec![
        Span::styled("takes ", DIM),
        Span::styled(format!("{approximate}{}", format_bytes(bundle + rest)), BOLD),
        Span::styled(format!(" · {app_part}{}, {}   ", plural(leftovers, "leftover"), format_bytes(rest)), DIM),
    ]
}

/// ` 15 selected  1.5 GB · 2 system files may need your password` and the key that removes them.
fn action_bar(list: &app::List, target: Target) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let count = list.selected().count();
    if count == 0 {
        return (vec![Span::styled(" Nothing selected. space picks an item, a picks every safe one.", DIM)], vec![]);
    }
    let mut left = vec![Span::styled(format!(" {count} selected "), BADGE), Span::styled(format!(" {} ", format_bytes(list.selected_bytes())), BOLD)];
    let system = list.selected().filter(|item| item.system).count();
    if system > 0 {
        left.push(Span::styled(format!("· {} may need your password", plural(system, "system file")), SYSTEM));
    }
    let verb = if target == Target::Leftovers { " uninstall " } else { " move to Trash " };
    let right = vec![Span::styled(" d ", Style::new().fg(Color::Black).bg(DANGER).add_modifier(Modifier::BOLD)), Span::styled(verb, Style::new().fg(DANGER).add_modifier(Modifier::BOLD))];
    (left, right)
}

/// `▾ [-] Caches   ~/Library/Caches     4 of 5   1.6 GB  ██████████`
fn group_row(group: &[FileItem], folder: &Path, open: bool, category_width: usize, largest: u64, bar_width: usize, width: usize) -> Line<'static> {
    let selected = group.iter().filter(|item| item.selected).count();
    let check = match selected {
        0 => Span::raw("[ ] "),
        _ if selected == group.len() => Span::styled("[x] ", Style::new().fg(ACCENT)),
        _ => Span::styled("[-] ", Style::new().fg(ACCENT)),
    };
    let bytes: u64 = group.iter().map(|item| item.size).sum();
    let approximate = if group.iter().any(|item| item.approximate) { "~" } else { "" };
    let right = format!(" {:>10}{:>9}  ", format!("{selected} of {}", group.len()), format!("{approximate}{}", format_bytes(bytes)));
    let folder_width = width.saturating_sub(6 + category_width + 2 + right.chars().count() + bar_width);
    let folder_style = if group[0].system { SYSTEM } else { DIM };
    Line::from(vec![
        Span::styled(if open { "▾ " } else { "▸ " }, DIM),
        check,
        Span::styled(format!("{}  ", pad(group[0].category, category_width)), BOLD),
        Span::styled(pad(&truncate_left(&tilde(&folder.to_string_lossy()), folder_width), folder_width), folder_style),
        Span::styled(right, DIM),
        Span::styled(bar(bytes as f64 / largest as f64, bar_width), Style::new().fg(ACCENT)),
    ])
}

/// `  [x] com.tinycast.app/extensions/xcode     3.3 MB  █`, the path inside the group's folder.
fn file_row(item: &FileItem, folder: &Path, largest: u64, bar_width: usize, width: usize) -> Line<'static> {
    let check = if item.selected { Span::styled("[x] ", Style::new().fg(ACCENT)) } else { Span::raw("[ ] ") };
    let size = format!("{:>9}  ", format!("{}{}", if item.approximate { "~" } else { "" }, format_bytes(item.size)));
    let inside = item.path.strip_prefix(folder).expect("a group's folder holds its items");
    let path_width = width.saturating_sub(6 + size.chars().count() + bar_width);
    let path_style = if item.system { SYSTEM } else { Style::new() };
    Line::from(vec![
        Span::raw("  "),
        check,
        Span::styled(pad(&truncate_left(&inside.to_string_lossy(), path_width), path_width), path_style),
        Span::raw(size),
        Span::styled(bar(item.size as f64 / largest as f64, bar_width), Style::new().fg(ACCENT)),
    ])
}

/// A Homebrew view: its list, and the detail pane on the right when the terminal is wide enough.
fn draw_brew(frame: &mut Frame, app: &App, view: View, area: Rect) {
    let pane = area.width >= DETAIL_FROM;
    let pane_width = if pane { DETAIL_WIDTH } else { 0 };
    let [main, pane_rule, pane_area] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(pane as u16), Constraint::Length(pane_width)]).areas(area);
    let [head, gap, columns, list_area] = Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Length(1), Constraint::Fill(1)]).areas(main);
    let (left, right) = brew_head(app, view);
    spread(frame, head, left, right);
    rule(frame, gap);
    if pane {
        vertical_rule(frame, pane_rule);
        let detail = brew_detail(app, view, pane_area.width.saturating_sub(2) as usize);
        frame.render_widget(Paragraph::new(detail).wrap(Wrap { trim: false }).block(Block::new().padding(Padding::horizontal(1))), pane_area);
    }
    let width = list_area.width.saturating_sub(LIST_CHROME) as usize;
    let table = match (app.brew_error(view), view) {
        (Some(error), _) => Err(error.to_owned()),
        (_, View::Services) => service_lines(app, width),
        (_, View::Taps) => tap_lines(app, width),
        (_, View::Maintenance) => Ok(chore_lines(app, width)),
        (_, _) => package_lines(app, view, width),
    };
    let (header, lines) = match table {
        Ok(table) => table,
        Err(text) => return placeholder(frame, list_area, &text),
    };
    frame.render_widget(Paragraph::new(header.patch_style(DIM)), Rect { x: columns.x + 2, width: columns.width.saturating_sub(2), ..columns });
    let cursor = app.brew_cursor(view);
    let rows = lines.into_iter().enumerate().map(|(index, line)| ListItem::new(if index == cursor { on_cursor(line) } else { line })).collect();
    cursor_list(frame, list_area, rows, cursor);
}

/// ` Homebrew › Installed  199 packages · 4.1 GB` and, on the right, the picks, sort and filter or search.
fn brew_head(app: &App, view: View) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let brew = &app.brew;
    let summary = match view {
        View::Installed => brew.packages.as_ref().map(|packages| {
            let measured: u64 = packages.iter().filter_map(|package| package.size).sum();
            let size = if measured > 0 { format!(" · {}", format_bytes(measured)) } else { String::new() };
            format!("{}{size}", plural(packages.len(), "package"))
        }),
        View::Outdated => brew.packages.as_ref().map(|packages| format!("{} outdated", packages.iter().filter(|package| package.newer.is_some()).count())),
        View::Search => brew.results.as_ref().map(|results| format!("{} found", results.len())),
        View::Services => brew.services.as_ref().map(|services| format!("{} running of {}", services.iter().filter(|service| service.status == "started").count(), services.len())),
        View::Taps => brew.taps.as_ref().map(|taps| plural(taps.len(), "tap")),
        View::Maintenance => None,
    };
    let left = vec![Span::styled(format!(" Homebrew › {}", Page::Brew(view).title()), BOLD), Span::styled(format!("  {}", summary.unwrap_or_default()), DIM)];
    let mut right = Vec::new();
    if matches!(view, View::Installed | View::Outdated | View::Search) {
        let picked = app.package_rows(view).iter().filter(|package| app.is_picked(package)).count();
        if picked > 0 {
            right.extend([Span::styled(format!(" {picked} picked "), BADGE), Span::raw("   ")]);
        }
    }
    match view {
        View::Installed | View::Outdated => right.extend([Span::styled("sort ", DIM), Span::styled(sort_label(brew.sort), KEY), Span::raw("   "), filter_span(app, &brew.filter)]),
        View::Search if app.editing_filter || !brew.query.is_empty() => right.push(filter_span(app, &brew.query)),
        View::Search => right.push(Span::styled("/ search ", DIM)),
        View::Taps if app.editing_filter => right.push(Span::styled(format!("add tap {}_ ", brew.new_tap), KEY)),
        View::Taps => right.push(Span::styled("a add a tap ", DIM)),
        View::Services | View::Maintenance => {}
    }
    (left, right)
}

/// The column header and one line per package, or why there is no list yet.
fn package_lines(app: &App, view: View, width: usize) -> Result<(Line<'static>, Vec<Line<'static>>), String> {
    let search = view == View::Search;
    if search && app.brew.searching {
        return Err(format!("{} Searching Homebrew...", spinner()));
    }
    if search && app.brew.results.is_none() {
        return Err("Press / and type a name, then enter.".into());
    }
    if !search && app.brew.packages.is_none() {
        return Err(format!("{} Asking Homebrew...", spinner()));
    }
    let packages = app.package_rows(view);
    if packages.is_empty() {
        let why = match view {
            View::Search => "Nothing found.",
            _ if !app.brew.filter.is_empty() => "No name matches the filter.",
            View::Outdated => "Everything is up to date.",
            _ => "Nothing installed.",
        };
        return Err(why.into());
    }
    let wide = width >= 80;
    let kind_width = if wide { 9 } else { 0 };
    let bar_width = if wide && !search { 8 } else { 0 };
    let fixed = 4 + kind_width + VERSION_WIDTH + if search { 0 } else { 11 + bar_width };
    // A search result's description takes the room an installed package's size and bar take.
    let name_width = if search { (width.saturating_sub(fixed) / 2).min(32) } else { width.saturating_sub(fixed) }.max(1);
    let description_width = width.saturating_sub(fixed + name_width);
    let columns = |check: Span<'static>, name: Span<'static>, kind: &str, version: Vec<Span<'static>>, tail: Vec<Span<'static>>| {
        let mut spans = vec![check, Span::styled(column(&name.content, name_width), name.style), Span::styled(pad(kind, kind_width), DIM)];
        spans.extend(version);
        spans.extend(tail);
        Line::from(spans)
    };
    let tail_header = if search { "DESCRIPTION".to_owned() } else { format!("{:>9}", "SIZE") };
    let header = columns(Span::raw("    "), Span::raw("NAME"), "KIND", vec![Span::raw(pad("VERSION", VERSION_WIDTH))], vec![Span::raw(tail_header)]);
    let largest = packages.iter().filter_map(|package| package.size).max().unwrap_or(0).max(1);
    let mut lines = Vec::new();
    for package in packages {
        let check = if app.is_picked(package) { Span::styled("[x] ", Style::new().fg(ACCENT)) } else { Span::raw("[ ] ") };
        let name_style = if search && app.is_installed(package) {
            Style::new().fg(ACCENT)
        } else if package.on_request {
            Style::new()
        } else {
            DIM
        };
        let tail = if search {
            vec![Span::styled(truncate(&package.desc, description_width), DIM)]
        } else {
            let size = package.size.map_or("…".to_owned(), format_bytes);
            let fraction = package.size.unwrap_or(0) as f64 / largest as f64;
            vec![Span::raw(format!("{size:>9}  ")), Span::styled(bar(fraction, bar_width), Style::new().fg(ACCENT))]
        };
        let kind = if package.cask { "cask" } else { "formula" };
        lines.push(columns(check, Span::styled(package.name.clone(), name_style), kind, version_spans(package), tail));
    }
    Ok((header, lines))
}

/// `9.0.2_1 ↑ pin`, padded to its column: ↑ marks an upgrade, pin a formula held back from upgrades.
fn version_spans(package: &Package) -> Vec<Span<'static>> {
    let up = if package.newer.is_some() { " ↑" } else { "" };
    let pin = if package.pinned { " pin" } else { "" };
    let marks = up.chars().count() + pin.len();
    let version = truncate(&package.version, VERSION_WIDTH.saturating_sub(marks + 1));
    let gap = VERSION_WIDTH.saturating_sub(version.chars().count() + marks);
    vec![Span::raw(version), Span::styled(up, Style::new().fg(ACCENT)), Span::styled(pin, SYSTEM), Span::raw(" ".repeat(gap))]
}

fn service_lines(app: &App, width: usize) -> Result<(Line<'static>, Vec<Line<'static>>), String> {
    let Some(services) = &app.brew.services else { return Err(format!("{} Asking Homebrew...", spinner())) };
    if services.is_empty() {
        return Err("No installed formula runs as a service.".into());
    }
    let name_width = services.iter().map(|service| service.name.chars().count() + 2).max().unwrap_or(0).min(width / 3);
    let file_width = width.saturating_sub(name_width + 10 + 12);
    let columns = |name: Span<'static>, status: Span<'static>, user: &str, file: &str| {
        Line::from(vec![
            Span::styled(column(&name.content, name_width), name.style),
            Span::styled(pad(&status.content, 10), status.style),
            Span::styled(pad(user, 12), DIM),
            Span::styled(truncate_left(file, file_width), DIM),
        ])
    };
    let header = columns(Span::raw("NAME"), Span::raw("STATUS"), "USER", "FILE");
    let mut lines = Vec::new();
    for service in services {
        let color = match service.status.as_str() {
            "started" => ACCENT,
            "error" => DANGER,
            _ => Color::DarkGray,
        };
        lines.push(columns(Span::raw(service.name.clone()), Span::styled(service.status.clone(), Style::new().fg(color)), &service.user, &tilde(&service.file)));
    }
    Ok((header, lines))
}

fn tap_lines(app: &App, width: usize) -> Result<(Line<'static>, Vec<Line<'static>>), String> {
    let Some(taps) = &app.brew.taps else { return Err(format!("{} Asking Homebrew...", spinner())) };
    if taps.is_empty() {
        return Err("No taps beyond Homebrew's own. a adds one.".into());
    }
    let name_width = taps.iter().map(|tap| tap.name.chars().count() + 2).max().unwrap_or(0).min(width / 2);
    let remote_width = width.saturating_sub(name_width + 10 + 8);
    let columns = |name: &str, formulae: &str, casks: &str, remote: &str| {
        Line::from(vec![Span::raw(column(name, name_width)), Span::raw(format!("{formulae:>8}  ")), Span::raw(format!("{casks:>6}  ")), Span::styled(truncate(remote, remote_width), DIM)])
    };
    let header = columns("NAME", "FORMULAE", "CASKS", "REMOTE");
    let lines = taps.iter().map(|tap| columns(&tap.name, &tap.formulae.to_string(), &tap.casks.to_string(), &tap.remote)).collect();
    Ok((header, lines))
}

/// One line per chore with what it would do now: what is outdated, what cleanup frees.
fn chore_lines(app: &App, width: usize) -> (Line<'static>, Vec<Line<'static>>) {
    let brew = &app.brew;
    // Cleanup and autoremove wait for their dry runs, which take a few seconds.
    let pending = if brew.measuring { format!("{} measuring", spinner()) } else { String::new() };
    let maintenance = brew.maintenance.as_ref();
    let mut lines = Vec::new();
    for chore in CHORES {
        let state = match chore {
            Chore::UpgradeAll => {
                let outdated: Vec<&str> = brew.packages.iter().flatten().filter(|package| package.newer.is_some() && !package.pinned).map(|package| package.name.as_str()).collect();
                if outdated.is_empty() { "nothing to upgrade".to_owned() } else { format!("{} outdated: {}", outdated.len(), outdated.join(", ")) }
            }
            Chore::Cleanup => maintenance.map_or(pending.clone(), |maintenance| maintenance.cleanup.as_ref().map_or("nothing to remove".to_owned(), |size| format!("frees about {size}"))),
            Chore::Autoremove => maintenance.map_or(pending.clone(), |maintenance| match maintenance.autoremove.len() {
                0 => "nothing unused".to_owned(),
                count => format!("{count} unused: {}", maintenance.autoremove.join(", ")),
            }),
            Chore::Update => "fetches the newest formula and cask list".to_owned(),
            Chore::Doctor => "lists problems with the install".to_owned(),
        };
        lines.push(row(chore.title(), &state, width, Style::new()));
    }
    (Line::raw(""), lines)
}

/// Everything known about the row under the cursor, as label and value lines.
fn brew_detail(app: &App, view: View, width: usize) -> Vec<Line<'static>> {
    let cursor = app.brew_cursor(view);
    match view {
        View::Services => {
            let Some(service) = app.brew.services.iter().flatten().nth(cursor) else { return vec![] };
            let user = if service.user.is_empty() { "nobody, not loaded" } else { &service.user };
            vec![Line::styled(service.name.clone(), BOLD), Line::raw(""), fact("status", &service.status, width), fact("user", user, width), path_fact("file", &service.file, width)]
        }
        View::Taps => {
            let Some(tap) = app.brew.taps.iter().flatten().nth(cursor) else { return vec![] };
            vec![
                Line::styled(tap.name.clone(), BOLD),
                Line::raw(""),
                fact("remote", &tap.remote, width),
                fact("formulae", &tap.formulae.to_string(), width),
                fact("casks", &tap.casks.to_string(), width),
            ]
        }
        View::Maintenance => {
            let chore = CHORES[cursor];
            let command = format!("brew {}", chore.args().join(" "));
            vec![Line::styled(chore.title(), BOLD), Line::styled(chore.about(), DIM), Line::raw(""), fact("runs", &command, width)]
        }
        View::Installed | View::Outdated | View::Search => app.package_at_cursor(view).map_or_else(Vec::new, |package| package_facts(app, package, width)),
    }
}

fn package_facts(app: &App, package: &Package, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::styled(package.name.clone(), BOLD)];
    if !package.desc.is_empty() {
        lines.push(Line::styled(package.desc.clone(), DIM));
    }
    lines.push(Line::raw(""));
    let version = match &package.newer {
        Some(newer) => format!("{} → {newer}", package.version),
        None => package.version.clone(),
    };
    lines.push(fact("version", &version, width));
    let mut kind = vec![if package.cask { "cask" } else { "formula" }];
    if package.pinned {
        kind.push("pinned");
    }
    if package.auto_updates {
        kind.push("updates itself");
    }
    lines.push(fact("kind", &kind.join(", "), width));
    if !app.is_installed(package) {
        lines.push(fact("installed", "no", width));
    } else if package.installed_at > 0 {
        let date: String = trash::local_time(package.installed_at, "").chars().take(10).collect();
        let by = if package.on_request { "by you" } else { "as a dependency" };
        lines.push(fact("installed", &format!("{date}, {by}"), width));
    }
    if let Some(size) = package.size {
        lines.push(fact("size", &format_bytes(size), width));
    }
    let homepage = package.homepage.trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/');
    let needs = package.dependencies.join(", ");
    let needed_by = brew::needed_by(app.brew.packages.as_deref().unwrap_or_default(), package).join(", ");
    for (label, value) in [("homepage", homepage), ("tap", package.tap.as_str()), ("needs", needs.as_str()), ("needed by", needed_by.as_str())] {
        if !value.is_empty() {
            lines.push(fact(label, value, width));
        }
    }
    // A cask's paths after its Caskroom folder are what it put elsewhere: its app, fonts and links.
    for (index, path) in package.paths.iter().skip(1).take(4).enumerate() {
        lines.push(path_fact(if index == 0 { "files" } else { "" }, &path.to_string_lossy(), width));
    }
    lines
}

/// ` version   1.144.0`: a label column, then the value cut to the pane.
fn fact(label: &str, value: &str, width: usize) -> Line<'static> {
    Line::from(vec![Span::styled(format!("{label:<10}"), DIM), Span::raw(truncate(value, width.saturating_sub(10)))])
}

/// A fact whose value is a path, cut from the left so the file name stays.
fn path_fact(label: &str, path: &str, width: usize) -> Line<'static> {
    fact(label, &truncate_left(&tilde(path), width.saturating_sub(10)), width)
}
fn draw_history(frame: &mut Frame, app: &App, area: Rect) {
    let [head, gap, list_area, rule_area, files_area] = Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1), Constraint::Length(6)]).areas(area);
    let left = vec![Span::styled(" Delete History", BOLD), Span::styled(format!("  {}", plural(app.history.len(), "removal")), DIM)];
    spread(frame, head, left, vec![Span::styled("files stay restorable until you empty the Trash ", DIM)]);
    rule(frame, gap);
    if app.history.is_empty() {
        placeholder(frame, list_area, "Nothing removed yet.");
        return;
    }
    let width = list_area.width.saturating_sub(LIST_CHROME) as usize;
    let rows: Vec<ListItem> = app
        .history
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let when = entry.secs.map(|secs| trash::local_time(secs, "")).unwrap_or_default();
            let restorable = trash::restorable(entry).len();
            let detail = format!("{}, {}{}", plural(entry.count as usize, "item"), format_bytes(entry.bytes), if restorable > 0 { format!(", {restorable} restorable") } else { String::new() });
            let line = row(&format!("{when}  {}", entry.origin), &detail, width, if restorable > 0 { Style::new() } else { DIM });
            ListItem::new(if index == app.history_cursor { on_cursor(line) } else { line })
        })
        .collect();
    cursor_list(frame, list_area, rows, app.history_cursor);
    rule(frame, rule_area);
    let paths = app.history.get(app.history_cursor).map(|entry| entry.paths.as_slice()).unwrap_or_default();
    let shown = if paths.len() > files_area.height as usize { (files_area.height as usize).saturating_sub(1) } else { paths.len() };
    let path_width = files_area.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = paths[..shown].iter().map(|path| Line::raw(format!(" {}", truncate_left(&tilde(path), path_width)))).collect();
    if shown < paths.len() {
        lines.push(Line::styled(format!(" and {} more", paths.len() - shown), DIM));
    }
    frame.render_widget(Paragraph::new(lines), files_area);
}

/// The update channel and this build, with what the channel does in the detail pane.
fn draw_settings(frame: &mut Frame, app: &App, area: Rect) {
    let pane = area.width >= DETAIL_FROM;
    let pane_width = if pane { DETAIL_WIDTH } else { 0 };
    let [main, pane_rule, pane_area] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(pane as u16), Constraint::Length(pane_width)]).areas(area);
    let [head, gap, list_area] = Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Fill(1)]).areas(main);
    frame.render_widget(Paragraph::new(Line::styled(" Settings", BOLD)), head);
    rule(frame, gap);
    let width = list_area.width.saturating_sub(LIST_CHROME) as usize;
    let (stable, pre_release) = if app.pre_releases { (DIM, Style::new().fg(ACCENT)) } else { (Style::new().fg(ACCENT), DIM) };
    let options = "stable  pre-release".len();
    let channel = Line::from(vec![Span::raw(pad("Update channel", width.saturating_sub(options))), Span::styled("stable", stable), Span::raw("  "), Span::styled("pre-release", pre_release)]);
    let build = if update::enabled() { "release build" } else { "local build, never updates itself" };
    let version = row("Version", &format!("{}, {build}", update::CURRENT), width, Style::new());
    cursor_list(frame, list_area, vec![ListItem::new(on_cursor(channel)), ListItem::new(version)], 0);
    if !pane {
        return;
    }
    vertical_rule(frame, pane_rule);
    let about = "Stable installs releases only, and a beta build moves to the release that follows it. Pre-release also installs betas, such as v0.3.0-beta.1.";
    let detail = vec![Line::styled("Update channel", BOLD), Line::styled(about, DIM), Line::raw(""), fact("checks", "at each start and on u", pane_area.width.saturating_sub(2) as usize)];
    frame.render_widget(Paragraph::new(detail).wrap(Wrap { trim: false }).block(Block::new().padding(Padding::horizontal(1))), pane_area);
}

/// The keys of the page in view, most used first; the footer shows as many as fit.
fn page_keys(app: &App) -> &'static [(&'static str, &'static str)] {
    match (app.page, app.target()) {
        (Page::Brew(View::Search), _) if app.editing_filter => &[("enter", "search"), ("esc", "clear")],
        (Page::Brew(View::Taps), _) if app.editing_filter => &[("enter", "add"), ("esc", "cancel")],
        _ if app.editing_filter => &[("enter", "done"), ("esc", "clear")],
        (_, Some(Target::Leftovers)) => &[("space", "select"), ("enter", "fold"), ("a", "safe only"), ("/", "filter"), ("o", "reveal"), ("esc", "back")],
        (_, Some(_)) => &[("space", "select"), ("enter", "fold"), ("a", "safe only"), ("/", "filter"), ("o", "reveal")],
        (Page::Apps, _) => &[("↑↓", "move"), ("enter", "open"), ("/", "filter"), ("S", "sort"), ("s", "system files")],
        (Page::Brew(View::Installed), _) => {
            &[("space", "pick"), ("u", "upgrade"), ("d", "uninstall"), ("z", "zap"), ("p", "pin"), ("/", "filter"), ("S", "sort"), ("o", "homepage"), ("a", "pick all")]
        }
        (Page::Brew(View::Outdated), _) => &[("u", "upgrade"), ("space", "pick"), ("a", "pick all"), ("p", "pin"), ("/", "filter"), ("o", "homepage"), ("d", "uninstall"), ("z", "zap")],
        (Page::Brew(View::Search), _) => &[("/", "search"), ("i", "install"), ("space", "pick"), ("o", "homepage")],
        (Page::Brew(View::Services), _) => &[("enter", "start/stop"), ("r", "restart")],
        (Page::Brew(View::Taps), _) => &[("a", "add"), ("d", "remove"), ("o", "open")],
        (Page::Brew(View::Maintenance), _) => &[("enter", "run")],
        (Page::Settings, _) => &[("enter", "change"), ("u", "check now")],
        _ => &[("u", "restore")],
    }
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let status = Span::styled(truncate(&app.status, area.width as usize / 2), Style::new().fg(ACCENT));
    let key_spans = |(key, label): &(&str, &str)| [Span::styled(format!(" {key}"), KEY), Span::styled(format!(" {label} "), DIM)];
    let always: Vec<Span> = [("?", "help"), ("q", "quit")].iter().flat_map(key_spans).collect();
    // Page keys that do not fit drop off whole, so ? and q always show.
    let mut room = (area.width as usize).saturating_sub(status.width() + 1 + always.iter().map(Span::width).sum::<usize>());
    let mut keys = Vec::new();
    for entry in page_keys(app) {
        let spans = key_spans(entry);
        let width: usize = spans.iter().map(Span::width).sum();
        if width > room {
            break;
        }
        room -= width;
        keys.extend(spans);
    }
    keys.extend(always);
    spread(frame, area, keys, vec![status, Span::raw(" ")]);
}

fn draw_help(frame: &mut Frame, app: &App) {
    let mut keys = page_keys(app).to_vec();
    if app.target().is_some() {
        keys.extend([("d", "move the selection to the Trash"), ("→ l, ← h", "open, fold a group"), ("space on a group", "select the whole group")]);
    }
    match app.page {
        Page::Brew(View::Installed | View::Outdated) => keys.extend([("a", "pick every package shown"), ("esc", "clear the filter, then the picks")]),
        Page::Brew(View::Search) => keys.extend([("a", "pick every package shown"), ("esc", "clear the picks")]),
        _ => {}
    }
    keys.extend([("↑ ↓ j k", "move"), ("pgup pgdn g G", "jump"), ("1-6, tab", "switch page")]);
    if !matches!(app.page, Page::Brew(_) | Page::Settings) {
        keys.push(("s", "include system locations"));
    }
    keys.extend([("R", "rescan"), ("q, ctrl-c", "quit")]);
    let mut seen = HashSet::new();
    keys.retain(|(key, _)| seen.insert(*key));
    let key_width = keys.iter().map(|(key, _)| key.chars().count()).max().unwrap_or(0);
    let lines: Vec<Line> = keys.iter().map(|(key, label)| Line::from(vec![Span::styled(format!(" {key:key_width$}  "), KEY), Span::raw(*label)])).collect();
    let block = Block::bordered().title(" Keys ").title_bottom(Line::styled(" any key closes ", DIM)).border_style(Style::new().fg(ACCENT));
    let popup = centered(frame.area(), 52, lines.len() as u16 + 2);
    frame.render_widget(Clear, popup);
    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

fn draw_confirm(frame: &mut Frame, app: &App, confirm: &Confirm) {
    let (mut text, verb, color) = match confirm {
        Confirm::Trash(target) => {
            let list = app.list(*target);
            let what = format!("{} ({})", plural(list.selected().count(), "item"), format_bytes(list.selected_bytes()));
            let mut text = match (target, &app.opened) {
                (Target::Leftovers, Some(opened)) => vec![Line::styled(format!("Uninstall {}?", opened.name), BOLD), Line::raw(format!("Moves {what} to the Trash."))],
                _ => vec![Line::styled(format!("Move {what} to the Trash?"), BOLD)],
            };
            text.push(Line::styled("Delete History puts them back while they are in the Trash.", DIM));
            let system = list.selected().filter(|item| item.system).count();
            if system > 0 {
                text.push(Line::styled(format!("{} may ask for your password.", plural(system, "system file")), SYSTEM));
            }
            (text, "move to Trash", DANGER)
        }
        Confirm::Restore(index) => {
            let entry = &app.history[*index];
            (vec![Line::raw(format!("Restore {} from \"{}\" to where they were?", plural(trash::restorable(entry).len(), "item"), entry.origin))], "restore", ACCENT)
        }
        Confirm::Brew(plan) => {
            let mut text = vec![Line::styled(plan.title.clone(), BOLD)];
            text.extend(plan.commands.iter().map(|args| Line::styled(command_line(args), KEY)));
            text.push(Line::styled(plan.note.as_str(), DIM));
            (text, "run in the terminal", if plan.removes { DANGER } else { ACCENT })
        }
    };
    text.extend([Line::raw(""), Line::from(vec![Span::styled("y", KEY), Span::raw(format!(" {verb}   ")), Span::styled("n", KEY), Span::raw(" cancel")])]);
    let width = frame.area().width.min(64);
    // Borders and padding take 4 columns; one spare row for word wrap.
    let room = usize::from(width.saturating_sub(4).max(1));
    let rows: usize = text.iter().map(|line| line.width().max(1).div_ceil(room)).sum();
    let popup = centered(frame.area(), width, rows as u16 + 3);
    frame.render_widget(Clear, popup);
    frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }).block(Block::bordered().padding(Padding::horizontal(1)).border_style(Style::new().fg(color))), popup);
}

/// A list with the `▌` cursor mark and bar, and a scrollbar on its last column when the rows overflow.
fn cursor_list(frame: &mut Frame, area: Rect, rows: Vec<ListItem>, cursor: usize) {
    let count = rows.len();
    let [list_area, bar_area] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    let list = List::new(rows).highlight_style(CURSOR).highlight_symbol(Span::styled("▌ ", Style::new().fg(ACCENT))).highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(list, list_area, &mut ListState::default().with_selected(Some(cursor)));
    if count > area.height as usize {
        let mut state = ScrollbarState::new(count).position(cursor);
        let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight).begin_symbol(None).end_symbol(None).track_symbol(None).thumb_style(DIM);
        frame.render_stateful_widget(bar, bar_area, &mut state);
    }
}

/// Default-colored text turns light, so the dark cursor bar reads on light terminal themes too.
fn on_cursor(mut line: Line<'static>) -> Line<'static> {
    for span in &mut line.spans {
        span.style.fg = span.style.fg.or(Some(CURSOR_TEXT));
    }
    line
}

/// `left` from the start of the row, `right` against its end.
fn spread(frame: &mut Frame, area: Rect, left: Vec<Span<'static>>, right: Vec<Span<'static>>) {
    let right = Line::from(right);
    // The spacer keeps a cut-off left side from running into the right one.
    let [left_area, _, right_area] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(2), Constraint::Length(right.width() as u16)]).areas(area);
    frame.render_widget(Paragraph::new(Line::from(left)), left_area);
    frame.render_widget(Paragraph::new(right), right_area);
}

fn rule(frame: &mut Frame, area: Rect) {
    frame.render_widget(Paragraph::new("─".repeat(area.width as usize)).style(DIM), area);
}

fn vertical_rule(frame: &mut Frame, area: Rect) {
    frame.render_widget(Paragraph::new(vec![Line::raw("│"); area.height as usize]).style(DIM), area);
}

fn placeholder(frame: &mut Frame, area: Rect, text: &str) {
    frame.render_widget(Paragraph::new(format!("   {text}")).style(DIM), area);
}

/// The filter with a cursor while it is typed, or the key that starts one.
fn filter_span(app: &App, filter: &str) -> Span<'static> {
    if app.editing_filter || !filter.is_empty() { Span::styled(format!("/{filter}{} ", if app.editing_filter { "_" } else { "" }), KEY) } else { Span::styled("/ filter ", DIM) }
}

fn spinner() -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let millis = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
    FRAMES[(millis / 100 % 10) as usize]
}

/// A share of `width` columns in eighth blocks, as ncdu draws it.
fn bar(fraction: f64, width: usize) -> String {
    const EIGHTHS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    let eighths = (fraction.clamp(0.0, 1.0) * width as f64 * 8.0).round() as usize;
    let (full, rest) = (eighths / 8, eighths % 8);
    let mut bar = "█".repeat(full);
    if rest > 0 {
        bar.push(EIGHTHS[rest]);
    }
    pad(&bar, width)
}

/// `text` cut and padded to `width`, with at least one space before the next column.
fn column(text: &str, width: usize) -> String {
    pad(&format!("{} ", truncate(text, width.saturating_sub(1))), width)
}

/// `brew` and its arguments, cut to about three popup rows with "and N more", so the y/n line stays on an 80x24 screen.
fn command_line(args: &[String]) -> String {
    let mut line = "brew".to_owned();
    for (index, arg) in args.iter().enumerate() {
        if line.len() + arg.len() > 170 {
            return format!("{line} and {} more", args.len() - index);
        }
        line += " ";
        line += arg;
    }
    line
}

/// A box of at most `width` by `height`, centered in `area`.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let (width, height) = (width.min(area.width), height.min(area.height));
    Rect { x: area.x + (area.width - width) / 2, y: area.y + (area.height - height) / 2, width, height }
}

/// Name on the left, detail on the right. The name stays whole; the detail gives way first.
fn row(left: &str, right: &str, width: usize, style: Style) -> Line<'static> {
    let left = truncate(left, width);
    let right = truncate(right, width.saturating_sub(left.chars().count() + 1));
    let gap = width.saturating_sub(left.chars().count() + right.chars().count());
    Line::from(vec![Span::styled(left, style), Span::raw(" ".repeat(gap)), Span::styled(right, DIM)])
}

/// Exactly `width` columns: cut with `…` or filled with spaces.
fn pad(text: &str, width: usize) -> String {
    format!("{:width$}", truncate(text, width))
}

fn truncate(text: &str, width: usize) -> String {
    match width {
        _ if text.chars().count() <= width => text.to_owned(),
        0 => String::new(),
        _ => text.chars().take(width - 1).chain(['…']).collect(),
    }
}

/// Keeps the end of a path, where the file name is.
fn truncate_left(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count <= width { text.to_owned() } else { ['…'].into_iter().chain(text.chars().skip(count + 1 - width.max(1))).collect() }
}

fn tilde(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    match path.strip_prefix(&home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => format!("~{rest}"),
        _ => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_keeps_the_name_and_cuts_the_detail() {
        let text = |line: Line| line.spans.iter().map(|span| span.content.as_ref()).collect::<String>();
        assert_eq!(text(row("Business Tour - Online Board Game", "2.1.3", 40, DIM)), "Business Tour - Online Board Game  2.1.3");
        assert_eq!(text(row("Business Tour - Online Board Game", "2026-11-01", 40, DIM)), "Business Tour - Online Board Game 2026-…");
        assert_eq!(text(row("Business Tour - Online Board Game", "1.0", 33, DIM)), "Business Tour - Online Board Game");
        assert_eq!(text(row("Business Tour - Online Board Game", "1.0", 10, DIM)), "Business …");
    }

    #[test]
    fn long_brew_commands_end_with_a_count() {
        let args = |names: &[String]| [&["uninstall".to_owned(), "--".to_owned()], names].concat();
        let few: Vec<String> = ["bat", "jq"].map(str::to_owned).to_vec();
        assert_eq!(command_line(&args(&few)), "brew uninstall -- bat jq");
        let many: Vec<String> = (0..199).map(|index| format!("package-{index}")).collect();
        let line = command_line(&args(&many));
        assert!(line.len() < 200 && line.ends_with(" and 185 more"), "{line}");
    }

    #[test]
    fn bar_fills_eighths_of_its_width() {
        assert_eq!(bar(0.5, 4), "██  ");
        assert_eq!(bar(1.0, 2), "██");
        assert_eq!(bar(2.0, 2), "██");
        assert_eq!(bar(1.0 / 16.0, 2), "▏ ");
        assert_eq!(bar(0.0, 3), "   ");
    }

    #[test]
    fn app_rows_line_up_at_every_width() {
        for width in [60, 95, 96, 140] {
            let line = app_row(Span::raw("Hopper Disassembler"), ["6.6.0-demo", "2026-03-30", "145 MB", "1.7%"], Some(0.5), width);
            assert_eq!(line.width(), width, "width {width}");
        }
    }

    #[test]
    fn a_cut_column_keeps_a_space_before_the_next() {
        assert_eq!(column("docker-credential-helper", 8), "docker… ");
        assert_eq!(column("bat", 5), "bat  ");
    }
}
