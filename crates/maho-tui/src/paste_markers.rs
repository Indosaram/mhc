//! Port of senpi `packages/tui/src/paste-markers.ts`.
//!
//! Registry-backed atomic `[paste #N ...]` markers: large pastes collapse to a single marker
//! that segments as one grapheme, expands back to its content on submit, and renumbers its
//! survivors when one is deleted.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use regex::Regex;

use crate::utils::{graphemes, word_segments};

static PASTE_MARKER_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[paste #([0-9]+)( (\+[0-9]+ lines|[0-9]+ chars))?\]")
        .expect("valid paste marker regex")
});
static PASTE_MARKER_SINGLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\[paste #([0-9]+)( (\+[0-9]+ lines|[0-9]+ chars))?\]$")
        .expect("valid paste marker regex")
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteMarkerEntry {
    pub content: String,
    pub marker: String,
    pub line_count: u64,
    pub char_count: u64,
}

/// Paste registry state for transfer between editor instances.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditorPasteState {
    pub pastes: BTreeMap<u64, String>,
    pub markers: Option<BTreeMap<u64, String>>,
    pub paste_counter: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteMarkerRemoval {
    pub text: String,
    pub removed: bool,
}

/// One `Intl.SegmentData`-shaped base segment: `index` is a byte offset into the source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentRef<'a> {
    pub index: usize,
    pub segment: &'a str,
}

/// A base segment or an atomic marker slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerSegment {
    pub index: usize,
    pub segment: String,
}

pub fn format_paste_marker(id: u64, line_count: u64, char_count: u64) -> String {
    if line_count > 10 {
        format!("[paste #{id} +{line_count} lines]")
    } else {
        format!("[paste #{id} {char_count} chars]")
    }
}

fn count_lines(content: &str) -> u64 {
    let mut line_count = 1;
    for ch in content.chars() {
        if ch == '\n' {
            line_count += 1;
        }
    }
    line_count
}

fn create_entry(id: u64, content: &str, marker: Option<&str>) -> PasteMarkerEntry {
    let char_count = content.encode_utf16().count() as u64;
    let line_count = marker
        .and_then(|marker| PASTE_MARKER_SINGLE.captures(marker))
        .and_then(|captures| captures.get(3).map(|m| m.as_str().to_string()))
        .and_then(|group| {
            group
                .strip_prefix('+')
                .and_then(|rest| rest.strip_suffix(" lines"))
                .and_then(|n| n.parse::<u64>().ok())
        })
        .unwrap_or_else(|| count_lines(content));
    let canonical = format_paste_marker(id, line_count, char_count);
    let marker = match marker {
        Some(marker) if marker == canonical => marker.to_string(),
        _ => canonical,
    };
    PasteMarkerEntry {
        content: content.to_string(),
        marker,
        line_count,
        char_count,
    }
}

fn count_occurrences(text: &str, marker: &str) -> usize {
    let mut count = 0;
    let mut offset = 0;
    while offset <= text.len().saturating_sub(marker.len()) {
        let Some(index) = text[offset..].find(marker).map(|i| i + offset) else {
            break;
        };
        count += 1;
        offset = index + marker.len();
    }
    count
}

fn replace_single_occurrence(text: &str, marker: &str, replacement: &str) -> String {
    let Some(index) = text.find(marker) else {
        return text.to_string();
    };
    if text[index + marker.len()..].contains(marker) {
        return text.to_string();
    }
    format!(
        "{}{}{}",
        &text[..index],
        replacement,
        &text[index + marker.len()..]
    )
}

fn entries_from_state(state: &EditorPasteState) -> BTreeMap<u64, PasteMarkerEntry> {
    state
        .pastes
        .iter()
        .map(|(id, content)| {
            let marker = state.markers.as_ref().and_then(|markers| markers.get(id));
            (*id, create_entry(*id, content, marker.map(String::as_str)))
        })
        .collect()
}

pub fn expand_paste_markers(text: &str, state: &EditorPasteState) -> String {
    let entries = entries_from_state(state);
    let mut by_marker: BTreeMap<&str, &PasteMarkerEntry> = BTreeMap::new();
    for entry in entries.values() {
        by_marker.insert(entry.marker.as_str(), entry);
    }

    let mut matches: Vec<(usize, usize, String)> = Vec::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for captures in PASTE_MARKER_REGEX.captures_iter(text) {
        let whole = captures.get(0).expect("group 0");
        let marker = whole.as_str();
        if !by_marker.contains_key(marker) {
            continue;
        }
        matches.push((whole.start(), whole.end(), marker.to_string()));
        *counts.entry(marker.to_string()).or_insert(0) += 1;
    }
    if matches.is_empty() {
        return text.to_string();
    }

    let mut out = String::new();
    let mut offset = 0;
    for (start, end, marker) in &matches {
        out.push_str(&text[offset..*start]);
        let entry = by_marker.get(marker.as_str()).expect("marker registered");
        if counts.get(marker.as_str()) == Some(&1) {
            out.push_str(&entry.content);
        } else {
            out.push_str(marker);
        }
        offset = *end;
    }
    out.push_str(&text[offset..]);
    out
}

pub fn is_paste_marker(segment: &str) -> bool {
    segment.len() >= 10 && PASTE_MARKER_SINGLE.is_match(segment)
}

pub fn paste_marker_id(segment: &str) -> Option<u64> {
    if segment.len() < 10 {
        return None;
    }
    PASTE_MARKER_SINGLE
        .captures(segment)
        .and_then(|captures| captures.get(1))
        .and_then(|m| m.as_str().parse().ok())
}

/// Grapheme segments of `text` as `Intl.SegmentData`-shaped refs.
pub fn grapheme_segment_refs(text: &str) -> Vec<SegmentRef<'_>> {
    let mut refs = Vec::new();
    let mut offset = 0;
    for grapheme in graphemes(text) {
        refs.push(SegmentRef {
            index: offset,
            segment: grapheme,
        });
        offset += grapheme.len();
    }
    refs
}

/// Word segments of `text` as `Intl.SegmentData`-shaped refs.
pub fn word_segment_refs(text: &str) -> Vec<SegmentRef<'_>> {
    word_segments(text)
        .into_iter()
        .map(|segment| SegmentRef {
            index: segment.index,
            segment: segment.segment,
        })
        .collect()
}

/// Segments `text` treating every literal in `valid_markers` as a single atomic segment.
pub fn segment_with_markers(
    text: &str,
    base: &[SegmentRef<'_>],
    valid_markers: &BTreeSet<String>,
) -> Vec<MarkerSegment> {
    let base_owned = || {
        base.iter()
            .map(|segment| MarkerSegment {
                index: segment.index,
                segment: segment.segment.to_string(),
            })
            .collect::<Vec<_>>()
    };
    if valid_markers.is_empty() {
        return base_owned();
    }

    let mut found: Vec<(usize, usize)> = Vec::new();
    for marker in valid_markers {
        if let Some(start) = text.find(marker.as_str()) {
            found.push((start, start + marker.len()));
        }
    }
    if found.is_empty() {
        return base_owned();
    }
    found.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));

    // Nested/overlapping literals would desync the single-pass walk below; keep the outermost.
    let mut markers: Vec<(usize, usize)> = Vec::new();
    for range in found {
        if let Some(previous) = markers.last() {
            if range.0 < previous.1 {
                continue;
            }
        }
        markers.push(range);
    }

    let mut result = Vec::new();
    let mut marker_index = 0usize;
    let mut marker = markers.first().copied();
    for segment in base {
        while let Some(current) = marker {
            if segment.index >= current.1 {
                marker_index += 1;
                marker = markers.get(marker_index).copied();
            } else {
                break;
            }
        }
        match marker {
            None => result.push(MarkerSegment {
                index: segment.index,
                segment: segment.segment.to_string(),
            }),
            Some(current) if segment.index < current.0 => result.push(MarkerSegment {
                index: segment.index,
                segment: segment.segment.to_string(),
            }),
            Some(current) if segment.index == current.0 => result.push(MarkerSegment {
                index: current.0,
                segment: text[current.0..current.1].to_string(),
            }),
            Some(_) => {}
        }
    }
    result
}

/// Backward-compatible wrapper: paste markers are just one atomic marker set.
pub fn segment_with_paste_markers(
    text: &str,
    base: &[SegmentRef<'_>],
    valid_markers: &BTreeSet<String>,
) -> Vec<MarkerSegment> {
    segment_with_markers(text, base, valid_markers)
}

#[derive(Debug, Default, Clone)]
pub struct PasteMarkerRegistry {
    entries: BTreeMap<u64, PasteMarkerEntry>,
    paste_counter: u64,
    metadata_builds: u64,
}

impl PasteMarkerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, content: &str, line_count: u64, char_count: u64) -> String {
        self.paste_counter += 1;
        let id = self.paste_counter;
        let marker = format_paste_marker(id, line_count, char_count);
        self.entries.insert(
            id,
            PasteMarkerEntry {
                content: content.to_string(),
                marker: marker.clone(),
                line_count,
                char_count,
            },
        );
        self.metadata_builds += 1;
        marker
    }

    pub fn authorized_markers(&self, text: &str) -> BTreeSet<String> {
        let mut markers = BTreeSet::new();
        for entry in self.entries.values() {
            if count_occurrences(text, &entry.marker) == 1 {
                markers.insert(entry.marker.clone());
            }
        }
        markers
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.paste_counter = 0;
    }

    pub fn expand(&self, text: &str) -> String {
        expand_paste_markers(text, &self.snapshot())
    }

    pub fn metadata_build_count(&self) -> u64 {
        self.metadata_builds
    }

    pub fn install(&mut self, state: &EditorPasteState, text: &str) {
        self.entries = entries_from_state(state);
        self.metadata_builds += self.entries.len() as u64;
        self.paste_counter = state
            .paste_counter
            .max(self.entries.keys().copied().max().unwrap_or(0));
        self.prune(text, None);
    }

    pub fn prune(&mut self, text: &str, previous_text: Option<&str>) {
        let ids: Vec<u64> = self.entries.keys().copied().collect();
        for id in ids {
            let entry = self.entries.get(&id).expect("entry present");
            let marker = entry.marker.clone();
            let was_authorized = previous_text
                .is_none_or(|previous| count_occurrences(previous, &marker) == 1);
            if !was_authorized || count_occurrences(text, &marker) != 1 {
                self.entries.remove(&id);
            }
        }
        self.paste_counter = self.entries.keys().copied().max().unwrap_or(0);
    }

    pub fn remove(&mut self, id: u64, text: &str) -> PasteMarkerRemoval {
        let Some(removed_entry) = self.entries.get(&id).cloned() else {
            return PasteMarkerRemoval {
                text: text.to_string(),
                removed: false,
            };
        };
        self.entries.remove(&id);

        let mut updated_text = replace_single_occurrence(text, &removed_entry.marker, "");
        let higher_ids: Vec<u64> = self
            .entries
            .keys()
            .copied()
            .filter(|entry_id| *entry_id > id)
            .collect();
        for old_id in higher_ids {
            let entry = self.entries.get(&old_id).cloned().expect("entry present");
            self.entries.remove(&old_id);
            if count_occurrences(&updated_text, &entry.marker) != 1 {
                continue;
            }
            let new_id = old_id - 1;
            let renamed = format_paste_marker(new_id, entry.line_count, entry.char_count);
            updated_text = replace_single_occurrence(&updated_text, &entry.marker, &renamed);
            self.entries.insert(
                new_id,
                PasteMarkerEntry {
                    marker: renamed,
                    ..entry
                },
            );
        }
        self.paste_counter = self.entries.keys().copied().max().unwrap_or(0);
        PasteMarkerRemoval {
            text: updated_text,
            removed: true,
        }
    }

    pub fn snapshot(&self) -> EditorPasteState {
        let mut pastes = BTreeMap::new();
        let mut markers = BTreeMap::new();
        for (id, entry) in &self.entries {
            pastes.insert(*id, entry.content.clone());
            markers.insert(*id, entry.marker.clone());
        }
        EditorPasteState {
            pastes,
            markers: Some(markers),
            paste_counter: self.paste_counter,
        }
    }

    pub fn restore(&mut self, state: &EditorPasteState) {
        self.entries = entries_from_state(state);
        self.metadata_builds += self.entries.len() as u64;
        self.paste_counter = state
            .paste_counter
            .max(self.entries.keys().copied().max().unwrap_or(0));
    }
}
