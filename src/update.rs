use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

const REPO: &str = "https://github.com/rokartur/cleaner";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// The startup check and a check from Settings can overlap, and both would write the same staged file.
static INSTALLING: Mutex<()> = Mutex::new(());

/// Only CI release builds update themselves, so a local `cargo build` never gets replaced.
pub fn enabled() -> bool {
    option_env!("CLEANER_RELEASE").is_some()
}

/// Whether updates include pre-releases, as set on the Settings page. Off when never set.
pub fn pre_releases() -> bool {
    let bytes = std::fs::read(settings_path()).unwrap_or_default();
    serde_json::from_slice::<serde_json::Value>(&bytes).is_ok_and(|settings| settings["pre_releases"] == true)
}

pub fn save_pre_releases(on: bool) -> Result<(), String> {
    let path = settings_path();
    let folder = path.parent().expect("the settings file sits in a folder");
    std::fs::create_dir_all(folder).and_then(|()| std::fs::write(&path, serde_json::json!({ "pre_releases": on }).to_string())).map_err(|e| format!("Cannot save {}: {e}", path.display()))
}

fn settings_path() -> PathBuf {
    crate::scan::home().join(".config/cleaner/settings.json")
}

/// The newest release on the channel when it is newer than this build; Err when GitHub cannot be reached.
pub fn newer_release(pre_releases: bool) -> Result<Option<String>, String> {
    // The releases feed is a web page, not the REST API, so it has no 60 calls an hour limit.
    let feed = run(Command::new("curl").args(["-fsSL", "--max-time", "10", &format!("{REPO}/releases.atom")])).map_err(|e| format!("Cannot check for updates: {e}"))?;
    Ok(newest(&feed, pre_releases).filter(|newest| is_newer(newest, CURRENT)).map(str::to_owned))
}

/// Replaces the running binary with release `version`, once the download answers `--version` with it.
pub fn install(version: &str) -> Result<(), String> {
    let _one_at_a_time = INSTALLING.lock();
    let exe = std::env::current_exe().map_err(|e| format!("Cannot find the cleaner binary: {e}"))?;
    let staged = exe.with_file_name(format!(".cleaner-update-{}", std::process::id()));
    let result = download(version, &staged).and_then(|()| verify(&staged, version)).and_then(|()| std::fs::rename(&staged, &exe).map_err(|e| format!("Cannot replace {}: {e}", exe.display())));
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result
}

/// The first version in the releases feed, which lists the newest first, skipping pre-releases unless asked for.
fn newest(feed: &str, pre_releases: bool) -> Option<&str> {
    let mut versions = feed.split("<entry>").skip(1).filter_map(|entry| entry.split_once("/releases/tag/v")?.1.split('"').next());
    versions.find(|version| pre_releases || !version.contains('-'))
}

fn download(version: &str, path: &Path) -> Result<(), String> {
    let url = format!("{REPO}/releases/download/v{version}/cleaner-{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    // Created first, so an unwritable binary folder gets this message instead of curl's.
    std::fs::File::create(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied => {
            format!("No write access to {}, where cleaner is installed. Reinstall it to a folder you own.", path.parent().unwrap_or(path).display())
        }
        _ => format!("Cannot write {}: {e}", path.display()),
    })?;
    run(Command::new("curl").args(["-fsSL", "--max-time", "300", "-o"]).arg(path).arg(&url)).map_err(|e| format!("Cannot download {url}: {e}"))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).map_err(|e| format!("Cannot make {} executable: {e}", path.display()))
}

/// A truncated or wrong download must never replace a working binary.
fn verify(path: &Path, version: &str) -> Result<(), String> {
    let reported = run(Command::new(path).arg("--version"))?;
    if reported.trim() != format!("cleaner {version}") {
        return Err(format!("The downloaded cleaner reports {:?}, expected {version}.", reported.trim()));
    }
    Ok(())
}

fn run(command: &mut Command) -> Result<String, String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let output = command.stdin(Stdio::null()).output().map_err(|e| format!("Cannot run {program}: {e}"))?;
    if !output.status.success() {
        return Err(format!("{program} failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Semver order: `0.2.0-beta.2` is newer than `0.2.0-beta.1` and older than `0.2.0`.
fn is_newer(latest: &str, current: &str) -> bool {
    matches!((sort_key(latest), sort_key(current)), (Some(latest), Some(current)) if latest > current)
}

/// A pre-release identifier. Semver sorts numbers below words, and numbers by value, so `beta.10` follows `beta.9`.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Identifier {
    Number(u64),
    Word(String),
}

/// The version numbers, then whether it is a release, which sorts above its pre-releases, then the pre-release identifiers.
fn sort_key(version: &str) -> Option<(Vec<u64>, bool, Vec<Identifier>)> {
    let (numbers, suffix) = match version.split_once('-') {
        Some((numbers, suffix)) => (numbers, Some(suffix)),
        None => (version, None),
    };
    let numbers = numbers.split('.').map(|n| n.parse().ok()).collect::<Option<Vec<u64>>>()?;
    let identifiers = suffix.into_iter().flat_map(|suffix| suffix.split('.')).map(|part| part.parse().map_or_else(|_| Identifier::Word(part.to_owned()), Identifier::Number)).collect();
    Some((numbers, suffix.is_none(), identifiers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_semver() {
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(is_newer("0.2.0-beta.1", "0.1.0"));
        assert!(is_newer("0.2.0", "0.2.0-beta.1"));
        assert!(!is_newer("0.2.0-beta.1", "0.2.0"));
        assert!(is_newer("0.2.0-beta.10", "0.2.0-beta.9"));
        assert!(is_newer("0.2.0-rc.1", "0.2.0-beta.2"));
        assert!(!is_newer("0.2.0-beta.1", "0.2.0-beta.1"));
        assert!(!is_newer("garbage", "0.1.0"));
    }

    #[test]
    fn the_channel_picks_from_the_newest_feed_entries() {
        let entry = |tag: &str| format!("<entry><id>tag:github.com,2008:Repository/1/{tag}</id><link href=\"{REPO}/releases/tag/{tag}\"/></entry>");
        let feed = format!("<feed><link href=\"{REPO}/releases\"/>{}{}{}</feed>", entry("v0.2.0-beta.1"), entry("v0.1.0"), entry("v26.1.1"));
        assert_eq!(newest(&feed, true), Some("0.2.0-beta.1"));
        assert_eq!(newest(&feed, false), Some("0.1.0"));
        assert_eq!(newest("<feed></feed>", true), None);
    }
}
