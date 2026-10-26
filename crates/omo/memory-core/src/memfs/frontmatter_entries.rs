//! Shared frontmatter entry reader and duplicate/lookalike key detection.

use std::collections::HashSet;

use crate::memfs::frontmatter_scalar::split_frontmatter;
use crate::memfs::lookalike_text::look_alike_skeleton;

/// A single key-value entry parsed from frontmatter text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontmatterEntry {
    pub key: String,
    pub value: String,
    pub line: String,
}

/// The one reading of a frontmatter block's key lines.
pub fn frontmatter_entries(frontmatter_text: &str) -> Vec<FrontmatterEntry> {
    let mut entries = Vec::new();
    for line in frontmatter_text.split('\n') {
        let line = line.trim_end_matches('\r');
        let Some(idx) = line.find(':') else {
            continue;
        };
        if idx == 0 {
            continue;
        }
        entries.push(FrontmatterEntry {
            key: line[..idx].trim().to_string(),
            value: line[idx + 1..].trim().to_string(),
            line: line.to_string(),
        });
    }
    entries
}

/// Extract frontmatter entries from full note content if frontmatter exists.
pub fn content_frontmatter_entries(content: &str) -> Option<Vec<FrontmatterEntry>> {
    split_frontmatter(content).map(|(header, _)| frontmatter_entries(&header))
}

/// Detect duplicate frontmatter keys in note content.
pub fn duplicate_frontmatter_keys(content: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut duplicates = Vec::new();
    let mut dup_set = HashSet::new();
    if let Some(entries) = content_frontmatter_entries(content) {
        for entry in entries {
            if !seen.insert(entry.key.clone())
                && dup_set.insert(entry.key.clone()) {
                    duplicates.push(entry.key);
                }
        }
    }
    duplicates
}

/// How a reader might take a key: its look-alike skeleton with surrounding quotes removed.
pub fn normalized_frontmatter_key(key: &str) -> String {
    look_alike_skeleton(key)
        .trim_matches(|c| c == '"' || c == '\'')
        .trim()
        .to_string()
}

/// Return all keys in content that read as `canonical` but are not spelled exactly so.
pub fn look_alike_frontmatter_keys(content: &str, canonical: &str) -> Vec<String> {
    content_frontmatter_entries(content)
        .unwrap_or_default()
        .into_iter()
        .map(|entry| entry.key)
        .filter(|key| key != canonical && normalized_frontmatter_key(key) == canonical)
        .collect()
}
