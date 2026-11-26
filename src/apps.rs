use crate::matcher::{Descriptor, normalize};
use std::collections::HashSet;
#[cfg(target_os = "macos")]
use std::os::macos::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Apps sit one or two folders down (`/Applications/Utilities`, `~/Applications/JetBrains Toolbox`).
#[cfg(target_os = "macos")]
const MAX_ROOT_DEPTH: usize = 4;
#[cfg(target_os = "macos")]
const SF_RESTRICTED: u32 = 0x0008_0000;
#[cfg(target_os = "macos")]
const HELPER_DIRS: &[&str] = &["Library/LoginItems", "Library/SystemExtensions", "Library/Spotlight", "Library/QuickLook", "PlugIns", "Extensions", "XPCServices", "Frameworks"];

#[derive(Clone, Debug)]
pub struct App {
    pub path: PathBuf,
    pub bundle_id: Option<String>,
    pub name: String,
    pub bundle_name: Option<String>,
    pub executable: Option<String>,
    /// Bundle ids of login items, plug-ins and XPC services: their files belong to this app.
    pub helper_ids: Vec<String>,
    /// The OS owns the app itself: Apple's SIP bundles on macOS, a package manager's on Linux. Only its leftovers go.
    pub is_system: bool,
    pub version: Option<String>,
    /// Size on disk of what `payload` names, None until measured after listing.
    pub size: Option<u64>,
    /// Seconds since the epoch the bundle was created here, close to its install date.
    pub installed: i64,
}

impl App {
    pub fn descriptor(&self) -> Descriptor {
        let file_name = self.path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
        let ids = self.bundle_id.iter().chain(&self.helper_ids).map(String::as_str);
        let names = [self.name_term(), self.bundle_name.as_deref(), Some(file_name.as_str()), self.executable.as_deref()];
        Descriptor::new(ids, names.into_iter().flatten())
    }

    /// The display name as a match term. A Linux app with a desktop id goes without it: GNOME and KDE
    /// name theirs Fonts, Files or Settings, which would select `~/.local/share/fonts`.
    pub fn name_term(&self) -> Option<&str> {
        (cfg!(target_os = "macos") || self.bundle_id.is_none()).then_some(self.name.as_str())
    }
}

#[cfg(target_os = "macos")]
pub fn installed() -> Vec<App> {
    let home = std::env::var("HOME").unwrap_or_default();
    let roots = [
        PathBuf::from("/Applications"),
        PathBuf::from(&home).join("Applications"),
        PathBuf::from("/System/Applications"),
        PathBuf::from("/System/Library/CoreServices/Applications"),
        PathBuf::from("/Network/Applications"),
    ];
    let mut bundles = Vec::new();
    for root in &roots {
        find_bundles(root, 0, &mut bundles);
    }
    let mut seen = HashSet::new();
    let mut apps: Vec<App> =
        bundles.into_iter().filter(|path| seen.insert(std::fs::canonicalize(path).unwrap_or_else(|_| path.clone()).to_string_lossy().to_lowercase())).filter_map(|path| read(&path)).collect();
    apps.sort_by_cached_key(|app| app.name.to_lowercase());
    apps
}

/// Every `.app` at or below `folder`, never descending into a bundle.
#[cfg(target_os = "macos")]
fn find_bundles(folder: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if path.extension().is_some_and(|ext| ext == "app") {
            found.push(path);
        } else if depth + 1 < MAX_ROOT_DEPTH && entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            find_bundles(&path, depth + 1, found);
        }
    }
}

#[cfg(target_os = "macos")]
pub fn read(path: &Path) -> Option<App> {
    if !path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("app")) || !path.is_dir() || is_in_trash(&path.to_string_lossy()) {
        return None;
    }
    let info = info_plist(path);
    let field = |key: &str| info.as_ref().and_then(|dict| dict.get(key)?.as_string()).filter(|value| !value.is_empty()).map(str::to_owned);
    let bundle_id = field("CFBundleIdentifier");
    let bundle_name = field("CFBundleName");
    let file_name = path.file_stem()?.to_string_lossy().into_owned();
    let name = field("CFBundleDisplayName").or_else(|| bundle_name.clone()).unwrap_or(file_name);
    Some(App {
        helper_ids: helper_ids(path, bundle_id.as_deref()),
        is_system: is_system_bundle(path),
        bundle_name: bundle_name.filter(|bundle_name| *bundle_name != name),
        executable: field("CFBundleExecutable"),
        version: field("CFBundleShortVersionString"),
        size: None,
        installed: std::fs::metadata(path).map_or(0, |meta| meta.st_birthtime()),
        path: path.to_path_buf(),
        bundle_id,
        name,
    })
}

#[cfg(target_os = "macos")]
fn info_plist(bundle: &Path) -> Option<plist::Dictionary> {
    let value = plist::Value::from_file(bundle.join("Contents/Info.plist")).ok()?;
    value.into_dictionary()
}

/// Everything under `/System/`, plus built-ins Apple links into `/Applications` from a cryptex
/// (Safari). The flag sits on the link itself; an unreadable bundle counts as system.
#[cfg(target_os = "macos")]
fn is_system_bundle(path: &Path) -> bool {
    path.starts_with("/System/") || std::fs::symlink_metadata(path).map_or(true, |meta| meta.st_flags() & SF_RESTRICTED != 0)
}

#[cfg(target_os = "macos")]
fn helper_ids(bundle: &Path, primary: Option<&str>) -> Vec<String> {
    let mut ids = Vec::new();
    for sub in HELPER_DIRS {
        let Ok(entries) = std::fs::read_dir(bundle.join("Contents").join(sub)) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let ext = path.extension().map(|ext| ext.to_string_lossy().to_lowercase()).unwrap_or_default();
            if *sub == "Frameworks" && ext != "app" && ext != "xpc" {
                continue;
            }
            let id = info_plist(&path).and_then(|dict| dict.get("CFBundleIdentifier")?.as_string().map(str::to_owned));
            if let Some(id) = id
                && Some(id.as_str()) != primary
                && !ids.contains(&id)
            {
                ids.push(id);
            }
        }
    }
    ids.sort();
    ids
}

/// What the size column measures: the bundle on macOS. A Linux launcher is a few bytes, so there it is
/// the flatpak or the program the launcher starts, and the launcher when neither is found.
#[cfg(target_os = "macos")]
pub fn payload(app: &App) -> PathBuf {
    app.path.clone()
}

#[cfg(target_os = "linux")]
pub fn payload(app: &App) -> PathBuf {
    let flatpak = app.bundle_id.iter().flat_map(|id| flatpak_roots().into_iter().map(move |root| root.join(id))).find(|path| path.is_dir());
    flatpak.or_else(|| launched_file(app)).unwrap_or_else(|| app.path.clone())
}

/// The file the launcher starts. A wrapper is skipped before its link is followed: `steam` resolves
/// to `bin_steam.sh`, which no longer reads as one.
#[cfg(target_os = "linux")]
fn launched_file(app: &App) -> Option<PathBuf> {
    let program = launcher_program(&app.path)?;
    if is_wrapper(Path::new(&program)) {
        return None;
    }
    let path = std::fs::canonicalize(which(&program)?).ok()?;
    path.is_file().then_some(path)
}

/// The `app` folders of every flatpak installation: system, user, and the extra ones an
/// `installations.d` file declares (an SD card on the Steam Deck).
#[cfg(target_os = "linux")]
pub fn flatpak_roots() -> Vec<PathBuf> {
    let user = std::env::var_os("FLATPAK_USER_DIR").map_or_else(|| crate::scan::xdg("XDG_DATA_HOME", ".local/share").join("flatpak"), PathBuf::from);
    let mut roots = vec![PathBuf::from("/var/lib/flatpak"), user];
    for entry in std::fs::read_dir("/etc/flatpak/installations.d").into_iter().flatten().flatten() {
        let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
        roots.extend(text.lines().filter_map(|line| line.trim().strip_prefix("Path=")).map(PathBuf::from));
    }
    roots.into_iter().map(|root| root.join("app")).collect()
}

/// Where the user's own launchers live; everything else in the menu a package manager installed.
#[cfg(target_os = "linux")]
pub fn user_launchers() -> PathBuf {
    crate::scan::xdg("XDG_DATA_HOME", ".local/share").join("applications")
}

/// Folders of desktop entries, the user's first: an entry id found twice keeps the first, as menus do.
#[cfg(target_os = "linux")]
fn desktop_folders() -> Vec<PathBuf> {
    let data_dirs = std::env::var("XDG_DATA_DIRS").ok().filter(|dirs| !dirs.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    let mut folders = vec![user_launchers()];
    let mut shares: Vec<PathBuf> = std::env::split_paths(&data_dirs).collect();
    shares.extend(flatpak_roots().iter().filter_map(|apps| Some(apps.parent()?.join("exports/share"))));
    shares.push("/var/lib/snapd/desktop".into());
    for folder in shares.into_iter().map(|share| share.join("applications")) {
        if !folders.contains(&folder) {
            folders.push(folder);
        }
    }
    folders
}

/// Every app with a menu entry. Package managers own the ones outside the user's launchers, so those
/// count as system: their leftovers can go, the app itself goes with `dnf`, `apt`, `flatpak` or `snap`.
#[cfg(target_os = "linux")]
pub fn installed() -> Vec<App> {
    let mut apps: Vec<App> = Vec::new();
    // Desktop file id to the index of its app, None when the first entry was hidden.
    let mut first: std::collections::HashMap<String, Option<usize>> = std::collections::HashMap::new();
    for folder in desktop_folders() {
        let mut entries = Vec::new();
        desktop_files(&folder, "", &mut entries);
        entries.sort();
        for (id, path) in entries {
            if let Some(&shadowed) = first.get(&id) {
                // A user copy of a package's launcher (edited flags) does not make the app the user's.
                if let Some(index) = shadowed {
                    apps[index].is_system = true;
                }
                continue;
            }
            let app = read(&path);
            first.insert(id, app.is_some().then_some(apps.len()));
            apps.extend(app);
        }
    }
    apps.sort_by_cached_key(|app| app.name.to_lowercase());
    apps
}

/// The desktop entries below `folder` with their desktop file ids: `wine/Programs/Foo.desktop` is
/// `wine-Programs-Foo.desktop`. Linked folders are never entered.
#[cfg(target_os = "linux")]
pub fn desktop_files(folder: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            desktop_files(&path, &format!("{prefix}{name}-"), out);
        } else if name.ends_with(".desktop") {
            out.push((format!("{prefix}{name}"), path));
        }
    }
}

#[cfg(target_os = "linux")]
pub fn read(path: &Path) -> Option<App> {
    let entry = desktop_entry(path)?;
    let field = |key: &str| entry.iter().find(|(name, _)| name == key).map(|(_, value)| value.as_str()).filter(|value| !value.is_empty());
    if field("Type") != Some("Application") || field("NoDisplay") == Some("true") || field("Hidden") == Some("true") || is_in_trash(&path.to_string_lossy()) {
        return None;
    }
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    // A desktop file id is reverse DNS when the app has one (org.kde.dolphin), as is every flatpak's.
    let bundle_id = field("X-Flatpak").map(str::to_owned).or_else(|| is_reverse_dns(&stem).then(|| stem.clone()));
    let name = field("Name").map_or(stem, str::to_owned);
    let program = field("Exec").and_then(exec_program);
    // Steam, Lutris and Heroic write launchers for the games they installed: the game goes with them.
    let wrapped = program.as_deref().is_some_and(|program| is_wrapper(Path::new(program)));
    let executable = program.filter(|_| !wrapped).as_deref().map(Path::new).and_then(Path::file_name).map(|name| name.to_string_lossy().into_owned());
    let created = std::fs::metadata(path).and_then(|meta| meta.created().or_else(|_| meta.modified()));
    let installed = created.ok().and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |elapsed| elapsed.as_secs() as i64);
    Some(App {
        is_system: wrapped || !path.starts_with(user_launchers()),
        bundle_name: field("StartupWMClass").filter(|class| *class != name && !is_wrapper(Path::new(class))).map(str::to_owned),
        executable,
        helper_ids: vec![],
        version: None,
        size: None,
        installed,
        path: path.to_path_buf(),
        bundle_id,
        name,
    })
}

/// The unlocalized keys of the `[Desktop Entry]` group, values unescaped.
#[cfg(target_os = "linux")]
fn desktop_entry(path: &Path) -> Option<Vec<(String, String)>> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut group = "";
    let mut entry = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            group = line;
        } else if group == "[Desktop Entry]"
            && let Some((key, value)) = line.split_once('=')
            && !key.contains('[')
        {
            entry.push((key.trim().to_owned(), unescape(value.trim())));
        }
    }
    Some(entry)
}

/// The spec's value escapes; any other backslash is left for the Exec quoting rules.
#[cfg(target_os = "linux")]
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => out.extend(['\\', other]),
            None => out.push('\\'),
        }
    }
    out
}

/// The program an Exec line starts, as written: quotes removed, `env` with its options and variables skipped.
#[cfg(target_os = "linux")]
pub fn exec_program(exec: &str) -> Option<String> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut quoted = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                word.get_or_insert_default();
            }
            '\\' if quoted => word.get_or_insert_default().extend(chars.next()),
            // Not in the spec, but AppImage tools write `Exec=env A=1 '/path/app.appimage'` and launchers accept it.
            '\'' if !quoted => word.get_or_insert_default().extend(chars.by_ref().take_while(|&c| c != '\'')),
            c if c.is_whitespace() && !quoted => words.extend(word.take()),
            c => word.get_or_insert_default().push(c),
        }
    }
    words.extend(word);
    let mut words = words.into_iter().peekable();
    if words.peek().is_some_and(|first| Path::new(first).file_name().is_some_and(|name| name == "env")) {
        words.next();
        while let Some(option) = words.next_if(|word| word.contains('=') || word.starts_with('-')) {
            // These options take the next word as their value: `env -u WAYLAND_DISPLAY app`.
            if matches!(option.as_str(), "-u" | "--unset" | "-C" | "--chdir" | "-S" | "--split-string") {
                words.next();
            }
        }
    }
    words.next().filter(|program| !program.is_empty())
}

#[cfg(target_os = "linux")]
pub fn launcher_program(desktop: &Path) -> Option<String> {
    let entry = desktop_entry(desktop)?;
    entry.iter().find(|(key, _)| key == "Exec").and_then(|(_, exec)| exec_program(exec))
}

/// A user file that switches a package's unit or autostart entry off: a mask linked to /dev/null, or an
/// entry with `Hidden=true`. Trashing it turns the package's own back on at the next login.
#[cfg(target_os = "linux")]
pub fn is_override(path: &Path) -> bool {
    let masked = std::fs::read_link(path).is_ok_and(|target| target == Path::new("/dev/null"));
    let off = |(key, value): &(String, String)| (key == "Hidden" && value == "true") || (key == "X-GNOME-Autostart-enabled" && value == "false");
    masked || desktop_entry(path).is_some_and(|entry| entry.iter().any(off))
}

/// A program as the session would find it: an absolute path as is, a bare name on PATH.
pub fn which(program: &str) -> Option<PathBuf> {
    if program.starts_with('/') {
        return std::fs::symlink_metadata(program).is_ok().then(|| PathBuf::from(program));
    }
    if program.contains('/') {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|folder| folder.join(program)).find(|candidate| candidate.is_file())
}

/// Programs that start other programs: their name says nothing about the app, and `steam` or
/// `flatpak` as a leftover name would take the whole Steam library or every flatpak.
#[cfg(target_os = "linux")]
fn is_wrapper(program: &Path) -> bool {
    const WRAPPERS: &[&str] = &[
        "flatpak",
        "flatpak-spawn",
        "snap",
        "sh",
        "bash",
        "zsh",
        "dash",
        "gtk-launch",
        "gio",
        "xdg-open",
        "kioclient",
        "kioclient5",
        "kioclient6",
        "wine",
        "wine64",
        "steam",
        "lutris",
        "heroic",
        "python",
        "python3",
        "java",
        "node",
        "electron",
        "mono",
        "dotnet",
        "appimage-run",
        "sudo",
        "pkexec",
        "gamemoderun",
        "gamescope",
        "mangohud",
        "prime-run",
        "optirun",
        "primusrun",
        "firejail",
        "bwrap",
        "distrobox-enter",
        "systemd-run",
        "nice",
        "taskset",
    ];
    program.file_name().is_some_and(|name| WRAPPERS.contains(&name.to_string_lossy().to_lowercase().as_str()))
}

/// Whether a name is an identifier such as `com.foo.Bar`. On Linux `mimeapps.list` and `dnf5.log.1`
/// have dots too, so there it also needs three parts and a top-level domain of 2 to 6 lowercase
/// letters first (com, org, io, design).
pub fn is_reverse_dns(text: &str) -> bool {
    if cfg!(target_os = "linux") {
        let mut parts = text.split('.');
        let tld = parts.next().unwrap_or_default();
        return (2..=6).contains(&tld.len()) && tld.bytes().all(|byte| byte.is_ascii_lowercase()) && parts.count() >= 2 && !text.contains(char::is_whitespace);
    }
    text.find('.').is_some_and(|dot| dot > 0)
}

/// Trashed files are already on their way out and never count as installed or as leftovers.
pub fn is_in_trash(path: &str) -> bool {
    let finder = path.contains("/.Trash/") || path.contains("/.Trashes/") || path.ends_with("/.Trash") || path.ends_with("/.Trashes");
    finder || (cfg!(target_os = "linux") && in_freedesktop_trash(path))
}

/// The freedesktop Trash: the user's in `~/.local/share/Trash`, a volume's in `<volume>/.Trash-<uid>`.
fn in_freedesktop_trash(path: &str) -> bool {
    // SAFETY: getuid has no preconditions.
    let volume = format!("/.Trash-{}", unsafe { libc::getuid() });
    Path::new(path).starts_with(crate::scan::home().join(".local/share/Trash")) || path.contains(&format!("{volume}/")) || path.ends_with(&volume)
}

/// Who owns an identifier or a folder name, for the orphan scan.
pub struct Index {
    ids: HashSet<String>,
    names: HashSet<String>,
}

impl Index {
    pub fn new(apps: &[App]) -> Self {
        let mut index = Index { ids: HashSet::new(), names: HashSet::new() };
        for app in apps {
            index.ids.extend(app.bundle_id.iter().chain(&app.helper_ids).map(|id| id.to_lowercase()));
            index.names.extend([Some(&app.name), app.executable.as_ref()].into_iter().flatten().map(|name| normalize(name)).filter(|name| !name.is_empty()));
        }
        index
    }

    /// Exact ids, helper children (`com.foo.Bar.helper`) and app-group prefixes (`group.com.foo.Bar`).
    pub fn owns_identifier(&self, candidate: &str) -> bool {
        let candidate = candidate.to_lowercase();
        self.ids.contains(&candidate) || self.ids.iter().any(|id| candidate.starts_with(&format!("{id}.")) || candidate.ends_with(&format!(".{id}")))
    }

    pub fn matches_name(&self, candidate: &str) -> bool {
        self.names.contains(&normalize(candidate))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_owns_helpers_and_app_groups() {
        let app = App {
            path: "/Applications/Bar.app".into(),
            bundle_id: Some("com.foo.Bar".into()),
            name: "Bar Pro".into(),
            bundle_name: None,
            executable: Some("bar".into()),
            helper_ids: vec![],
            is_system: false,
            version: None,
            size: None,
            installed: 0,
        };
        let index = Index::new(&[app]);
        assert!(index.owns_identifier("com.foo.bar"));
        assert!(index.owns_identifier("com.foo.Bar.helper"));
        assert!(index.owns_identifier("4FG648TM2A.group.com.foo.Bar"));
        assert!(!index.owns_identifier("com.foo.Barista"));
        assert!(index.matches_name("Bar-Pro"));
        assert!(!index.matches_name(""));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn exec_lines_name_the_program_they_start() {
        assert_eq!(exec_program(r#"env -i FOO=1 "/opt/My App/app" --flag %U"#).as_deref(), Some("/opt/My App/app"));
        assert_eq!(exec_program("flatpak run org.x.Y").as_deref(), Some("flatpak"));
        assert_eq!(exec_program(r#""/a/say \"hi\"" %f"#).as_deref(), Some(r#"/a/say "hi""#));
        assert_eq!(exec_program("env DESKTOPINTEGRATION=1 '/home/a/My App.appimage' %U").as_deref(), Some("/home/a/My App.appimage"));
        assert_eq!(exec_program("  "), None);
        assert_eq!(exec_program("env -u WAYLAND_DISPLAY /opt/x/x %U").as_deref(), Some("/opt/x/x"));
        assert_eq!(exec_program("/usr/bin/env -C /opt/x ./run").as_deref(), Some("./run"));
    }

    #[cfg(target_os = "linux")]
    fn test_app() -> App {
        App {
            path: "/usr/share/applications/x.desktop".into(),
            bundle_id: None,
            name: String::new(),
            bundle_name: None,
            executable: None,
            helper_ids: vec![],
            is_system: true,
            version: None,
            size: None,
            installed: 0,
        }
    }

    #[test]
    fn freedesktop_trash_is_the_user_s_own() {
        assert!(is_in_trash("/Users/a/.Trash/Foo.app"));
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        let volume = format!("/mnt/disk/.Trash-{uid}/files/x");
        assert_eq!(is_in_trash(&volume), cfg!(target_os = "linux"), "only Linux has freedesktop Trash folders");
        assert!(!is_in_trash("/tmp/.Trash-x/payload"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn desktop_ids_stay_out_of_name_matching() {
        let app = App { bundle_id: Some("org.gnome.font-viewer".into()), name: "Fonts".into(), ..test_app() };
        assert_eq!(crate::matcher::classify("fonts", &app.descriptor()), None, "~/.local/share/fonts is not the app's");
        let id_less = App { bundle_id: None, name: "Fonts".into(), ..test_app() };
        assert!(crate::matcher::classify("fonts", &id_less.descriptor()).is_some());
        let flathub = App { bundle_id: Some("io.github.someone.Tool".into()), ..test_app() };
        assert!(!flathub.descriptor().vendor_tokens().contains(&"github"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn desktop_entries_read_as_apps() {
        let root = std::env::temp_dir().join(format!("cleaner-desktop-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let entry = |file: &str, body: &str| {
            std::fs::write(root.join(file), format!("[Desktop Entry]\nType=Application\n{body}\n[Desktop Action new]\nName=Other\nExec=other\n")).unwrap();
            read(&root.join(file))
        };
        let app = entry("org.foo.Bar.desktop", "Name=Bar\\sPro\nName[pl]=Bar PL\nExec=env A=1 /opt/bar/bar-bin %U\nStartupWMClass=bar-window").unwrap();
        assert_eq!((app.name.as_str(), app.bundle_id.as_deref(), app.executable.as_deref(), app.bundle_name.as_deref()), ("Bar Pro", Some("org.foo.Bar"), Some("bar-bin"), Some("bar-window")));
        assert!(app.is_system, "outside ~/.local/share/applications a package manager owns it");
        let flatpak = entry("brave.desktop", "Name=Brave\nExec=/usr/bin/flatpak run com.brave.Browser\nX-Flatpak=com.brave.Browser").unwrap();
        assert_eq!((flatpak.bundle_id.as_deref(), flatpak.executable), (Some("com.brave.Browser"), None));
        assert!(entry("hidden.desktop", "Name=Hidden\nNoDisplay=true\nExec=hidden").is_none());
        assert!(entry("emacs-29.1.desktop", "Name=Emacs\nExec=emacs").is_some_and(|app| app.bundle_id.is_none()));
        assert!(entry("blender-4.1.1.desktop", "Name=Blender\nExec=blender").is_some_and(|app| app.bundle_id.is_none()), "a version is not reverse DNS");
        let game = entry("Terraria.desktop", "Name=Terraria\nExec=steam steam://rungameid/105600").unwrap();
        assert!(game.executable.is_none(), "steam names no app");
        std::fs::write(root.join("masked.service"), "").unwrap();
        std::os::unix::fs::symlink("/dev/null", root.join("mask.service")).unwrap();
        assert!(is_override(&root.join("mask.service")) && !is_override(&root.join("masked.service")));
        std::fs::write(root.join("off.desktop"), "[Desktop Entry]\nType=Application\nHidden=true\n").unwrap();
        assert!(is_override(&root.join("off.desktop")));
        std::fs::create_dir_all(root.join("wine/Programs")).unwrap();
        std::fs::write(root.join("wine/Programs/Foo.desktop"), "").unwrap();
        let mut found = Vec::new();
        desktop_files(&root, "", &mut found);
        assert!(found.iter().any(|(id, _)| id == "wine-Programs-Foo.desktop"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
