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

/// FIPS 180-4, one implementation for both systems: Linux has no hash library every distribution
/// ships. Reads the download in place: only the padded tail, at most two blocks, is copied.
fn sha256(bytes: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut state: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut compress = |block: &[u8; 64]| {
        let mut w = [0u32; 64];
        for (word, chunk) in w.iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*chunk);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for (k, w) in K.iter().zip(w) {
            let t1 = h.wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25)).wrapping_add((e & f) ^ (!e & g)).wrapping_add(*k).wrapping_add(w);
            let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22)).wrapping_add((a & b) ^ (a & c) ^ (b & c));
            (h, g, f, e, d, c, b, a) = (g, f, e, d.wrapping_add(t1), c, b, a, t1.wrapping_add(t2));
        }
        for (word, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *word = word.wrapping_add(value);
        }
    };
    let (blocks, rest) = bytes.as_chunks::<64>();
    blocks.iter().for_each(&mut compress);
    let mut tail = rest.to_vec();
    tail.push(0x80);
    tail.resize(if tail.len() <= 56 { 56 } else { 120 }, 0);
    tail.extend_from_slice(&(bytes.len() as u64 * 8).to_be_bytes());
    tail.as_chunks::<64>().0.iter().for_each(&mut compress);
    let mut digest = [0; 32];
    for (chunk, word) in digest.as_chunks_mut::<4>().0.iter_mut().zip(state) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
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
        assert_eq!(hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"), "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1");
        assert_eq!(hex(&[b'a'; 1000]), "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3");
        // Lengths around the padding boundaries: one tail block up to 55 bytes, two from 56.
        for (length, digest) in [
            (55, "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"),
            (56, "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"),
            (63, "7d3e74a05d7db15bce4ad9ec0658ea98e3f06eeecf16b4c6fff2da457ddc2f34"),
            (64, "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"),
            (119, "31eba51c313a5c08226adf18d4a359cfdfd8d2e816b13f4af952f7ea6584dcfb"),
            (120, "2f3d335432c70b580af0e8e1b3674a7c020d683aa5f73aaaedfdc55af904c21c"),
        ] {
            assert_eq!(hex(&vec![b'a'; length]), digest, "{length} bytes");
        }
        let asset = "cleaner-macos-aarch64";
        let sums = format!("{}  cleaner-macos-x86_64\n{}  *{asset}\n", hex(b"intel"), hex(b"binary").to_uppercase());
        assert_eq!(check_sum(&sums, asset, b"binary"), Ok(()));
        assert_eq!(check_sum(&sums, asset, b"other"), Err("The downloaded cleaner-macos-aarch64 does not match its published SHA-256.".into()));
        let only_intel = format!("{}  cleaner-macos-x86_64\n", hex(b"binary"));
        assert_eq!(check_sum(&only_intel, asset, b"binary"), Err("SHA256SUMS of the release does not list cleaner-macos-aarch64.".into()));
    }
}
