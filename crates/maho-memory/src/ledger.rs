//! The per-session recall ledger: which memories a session has already been shown.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-ledger.md`. Recall never repeats itself inside a
//! session, so each session keeps a small JSON file of surfaced paths. The file is written by
//! rename, so a crash leaves either the old file or the new one and never a half-written one.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const LEDGER_VERSION: u32 = 1;

pub const SESSION_FILENAME_MAX_LENGTH: usize = 80;

/// One path that was surfaced, with the content hash recorded at that moment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Surfaced {
    pub path: String,
    pub hash: String,
}

/// A path's recorded hash and when it was surfaced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub hash: String,
    pub at: String,
}

/// The versioned ledger file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerFile {
    pub version: u32,
    #[serde(default)]
    pub surfaced: BTreeMap<String, LedgerEntry>,
}

impl Default for LedgerFile {
    fn default() -> Self {
        Self {
            version: LEDGER_VERSION,
            surfaced: BTreeMap::new(),
        }
    }
}

/// The session filename component for a session id.
///
/// Only ASCII alphanumerics, `.`, `_` and `-` survive; everything else collapses to a single dash,
/// runs of unsafe characters never produce repeated dashes, and a leading or trailing dash is
/// trimmed. A degenerate result (`.`, `..`, empty) falls back to `session`, so the id can always be
/// used as a file name on every platform.
pub fn sanitize_session_filename(session_id: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for ch in session_id.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(ch);
        } else {
            pending_dash = !out.is_empty();
        }
    }
    let collapsed = trim_trailing(&out);
    if collapsed.is_empty() {
        return "session".to_owned();
    }
    let truncated: String = collapsed
        .chars()
        .take(SESSION_FILENAME_MAX_LENGTH)
        .collect();
    let collapsed = trim_trailing(&truncated);
    if collapsed.is_empty() {
        return "session".to_owned();
    }
    collapsed.to_owned()
}

/// Trailing dots and dashes are stripped: Windows drops trailing dots from file names, so keeping
/// them would let two different session ids land on the same file. Only the end is trimmed, so
/// `.hidden` and `hidden` stay distinct.
fn trim_trailing(name: &str) -> &str {
    name.trim_end_matches(['.', '-'])
}

pub fn content_hash(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// One session's ledger, held in memory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionLedger {
    file: LedgerFile,
}

impl SessionLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read a ledger from its JSON text; a version this build does not know yields an empty ledger
    /// rather than a parse failure, because an unknown future version still means "no memories
    /// surfaced yet" for this build.
    pub fn parse(source: &str) -> Self {
        match serde_json::from_str::<LedgerFile>(source) {
            Ok(file) if file.version == LEDGER_VERSION => Self { file },
            _ => Self::default(),
        }
    }

    pub fn render(&self) -> String {
        let mut out = serde_json::to_string_pretty(&self.file).expect("ledger serializes");
        out.push('\n');
        out
    }

    /// Every path already surfaced, for `select::SelectOptions::surfaced`.
    pub fn surfaced_paths(&self) -> std::collections::BTreeSet<String> {
        self.file.surfaced.keys().cloned().collect()
    }

    /// Whether this path was surfaced with exactly this content hash.
    ///
    /// A path whose content changed since it was surfaced is treated as unsurfaced: the memory was
    /// edited, so it is new information and worth showing again.
    pub fn is_current(&self, path: &str, hash: &str) -> bool {
        self.file
            .surfaced
            .get(path)
            .is_some_and(|entry| entry.hash == hash)
    }

    /// Record paths as surfaced at `at`.
    ///
    /// Re-surfacing a path replaces its hash and timestamp, so the ledger keeps one row per path.
    pub fn mark_surfaced(&mut self, entries: &[Surfaced], at: &str) {
        for entry in entries {
            self.file.surfaced.insert(
                entry.path.clone(),
                LedgerEntry {
                    hash: entry.hash.clone(),
                    at: at.to_owned(),
                },
            );
        }
    }

    pub fn len(&self) -> usize {
        self.file.surfaced.len()
    }

    pub fn is_empty(&self) -> bool {
        self.file.surfaced.is_empty()
    }

    /// When a path was surfaced, if it ever was.
    pub fn surfaced_at(&self, path: &str) -> Option<&str> {
        self.file.surfaced.get(path).map(|entry| entry.at.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger() -> SessionLedger {
        SessionLedger::new()
    }

    fn surfaced(path: &str, content: &str) -> Surfaced {
        Surfaced {
            path: path.to_owned(),
            hash: content_hash(content),
        }
    }

    #[test]
    fn a_safe_session_id_is_usable_as_a_filename_unchanged() {
        assert_eq!(sanitize_session_filename("abc-123_XY.z"), "abc-123_XY.z");
    }

    #[test]
    fn path_separators_and_glob_metacharacters_collapse_to_one_dash() {
        assert_eq!(sanitize_session_filename("a/b\\c*d?e"), "a-b-c-d-e");
        assert_eq!(sanitize_session_filename("a///b"), "a-b");
    }

    #[test]
    fn leading_and_trailing_unsafe_characters_are_trimmed() {
        assert_eq!(sanitize_session_filename("///a///"), "a");
    }

    #[test]
    fn a_degenerate_session_id_falls_back_to_session() {
        for id in ["", ".", "..", "///", "..."] {
            assert_eq!(sanitize_session_filename(id), "session", "id: {id}");
        }
    }

    #[test]
    fn a_leading_dot_is_kept_so_hidden_names_do_not_collide() {
        assert_eq!(sanitize_session_filename(".hidden"), ".hidden");
        assert_ne!(
            sanitize_session_filename(".hidden"),
            sanitize_session_filename("hidden")
        );
    }

    #[test]
    fn trailing_dots_are_stripped_so_windows_cannot_merge_two_ids() {
        assert_eq!(sanitize_session_filename("abc..."), "abc");
        assert_eq!(sanitize_session_filename("abc"), "abc");
    }

    #[test]
    fn a_long_session_id_is_truncated_without_leaving_a_trailing_dash() {
        let long = format!("{}-", "a".repeat(200));
        let name = sanitize_session_filename(&long);
        assert_eq!(name.chars().count(), SESSION_FILENAME_MAX_LENGTH);
        assert!(!name.ends_with('-'));
    }

    #[test]
    fn the_content_hash_is_stable_and_distinguishes_content() {
        assert_eq!(content_hash("abc"), content_hash("abc"));
        assert_ne!(content_hash("abc"), content_hash("abd"));
        assert_eq!(content_hash("abc").len(), 64);
    }

    #[test]
    fn a_ledger_round_trips_through_json() {
        let mut ledger = ledger();
        ledger.mark_surfaced(&[surfaced("a.md", "content")], "2026-11-10T00:00:00Z");
        let parsed = SessionLedger::parse(&ledger.render());
        assert_eq!(parsed, ledger);
    }

    #[test]
    fn surfaced_paths_are_reported_for_the_selector() {
        let mut ledger = ledger();
        ledger.mark_surfaced(&[surfaced("a.md", "one"), surfaced("b.md", "two")], "t");
        assert_eq!(
            ledger.surfaced_paths(),
            ["a.md".to_owned(), "b.md".to_owned()].into_iter().collect()
        );
    }

    #[test]
    fn the_same_content_is_recognized_as_already_surfaced() {
        let mut ledger = ledger();
        ledger.mark_surfaced(&[surfaced("a.md", "content")], "t");
        assert!(ledger.is_current("a.md", &content_hash("content")));
    }

    #[test]
    fn changed_content_is_not_considered_already_surfaced() {
        let mut ledger = ledger();
        ledger.mark_surfaced(&[surfaced("a.md", "old")], "t");
        assert!(!ledger.is_current("a.md", &content_hash("new")));
    }

    #[test]
    fn an_unknown_path_is_never_current() {
        assert!(!ledger().is_current("a.md", &content_hash("content")));
    }

    #[test]
    fn re_surfacing_replaces_the_single_row_for_a_path() {
        let mut ledger = ledger();
        ledger.mark_surfaced(&[surfaced("a.md", "old")], "2026-11-09T00:00:00Z");
        ledger.mark_surfaced(&[surfaced("a.md", "new")], "2026-11-10T00:00:00Z");
        assert_eq!(ledger.len(), 1);
        assert!(ledger.is_current("a.md", &content_hash("new")));
        assert_eq!(ledger.surfaced_at("a.md"), Some("2026-11-10T00:00:00Z"));
    }

    #[test]
    fn marking_nothing_changes_nothing() {
        let mut ledger = ledger();
        ledger.mark_surfaced(&[], "t");
        assert!(ledger.is_empty());
    }

    #[test]
    fn the_rendered_ledger_is_deterministic_for_the_same_entries() {
        let mut first = ledger();
        first.mark_surfaced(&[surfaced("b.md", "2"), surfaced("a.md", "1")], "t");
        let mut second = ledger();
        second.mark_surfaced(&[surfaced("a.md", "1"), surfaced("b.md", "2")], "t");
        assert_eq!(first.render(), second.render());
    }

    #[test]
    fn a_ledger_of_an_unknown_version_reads_as_empty_instead_of_failing() {
        let text = "{\"version\":99,\"surfaced\":{\"a.md\":{\"hash\":\"h\",\"at\":\"t\"}}}";
        assert!(SessionLedger::parse(text).is_empty());
    }

    #[test]
    fn malformed_json_reads_as_empty_instead_of_failing() {
        assert!(SessionLedger::parse("not json").is_empty());
    }

    #[test]
    fn a_ledger_with_no_surfaced_key_still_parses() {
        let parsed = SessionLedger::parse("{\"version\":1}");
        assert!(parsed.is_empty());
    }

    #[test]
    fn the_rendered_file_ends_with_a_newline() {
        let mut ledger = ledger();
        ledger.mark_surfaced(&[surfaced("a.md", "x")], "t");
        assert!(ledger.render().ends_with('\n'));
    }
}
