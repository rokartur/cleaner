use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const RELEASES: &str = "https://github.com/rokartur/cleaner/releases";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// `latest` follows stable releases only, `pre-release` also takes pre-releases, `off` never checks.
pub const CHANNELS: [&str; 3] = ["latest", "pre-release", "off"];
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// The startup check and a check from Settings can overlap, and both would write the same staged file.
static INSTALLING: Mutex<()> = Mutex::new(());

/// What an update is doing right now; `cleaner update` draws it, the TUI passes `&|_| ()`.
pub enum Progress<'a> {
    Checking,
    Downloading { version: &'a str, received: u64, total: Option<u64> },
    Verifying,
    Installing,
}

/// Only CI release builds update themselves, so a local `cargo build` never gets replaced.
pub fn enabled() -> bool {
    option_env!("CLEANER_RELEASE").is_some()
}

/// One of `CHANNELS`, as set on the Settings page; `latest` when never set.
pub fn channel() -> &'static str {
    let bytes = std::fs::read(settings_path()).unwrap_or_default();
    let settings = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap_or_default();
    CHANNELS.into_iter().find(|channel| settings["update_channel"] == *channel).unwrap_or("latest")
}

pub fn save_channel(channel: &str) -> Result<(), String> {
    let path = settings_path();
    let folder = path.parent().expect("the settings file sits in a folder");
    std::fs::create_dir_all(folder).and_then(|()| std::fs::write(&path, serde_json::json!({ "update_channel": channel }).to_string())).map_err(|e| format!("Cannot save {}: {e}", path.display()))
}

fn settings_path() -> PathBuf {
    crate::scan::home().join(".config/cleaner/settings.json")
}

/// Replaces the running binary with the newest release on `channel`, once a day. Returns the installed version.
pub fn install_latest(channel: &str) -> Result<Option<String>, String> {
    let stamp = crate::scan::home().join(".config/cleaner/update-check");
    if channel == "off" || !due(&stamp, channel) {
        return Ok(None);
    }
    let latest = latest_version(channel == "pre-release");
    // Ok(None) is an offline start, which checks again next start instead of next day.
    if !matches!(latest, Ok(None)) {
        mark_checked(&stamp, channel)?;
    }
    let Some(latest) = latest? else { return Ok(None) };
    install_if_newer(latest, &|_| ())
}

/// `cleaner update` and `u` on Settings: checks now, whatever the channel and the last check, and reports an offline start as an error.
pub fn install_now(channel: &str, report: &dyn Fn(Progress)) -> Result<Option<String>, String> {
    report(Progress::Checking);
    let latest = latest_version(channel == "pre-release")?.ok_or("Cannot reach GitHub to check for updates.")?;
    install_if_newer(latest, report)
}

fn install_if_newer(latest: String, report: &dyn Fn(Progress)) -> Result<Option<String>, String> {
    if !is_newer(&latest, CURRENT) {
        return Ok(None);
    }
    let _one_at_a_time = INSTALLING.lock();
    let exe = std::env::current_exe().map_err(|e| format!("Cannot find the cleaner binary: {e}"))?;
    let staged = exe.with_file_name(format!(".cleaner-update-{}", std::process::id()));
    let result = download(&latest, &staged, report)
        .and_then(|()| {
            report(Progress::Verifying);
            verify(&staged, &latest)
        })
        .and_then(|()| {
            report(Progress::Installing);
            std::fs::rename(&staged, &exe).map_err(|e| format!("Cannot replace {}: {e}", exe.display()))
        });
    if let Err(error) = result {
        let _ = std::fs::remove_file(&staged);
        return Err(error);
    }
    Ok(Some(latest))
}

/// The stamp holds the channel last checked, so switching channels checks at once.
fn due(stamp: &Path, channel: &str) -> bool {
    let Ok(checked) = std::fs::read_to_string(stamp) else { return true };
    let age = std::fs::metadata(stamp).and_then(|meta| meta.modified()).ok().and_then(|modified| modified.elapsed().ok());
    checked != channel || age.is_none_or(|age| age >= CHECK_EVERY)
}

/// A fresh install has no ~/.config/cleaner yet.
fn mark_checked(stamp: &Path, channel: &str) -> Result<(), String> {
    let folder = stamp.parent().expect("the stamp sits in ~/.config/cleaner");
    std::fs::create_dir_all(folder).map_err(|e| format!("Cannot create {}: {e}", folder.display()))?;
    std::fs::write(stamp, channel).map_err(|e| format!("Cannot write {}: {e}", stamp.display()))
}

/// Offline start is normal, so a network failure means no update; an HTTP error such as a 429 rate limit is reported.
fn latest_version(include_prereleases: bool) -> Result<Option<String>, String> {
    // The releases feed is a web page, not the REST API, so it has no 60 calls an hour limit.
    // The status comes from -w: over HTTP/2, curl 8.7 exits with 56 instead of 22 on an error status.
    let output =
        Command::new("curl").args(["-sSL", "--max-time", "10", "-w", "\n%{http_code}", &format!("{RELEASES}.atom")]).stdin(Stdio::null()).output().map_err(|e| format!("Cannot run curl: {e}"))?;
    let body = String::from_utf8_lossy(&output.stdout);
    let (feed, status) = body.rsplit_once('\n').unwrap_or_default();
    if status.parse::<u16>().is_ok_and(|status| status >= 400) {
        return Err(format!("Cannot check for updates: GitHub answered {status}."));
    }
    // A captive portal answers with its own page instead of the feed.
    if !output.status.success() || !feed.contains("<feed") {
        return Ok(None);
    }
    // A channel without releases yet counts as up to date.
    Ok(Some(newest(feed, include_prereleases).unwrap_or(CURRENT).to_owned()))
}

/// The first version in the releases feed, which lists the newest first, skipping pre-releases unless asked for.
/// Not the highest version: that is v26.1.1 of the old macOS app.
fn newest(feed: &str, include_prereleases: bool) -> Option<&str> {
    let mut versions = feed.split("<entry>").skip(1).filter_map(|entry| entry.split_once("/releases/tag/v")?.1.split('"').next());
    versions.find(|version| include_prereleases || !version.contains('-'))
}

fn download(version: &str, path: &Path, report: &dyn Fn(Progress)) -> Result<(), String> {
    let asset = format!("cleaner-{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let url = format!("{RELEASES}/download/v{version}/{asset}");
    // Created first, so an unwritable binary folder fails before the transfer.
    std::fs::File::create(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied => {
            let folder = path.parent().unwrap_or(path).display();
            format!("No write access to {folder}, where cleaner is installed. Reinstall it to a folder you own, or set the update channel to off.")
        }
        _ => format!("Cannot write {}: {e}", path.display()),
    })?;
    let total = size(&url);
    let mut transfer = Command::new("curl")
        .args(["-fsSL", "--max-time", "300", "--max-filesize", "100M", "-o"])
        .arg(path)
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Cannot run curl: {e}"))?;
    // curl writes the file, so its size so far is the progress.
    loop {
        let finished = transfer.try_wait().map_err(|e| format!("Cannot download {url}: {e}"))?.is_some();
        let received = std::fs::metadata(path).map_or(0, |meta| meta.len());
        report(Progress::Downloading { version, received, total });
        if finished {
            break;
        }
        std::thread::sleep(Duration::from_millis(80));
    }
    let output = transfer.wait_with_output().map_err(|e| format!("Cannot download {url}: {e}"))?;
    if !output.status.success() {
        return Err(format!("Cannot download {url}: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    let sums_url = format!("{RELEASES}/download/v{version}/SHA256SUMS");
    let sums = curl(&["-fsSL", "--max-time", "10", "--max-filesize", "64K", &sums_url]).map_err(|e| format!("Cannot download {sums_url}: {e}"))?;
    let bytes = std::fs::read(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    check_sum(&sums, &asset, &bytes)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).map_err(|e| format!("Cannot make {} executable: {e}", path.display()))
}

/// The size of the last response, after GitHub's redirect to its storage; None leaves the progress bar out.
fn size(url: &str) -> Option<u64> {
    let headers = curl(&["-fsSIL", "--max-time", "10", url]).ok()?;
    headers.lines().filter_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:")?.trim().parse().ok()).next_back()
}

/// `sums` is the release's SHA256SUMS, one `<hex digest>  <name>` line per asset, as sha256sum prints.
fn check_sum(sums: &str, asset: &str, bytes: &[u8]) -> Result<(), String> {
    let line = |line: &str| {
        let mut parts = line.split_whitespace();
        let (digest, name) = (parts.next()?, parts.next()?);
        // sha256sum marks binary mode with a leading star.
        (name.strip_prefix('*').unwrap_or(name) == asset).then(|| digest.to_lowercase())
    };
    let published = sums.lines().find_map(line).ok_or(format!("SHA256SUMS of the release does not list {asset}."))?;
    let actual: String = sha256(bytes).iter().map(|byte| format!("{byte:02x}")).collect();
    if published != actual {
        return Err(format!("The downloaded {asset} does not match its published SHA-256."));
    }
    Ok(())
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    // CommonCrypto, part of libSystem on every macOS.
    unsafe extern "C" {
        fn CC_SHA256(data: *const u8, len: u32, digest: *mut u8) -> *mut u8;
    }
    let len = u32::try_from(bytes.len()).expect("the 100 MiB download limit keeps it under 4 GiB");
    let mut digest = [0; 32];
    // SAFETY: CC_SHA256 reads `len` bytes from `data` and writes 32 bytes to `digest`.
    unsafe { CC_SHA256(bytes.as_ptr(), len, digest.as_mut_ptr()) };
    digest
}

/// A truncated or wrong download must never replace a working binary.
fn verify(path: &Path, version: &str) -> Result<(), String> {
    let cannot_run = |e: std::io::Error| format!("Cannot run the downloaded cleaner: {e}");
    let mut child = Command::new(path).arg("--version").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(cannot_run)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().map_err(cannot_run)?.is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("The downloaded cleaner did not answer --version within 10 s.".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().map_err(cannot_run)?;
    let reported = String::from_utf8_lossy(&output.stdout);
    if reported.trim() != format!("cleaner {version}") {
        return Err(format!("The downloaded cleaner reports {:?}, expected {version}.", reported.trim()));
    }
    Ok(())
}

/// curl's output, or its message, such as `curl: (22) The requested URL returned error: 404`.
fn curl(args: &[&str]) -> Result<String, String> {
    let output = Command::new("curl").args(args).stdin(Stdio::null()).output().map_err(|e| format!("Cannot run curl: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
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
    // Build metadata (`+7`) does not take part in semver order.
    let version = version.split_once('+').map_or(version, |(version, _)| version);
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
        assert!(is_newer("0.3.1+7", "0.3.0+9") && !is_newer("0.3.0+9", "0.3.0"));
        assert!(!is_newer("garbage", "0.1.0"));
    }

    #[test]
    fn the_channel_picks_from_the_newest_feed_entries() {
        let entry = |tag: &str| format!("<entry><id>tag:github.com,2008:Repository/1/{tag}</id><link href=\"{RELEASES}/tag/{tag}\"/></entry>");
        let feed = format!("<feed><link href=\"{RELEASES}\"/>{}{}{}</feed>", entry("v0.2.0-beta.1"), entry("v0.1.0"), entry("v26.1.1"));
        assert_eq!(newest(&feed, true), Some("0.2.0-beta.1"));
        assert_eq!(newest(&feed, false), Some("0.1.0"));
        assert_eq!(newest("<feed></feed>", true), None);
    }

    #[test]
    fn a_start_checks_at_most_once_a_day_per_channel() {
        let folder = std::env::temp_dir().join(format!("cleaner-update-check-{}", std::process::id()));
        let stamp = folder.join(".config/cleaner/update-check");
        assert!(due(&stamp, "latest"));
        mark_checked(&stamp, "latest").unwrap();
        assert!(!due(&stamp, "latest"));
        assert!(due(&stamp, "pre-release"));
        let file = std::fs::File::options().write(true).open(&stamp).unwrap();
        file.set_modified(std::time::SystemTime::now() - CHECK_EVERY).unwrap();
        assert!(due(&stamp, "latest"));
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn a_download_that_differs_from_the_published_sum_is_refused() {
        let hex = |bytes: &[u8]| sha256(bytes).iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        assert_eq!(hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let asset = "cleaner-macos-aarch64";
        let sums = format!("{}  cleaner-macos-x86_64\n{}  *{asset}\n", hex(b"intel"), hex(b"binary").to_uppercase());
        assert_eq!(check_sum(&sums, asset, b"binary"), Ok(()));
        assert_eq!(check_sum(&sums, asset, b"other"), Err("The downloaded cleaner-macos-aarch64 does not match its published SHA-256.".into()));
        let only_intel = format!("{}  cleaner-macos-x86_64\n", hex(b"binary"));
        assert_eq!(check_sum(&only_intel, asset, b"binary"), Err("SHA256SUMS of the release does not list cleaner-macos-aarch64.".into()));
    }
}
