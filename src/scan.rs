use crate::apps::{self, App, Index};
use crate::matcher::{self, Collisions, Descriptor, Kind, Strength};
use crate::size;
use std::collections::HashSet;
use std::ffi::CStr;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct FileItem {
    pub path: PathBuf,
    pub category: &'static str,
    /// Lives outside the user's own files, so moving it may need an administrator.
    pub system: bool,
    pub size: u64,
    /// Part of the tree could not be read, so `size` is a lower bound.
    pub approximate: bool,
    pub selected: bool,
    /// False keeps "select all" off rows that need a deliberate tick.
    pub auto_selectable: bool,
    /// Why an app's leftover matched, shown under the list; empty for junk and orphans.
    pub why: &'static str,
}

impl FileItem {
    fn new(path: PathBuf, category: &'static str, system: bool, selected: bool) -> Self {
        FileItem { path, category, system, size: 0, approximate: false, selected, auto_selectable: selected, why: "" }
    }
}

const CATEGORY_ORDER: &[&str] = &[
    "Application",
    "Application Support",
    "Caches",
    "Preferences",
    "Containers",
    "Group Containers",
    "Logs",
    "Crash Reports",
    "Saved Application State",
    "Autosave Information",
    "Cookies",
    "HTTPStorages",
    "WebKit",
    "LaunchAgents",
    "LaunchDaemons",
    "Application Scripts",
    "Internet Plug-Ins",
    "PreferencePanes",
    "PrivilegedHelperTools",
    "Services",
    "Extensions",
    "Configuration",
    "Command Line Tools",
    "Shell Completions",
    "Fonts",
    "Home",
    "Temporary",
    "User Caches",
    "User Logs",
    "Developer",
    "iOS Backups",
    "iOS Software Updates",
    "System Caches",
    "System Logs",
    "Orphans (High confidence)",
    "Orphans (Medium confidence)",
    "Orphans (Low confidence)",
];

/// Groups rows by category in display order, user files before system ones, biggest first inside each group.
pub fn sort(items: &mut [FileItem]) {
    let rank = |category: &str| CATEGORY_ORDER.iter().position(|known| *known == category).unwrap_or(CATEGORY_ORDER.len());
    items.sort_by(|a, b| rank(a.category).cmp(&rank(b.category)).then(a.category.cmp(b.category)).then(a.system.cmp(&b.system)).then(b.size.cmp(&a.size)));
}

/// Decimal units, as Finder shows them.
pub fn format_bytes(bytes: u64) -> String {
    let value = bytes as f64;
    for (scale, unit) in [(1e12, "TB"), (1e9, "GB"), (1e6, "MB"), (1e3, "KB")] {
        if value >= scale {
            let scaled = value / scale;
            return if scaled < 10.0 { format!("{scaled:.1} {unit}") } else { format!("{scaled:.0} {unit}") };
        }
    }
    format!("{bytes} B")
}

fn measure(items: &mut [FileItem]) {
    let paths: Vec<PathBuf> = items.iter().map(|item| item.path.clone()).collect();
    for (item, (size, complete)) in items.iter_mut().zip(size::sizes(&paths)) {
        item.size = size;
        item.approximate = !complete;
    }
}

/// Runs `work` for each input on four threads and keeps the results in input order.
fn parallel<T: Sync, R: Send>(inputs: &[T], work: impl Fn(&T) -> Vec<R> + Sync) -> Vec<R> {
    let chunk = inputs.len().div_ceil(4).max(1);
    std::thread::scope(|scope| {
        let handles: Vec<_> = inputs.chunks(chunk).map(|part| scope.spawn(|| part.iter().flat_map(&work).collect::<Vec<_>>())).collect();
        handles.into_iter().flat_map(|handle| handle.join().expect("scan thread panicked")).collect()
    })
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

/// One folder to search for leftovers and how to search it.
pub struct Location {
    pub category: &'static str,
    pub path: PathBuf,
    pub system: bool,
    /// Levels walked: 2 also finds vendor-grouped data such as `Application Support/Google/Chrome`.
    pub depth: usize,
    /// Reads the sandbox metadata of UUID-named containers.
    pub resolves_container: bool,
    /// False for Fonts and the like, where a stray name match is costly.
    pub auto_selectable: bool,
    /// The home folder: only dot-entries are leftovers, `~/cmux` is a user project.
    pub hidden_only: bool,
}

const fn spec(category: &'static str, sub: &'static str, depth: usize) -> (&'static str, &'static str, usize) {
    (category, sub, depth)
}

/// Library subfolders an app or its helpers write to. Depth 3 under Application Support reaches
/// bundle-id files inside shared containers such as com.apple.sharedfilelist.
const LIBRARY: &[(&str, &str, usize)] = &[
    spec("Application Support", "Application Support", 3),
    spec("Application Support", "Application Support/CrashReporter", 1),
    spec("Caches", "Caches", 2),
    spec("Preferences", "Preferences", 1),
    spec("Preferences", "Preferences/ByHost", 1),
    spec("Containers", "Containers", 1),
    spec("Group Containers", "Group Containers", 1),
    spec("Logs", "Logs", 2),
    spec("Crash Reports", "Logs/DiagnosticReports", 1),
    spec("Saved Application State", "Saved Application State", 1),
    spec("Autosave Information", "Autosave Information", 1),
    spec("Cookies", "Cookies", 1),
    spec("HTTPStorages", "HTTPStorages", 1),
    spec("WebKit", "WebKit", 1),
    spec("LaunchAgents", "LaunchAgents", 1),
    spec("Application Scripts", "Application Scripts", 1),
    spec("Internet Plug-Ins", "Internet Plug-Ins", 1),
    spec("PreferencePanes", "PreferencePanes", 1),
    spec("Services", "Services", 1),
    spec("Audio Plug-Ins", "Audio/Plug-Ins", 2),
    spec("Components", "Components", 1),
    spec("Filesystems", "Filesystems", 1),
    spec("Frameworks", "Frameworks", 1),
    spec("Keyboard Layouts", "Keyboard Layouts", 1),
    spec("PDF Services", "PDF Services", 1),
    spec("Scripting Additions", "ScriptingAdditions", 1),
    spec("Startup Items", "StartupItems", 1),
    spec("Widgets", "Widgets", 1),
    spec("Screen Savers", "Screen Savers", 1),
    spec("Color Pickers", "ColorPickers", 1),
    spec("Input Methods", "Input Methods", 1),
    spec("Spotlight", "Spotlight", 1),
    spec("QuickLook", "QuickLook", 1),
    spec("Contextual Menu Items", "Contextual Menu Items", 1),
    spec("Address Book Plug-Ins", "Address Book Plug-Ins", 1),
    spec("Mail Bundles", "Mail/Bundles", 1),
    spec("Automator", "Automator", 1),
    spec("Dictionaries", "Dictionaries", 1),
    spec("Sounds", "Sounds", 1),
    spec("Developer", "Developer", 1),
    spec("Fonts", "Fonts", 1),
];

const SYSTEM_LIBRARY_ONLY: &[(&str, &str, usize)] = &[spec("LaunchDaemons", "LaunchDaemons", 1), spec("PrivilegedHelperTools", "PrivilegedHelperTools", 1), spec("Extensions", "Extensions", 1)];

/// CLI binaries and shell completions pkg and brew installers drop outside Library.
const UNIX_TOOLS: &[(&str, &str, usize)] = &[
    spec("Command Line Tools", "/usr/local/bin", 1),
    spec("Command Line Tools", "/usr/local/sbin", 1),
    spec("Command Line Tools", "/opt/homebrew/bin", 1),
    spec("Command Line Tools", "/opt/homebrew/sbin", 1),
    spec("Shell Completions", "/usr/local/share/zsh/site-functions", 1),
    spec("Shell Completions", "/usr/local/share/fish/vendor_completions.d", 1),
    spec("Shell Completions", "/usr/local/share/bash-completion/completions", 1),
    spec("Shell Completions", "/opt/homebrew/share/zsh/site-functions", 1),
    spec("Shell Completions", "/opt/homebrew/share/fish/vendor_completions.d", 1),
    spec("Shell Completions", "/opt/homebrew/share/bash-completion/completions", 1),
];

const SYSTEM_DATA: &[(&str, &str, usize)] = &[spec("Configuration", "/private/etc", 1), spec("Logs", "/private/var/log", 1)];

fn build(root: &Path, specs: &[(&'static str, &str, usize)], system: bool, out: &mut Vec<Location>) {
    for &(category, sub, depth) in specs {
        let path = root.join(sub);
        if path.is_dir() && !out.iter().any(|location| location.path == path) {
            let resolves_container = category.ends_with("Containers");
            // Package managers own most of bin and completions; links into the bundle come from `cli_links`, selected.
            let auto_selectable = !matches!(category, "Fonts" | "Command Line Tools" | "Shell Completions");
            out.push(Location { category, path, system, depth, resolves_container, auto_selectable, hidden_only: false });
        }
    }
}

pub fn locations(include_system: bool) -> Vec<Location> {
    let mut out = Vec::new();
    build(&home().join("Library"), LIBRARY, false, &mut out);
    build(Path::new("/"), UNIX_TOOLS, true, &mut out);
    if include_system {
        let system: Vec<_> = LIBRARY.iter().chain(SYSTEM_LIBRARY_ONLY).copied().collect();
        build(Path::new("/Library"), &system, true, &mut out);
        build(Path::new("/"), SYSTEM_DATA, true, &mut out);
    }
    out
}

/// Per-user temp folders, XDG folders and home dot-folders, where CLIs and cross-platform apps write.
fn home_and_temp_locations() -> Vec<Location> {
    let home = home();
    let mut paths: Vec<(&'static str, PathBuf, bool)> = Vec::new();
    for name in [libc::_CS_DARWIN_USER_CACHE_DIR, libc::_CS_DARWIN_USER_TEMP_DIR] {
        if let Some(path) = confstr(name) {
            paths.push(("Temporary", path, false));
        }
    }
    if let Ok(tmpdir) = std::env::var("TMPDIR") {
        paths.push(("Temporary", PathBuf::from(tmpdir), false));
    }
    paths.push(("Home", home.clone(), true));
    for (variable, fallback, category) in [
        ("XDG_CONFIG_HOME", ".config", "Configuration"),
        ("XDG_CACHE_HOME", ".cache", "Caches"),
        ("XDG_DATA_HOME", ".local/share", "Application Support"),
        ("XDG_STATE_HOME", ".local/state", "Application Support"),
    ] {
        // An inherited XDG_DATA_HOME=/ would point an auto-selecting walk at the whole disk.
        let path = std::env::var(variable).ok().map(PathBuf::from).filter(|path| path.starts_with(&home) && path != &home && !path.components().any(|c| c.as_os_str() == ".."));
        paths.push((category, path.unwrap_or_else(|| home.join(fallback)), false));
    }
    paths.push(("Application Support", home.join(".var/app"), false));
    paths.push(("Temporary", PathBuf::from("/private/tmp"), false));

    let mut out: Vec<Location> = Vec::new();
    for (category, path, hidden_only) in paths {
        let path = PathBuf::from(path.to_string_lossy().trim_end_matches('/'));
        if path.is_dir() && !out.iter().any(|location| location.path == path) {
            out.push(Location { category, path, system: false, depth: 1, resolves_container: false, auto_selectable: true, hidden_only });
        }
    }
    out
}

fn confstr(name: libc::c_int) -> Option<PathBuf> {
    let mut buffer = [0 as libc::c_char; 1024];
    // SAFETY: confstr writes at most buffer.len() bytes, null-terminated, into the buffer.
    let length = unsafe { libc::confstr(name, buffer.as_mut_ptr(), buffer.len()) };
    if length == 0 || length > buffer.len() {
        return None;
    }
    // SAFETY: a successful confstr left a null-terminated string in the buffer.
    let text = unsafe { CStr::from_ptr(buffer.as_ptr()) };
    Some(PathBuf::from(text.to_string_lossy().into_owned()))
}

struct Context<'a> {
    app: &'a Descriptor,
    /// The app's vendor token belongs to no other installed app, so vendor folders are its own.
    promote_vendor: bool,
    collisions: Collisions,
}

/// The app bundle and every file found to belong to it, sized.
pub fn leftovers(app: &App, installed: &[App], include_system: bool) -> Vec<FileItem> {
    let descriptor = app.descriptor();
    // A second copy of the same app is not a competitor: it would veto every match.
    let competitors: Vec<Descriptor> = installed.iter().filter(|other| other.path != app.path && (app.bundle_id.is_none() || other.bundle_id != app.bundle_id)).map(App::descriptor).collect();
    let other_vendors: HashSet<&str> = competitors.iter().flat_map(Descriptor::vendor_tokens).collect();
    let vendors = descriptor.vendor_tokens();
    let context = Context { app: &descriptor, promote_vendor: !vendors.is_empty() && vendors.iter().all(|vendor| !other_vendors.contains(vendor)), collisions: Collisions::new(&competitors) };

    let mut items = Vec::new();
    if !app.is_system {
        items.push(FileItem { why: "the app itself", ..FileItem::new(app.path.clone(), "Application", false, true) });
    }
    let mut locations = locations(include_system);
    locations.extend(home_and_temp_locations());
    items.extend(parallel(&locations, |location| {
        let mut found = Vec::new();
        collect(&location.path, location, &context, location.depth, &mut found);
        found
    }));
    items.extend(cli_links(&app.path));

    // A file both a deep walk and its own location found keeps the selected copy.
    items.sort_by_key(|item| !item.selected);
    let mut seen = HashSet::new();
    items.retain(|item| seen.insert(item.path.clone()));
    measure(&mut items);
    sort(&mut items);
    items
}

/// A matched folder is taken whole; an unmatched one is followed to the location's depth.
fn collect(folder: &Path, location: &Location, context: &Context, remaining: usize, items: &mut Vec<FileItem>) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') != location.hidden_only {
            continue;
        }
        let path = entry.path();
        if matcher::is_protected(&path) {
            continue;
        }
        // file_type does not follow links, so a linked folder is never walked into.
        let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
        let mut found = matcher::classify(&name, context.app);
        if found.is_none() && is_dir && location.resolves_container && remaining == location.depth {
            found = matcher::container_identifier(&path).filter(|id| matcher::container_matches(id, context.app)).map(|_| matcher::Match { strength: Strength::Strong, kind: Kind::BundleId });
        }
        let shallow = location.depth - remaining <= 1;
        if let Some(found) = found
            && (found.strength == Strength::Strong || shallow || context.promote_vendor)
        {
            // Below the top level the name hit sits inside a folder that did not match: another app's.
            let top = remaining == location.depth;
            let claimed = found.kind == Kind::Name && context.collisions.claims(&name);
            let selected = location.auto_selectable
                && match found.kind {
                    Kind::BundleId => found.strength == Strength::Strong,
                    Kind::Name => found.strength == Strength::Strong && top && !claimed,
                    Kind::Vendor => context.promote_vendor,
                };
            let why = match (found.kind, found.strength) {
                (Kind::BundleId, Strength::Strong) => "bundle id",
                (Kind::BundleId, Strength::Weak) => "bundle id followed by more letters",
                (Kind::Name, Strength::Weak) => "starts with the app's name",
                (Kind::Name, _) if !top => "app name, inside another folder",
                (Kind::Name, _) if claimed => "app name, also another installed app's",
                (Kind::Name, _) => "app name",
                (Kind::Vendor, _) => "vendor",
            };
            items.push(FileItem { why, ..FileItem::new(path, location.category, location.system, selected) });
            continue;
        }
        if remaining > 1 && is_dir {
            collect(&path, location, context, remaining - 1, items);
        }
    }
}

/// Links in `/usr/local/bin` that point into the app bundle: CLI tools its installer put on PATH.
fn cli_links(app_path: &Path) -> Vec<FileItem> {
    let mut items = Vec::new();
    let Ok(app_path) = std::fs::canonicalize(app_path) else { return items };
    for folder in ["/usr/local/bin", "/usr/local/sbin"] {
        let Ok(entries) = std::fs::read_dir(folder) else { continue };
        for entry in entries.flatten() {
            let is_link = entry.file_type().is_ok_and(|kind| kind.is_symlink());
            if is_link && std::fs::canonicalize(entry.path()).is_ok_and(|target| target.starts_with(&app_path)) {
                items.push(FileItem { why: "command linked into the app", ..FileItem::new(entry.path(), "Command Line Tools", true, true) });
            }
        }
    }
    items
}

/// Regenerable developer caches are selected up front; toolchains and package stores need a tick.
const DEVELOPER: &[(&str, bool)] = &[
    ("Library/Developer/Xcode/DerivedData", true),
    ("Library/Developer/Xcode/Archives", false),
    ("Library/Developer/Xcode/iOS DeviceSupport", true),
    ("Library/Developer/Xcode/watchOS DeviceSupport", true),
    ("Library/Developer/Xcode/tvOS DeviceSupport", true),
    ("Library/Developer/Xcode/macOS DeviceSupport", true),
    ("Library/Developer/Xcode/DocumentationCache", true),
    ("Library/Developer/CoreSimulator/Caches", true),
    ("Library/Developer/DeveloperDiskImages", true),
    ("Library/Caches/com.apple.dt.Xcode", true),
    ("Library/Caches/com.apple.dt.xcodebuild", true),
    ("Library/Caches/com.apple.dt.Xcode.sourcecontrol.Git", true),
    ("Library/Caches/org.swift.swiftpm", true),
    (".swiftpm/cache", true),
    (".npm/_cacache", true),
    ("Library/Caches/Yarn", true),
    (".yarn/cache", true),
    (".cache/yarn", true),
    (".yarn-cache", true),
    (".pnpm-store", true),
    ("Library/pnpm/store", true),
    (".node-gyp", true),
    (".bun/install/cache", true),
    ("Library/Caches/deno", true),
    (".cache/deno", true),
    (".nvm/versions/node/*", false),
    (".cache/puppeteer", true),
    ("Library/Caches/ms-playwright", true),
    ("Library/Caches/Cypress", true),
    ("Library/Caches/electron", true),
    ("Library/Caches/pip", true),
    (".cache/pip", true),
    ("Library/Caches/pypoetry", true),
    (".cache/pypoetry", true),
    (".cache/uv", true),
    (".local/share/uv", false),
    (".pyenv/cache", true),
    (".gem", false),
    (".bundle/cache", true),
    (".cargo/registry", false),
    (".cargo/git", false),
    (".cache/sccache", true),
    ("Library/Caches/Mozilla.sccache", true),
    ("Library/Caches/go-build", true),
    ("go/pkg/mod/cache", false),
    ("go/bin", false),
    (".gradle/caches", false),
    (".gradle/wrapper", true),
    (".m2/repository", false),
    (".ivy2/cache", true),
    (".sbt/boot", true),
    ("Library/Caches/Coursier", true),
    (".cache/coursier", true),
    ("Library/Caches/CocoaPods", true),
    (".cocoapods/repos", false),
    ("Library/Caches/org.carthage.CarthageKit", true),
    (".composer/cache", true),
    ("Library/Caches/composer", true),
    (".nuget/packages", false),
    ("Library/Caches/NuGet", true),
    (".dotnet", false),
    (".hex/packages", true),
    (".cache/rebar3", true),
    (".stack", false),
    (".cabal/packages", false),
    (".julia/compiled", true),
    (".julia/artifacts", false),
    (".opam/download-cache", true),
    (".cache/shards", true),
    (".cache/zig", true),
    (".cache/nix", true),
    (".cache/clangd", true),
    (".pub-cache", false),
    ("Library/Caches/flutter_engine", true),
    ("Library/Containers/com.docker.docker/Data/vms", false),
    (".local/share/containers", false),
    (".vagrant.d/boxes", false),
    (".minikube/cache", true),
    (".kube/cache", true),
    (".terraform.d/plugin-cache", true),
    ("Library/Caches/helm", true),
    ("Library/Android/sdk/system-images", false),
    (".android", false),
    ("Library/Logs/AndroidStudio", true),
    ("Library/Caches/Google/AndroidStudio*", true),
    ("Library/Caches/com.unity3d.UnityEditor", true),
    ("Library/Unity/cache", true),
    ("Library/Caches/UnrealEngine", true),
    (".cache/bazel", true),
    (".ccache", true),
    ("Library/Caches/Homebrew", true),
    ("miniconda3/pkgs", false),
    ("anaconda3/pkgs", false),
    (".conda/pkgs", false),
    ("Library/Caches/JetBrains", true),
    ("Library/Logs/JetBrains", true),
    ("Library/Caches/Zed", true),
    ("Library/Application Support/Zed/node/cache", true),
    (".vscode/extensions", false),
    (".cursor/extensions", false),
];

/// Electron editors share one regenerable cache layout under `Application Support/<editor>/`.
const ELECTRON_EDITORS: &[&str] = &["Code", "Code - Insiders", "VSCodium", "Cursor", "Windsurf"];
const ELECTRON_CACHES: &[&str] =
    &["Cache", "GPUCache", "CachedData", "CachedExtensionVSIXs", "Code Cache", "CachedConfigurations", "CachedProfilesData", "DawnGraphiteCache", "DawnWebGPUCache", "Service Worker/CacheStorage"];

/// Only a trailing `*` in the last component is used by the catalog.
fn expand(path: PathBuf) -> Vec<PathBuf> {
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let Some(prefix) = name.strip_suffix('*') else { return vec![path] };
    let Some(parent) = path.parent() else { return vec![] };
    let Ok(entries) = std::fs::read_dir(parent) else { return vec![] };
    entries.flatten().filter(|entry| entry.file_name().to_string_lossy().starts_with(prefix)).map(|entry| entry.path()).collect()
}

enum Mode {
    /// Each child of the folder is one row.
    Contents,
    /// The folder itself is one row.
    Whole,
}

/// Caches, logs, saved state and developer caches, independent of any app.
pub fn junk(include_system: bool) -> Vec<FileItem> {
    let home = home();
    let mut catalog = vec![
        ("User Caches", home.join("Library/Caches"), false, Mode::Contents, true),
        ("Saved Application State", home.join("Library/Saved Application State"), false, Mode::Contents, true),
        ("User Logs", home.join("Library/Logs"), false, Mode::Contents, true),
        ("iOS Backups", home.join("Library/Application Support/MobileSync/Backup"), false, Mode::Contents, false),
        ("iOS Software Updates", home.join("Library/iTunes/iPhone Software Updates"), false, Mode::Contents, false),
    ];
    for &(sub, safe) in DEVELOPER {
        for path in expand(home.join(sub)) {
            catalog.push(("Developer", path, false, Mode::Whole, safe));
        }
    }
    for editor in ELECTRON_EDITORS {
        for cache in ELECTRON_CACHES {
            catalog.push(("Developer", home.join("Library/Application Support").join(editor).join(cache), false, Mode::Whole, true));
        }
    }
    if include_system {
        catalog.push(("System Caches", PathBuf::from("/Library/Caches"), true, Mode::Contents, false));
        catalog.push(("System Logs", PathBuf::from("/Library/Logs"), true, Mode::Contents, false));
    }

    let mut items = Vec::new();
    for (category, folder, system, mode, safe) in catalog {
        let paths = match mode {
            Mode::Whole if folder.is_dir() => vec![folder],
            Mode::Whole => vec![],
            Mode::Contents => {
                std::fs::read_dir(folder).map(|entries| entries.flatten().filter(|entry| !entry.file_name().to_string_lossy().starts_with('.')).map(|entry| entry.path()).collect()).unwrap_or_default()
            }
        };
        items.extend(paths.into_iter().filter(|path| !matcher::is_protected(path)).map(|path| FileItem::new(path, category, system, safe)));
    }
    // A developer cache inside ~/Library/Caches is listed once, under the first catalog row.
    let mut seen = HashSet::new();
    items.retain(|item| seen.insert(item.path.clone()));
    let kept: HashSet<PathBuf> = items.iter().map(|item| item.path.clone()).collect();
    items.retain(|item| !item.path.ancestors().skip(1).any(|ancestor| kept.contains(ancestor)));
    measure(&mut items);
    items.retain(|item| item.size > 0);
    sort(&mut items);
    items
}

const ORPHAN_CATEGORIES: &[&str] =
    &["Application Support", "Caches", "Preferences", "Containers", "Group Containers", "Saved Application State", "HTTPStorages", "Logs", "WebKit", "Application Scripts"];

/// Folder names that look app-named but are shared vendor or system folders.
const VENDOR_ALLOWLIST: &[&str] = &[
    "google",
    "microsoft",
    "adobe",
    "mozilla",
    "apple",
    "comapple",
    "crashreporter",
    "mobilesync",
    "caches",
    "logs",
    "preferences",
    "containers",
    "groupcontainers",
    "knowledge",
    "icloud",
    "homebrew",
    "java",
    "jetbrains",
    "steam",
    "epicgames",
    "discord",
];

const HIGH: &str = "Orphans (High confidence)";
const MEDIUM: &str = "Orphans (Medium confidence)";
const LOW: &str = "Orphans (Low confidence)";

/// Files of apps no longer installed. High: a launch item whose program is gone. Medium: a
/// reverse-DNS name no installed app owns. Low: an app-named Application Support folder.
pub fn orphans(installed: &[App], include_system: bool) -> Vec<FileItem> {
    let index = Index::new(installed);
    let mut items = Vec::new();
    let home = home();
    for (folder, system) in [(home.join("Library/LaunchAgents"), false), ("/Library/LaunchAgents".into(), true), ("/Library/LaunchDaemons".into(), true)] {
        let Ok(entries) = std::fs::read_dir(folder) else { continue };
        for path in entries.flatten().map(|entry| entry.path()) {
            if path.extension().is_some_and(|ext| ext == "plist")
                && !matcher::is_protected(&path)
                && program_path(&path).is_some_and(|program| program.starts_with('/') && std::fs::symlink_metadata(&program).is_err())
            {
                items.push(FileItem { auto_selectable: true, ..FileItem::new(path, HIGH, system, false) });
            }
        }
    }
    for location in locations(include_system).iter().filter(|location| ORPHAN_CATEGORIES.contains(&location.category)) {
        let Ok(entries) = std::fs::read_dir(&location.path) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if name.starts_with('.') || matcher::is_protected(&path) {
                continue;
            }
            let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
            let mut identifier = [".plist", ".savedState", ".binarycookies"].iter().find_map(|ext| name.strip_suffix(ext)).unwrap_or(&name).to_owned();
            if !is_reverse_dns(&identifier)
                && location.resolves_container
                && let Some(id) = matcher::container_identifier(&path)
            {
                identifier = id;
            }
            if let Some(category) = orphan_confidence(&name, &identifier, is_dir, location.category, &index) {
                items.push(FileItem::new(path, category, location.system, false));
            }
        }
    }
    let mut seen = HashSet::new();
    items.retain(|item| seen.insert(item.path.clone()));
    measure(&mut items);
    sort(&mut items);
    items
}

fn orphan_confidence(name: &str, identifier: &str, is_dir: bool, category: &str, index: &Index) -> Option<&'static str> {
    if is_reverse_dns(identifier) {
        return (!index.owns_identifier(identifier) && !index.matches_name(identifier)).then_some(MEDIUM);
    }
    let normalized = matcher::normalize(name);
    let app_named = category == "Application Support" && is_dir && normalized.chars().count() >= 4 && !VENDOR_ALLOWLIST.contains(&normalized.as_str());
    (app_named && !index.matches_name(name)).then_some(LOW)
}

fn is_reverse_dns(text: &str) -> bool {
    text.find('.').is_some_and(|dot| dot > 0)
}

/// The program a launchd plist starts.
pub fn program_path(plist_path: &Path) -> Option<String> {
    let value = plist::Value::from_file(plist_path).ok()?;
    let dict = value.as_dictionary()?;
    let first_argument = dict.get("ProgramArguments").and_then(|args| args.as_array()?.first()?.as_string());
    let program = dict.get("Program").and_then(plist::Value::as_string).or(first_argument).or_else(|| dict.get("BundleProgram")?.as_string());
    program.filter(|program| !program.is_empty()).map(str::to_owned)
}

pub fn launchd_label(plist_path: &Path) -> Option<String> {
    let value = plist::Value::from_file(plist_path).ok()?;
    let label = value.as_dictionary()?.get("Label")?.as_string()?;
    (!label.is_empty() && !label.contains("..") && !label.starts_with('/')).then(|| label.to_owned())
}

/// Whether the app list should drop this bundle: it was moved away.
pub fn gone(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_err() || apps::is_in_trash(&path.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, name: &str) -> App {
        App {
            path: format!("/Applications/{name}.app").into(),
            bundle_id: Some(id.into()),
            name: name.into(),
            bundle_name: None,
            executable: None,
            helper_ids: vec![],
            is_system: false,
            version: None,
            size: None,
            installed: 0,
        }
    }

    #[test]
    fn orphan_confidence_follows_ownership() {
        let index = Index::new(&[app("com.spotify.client", "Spotify")]);
        assert_eq!(orphan_confidence("com.spotify.client.plist", "com.spotify.client", false, "Preferences", &index), None);
        assert_eq!(orphan_confidence("com.gone.app.plist", "com.gone.app", false, "Preferences", &index), Some(MEDIUM));
        assert_eq!(orphan_confidence("Gone App", "Gone App", true, "Application Support", &index), Some(LOW));
        assert_eq!(orphan_confidence("Spotify", "Spotify", true, "Application Support", &index), None);
        assert_eq!(orphan_confidence("Google", "Google", true, "Application Support", &index), None);
        assert_eq!(orphan_confidence("Gone App", "Gone App", true, "Caches", &index), None);
    }

    #[test]
    fn leftovers_select_strong_matches_and_leave_shared_vendor_folders() {
        let root = std::env::temp_dir().join(format!("cleaner-scan-{}", std::process::id()));
        let library = root.join("Library");
        for folder in ["Preferences", "Application Support/Spotify", "Application Support/Other/Spotify", "Caches/com.spotify.client"] {
            std::fs::create_dir_all(library.join(folder)).unwrap();
        }
        std::fs::write(library.join("Preferences/com.spotify.client.plist"), b"x").unwrap();
        let location = |category, sub: &str, depth| Location { category, path: library.join(sub), system: false, depth, resolves_container: false, auto_selectable: true, hidden_only: false };
        let descriptor = app("com.spotify.client", "Spotify").descriptor();
        let context = Context { app: &descriptor, promote_vendor: false, collisions: Collisions::new(&[]) };
        let mut found = Vec::new();
        for location in [location("Preferences", "Preferences", 1), location("Application Support", "Application Support", 3), location("Caches", "Caches", 2)] {
            collect(&location.path, &location, &context, location.depth, &mut found);
        }
        let mut names: Vec<_> = found.iter().map(|item| (item.path.strip_prefix(&library).unwrap().to_string_lossy().into_owned(), item.selected)).collect();
        names.sort();
        assert_eq!(
            names,
            [
                ("Application Support/Other/Spotify".into(), false),
                ("Application Support/Spotify".into(), true),
                ("Caches/com.spotify.client".into(), true),
                ("Preferences/com.spotify.client.plist".into(), true)
            ]
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
