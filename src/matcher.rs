use std::path::Path;

/// File names that are never matched to an app, so a broad name match cannot take shared system state.
const PROTECTED_NAMES: &[&str] = &[
    ".globalpreferences",
    "com.apple.globalpreferences",
    "com.apple.finder",
    "com.apple.dock",
    "com.apple.systempreferences",
    "com.apple.systemuiserver",
    "com.apple.loginwindow",
    "com.apple.spotlight",
    "com.apple.universalaccess",
    "com.apple.security",
    "com.apple.appstore",
    "com.apple.coreservices",
    "com.apple.safari",
    "com.apple.mail",
    "com.apple.messages",
    "com.apple.icloud",
    "com.apple.keychainaccess",
    "knowledge",
    "group containers",
    "containers",
];

/// Reverse-DNS components that never name a vendor on their own.
const GENERIC_VENDORS: &[&str] = &["apple", "com", "org", "net", "io", "dev", "app", "apps", "www", "co", "group", "containers", "macos", "osx"];

/// Every signal used to find an app's leftover files.
#[derive(Clone, Debug, Default)]
pub struct Descriptor {
    /// Primary and nested helper bundle ids, lowercased, primary first.
    pub ids: Vec<String>,
    /// Normalized display name, bundle name, bundle file name and executable.
    pub terms: Vec<String>,
}

impl Descriptor {
    pub fn new<'a>(ids: impl IntoIterator<Item = &'a str>, names: impl IntoIterator<Item = &'a str>) -> Self {
        let mut descriptor = Descriptor::default();
        for id in ids {
            let id = id.to_lowercase();
            if !id.is_empty() && !descriptor.ids.contains(&id) {
                descriptor.ids.push(id);
            }
        }
        for name in names {
            let term = normalize(name);
            if !term.is_empty() && !descriptor.terms.contains(&term) {
                descriptor.terms.push(term);
            }
        }
        descriptor
    }

    /// The second reverse-DNS component of each bundle id (`com.PARALLELS.desktop`), at least 4 characters.
    pub fn vendor_tokens(&self) -> Vec<&str> {
        let mut tokens = Vec::new();
        for id in &self.ids {
            if let Some(vendor) = vendor(id)
                && !tokens.contains(&vendor)
            {
                tokens.push(vendor);
            }
        }
        tokens
    }

    fn vendor_namespaces(&self) -> Vec<String> {
        let mut namespaces = Vec::new();
        for id in &self.ids {
            if let (Some(vendor), Some((first, _))) = (vendor(id), id.split_once('.')) {
                let namespace = format!("{first}.{vendor}.");
                if !namespaces.contains(&namespace) {
                    namespaces.push(namespace);
                }
            }
        }
        namespaces
    }
}

fn vendor(id: &str) -> Option<&str> {
    let vendor = id.split('.').nth(1)?;
    (vendor.chars().count() >= 4 && !GENERIC_VENDORS.contains(&vendor)).then_some(vendor)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strength {
    /// A bundle id or a full app name: safe to select up front.
    Strong,
    /// Only the vendor or a glued bundle id matched: shown, never selected up front.
    Weak,
}

/// A bundle id belongs to one app; a name or vendor match can be claimed by another installed app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    BundleId,
    Name,
    Vendor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    pub strength: Strength,
    pub kind: Kind,
}

const fn found(strength: Strength, kind: Kind) -> Option<Match> {
    Some(Match { strength, kind })
}

/// Lowercase and keep only letters and digits, so "Sublime Text" and "sublimetext" compare equal.
pub fn normalize(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

pub fn is_protected(path: &Path) -> bool {
    let Some(name) = path.file_name() else { return true };
    let name = name.to_string_lossy().to_lowercase();
    let stem = match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => &name,
    };
    PROTECTED_NAMES.contains(&name.as_str()) || PROTECTED_NAMES.contains(&stem)
}

/// Whether `file_name`, one directory entry, belongs to the app, and on what evidence.
pub fn classify(file_name: &str, app: &Descriptor) -> Option<Match> {
    let lower = file_name.to_lowercase();
    for id in &app.ids {
        if lower == *id || lower.starts_with(&format!("{id}.")) || lower.starts_with(&format!("{id} ")) || contains_bounded(&lower, id) {
            return found(Strength::Strong, Kind::BundleId);
        }
        // "com.foo.BarHelper" is a helper or the sibling "com.foo.Bartender": review only.
        if lower.starts_with(id.as_str()) {
            return found(Strength::Weak, Kind::BundleId);
        }
    }

    // An app named "Mail" or "Safari" must never take com.apple.* state by name.
    let apple_app = app.ids.iter().any(|id| id.starts_with("com.apple."));
    if lower.starts_with("com.apple.") && !apple_app {
        return None;
    }

    let normalized = normalize(file_name);
    if app.terms.iter().any(|term| term.chars().count() >= 3 && normalized == *term) {
        return found(Strength::Strong, Kind::Name);
    }
    if app.terms.iter().any(|term| term.chars().count() >= 4 && lower.split(|c: char| !c.is_alphanumeric()).any(|token| token == term)) {
        return found(Strength::Strong, Kind::Name);
    }
    // "xcodegen" is another tool, not Xcode: review only.
    if app.terms.iter().any(|term| term.chars().count() >= 4 && starts_a_token(&lower, term)) {
        return found(Strength::Weak, Kind::Name);
    }

    if app.vendor_namespaces().iter().any(|namespace| lower.starts_with(namespace.as_str())) {
        return found(Strength::Weak, Kind::Vendor);
    }
    if app.vendor_tokens().iter().any(|token| starts_a_token(&lower, token)) {
        return found(Strength::Weak, Kind::Vendor);
    }
    None
}

/// Other installed apps' strong signals, built once per scan: a name match another app also
/// claims is shown but not selected up front.
pub struct Collisions {
    ids: Vec<String>,
    exact_terms: Vec<String>,
    prefix_terms: Vec<String>,
}

impl Collisions {
    pub fn new<'a>(others: impl IntoIterator<Item = &'a Descriptor>) -> Self {
        let mut collisions = Collisions { ids: Vec::new(), exact_terms: Vec::new(), prefix_terms: Vec::new() };
        for other in others {
            collisions.ids.extend(other.ids.iter().cloned());
            for term in &other.terms {
                let length = term.chars().count();
                if length >= 3 {
                    collisions.exact_terms.push(term.clone());
                }
                if length >= 4 {
                    collisions.prefix_terms.push(term.clone());
                }
            }
        }
        for list in [&mut collisions.ids, &mut collisions.exact_terms, &mut collisions.prefix_terms] {
            list.sort_unstable();
            list.dedup();
        }
        collisions
    }

    /// Mirrors the bundle-id and name predicates of `classify`.
    pub fn claims(&self, file_name: &str) -> bool {
        let lower = file_name.to_lowercase();
        if self.ids.iter().any(|id| lower == *id || lower.starts_with(&format!("{id}.")) || lower.starts_with(&format!("{id} ")) || contains_bounded(&lower, id)) {
            return true;
        }
        self.exact_terms.binary_search(&normalize(file_name)).is_ok() || self.prefix_terms.iter().any(|term| starts_a_token(&lower, term))
    }
}

/// Whether some alphanumeric run of `lower` begins with `term`: "spotify" hits "com.spotify.client".
fn starts_a_token(lower: &str, term: &str) -> bool {
    lower.split(|c: char| !c.is_alphanumeric()).any(|token| token.starts_with(term))
}

/// Whether `needle` occurs in `haystack` with a non-alphanumeric character or an edge on both sides.
fn contains_bounded(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
    haystack.match_indices(needle).any(|(start, _)| edge(haystack[..start].chars().next_back()) && edge(haystack[start + needle.len()..].chars().next()))
}

/// The sandbox identifier of a `Containers` or `Group Containers` folder, which may be named by UUID.
pub fn container_identifier(folder: &Path) -> Option<String> {
    for name in [".com.apple.containermanager.metadata.plist", "Container.plist"] {
        let Ok(value) = plist::Value::from_file(folder.join(name)) else { continue };
        let Some(dict) = value.as_dictionary() else { continue };
        for key in ["MCMMetadataIdentifier", "CFBundleIdentifier"] {
            if let Some(id) = dict.get(key).and_then(plist::Value::as_string).filter(|id| !id.is_empty()) {
                return Some(id.to_owned());
            }
        }
    }
    None
}

/// Whether a container identifier is one of the app's ids, or a parent or child of one.
pub fn container_matches(identifier: &str, app: &Descriptor) -> bool {
    let identifier = identifier.to_lowercase();
    !identifier.is_empty() && app.ids.iter().any(|id| identifier == *id || identifier.starts_with(&format!("{id}.")) || id.starts_with(&format!("{identifier}.")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spotify() -> Descriptor {
        Descriptor::new(["com.spotify.client"], ["Spotify"])
    }

    #[test]
    fn bundle_ids_match_strongly_and_glued_ids_only_weakly() {
        let app = spotify();
        let strong = found(Strength::Strong, Kind::BundleId);
        assert_eq!(classify("com.spotify.client.plist", &app), strong);
        assert_eq!(classify("com.apple.sharedfilelist.com.spotify.client.sfl2", &app), strong);
        assert_eq!(classify("com.spotify.clientHelper", &app), found(Strength::Weak, Kind::BundleId));
    }

    #[test]
    fn names_match_at_token_starts_only() {
        let app = spotify();
        assert_eq!(classify("Spotify", &app), found(Strength::Strong, Kind::Name));
        assert_eq!(classify("spotify-cache", &app), found(Strength::Strong, Kind::Name));
        assert_eq!(classify("spotifyd", &app), found(Strength::Weak, Kind::Name));
        assert_eq!(classify("multispotlight", &app), None);
        assert_eq!(classify("com.apple.Spotify", &app), None);
    }

    #[test]
    fn short_names_need_an_exact_match() {
        let app = Descriptor::new(["org.golang.go"], ["Go"]);
        assert_eq!(classify("Go", &app), None);
        assert_eq!(classify("Google", &app), None);
    }

    #[test]
    fn vendor_folders_match_weakly() {
        let app = Descriptor::new(["com.parallels.desktop.console"], ["Parallels Desktop"]);
        assert_eq!(classify("Parallels", &app), found(Strength::Weak, Kind::Vendor));
        assert_eq!(classify("com.parallels.Parallels Desktop.plist", &app), found(Strength::Weak, Kind::Vendor));
    }

    #[test]
    fn other_apps_claim_their_names_and_ids() {
        let others = [Descriptor::new(["com.microsoft.vscode"], ["Visual Studio Code", "Code"])];
        let collisions = Collisions::new(&others);
        assert!(collisions.claims("Code"));
        assert!(collisions.claims("com.microsoft.VSCode.plist"));
        assert!(!collisions.claims("Spotify"));
    }

    #[test]
    fn protected_names_ignore_case_and_extension() {
        assert!(is_protected(Path::new("/Users/a/Library/Preferences/com.apple.Finder.plist")));
        assert!(is_protected(Path::new("/Users/a/Library/Containers")));
        assert!(!is_protected(Path::new("/Users/a/Library/Preferences/com.spotify.client.plist")));
    }

    #[test]
    fn containers_match_parents_and_children() {
        let app = spotify();
        assert!(container_matches("com.spotify.client.helper", &app));
        assert!(container_matches("com.spotify", &app));
        assert!(!container_matches("com.spotifyx", &app));
    }
}
