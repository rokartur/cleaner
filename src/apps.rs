use crate::matcher::{Descriptor, normalize};
use std::collections::HashSet;
use std::os::macos::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Apps sit one or two folders down (`/Applications/Utilities`, `~/Applications/JetBrains Toolbox`).
const MAX_ROOT_DEPTH: usize = 4;
const SF_RESTRICTED: u32 = 0x0008_0000;
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
    /// Apple's own SIP-protected bundles, never removable.
    pub is_system: bool,
    pub version: Option<String>,
    /// Bundle size on disk, None until measured after listing.
    pub size: Option<u64>,
    /// Seconds since the epoch the bundle was created here, close to its install date.
    pub installed: i64,
}

impl App {
    pub fn descriptor(&self) -> Descriptor {
        let file_name = self.path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
        let ids = self.bundle_id.iter().chain(&self.helper_ids).map(String::as_str);
        let names = [Some(self.name.as_str()), self.bundle_name.as_deref(), Some(file_name.as_str()), self.executable.as_deref()];
        Descriptor::new(ids, names.into_iter().flatten())
    }
}

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

fn info_plist(bundle: &Path) -> Option<plist::Dictionary> {
    let value = plist::Value::from_file(bundle.join("Contents/Info.plist")).ok()?;
    value.into_dictionary()
}

/// Everything under `/System/`, plus built-ins Apple links into `/Applications` from a cryptex
/// (Safari). The flag sits on the link itself; an unreadable bundle counts as system.
fn is_system_bundle(path: &Path) -> bool {
    path.starts_with("/System/") || std::fs::symlink_metadata(path).map_or(true, |meta| meta.st_flags() & SF_RESTRICTED != 0)
}

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

/// Trashed files are already on their way out and never count as installed or as leftovers.
pub fn is_in_trash(path: &str) -> bool {
    path.contains("/.Trash/") || path.contains("/.Trashes/") || path.ends_with("/.Trash") || path.ends_with("/.Trashes")
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
}
