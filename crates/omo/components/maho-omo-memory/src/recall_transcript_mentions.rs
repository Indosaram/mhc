//! Transcript-mention index for the kibitzer recall channel (latest `recall-transcript-mentions.ts`).
//!
//! Candidate collection runs at every prompt and every tool call. The original shape serialized the
//! whole window once and ran one match per corpus document over that string; this index computes
//! mentions per entry, caches them by entry id, and unions the window, so a trigger pays only for
//! the entries it has not seen. The newest branch entry is never cached (it may still be
//! streaming). A path that could straddle an array seam falls back to the whole-window scan, so the
//! result stays exact either way.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

/// Raw-entry window the exclusion scan covers (tool calls and results included).
pub const RECALL_PATH_ENTRY_WINDOW: usize = 200;

/// Sessions kept in the per-entry cache; a shared host interleaves a handful at most.
const MAX_TRACKED_SESSIONS: usize = 8;

/// Array separators of the serialized window. A path containing one could straddle two entries.
const ARRAY_SEAMS: [&str; 4] = ["[{", "},", ",{", "}]"];

/// Minimal corpus projection the index needs; `RecallDocument` satisfies it structurally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MentionDocument {
    pub path: String,
}

/// One collection trigger's window and corpus.
#[derive(Clone, Debug)]
pub struct TranscriptMentionInput<'a> {
    pub session_id: &'a str,
    /// The full branch; the window is applied here so "newest entry" is unambiguous.
    pub entries: &'a [Value],
    pub documents: &'a [MentionDocument],
}

/// Corpus paths mentioned anywhere in the last [`RECALL_PATH_ENTRY_WINDOW`] branch entries.
pub trait TranscriptMentionIndex {
    fn excluded_paths(&mut self, input: TranscriptMentionInput<'_>) -> BTreeSet<String>;
}

#[derive(Clone, Debug)]
struct MatcherSet {
    documents: Vec<String>,
    matchers: Vec<String>,
    /// False when any path could straddle the array seam, which forces the whole-window scan.
    per_entry_safe: bool,
}

/// `JSON.stringify(path).slice(1, -1)`: the literal the serialized window carries for this path.
fn json_inner(path: &str) -> String {
    let quoted = serde_json::to_string(path).unwrap_or_else(|_| format!("\"{path}\""));
    quoted
        .get(1..quoted.len().saturating_sub(1))
        .unwrap_or("")
        .to_string()
}

/// JS `\s`: the delimiters a mention must be followed by at end of an entry's JSON.
fn is_js_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\u{0009}'..='\u{000d}'
            | '\u{0020}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

/// The upstream lookahead `(?=$|[\s"'`\]})>:;,!?]|\\["nrtbf])`, both arms reachable: a bare
/// delimiter character, or a JSON escape sequence (backslash + one of `"nrtbf`).
fn delimiter_follows(after: &str) -> bool {
    let mut chars = after.chars();
    let Some(first) = chars.next() else {
        return true;
    };
    if is_js_whitespace(first)
        || matches!(first, '"' | '\'' | '`' | ']' | ')' | '}' | '>' | ':' | ';' | ',' | '!' | '?')
    {
        return true;
    }
    if first == '\\'
        && let Some(second) = chars.next()
    {
        return matches!(second, '"' | 'n' | 'r' | 't' | 'b' | 'f');
    }
    false
}

/// Literal occurrence of `matcher` in `json` that is followed by a transcript delimiter.
fn mentions(json: &str, matcher: &str) -> bool {
    if matcher.is_empty() {
        return false;
    }
    let mut from = 0usize;
    while from < json.len() {
        let Some(relative) = json[from..].find(matcher) else {
            return false;
        };
        let at = from + relative;
        let after = at + matcher.len();
        if delimiter_follows(json.get(after..).unwrap_or("")) {
            return true;
        }
        from = at + json[at..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

fn build_matcher_set(documents: &[MentionDocument]) -> MatcherSet {
    let paths: Vec<String> = documents.iter().map(|document| document.path.clone()).collect();
    let matchers: Vec<String> = paths.iter().map(|path| json_inner(path)).collect();
    let per_entry_safe = documents
        .iter()
        .all(|document| !ARRAY_SEAMS.iter().any(|seam| document.path.contains(seam)));
    MatcherSet { documents: paths, matchers, per_entry_safe }
}

fn mentions_in(json: &str, set: &MatcherSet) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for (path, matcher) in set.documents.iter().zip(set.matchers.iter()) {
        if mentions(json, matcher) {
            found.insert(path.clone());
        }
    }
    found
}

fn entry_id(entry: &Value) -> Option<String> {
    entry
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// The pre-index behaviour, kept as the fallback for seam-unsafe corpora and as the differential
/// oracle in tests: one serialized window, one match per document.
pub fn excluded_paths_by_window_scan(
    window: &[Value],
    documents: &[MentionDocument],
) -> BTreeSet<String> {
    let json = serde_json::to_string(window).unwrap_or_else(|_| "[]".to_string());
    mentions_in(&json, &build_matcher_set(documents))
}

#[derive(Default)]
struct SessionMentions {
    documents: Option<Vec<String>>,
    by_entry_id: BTreeMap<String, BTreeSet<String>>,
}

/// Incremental per-entry mention cache; insertion order is the LRU order.
#[derive(Default)]
pub struct BranchMentionIndex {
    matcher_set: Option<MatcherSet>,
    sessions: Vec<(String, SessionMentions)>,
}

impl BranchMentionIndex {
    pub fn new() -> Self {
        Self::default()
    }

    fn matchers_for(&mut self, documents: &[MentionDocument]) -> MatcherSet {
        let paths: Vec<String> = documents.iter().map(|document| document.path.clone()).collect();
        if let Some(set) = &self.matcher_set
            && set.documents == paths
        {
            return set.clone();
        }
        let set = build_matcher_set(documents);
        self.matcher_set = Some(set.clone());
        set
    }

    /// Moves the session to the back of the LRU and drops its cache when the corpus moved.
    fn session_for(&mut self, session_id: &str, documents: Vec<String>) -> &mut SessionMentions {
        let mut session = match self.sessions.iter().position(|(id, _)| id == session_id) {
            Some(position) => self.sessions.remove(position).1,
            None => SessionMentions::default(),
        };
        if session.documents.as_ref() != Some(&documents) {
            session.by_entry_id.clear();
            session.documents = Some(documents);
        }
        self.sessions.push((session_id.to_string(), session));
        while self.sessions.len() > MAX_TRACKED_SESSIONS {
            self.sessions.remove(0);
        }
        self
            .sessions
            .last_mut()
            .map(|(_, session)| session)
            .expect("the session was just pushed")
    }
}

impl TranscriptMentionIndex for BranchMentionIndex {
    fn excluded_paths(&mut self, input: TranscriptMentionInput<'_>) -> BTreeSet<String> {
        let set = self.matchers_for(input.documents);
        let start = input.entries.len().saturating_sub(RECALL_PATH_ENTRY_WINDOW);
        let window = &input.entries[start..];
        if !set.per_entry_safe {
            let json = serde_json::to_string(window).unwrap_or_else(|_| "[]".to_string());
            return mentions_in(&json, &set);
        }

        let session = self.session_for(input.session_id, set.documents.clone());
        let mut excluded = BTreeSet::new();
        let mut live = BTreeSet::new();
        for (index, entry) in window.iter().enumerate() {
            let newest = index + 1 == window.len();
            let id = if newest { None } else { entry_id(entry) };
            match id {
                None => {
                    let json = serde_json::to_string(entry).unwrap_or_else(|_| "null".to_string());
                    excluded.extend(mentions_in(&json, &set));
                }
                Some(id) => {
                    let found = match session.by_entry_id.get(&id).cloned() {
                        Some(found) => found,
                        None => {
                            let json =
                                serde_json::to_string(entry).unwrap_or_else(|_| "null".to_string());
                            let found = mentions_in(&json, &set);
                            session.by_entry_id.insert(id.clone(), found.clone());
                            found
                        }
                    };
                    live.insert(id);
                    excluded.extend(found);
                }
            }
        }
        session.by_entry_id.retain(|id, _| live.contains(id));
        excluded
    }
}

/// `createTranscriptMentionIndex`.
pub fn create_transcript_mention_index() -> BranchMentionIndex {
    BranchMentionIndex::new()
}

#[cfg(test)]
#[path = "recall_transcript_mentions_tests.rs"]
mod tests;
