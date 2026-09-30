//! Port of senpi `packages/tui/src/image-markers.ts`.
//!
//! Tracks the ids of atomic `[Image #N]` markers living in editor text. The registry
//! deliberately stores NO image payload: the owner keeps the bytes keyed by id, and this module
//! only guarantees that the visible numbers stay a contiguous `1..k` sequence so the Nth marker
//! in reading order maps to the Nth submitted image.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;

pub static IMAGE_MARKER_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[Image #([1-9][0-9]*)\]").expect("valid image marker regex"));

static IMAGE_MARKER_SINGLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[Image #([1-9][0-9]*)\]$").expect("valid image marker regex"));

/// Registry state for transfer between editor instances; ids only, never image bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditorImageState {
    pub ids: Vec<u64>,
    pub image_counter: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageMarkerRemoval {
    pub text: String,
    pub removed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageMarkerCanonicalization {
    pub text: String,
    /// Original ids in reading order; index `i` became marker `[Image #${i + 1}]`.
    pub order: Vec<u64>,
}

pub fn format_image_marker(id: u64) -> String {
    format!("[Image #{id}]")
}

pub fn is_image_marker(segment: &str) -> bool {
    segment.len() >= 10 && IMAGE_MARKER_SINGLE.is_match(segment)
}

pub fn image_marker_id(segment: &str) -> Option<u64> {
    if segment.len() < 10 {
        return None;
    }
    IMAGE_MARKER_SINGLE
        .captures(segment)
        .and_then(|captures| captures.get(1))
        .and_then(|m| m.as_str().parse().ok())
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

#[derive(Debug, Default, Clone)]
pub struct ImageMarkerRegistry {
    entries: BTreeSet<u64>,
    image_counter: u64,
}

impl ImageMarkerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers the next id and returns its canonical marker.
    pub fn add(&mut self) -> String {
        self.image_counter += 1;
        self.entries.insert(self.image_counter);
        format_image_marker(self.image_counter)
    }

    /// Registered markers occurring EXACTLY once in `text` - the only ones safe to treat as atomic.
    pub fn authorized_markers(&self, text: &str) -> BTreeSet<String> {
        let mut markers = BTreeSet::new();
        for id in &self.entries {
            let marker = format_image_marker(*id);
            if count_occurrences(text, &marker) == 1 {
                markers.insert(marker);
            }
        }
        markers
    }

    /// Rewrites authorized markers to `1..k` in reading order, returning the original id order.
    pub fn canonicalize(&mut self, text: &str) -> ImageMarkerCanonicalization {
        let order = self.ids(text);
        if order.is_empty() {
            return ImageMarkerCanonicalization {
                text: text.to_string(),
                order,
            };
        }

        let renumbered: std::collections::BTreeMap<u64, u64> = order
            .iter()
            .enumerate()
            .map(|(index, id)| (*id, index as u64 + 1))
            .collect();
        let updated_text = IMAGE_MARKER_REGEX
            .replace_all(text, |captures: &regex::Captures<'_>| {
                let marker = captures.get(0).expect("group 0").as_str();
                let id = image_marker_id(marker);
                match id.and_then(|id| renumbered.get(&id)) {
                    Some(new_id) => format_image_marker(*new_id),
                    None => marker.to_string(),
                }
            })
            .into_owned();
        self.entries = (1..=order.len() as u64).collect();
        self.image_counter = self.entries.len() as u64;
        ImageMarkerCanonicalization {
            text: updated_text,
            order,
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.image_counter = 0;
    }

    /// Registered, authorized ids in TEXT READING ORDER (not insertion order).
    pub fn ids(&self, text: &str) -> Vec<u64> {
        let authorized = self.authorized_markers(text);
        let mut ordered = Vec::new();
        for captures in IMAGE_MARKER_REGEX.captures_iter(text) {
            let marker = captures.get(0).expect("group 0").as_str();
            if !authorized.contains(marker) {
                continue;
            }
            if let Some(id) = captures.get(1).and_then(|m| m.as_str().parse::<u64>().ok()) {
                ordered.push(id);
            }
        }
        ordered
    }

    pub fn install(&mut self, state: &EditorImageState, text: &str) {
        self.restore(state);
        self.prune(text, None);
    }

    pub fn prune(&mut self, text: &str, previous_text: Option<&str>) {
        let entries: Vec<u64> = self.entries.iter().copied().collect();
        for id in entries {
            let marker = format_image_marker(id);
            let was_authorized =
                previous_text.is_none_or(|previous| count_occurrences(previous, &marker) == 1);
            if !was_authorized || count_occurrences(text, &marker) != 1 {
                self.entries.remove(&id);
            }
        }
        self.image_counter = self.entries.iter().copied().max().unwrap_or(0);
    }

    /// Deletes `id`'s marker from `text` and renumbers every higher id downward by one.
    pub fn remove(&mut self, id: u64, text: &str) -> ImageMarkerRemoval {
        if !self.entries.remove(&id) {
            return ImageMarkerRemoval {
                text: text.to_string(),
                removed: false,
            };
        }

        let mut updated_text = replace_single_occurrence(text, &format_image_marker(id), "");
        let higher_ids: Vec<u64> = self
            .entries
            .iter()
            .copied()
            .filter(|entry_id| *entry_id > id)
            .collect();
        for old_id in higher_ids {
            self.entries.remove(&old_id);
            let marker = format_image_marker(old_id);
            if count_occurrences(&updated_text, &marker) != 1 {
                continue;
            }
            let new_id = old_id - 1;
            updated_text = replace_single_occurrence(
                &updated_text,
                &marker,
                &format_image_marker(new_id),
            );
            self.entries.insert(new_id);
        }
        self.image_counter = self.entries.iter().copied().max().unwrap_or(0);
        ImageMarkerRemoval {
            text: updated_text,
            removed: true,
        }
    }

    pub fn restore(&mut self, state: &EditorImageState) {
        self.entries = state.ids.iter().copied().collect();
        self.image_counter = self
            .entries
            .iter()
            .copied()
            .max()
            .unwrap_or(0)
            .max(state.image_counter);
    }

    pub fn snapshot(&self) -> EditorImageState {
        EditorImageState {
            ids: self.entries.iter().copied().collect(),
            image_counter: self.image_counter,
        }
    }
}
