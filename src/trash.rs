use crate::apps::is_in_trash;
use crate::matcher::is_protected;
use crate::scan::{FileItem, format_bytes};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::ffi::CString;
use std::io::ErrorKind;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_ENTRIES: usize = 1000;
const ORIGIN_ATTR: &str = "com.rokartur.cleaner.origin";
const DATE_ATTR: &str = "com.rokartur.cleaner.removedAt";

#[derive(Clone, Debug)]
pub struct Move {
    pub from: PathBuf,
    pub to: PathBuf,
    pub system: bool,
    pub size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Trash,
    Restore,
}

/// One removal or restore. Moves that hit a permission error wait in `pending` for sudo.
#[derive(Debug)]
pub struct Batch {
    pub kind: Kind,
    pub origin: String,
    pub secs: i64,
    /// The dated folder in the Trash this removal is grouped into.
    pub folder: PathBuf,
    pub moved: Vec<Move>,
    pub pending: Vec<Move>,
    pub failed: Vec<(PathBuf, String)>,
    pub skipped: usize,
}

#[cfg(not(test))]
fn trash_root() -> PathBuf {
    crate::scan::home().join(".Trash")
}

#[cfg(test)]
fn trash_root() -> PathBuf {
    std::env::temp_dir().join(format!("cleaner-test-{}/.Trash", std::process::id()))
}

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs() as i64)
}

/// Moves the items into one `<origin> — <date>` folder in the Trash.
pub fn move_to_trash<'a>(items: impl IntoIterator<Item = &'a FileItem>, origin: &str) -> Batch {
    let secs = now();
    let folder = make_folder(origin, secs);
    let mut batch = Batch { kind: Kind::Trash, origin: origin.to_owned(), secs, folder, moved: vec![], pending: vec![], failed: vec![], skipped: 0 };
    let trash_device = std::fs::metadata(trash_root()).map(|meta| meta.dev()).ok();
    let mut used = HashSet::new();
    for item in items {
        // Checked again at removal time, on the path and on what a link points to.
        if is_protected(&item.path) || std::fs::canonicalize(&item.path).is_ok_and(|target| is_protected(&target)) {
            batch.skipped += 1;
            continue;
        }
        let meta = match std::fs::symlink_metadata(&item.path) {
            Ok(meta) => meta,
            Err(error) => {
                batch.failed.push((item.path.clone(), error.to_string()));
                continue;
            }
        };
        // shortcut: a rename cannot cross volumes and copying would fill the boot disk; use Finder for external drives.
        if trash_device.is_some_and(|device| device != meta.dev()) {
            batch.failed.push((item.path.clone(), "on another volume, move it to the Trash in Finder".into()));
            continue;
        }
        let name = item.path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        let to = batch.folder.join(unique_name(&batch.folder, &name, &mut used));
        tag(&item.path, origin, secs);
        let next = Move { from: item.path.clone(), to, system: item.system, size: item.size };
        match std::fs::rename(&next.from, &next.to) {
            Ok(()) => batch.moved.push(next),
            Err(error) if error.kind() == ErrorKind::PermissionDenied => batch.pending.push(next),
            Err(error) => batch.failed.push((next.from, error.to_string())),
        }
    }
    batch
}

/// The files of a history entry that can still go back: still in a Trash, original place free.
pub fn restore(entry: &Entry) -> Batch {
    let mut batch = Batch { kind: Kind::Restore, origin: entry.origin.clone(), secs: now(), folder: PathBuf::new(), moved: vec![], pending: vec![], failed: vec![], skipped: 0 };
    for file in restorable(entry) {
        if let Some(parent) = file.to.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::rename(&file.from, &file.to) {
            Ok(()) => batch.moved.push(file),
            Err(error) if error.kind() == ErrorKind::PermissionDenied => batch.pending.push(file),
            Err(error) => batch.failed.push((file.to, error.to_string())),
        }
    }
    batch
}

/// Trash moves back to their original paths. The history file is user-writable, so a trash path
/// outside a Trash never counts: sudo would otherwise move any file an edited entry names.
pub fn restorable(entry: &Entry) -> Vec<Move> {
    let mut moves = Vec::new();
    for file in &entry.files {
        let Some(trash) = &file.trash else { continue };
        let lexical = |path: &Path| path.is_absolute() && !path.components().any(|part| part == Component::ParentDir);
        let safe = file.recoverable && lexical(trash) && lexical(&file.original) && is_in_trash(&trash.to_string_lossy()) && !is_protected(&file.original);
        if safe && std::fs::symlink_metadata(trash).is_ok() && std::fs::symlink_metadata(&file.original).is_err() {
            moves.push(Move { from: trash.clone(), to: file.original.clone(), system: file.system, size: 0 });
        }
    }
    moves
}

/// Settles the moves sudo ran, records a removal in the history and says what happened.
pub fn finish(mut batch: Batch) -> String {
    for pending in std::mem::take(&mut batch.pending) {
        if std::fs::symlink_metadata(&pending.to).is_ok() && std::fs::symlink_metadata(&pending.from).is_err() {
            batch.moved.push(pending);
        } else {
            batch.failed.push((pending.from, "needs administrator rights".into()));
        }
    }
    let bytes: u64 = batch.moved.iter().map(|moved| moved.size).sum();
    let mut message = match batch.kind {
        Kind::Trash => {
            if batch.folder != trash_root() {
                // Only removes an empty folder.
                let _ = std::fs::remove_dir(&batch.folder);
            }
            record(&batch.origin, &batch.moved, batch.secs);
            format!("Moved {} to the Trash, {}.", plural(batch.moved.len(), "item"), format_bytes(bytes))
        }
        Kind::Restore => format!("Restored {}.", plural(batch.moved.len(), "item")),
    };
    if let Some((path, reason)) = batch.failed.first() {
        message.push_str(&format!(" {} failed: {} ({reason}).", batch.failed.len(), path.display()));
    }
    if batch.skipped > 0 {
        message.push_str(&format!(" {} left in place (protected).", plural(batch.skipped, "item")));
    }
    message
}

pub fn plural(count: usize, noun: &str) -> String {
    if count == 1 { format!("1 {noun}") } else { format!("{count} {noun}s") }
}

fn make_folder(origin: &str, secs: i64) -> PathBuf {
    let root = trash_root();
    let origin = origin.replace(['/', ':'], "-");
    let origin = if origin.trim().is_empty() { "Cleaner" } else { origin.trim() };
    let base = format!("{origin} — {}", local_time(secs, "at"));
    let mut name = base.clone();
    let mut suffix = 2;
    while root.join(&name).exists() {
        name = format!("{base} ({suffix})");
        suffix += 1;
    }
    let folder = root.join(name);
    if std::fs::create_dir_all(&folder).is_ok() { folder } else { root }
}

fn unique_name(folder: &Path, name: &str, used: &mut HashSet<String>) -> String {
    let name = if name.is_empty() { "item" } else { name };
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, Some(ext)),
        _ => (name, None),
    };
    let mut candidate = name.to_owned();
    let mut suffix = 2;
    while used.contains(&candidate.to_lowercase()) || folder.join(&candidate).exists() {
        candidate = match ext {
            Some(ext) => format!("{stem} ({suffix}).{ext}"),
            None => format!("{stem} ({suffix})"),
        };
        suffix += 1;
    }
    used.insert(candidate.to_lowercase());
    candidate
}

/// Provenance attributes that travel into the Trash: `xattr -p com.rokartur.cleaner.origin <file>`.
fn tag(path: &Path, origin: &str, secs: i64) {
    let Ok(file) = CString::new(path.as_os_str().as_bytes()) else { return };
    for (name, value) in [(ORIGIN_ATTR, origin.to_owned()), (DATE_ATTR, iso_utc(secs))] {
        let name = CString::new(name).expect("attribute names have no NUL");
        // SAFETY: both strings are NUL-terminated and value points to value.len() readable bytes.
        unsafe { libc::setxattr(file.as_ptr(), name.as_ptr(), value.as_ptr().cast(), value.len(), 0, libc::XATTR_NOFOLLOW) };
    }
}

fn tm(secs: i64, local: bool) -> libc::tm {
    // SAFETY: tm is plain data; gmtime_r and localtime_r only write into it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let time = secs as libc::time_t;
    unsafe {
        if local {
            libc::localtime_r(&time, &mut tm);
        } else {
            libc::gmtime_r(&time, &mut tm);
        }
    }
    tm
}

fn iso_utc(secs: i64) -> String {
    let t = tm(secs, false);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", t.tm_year + 1900, t.tm_mon + 1, t.tm_mday, t.tm_hour, t.tm_min, t.tm_sec)
}

/// `2026-06-03 at 14.30.05` for Trash folder names, `2026-06-03 14:30` for display.
pub fn local_time(secs: i64, style: &str) -> String {
    let t = tm(secs, true);
    let date = format!("{:04}-{:02}-{:02}", t.tm_year + 1900, t.tm_mon + 1, t.tm_mday);
    if style == "at" { format!("{date} at {:02}.{:02}.{:02}", t.tm_hour, t.tm_min, t.tm_sec) } else { format!("{date} {:02}:{:02}", t.tm_hour, t.tm_min) }
}

/// Reads the `2026-06-03T14:30:05Z` form history dates use.
fn parse_iso(text: &str) -> Option<i64> {
    let text = text.strip_suffix('Z')?;
    let (date, time) = text.split_once('T')?;
    let numbers: Vec<i32> = date.split('-').chain(time.split(':')).map(|part| part.parse().ok()).collect::<Option<_>>()?;
    let [year, month, day, hour, minute, second] = numbers[..] else { return None };
    // SAFETY: tm is plain data.
    let mut t: libc::tm = unsafe { std::mem::zeroed() };
    (t.tm_year, t.tm_mon, t.tm_mday, t.tm_hour, t.tm_min, t.tm_sec) = (year - 1900, month - 1, day, hour, minute, second);
    // SAFETY: timegm only reads and normalizes the struct it is given.
    Some(unsafe { libc::timegm(&mut t) } as i64)
}

#[derive(Clone, Debug)]
pub struct FileRecord {
    pub original: PathBuf,
    pub trash: Option<PathBuf>,
    pub system: bool,
    /// False for removals that never went to the Trash, such as Homebrew uninstalls.
    pub recoverable: bool,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub secs: Option<i64>,
    pub origin: String,
    pub count: u64,
    pub bytes: u64,
    pub paths: Vec<String>,
    pub files: Vec<FileRecord>,
}

/// Where history is written, then the file the BetterCleaner app shares, read until the first write here.
#[cfg(not(test))]
fn history_paths() -> [PathBuf; 2] {
    let home = crate::scan::home();
    [home.join(".config/cleaner/trash-history.json"), home.join("Library/Application Support/BetterCleaner/trash-history.json")]
}

#[cfg(test)]
fn history_paths() -> [PathBuf; 2] {
    let root = std::env::temp_dir().join(format!("cleaner-test-{}", std::process::id()));
    [root.join("trash-history.json"), root.join("legacy/trash-history.json")]
}

/// An unreadable history reads as empty.
fn load() -> Vec<Value> {
    let [path, legacy] = history_paths();
    let bytes = match std::fs::read(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => std::fs::read(legacy),
        read => read,
    }
    .unwrap_or_default();
    serde_json::from_slice::<Vec<Value>>(&bytes).unwrap_or_default()
}

/// Newest first.
pub fn entries() -> Vec<Entry> {
    let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
    load()
        .iter()
        .rev()
        .map(|entry| Entry {
            secs: entry["date"].as_str().and_then(parse_iso),
            origin: text(&entry["origin"]),
            count: entry["count"].as_u64().unwrap_or_default(),
            bytes: entry["bytes"].as_u64().unwrap_or_default(),
            paths: entry["paths"].as_array().map(|paths| paths.iter().map(text).collect()).unwrap_or_default(),
            files: entry["files"]
                .as_array()
                .map(|files| {
                    files
                        .iter()
                        .map(|file| FileRecord {
                            original: PathBuf::from(text(&file["originalPath"])),
                            trash: file["trashPath"].as_str().map(PathBuf::from),
                            system: file["domain"] == "system",
                            recoverable: file["recoverable"].as_bool().unwrap_or(false),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect()
}

fn record(origin: &str, moved: &[Move], secs: i64) {
    if moved.is_empty() {
        return;
    }
    let files: Vec<Value> = moved.iter().map(|moved| json!({"originalPath": moved.from, "trashPath": moved.to, "domain": if moved.system { "system" } else { "user" }, "recoverable": true})).collect();
    let mut all = load();
    all.push(json!({
        "date": iso_utc(secs),
        "origin": origin,
        "count": moved.len(),
        "bytes": moved.iter().map(|moved| moved.size).sum::<u64>(),
        "paths": moved.iter().map(|moved| &moved.from).collect::<Vec<_>>(),
        "files": files,
    }));
    let overflow = all.len().saturating_sub(MAX_ENTRIES);
    all.drain(..overflow);
    let [path, _] = history_paths();
    if let Some(folder) = path.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    // Written beside and renamed over, so a crash never leaves half a history.
    let staged = path.with_extension(format!("json.{}", std::process::id()));
    let text = serde_json::to_vec_pretty(&all).expect("JSON values always serialize");
    if std::fs::write(&staged, text).is_ok() {
        let _ = std::fs::rename(&staged, &path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip_through_the_app_format() {
        assert_eq!(iso_utc(1_780_497_005), "2026-06-03T14:30:05Z");
        assert_eq!(parse_iso("2026-06-03T14:30:05Z"), Some(1_780_497_005));
        assert_eq!(parse_iso("garbage"), None);
    }

    #[test]
    fn names_in_one_batch_never_collide() {
        let mut used = HashSet::new();
        let folder = Path::new("/nonexistent");
        assert_eq!(unique_name(folder, "a.plist", &mut used), "a.plist");
        assert_eq!(unique_name(folder, "A.plist", &mut used), "A (2).plist");
        assert_eq!(unique_name(folder, "Cache", &mut used), "Cache");
        assert_eq!(unique_name(folder, "Cache", &mut used), "Cache (2)");
    }

    #[test]
    fn trash_records_history_and_restores() {
        let root = trash_root().parent().unwrap().to_path_buf();
        std::fs::create_dir_all(trash_root()).unwrap();
        let file = root.join("prefs/com.example.app.plist");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"x").unwrap();
        let [_, legacy] = history_paths();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, br#"[{"origin": "BetterCleaner", "count": 1}]"#).unwrap();
        let item = FileItem { path: file.clone(), category: "Preferences", system: false, size: 1, approximate: false, selected: true, auto_selectable: true, why: "" };

        let message = finish(move_to_trash([&item], "Example"));
        assert!(message.starts_with("Moved 1 item"), "{message}");
        assert!(!file.exists());
        let entries = entries();
        assert_eq!(entries[1].origin, "BetterCleaner", "the first write keeps the legacy history");
        let entry = entries[0].clone();
        assert_eq!((entry.origin.as_str(), entry.count, entry.files.len()), ("Example", 1, 1));
        assert!(entry.files[0].trash.as_ref().unwrap().starts_with(trash_root()));

        assert_eq!(finish(restore(&entry)), "Restored 1 item.");
        assert!(file.exists());
        assert!(restorable(&entry).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restore_ignores_paths_outside_a_trash() {
        let record = |trash: &str| FileRecord { original: "/tmp/x".into(), trash: Some(trash.into()), system: true, recoverable: true };
        let entry = Entry { secs: None, origin: String::new(), count: 2, bytes: 0, paths: vec![], files: vec![record("/etc/hosts"), record("/Users/a/.Trash/../../etc/hosts")] };
        assert!(restorable(&entry).is_empty());
    }
}
