use std::{path::Path, sync::LazyLock};
#[derive(Clone)]
pub struct ChangelogEntry { pub major: u64, pub minor: u64, pub patch: u64, pub suffix: Option<String>, pub version: Option<String>, pub content: String }
fn entry_version(entry: &ChangelogEntry) -> String { entry.version.clone().unwrap_or_else(|| format!("{}.{}.{}{}", entry.major, entry.minor, entry.patch, entry.suffix.as_ref().map(|suffix| format!("-{suffix}")).unwrap_or_default())) }
fn foreign(version: &str) -> bool {
    static FOREIGN: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^\d+\.\d+\.\d+-0\.beta\.").expect("foreign version regex"));
    version.starts_with("0.0.0-omob.") || FOREIGN.is_match(version)
}
fn compare_strings(left: &str, right: &str) -> Option<std::cmp::Ordering> {
    static CALVER: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^(\d{4})\.(\d{1,2})\.(\d{1,2})(?:-([2-9]\d*))?$").expect("calver regex"));
    if let (Some(left), Some(right)) = (CALVER.captures(left), CALVER.captures(right)) {
        let parts = |captures: regex::Captures<'_>| -> Option<Vec<u64>> { (1..=4).map(|index| captures.get(index).map_or(Ok(1), |value| value.as_str().parse())).collect::<Result<Vec<_>, _>>().ok() };
        return Some(parts(left)?.cmp(&parts(right)?));
    }
    if foreign(left) != foreign(right) { return None; }
    Some(semver::Version::parse(left.strip_prefix('v').unwrap_or(left)).ok()?.cmp(&semver::Version::parse(right.strip_prefix('v').unwrap_or(right)).ok()?))
}
pub fn compare_versions(left: &ChangelogEntry, right: &ChangelogEntry) -> i8 { match compare_strings(&entry_version(left), &entry_version(right)) { Some(std::cmp::Ordering::Less) => -1, Some(std::cmp::Ordering::Greater) => 1, _ => 0 } }
pub fn get_new_entries(entries: &[ChangelogEntry], last: &str, current: Option<&str>) -> Vec<ChangelogEntry> {
    static VERSION: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$").expect("version regex"));
    if !VERSION.is_match(last) || foreign(last) { return Vec::new(); }
    let current = current.filter(|version| VERSION.is_match(version));
    if current.is_some_and(|current| foreign(current) || compare_strings(last, current).is_none()) { return Vec::new(); }
    let mut seen = std::collections::BTreeSet::new();
    entries.iter().filter(|entry| {
        let version = entry_version(entry);
        compare_strings(&version, last) == Some(std::cmp::Ordering::Greater)
            && current.is_none_or(|current| compare_strings(&version, current) != Some(std::cmp::Ordering::Greater)) && seen.insert(version)
    }).cloned().collect()
}
fn valid_date(value: &str) -> bool {
    let parts: Vec<_> = value.split('-').filter_map(|part| part.parse::<u32>().ok()).collect();
    if parts.len() != 3 { return false; }
    let (year, month, day) = (parts[0], parts[1], parts[2]);
    let days = match month { 1 | 3 | 5 | 7 | 8 | 10 | 12 => 31, 4 | 6 | 9 | 11 => 30, 2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29, 2 => 28, _ => 0 };
    day > 0 && day <= days
}
pub fn parse_changelog(path: &Path) -> Vec<ChangelogEntry> {
    let Ok(content) = std::fs::read_to_string(path) else { return Vec::new(); };
    parse_changelog_text(&content)
}
pub fn parse_changelog_text(content: &str) -> Vec<ChangelogEntry> {
    static HEADER: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^##[ \t]+(?:\[([0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?)\]|([0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)))(?:[ \t]+-[ \t]+(\d{4}-\d{2}-\d{2}))?[ \t]*$").expect("changelog header regex"));
    static UNRELEASED: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^##[ \t]+\[?Unreleased\]?[ \t]*(?:-[ \t]+\d{4}-\d{2}-\d{2})?[ \t]*$").expect("unreleased regex"));
    let mut entries = Vec::new();
    let mut current: Option<ChangelogEntry> = None;
    let mut lines = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for line in content.split('\n') {
        let indentation = line.chars().take_while(|character| *character == ' ').count();
        let tail = &line[indentation..];
        if indentation <= 3 && let Some(character @ ('`' | '~')) = tail.chars().next() {
            let length = tail.chars().take_while(|value| *value == character).count();
            if length >= 3 { if let Some((open, size)) = fence { if open == character && length >= size { fence = None; } } else { fence = Some((character, length)); } }
        }
        let header = if fence.is_none() { HEADER.captures(line) } else { None };
        if header.is_some() || (fence.is_none() && UNRELEASED.is_match(line)) {
            if let Some(mut entry) = current.take() && !lines.is_empty() { entry.content = lines.join("\n").trim().to_owned(); entries.push(entry); }
            lines.clear();
            if let Some(header) = header && valid_date(header.get(3).map_or("2026-01-01", |date| date.as_str())) {
                let version = header.get(1).or_else(|| header.get(2)).expect("header version").as_str();
                let (numbers, suffix) = version.split_once('-').map_or((version, None), |(numbers, suffix)| (numbers, Some(suffix.to_owned())));
                let parts: Vec<u64> = numbers.split('.').filter_map(|part| part.parse().ok()).collect();
                if parts.len() == 3 { current = Some(ChangelogEntry { major: parts[0], minor: parts[1], patch: parts[2], suffix, version: Some(version.to_owned()), content: String::new() }); }
            }
        } else if current.is_some() { lines.push(line); }
    }
    if let Some(mut entry) = current && !lines.is_empty() { entry.content = lines.join("\n").trim().to_owned(); entries.push(entry); }
    entries
}
pub use crate::config::get_changelog_path;
pub fn normalize_changelog_links(markdown: &str, version: &str) -> String {
    static LINK: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(!?\[[^\]\n]+\]\()([^\s)]+)((?:\s+[^)]*)?\))").expect("markdown link regex"));
    static SCHEME: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(?i)^[a-z][a-z0-9+.-]*:").expect("URL scheme regex"));
    let tag = if version.starts_with('v') { version.to_owned() } else { format!("v{version}") };
    LINK.replace_all(markdown, |captures: &regex::Captures<'_>| {
        let mut target = captures[2].to_owned();
        let repo = "https://github.com/earendil-works/pi";
        for legacy in ["https://github.com/badlogic/pi-mono", "https://github.com/earendil-works/pi-mono"] {
            if let Some(rest) = target.strip_prefix(legacy) && (rest.is_empty() || rest.starts_with('/')) { target = format!("{repo}{rest}"); break; }
        }
        for route in ["blob", "tree"] { for branch in ["main", "master"] {
            if let Some(rest) = target.strip_prefix(&format!("{repo}/{route}/{branch}/")) { target = format!("{repo}/{route}/{tag}/{rest}"); }
        } }
        if !target.starts_with('#') && !target.starts_with("//") && !SCHEME.is_match(&target) {
            let (before_hash, fragment) = target.split_once('#').map_or((target.as_str(), String::new()), |(path, fragment)| (path, format!("#{fragment}")));
            let (path, query) = before_hash.split_once('?').map_or((before_hash, String::new()), |(path, query)| (path, format!("?{query}")));
            if !path.is_empty() {
                let path = path.replace('\\', "/");
                let joined = if path.starts_with('/') { path.trim_start_matches('/').to_owned() } else { format!("packages/coding-agent/{path}") };
                let mut parts = Vec::new();
                for part in joined.split('/') { match part { "" | "." => {}, ".." if parts.last().is_some_and(|part| *part != "..") => { parts.pop(); }, part => parts.push(part) } }
                let normalized = parts.join("/");
                if !normalized.is_empty() && !normalized.starts_with("../") && normalized != ".." {
                    let route = if path.ends_with('/') || !parts.last().is_some_and(|part| part.contains('.')) { "tree" } else { "blob" };
                    const URI: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS.add(b' ').add(b'"').add(b'<').add(b'>').add(b'[').add(b']').add(b'\\').add(b'^').add(b'`').add(b'{').add(b'|').add(b'}').add(b'%');
                    target = format!("{repo}/{route}/{tag}/{}{query}{fragment}", percent_encoding::utf8_percent_encode(&normalized, URI));
                }
            }
        }
        format!("{}{target}{}", &captures[1], &captures[3])
    }).into_owned()
}
