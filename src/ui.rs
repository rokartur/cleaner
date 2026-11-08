use crate::app::{self, App, AppSort, Confirm, PAGES, Page, Row, Target};
use crate::scan::{FileItem, format_bytes};
use crate::trash::{self, plural};
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

pub fn draw(frame: &mut Frame, app: &App) {
    let [tabs, top_rule, body, bottom_rule, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1), Constraint::Length(1)]).areas(frame.area());
    draw_tabs(frame, app, tabs);
    rule(frame, top_rule);
    match app.target() {
        Some(target) => draw_files(frame, app, target, body),
        None if app.page == Page::Apps => draw_apps(frame, app, body),
        None => draw_history(frame, app, body),
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

/// ` Cleaner  1 Apps  115   2 Junk 31 GB ...` on the left, the disk gauge on the right when it fits.
fn draw_tabs(frame: &mut Frame, app: &App, area: Rect) {
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
    let mut tabs = vec![Span::styled(" Cleaner ", BOLD)];
    for (index, page) in PAGES.iter().enumerate() {
        let (name, count) = match page {
            Page::Apps => ("Apps", app.apps.as_ref().map_or(spinner().to_string(), |apps| apps.len().to_string())),
            Page::Junk => ("Junk", summary(Target::Junk)),
            Page::Orphans => ("Orphans", summary(Target::Orphans)),
            Page::History => ("History", app.history.len().to_string()),
        };
        if *page == app.page {
            tabs.push(Span::styled(format!(" {} {name} ", index + 1), BADGE));
        } else {
            tabs.extend([Span::styled(format!(" {} ", index + 1), DIM), Span::raw(name)]);
        }
        tabs.push(Span::styled(format!(" {count}  "), DIM));
    }
    let mut gauge = Vec::new();
    if let Some((total, free)) = app.disk.filter(|(total, _)| *total > 0) {
        let used = 1.0 - free as f64 / total as f64;
        let filled = (used * 16.0).round() as usize;
        let color = if used > 0.9 { DANGER } else { ACCENT };
        gauge =
            vec![Span::styled("disk ", DIM), Span::styled("━".repeat(filled), Style::new().fg(color)), Span::styled("━".repeat(16 - filled), DIM), Span::raw(format!(" {} free ", format_bytes(free)))];
    }
    if Line::from(tabs.clone()).width() + Line::from(gauge.clone()).width() > area.width as usize {
        gauge.clear();
    }
    spread(frame, area, tabs, gauge);
}

fn draw_apps(frame: &mut Frame, app: &App, area: Rect) {
    let [head, _, columns, list_area] = Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let apps = app.visible_apps();
    let user_count = apps.iter().filter(|installed| !installed.is_system).count();
    let sort = match app.app_sort {
        AppSort::Name => "name",
        AppSort::Size => "size ▾",
        AppSort::Installed => "install date ▾",
    };
    let left = vec![Span::styled(" Applications", BOLD), Span::styled(format!("  {user_count} user · {} system", apps.len() - user_count), DIM)];
    spread(frame, head, left, vec![Span::styled("sort ", DIM), Span::styled(sort, KEY), Span::raw("   "), filter_span(app, &app.filter)]);
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
    let [head, _, list_area, rule_area, detail_area, _, action_area] =
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

fn draw_history(frame: &mut Frame, app: &App, area: Rect) {
    let [head, _, list_area, rule_area, files_area] = Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1), Constraint::Length(6)]).areas(area);
    let left = vec![Span::styled(" Delete History", BOLD), Span::styled(format!("  {}", plural(app.history.len(), "removal")), DIM)];
    spread(frame, head, left, vec![Span::styled("files stay restorable until you empty the Trash ", DIM)]);
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

/// The keys of the page in view, most used first; the footer shows as many as fit.
fn page_keys(app: &App) -> &'static [(&'static str, &'static str)] {
    match app.target() {
        _ if app.editing_filter => &[("enter", "done"), ("esc", "clear")],
        Some(Target::Leftovers) => &[("space", "select"), ("enter", "fold"), ("a", "safe only"), ("/", "filter"), ("o", "reveal"), ("esc", "back")],
        Some(_) => &[("space", "select"), ("enter", "fold"), ("a", "safe only"), ("/", "filter"), ("o", "reveal")],
        None if app.page == Page::Apps => &[("↑↓", "move"), ("enter", "open"), ("/", "filter"), ("S", "sort"), ("s", "system files")],
        None => &[("u", "restore")],
    }
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let mut keys = Vec::new();
    for (key, label) in page_keys(app).iter().chain(&[("?", "help"), ("q", "quit")]) {
        keys.push(Span::styled(format!(" {key}"), KEY));
        keys.push(Span::styled(format!(" {label} "), DIM));
    }
    let status = Span::styled(truncate(&app.status, area.width as usize / 2), Style::new().fg(ACCENT));
    spread(frame, area, keys, vec![status, Span::raw(" ")]);
}

fn draw_help(frame: &mut Frame, app: &App) {
    let mut keys = page_keys(app).to_vec();
    if app.target().is_some() {
        keys.extend([("d", "move the selection to the Trash"), ("→ l, ← h", "open, fold a group"), ("space on a group", "select the whole group")]);
    }
    keys.extend([("↑ ↓ j k", "move"), ("pgup pgdn g G", "jump"), ("1-4, tab", "switch page"), ("s", "include system locations"), ("R", "rescan"), ("q, ctrl-c", "quit")]);
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
    };
    text.extend([Line::raw(""), Line::from(vec![Span::styled("y", KEY), Span::raw(format!(" {verb}   ")), Span::styled("n", KEY), Span::raw(" cancel")])]);
    // One spare row for a long app name that wraps.
    let popup = centered(frame.area(), 64, text.len() as u16 + 3);
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
}
