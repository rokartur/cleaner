use crate::size;
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// An installed formula or cask, or a search result.
#[derive(Clone, Debug, PartialEq)]
pub struct Package {
    pub name: String,
    /// Tap-qualified outside Homebrew's own taps: the name commands take.
    pub full_name: String,
    pub cask: bool,
    pub desc: String,
    pub homepage: String,
    pub tap: String,
    /// The installed version, or the one an install would get.
    pub version: String,
    /// The version an upgrade installs, when the package is outdated.
    pub newer: Option<String>,
    pub pinned: bool,
    /// False for a formula installed only because another one needs it.
    pub on_request: bool,
    pub auto_updates: bool,
    pub installed_at: i64,
    pub dependencies: Vec<String>,
    /// The keg or Caskroom folder and what the cask put elsewhere: its app, fonts and links.
    pub paths: Vec<PathBuf>,
    pub size: Option<u64>,
}

impl Package {
    /// Kind and full name: a name can be both a formula and a cask.
    pub fn key(&self) -> (bool, String) {
        (self.cask, self.full_name.clone())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Service {
    pub name: String,
    /// `started`, `stopped`, `none`, `error` or `scheduled`, as `brew services` reports it.
    pub status: String,
    pub user: String,
    pub file: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tap {
    pub name: String,
    pub remote: String,
    pub formulae: usize,
    pub casks: usize,
}

/// What the Maintenance view offers to free.
#[derive(Clone, Debug, PartialEq)]
pub struct Maintenance {
    /// `brew cleanup`'s own estimate, such as `1.2GB`; None when there is nothing to remove.
    pub cleanup: Option<String>,
    pub autoremove: Vec<String>,
}

/// Arguments for one `brew` run; a plan runs them in order and stops at the first failure.
pub type Args = Vec<String>;

/// The `brew` on PATH, else the default prefixes, since cleaner may start with a bare PATH.
/// Not cached: after brew is installed mid-session, R finds it.
pub fn program() -> Option<PathBuf> {
    let on_path: Vec<PathBuf> = std::env::var_os("PATH").map(|path| std::env::split_paths(&path).map(|folder| folder.join("brew")).collect()).unwrap_or_default();
    on_path.into_iter().chain(["/opt/homebrew/bin/brew", "/usr/local/bin/brew"].map(PathBuf::from)).find(|path| path.is_file())
}

const MISSING: &str = "Homebrew is not installed. Get it at https://brew.sh, then press R.";

/// Runs brew without a terminal and returns its standard output, or its last error line.
pub fn output(args: &[&str]) -> Result<String, String> {
    let program = program().ok_or(MISSING)?;
    let output = Command::new(program).args(args).output().map_err(|error| format!("Cannot run brew: {error}"))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let last = stderr.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or_default();
    Err(if last.is_empty() { format!("brew {} failed.", args.join(" ")) } else { last.trim_start_matches("Error: ").to_owned() })
}

pub fn installed() -> Result<Vec<Package>, String> {
    let prefix = PathBuf::from(output(&["--prefix"])?.trim());
    let mut packages = parse_info(&output(&["info", "--json=v2", "--installed"])?, &prefix)?;
    packages.sort_by_key(|package| package.name.to_lowercase());
    Ok(packages)
}

pub fn measure(packages: &mut [Package]) {
    let paths: Vec<PathBuf> = packages.iter().flat_map(|package| package.paths.clone()).collect();
    let mut sizes = size::sizes(&paths).into_iter();
    for package in packages {
        package.size = Some(sizes.by_ref().take(package.paths.len()).map(|(size, _)| size).sum());
    }
}

/// Reads `brew info --json=v2`: installed packages, or the search results it describes.
pub fn parse_info(json: &str, prefix: &Path) -> Result<Vec<Package>, String> {
    let value: Value = serde_json::from_str(json).map_err(|error| format!("brew info printed something unexpected: {error}"))?;
    let mut packages = Vec::new();
    for formula in value["formulae"].as_array().into_iter().flatten() {
        let keg = formula["installed"].as_array().and_then(|kegs| kegs.last());
        let stable = text(&formula["versions"], "stable");
        let available = match formula["revision"].as_u64() {
            Some(revision) if revision > 0 => format!("{stable}_{revision}"),
            _ => stable,
        };
        let name = text(formula, "name");
        packages.push(Package {
            full_name: text(formula, "full_name"),
            cask: false,
            desc: text(formula, "desc"),
            homepage: text(formula, "homepage"),
            tap: text(formula, "tap"),
            version: formula["linked_keg"].as_str().map(str::to_owned).or_else(|| keg.map(|keg| text(keg, "version"))).unwrap_or_else(|| available.clone()),
            newer: formula["outdated"].as_bool().unwrap_or(false).then_some(available),
            pinned: formula["pinned"].as_bool().unwrap_or(false),
            on_request: keg.is_none_or(|keg| keg["installed_on_request"].as_bool().unwrap_or(false)),
            auto_updates: false,
            installed_at: keg.and_then(|keg| keg["time"].as_i64()).unwrap_or(0),
            dependencies: formula["dependencies"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect(),
            paths: keg.map(|_| vec![prefix.join("Cellar").join(&name)]).unwrap_or_default(),
            name,
            size: None,
        });
    }
    for cask in value["casks"].as_array().into_iter().flatten() {
        let token = text(cask, "token");
        let available = text(cask, "version");
        let installed = cask["installed"].as_str();
        let mut paths = Vec::new();
        if installed.is_some() {
            paths.push(prefix.join("Caskroom").join(&token));
            paths.extend(cask["artifacts"].as_array().into_iter().flatten().filter_map(|artifact| artifact["target"].as_str()).map(PathBuf::from));
        }
        let names: Vec<&str> = cask["name"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        packages.push(Package {
            full_name: text(cask, "full_token"),
            cask: true,
            desc: cask["desc"].as_str().map_or_else(|| names.join(", "), str::to_owned),
            homepage: text(cask, "homepage"),
            tap: text(cask, "tap"),
            version: installed.unwrap_or(&available).to_owned(),
            newer: cask["outdated"].as_bool().unwrap_or(false).then_some(available),
            pinned: cask["pinned"].as_bool().unwrap_or(false),
            on_request: true,
            auto_updates: cask["auto_updates"].as_bool().unwrap_or(false),
            installed_at: cask["installed_time"].as_i64().unwrap_or(0),
            dependencies: ["formula", "cask"].into_iter().flat_map(|kind| cask["depends_on"][kind].as_array().into_iter().flatten()).filter_map(Value::as_str).map(str::to_owned).collect(),
            paths,
            name: token,
            size: None,
        });
    }
    Ok(packages)
}

fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_owned()
}

/// Formulae and casks whose name holds `query`, the exact match first, then names that start with it.
pub fn search(query: &str) -> Result<Vec<Package>, String> {
    let lowercase = query.to_lowercase();
    let find = |kind: &str| -> Result<Vec<Package>, String> {
        let found = output(&["search", kind, "--", query]);
        let mut names: Vec<&str> = match &found {
            Ok(found) => found.lines().map(str::trim).filter(|line| !line.is_empty()).collect(),
            Err(error) if error.starts_with("No formulae or casks found") => return Ok(Vec::new()),
            Err(error) => return Err(error.clone()),
        };
        // brew lists names alphabetically: `go` comes after 30 `cargo-*`, so the cut follows the relevance sort.
        names.sort_by_key(|name| relevance(name.rsplit('/').next().unwrap_or(name), &lowercase));
        names.truncate(SEARCH_LIMIT);
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let mut args = vec!["info", "--json=v2", kind, "--"];
        args.extend(names);
        parse_info(&output(&args)?, Path::new(""))
    };
    let (formulae, casks) = std::thread::scope(|scope| {
        let casks = scope.spawn(|| find("--cask"));
        (find("--formula"), casks.join().expect("brew search thread panicked"))
    });
    let mut found = formulae?;
    found.extend(casks?);
    found.sort_by_key(|package| relevance(&package.name, &lowercase));
    Ok(found)
}

/// Sorts the exact name first, then names that start with the query, then the rest, each alphabetically.
fn relevance(name: &str, lowercase_query: &str) -> (bool, bool, String) {
    let name = name.to_lowercase();
    (name != lowercase_query, !name.starts_with(lowercase_query), name)
}

/// `brew info` takes a second per few dozen names, so a broad word stops at this many of each kind.
const SEARCH_LIMIT: usize = 40;

pub fn services() -> Result<Vec<Service>, String> {
    parse_services(&output(&["services", "list", "--json"])?)
}

pub fn parse_services(json: &str) -> Result<Vec<Service>, String> {
    let value: Value = serde_json::from_str(json).map_err(|error| format!("brew services printed something unexpected: {error}"))?;
    Ok(value
        .as_array()
        .into_iter()
        .flatten()
        .map(|service| Service { name: text(service, "name"), status: text(service, "status"), user: text(service, "user"), file: text(service, "file") })
        .collect())
}

pub fn taps() -> Result<Vec<Tap>, String> {
    parse_taps(&output(&["tap-info", "--installed", "--json"])?)
}

pub fn parse_taps(json: &str) -> Result<Vec<Tap>, String> {
    let value: Value = serde_json::from_str(json).map_err(|error| format!("brew tap-info printed something unexpected: {error}"))?;
    let count = |tap: &Value, key: &str| tap[key].as_array().map_or(0, Vec::len);
    Ok(value
        .as_array()
        .into_iter()
        .flatten()
        .map(|tap| Tap { name: text(tap, "name"), remote: text(tap, "remote"), formulae: count(tap, "formula_names"), casks: count(tap, "cask_tokens") })
        .collect())
}

pub fn maintenance() -> Result<Maintenance, String> {
    let cleanup = output(&["cleanup", "--dry-run", "--prune=all"])?;
    let autoremove = output(&["autoremove", "--dry-run"])?;
    Ok(parse_maintenance(&cleanup, &autoremove))
}

/// Reads `==> This operation would free approximately 1.2GB of disk space.` and the names under
/// `==> Would autoremove 2 unneeded formulae:`.
pub fn parse_maintenance(cleanup: &str, autoremove: &str) -> Maintenance {
    let cleanup = cleanup.lines().find_map(|line| Some(line.split_once("would free approximately ")?.1.split_once(" of disk space")?.0.to_owned()));
    let autoremove = autoremove
        .lines()
        .skip_while(|line| !line.starts_with("==> Would autoremove"))
        .skip(1)
        .take_while(|line| !line.starts_with("==>"))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    Maintenance { cleanup, autoremove }
}

/// One command per kind, `--formula` and `--cask`, so a name that is both means the package listed.
/// Names follow `--`, so a tap's package named like a flag stays a name.
pub fn per_kind(formula_args: &[&str], cask_args: &[&str], packages: &[&Package]) -> Vec<Args> {
    let mut commands = Vec::new();
    // Casks first: brew will not uninstall a formula while a cask that needs it is installed.
    for (cask, args, kind) in [(true, cask_args, "--cask"), (false, formula_args, "--formula")] {
        let names: Vec<String> = packages.iter().filter(|package| package.cask == cask).map(|package| package.full_name.clone()).collect();
        if !names.is_empty() {
            commands.push(args.iter().map(|arg| arg.to_string()).chain([kind.to_owned(), "--".to_owned()]).chain(names).collect());
        }
    }
    commands
}

/// The installed packages that depend on `package`, which brew will not uninstall it under.
pub fn needed_by<'a>(installed: &'a [Package], package: &Package) -> Vec<&'a str> {
    installed.iter().filter(|other| other.dependencies.iter().any(|name| *name == package.name || *name == package.full_name)).map(|other| other.name.as_str()).collect()
}

/// Splits picked packages into the ones brew uninstalls and the ones it keeps: pinned, or needed by a package that stays.
/// brew skips a pinned package yet exits 0, and refuses a whole command while a package outside it needs one of its names.
pub fn removable<'a>(installed: &[Package], picked: Vec<&'a Package>) -> (Vec<&'a Package>, Vec<&'a Package>) {
    let (mut targets, mut kept) = (picked, Vec::new());
    // A kept package keeps what it needs, so repeat until nothing more stays.
    loop {
        let names: HashSet<&str> = targets.iter().map(|package| package.name.as_str()).collect();
        let (removed, stays): (Vec<&Package>, Vec<&Package>) = targets.iter().partition(|package| !package.pinned && needed_by(installed, package).iter().all(|name| names.contains(name)));
        if stays.is_empty() {
            return (removed, kept);
        }
        kept.extend(stays);
        targets = removed;
    }
}

/// `user/repo`, the form `brew tap` takes without a URL.
pub fn is_tap_name(name: &str) -> bool {
    let valid = |part: &str| !part.is_empty() && !part.starts_with(['-', '.']) && part.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    name.split_once('/').is_some_and(|(user, repo)| valid(user) && valid(repo))
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: &str = r#"{"formulae": [
        {"name": "ffmpeg", "full_name": "ffmpeg", "tap": "homebrew/core", "desc": "Play, record, convert", "homepage": "https://ffmpeg.org",
         "versions": {"stable": "9.0.3"}, "revision": 1, "outdated": true, "pinned": true, "linked_keg": "9.0.2_1", "dependencies": ["x264", "lame"],
         "installed": [{"version": "9.0.2_1", "time": 1791527154, "installed_on_request": true}]},
        {"name": "abseil", "full_name": "abseil", "tap": "homebrew/core", "desc": "C++ Common Libraries", "homepage": "https://abseil.io",
         "versions": {"stable": "20260817.0"}, "revision": 0, "outdated": false, "pinned": false, "linked_keg": null, "dependencies": [],
         "installed": [{"version": "20260817.0", "time": 5, "installed_on_request": false}]},
        {"name": "ripgrep-all", "full_name": "ripgrep-all", "tap": "homebrew/core", "desc": "Wrapper around ripgrep", "homepage": "",
         "versions": {"stable": "0.10.9"}, "revision": 0, "outdated": false, "pinned": false, "linked_keg": null, "dependencies": [], "installed": []}
    ], "casks": [
        {"token": "visual-studio-code", "full_token": "visual-studio-code", "tap": "homebrew/cask", "name": ["Microsoft Visual Studio Code"], "desc": null,
         "homepage": "https://code.visualstudio.com", "version": "1.145.0", "installed": "1.144.0", "installed_time": 9, "outdated": true, "pinned": true, "auto_updates": true, "depends_on": {"cask": ["macfuse"], "macos": {}},
         "artifacts": [{"uninstall": [{"quit": "com.microsoft.VSCode"}]}, {"app": ["Visual Studio Code.app"], "target": "/Applications/Visual Studio Code.app"}]},
        {"token": "ripme", "full_token": "ripme", "tap": "homebrew/cask", "name": ["RipMe"], "desc": "Album ripper", "homepage": "",
         "version": "2.1.20", "installed": null, "installed_time": null, "outdated": false, "auto_updates": null, "artifacts": [{"app": ["RipMe.app"]}]}
    ]}"#;

    #[test]
    fn uninstall_keeps_pinned_packages_and_what_a_kept_formula_needs() {
        let template = parse_info(INFO, Path::new("/opt/homebrew")).unwrap().remove(0);
        let formula = |name: &str, dependencies: &[&str], pinned| Package {
            name: name.into(),
            full_name: name.into(),
            dependencies: dependencies.iter().map(|name| name.to_string()).collect(),
            pinned,
            ..template.clone()
        };
        let installed = [
            formula("bat", &[], false),
            formula("wget", &[], true),
            formula("libb2", &[], false),
            formula("libarchive", &["libb2"], false),
            formula("cmake", &["libarchive"], false),
            formula("zstd", &[], false),
            formula("curl", &["zstd"], false),
        ];
        let [bat, wget, libb2, libarchive, _, zstd, curl] = &installed;
        let (removed, kept) = removable(&installed, vec![bat, wget, libb2, libarchive, zstd, curl]);
        let names = |packages: Vec<&Package>| packages.into_iter().map(|package| package.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(removed), ["bat", "zstd", "curl"]);
        assert_eq!(names(kept), ["wget", "libarchive", "libb2"]);
    }

    #[test]
    fn info_reads_installed_versions_upgrades_and_paths() {
        let packages = parse_info(INFO, Path::new("/opt/homebrew")).unwrap();
        let [ffmpeg, abseil, ripgrep_all, code, ripme] = &packages[..] else { panic!("{packages:?}") };
        assert_eq!((ffmpeg.version.as_str(), ffmpeg.newer.as_deref(), ffmpeg.pinned, ffmpeg.on_request), ("9.0.2_1", Some("9.0.3_1"), true, true));
        assert_eq!(ffmpeg.paths, [PathBuf::from("/opt/homebrew/Cellar/ffmpeg")]);
        assert_eq!(ffmpeg.dependencies, ["x264", "lame"]);
        assert_eq!((abseil.version.as_str(), abseil.newer.as_deref(), abseil.on_request), ("20260817.0", None, false));
        assert_eq!((ripgrep_all.version.as_str(), ripgrep_all.paths.len()), ("0.10.9", 0));
        assert_eq!((code.version.as_str(), code.newer.as_deref(), code.auto_updates, code.pinned), ("1.144.0", Some("1.145.0"), true, true));
        assert_eq!(code.desc, "Microsoft Visual Studio Code");
        assert_eq!(code.dependencies, ["macfuse"]);
        assert_eq!(code.paths, [PathBuf::from("/opt/homebrew/Caskroom/visual-studio-code"), PathBuf::from("/Applications/Visual Studio Code.app")]);
        assert_eq!((ripme.version.as_str(), ripme.paths.len(), ripme.installed_at), ("2.1.20", 0, 0));
    }

    #[test]
    fn services_and_taps_read_brew_json() {
        let services = parse_services(
            r#"[{"name": "lighter", "status": "started", "user": "artur", "file": "/Users/artur/Library/LaunchAgents/sh.brew.lighter.plist", "exit_code": null},
            {"name": "nginx", "status": "none", "user": null, "file": "/opt/homebrew/opt/nginx/sh.brew.nginx.plist", "exit_code": null}]"#,
        )
        .unwrap();
        assert_eq!((services[0].status.as_str(), services[0].user.as_str(), services[1].user.as_str()), ("started", "artur", ""));
        let taps =
            parse_taps(r#"[{"name": "stripe/stripe-cli", "remote": "https://github.com/stripe/homebrew-stripe-cli", "formula_names": ["stripe/stripe-cli/stripe"], "cask_tokens": []}]"#).unwrap();
        assert_eq!(taps, [Tap { name: "stripe/stripe-cli".into(), remote: "https://github.com/stripe/homebrew-stripe-cli".into(), formulae: 1, casks: 0 }]);
    }

    #[test]
    fn maintenance_reads_the_dry_runs() {
        let cleanup = "Would remove: /Users/a/Library/Caches/Homebrew/x.bottle.tar.gz (12MB)\n==> This operation would free approximately 1.2GB of disk space.\n";
        let autoremove = "==> Would autoremove 2 unneeded formulae:\nlibfoo\nlibbar\n";
        assert_eq!(parse_maintenance(cleanup, autoremove), Maintenance { cleanup: Some("1.2GB".into()), autoremove: vec!["libfoo".into(), "libbar".into()] });
        assert_eq!(parse_maintenance("", ""), Maintenance { cleanup: None, autoremove: vec![] });
    }

    #[test]
    fn commands_split_formulae_from_casks() {
        let packages = parse_info(INFO, Path::new("/opt/homebrew")).unwrap();
        let picked: Vec<&Package> = packages.iter().filter(|package| package.name != "ripme" && package.name != "ripgrep-all").collect();
        assert_eq!(
            per_kind(&["uninstall"], &["uninstall", "--zap"], &picked),
            [vec!["uninstall", "--zap", "--cask", "--", "visual-studio-code"], vec!["uninstall", "--formula", "--", "ffmpeg", "abseil"]]
        );
        assert_eq!(per_kind(&["upgrade"], &["upgrade"], &picked[..1]), [vec!["upgrade", "--formula", "--", "ffmpeg"]]);
    }

    #[test]
    fn tap_names_are_user_slash_repo() {
        assert!(is_tap_name("stripe/stripe-cli"));
        assert!(is_tap_name("artginzburg/tap"));
        for name in ["stripe", "--force/x", "a/b/c", "a/", "/b", "a/../b", "a b/c"] {
            assert!(!is_tap_name(name), "{name}");
        }
    }

    #[test]
    fn search_ranks_the_exact_name_before_the_alphabet() {
        let mut names = vec!["algol68g", "cargo-audit", "gobject", "go"];
        names.sort_by_key(|name| relevance(name, "go"));
        assert_eq!(names, ["go", "gobject", "algol68g", "cargo-audit"]);
    }
}
