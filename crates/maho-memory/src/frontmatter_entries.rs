//! One reading of a frontmatter block's key lines, plus duplicate and look-alike key detection.
//!
//! Spec: `docs/cleanroom/specs/memory-frontmatter-entries.md`.

use std::collections::HashSet;

use crate::frontmatter::split_frontmatter;
use crate::lookalike_keys::look_alike_skeleton;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontmatterEntry {
    pub key: String,
    pub value: String,
    pub line: String,
}

/// A line is an entry when it contains a `:` that is not the first character; the value is
/// everything after the first `:`, trimmed. Continuation lines and list items have no entry.
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

pub fn content_frontmatter_entries(content: &str) -> Option<Vec<FrontmatterEntry>> {
    split_frontmatter(content).map(|(header, _)| frontmatter_entries(&header))
}

/// Each key reported once, in first-duplicate order, however many times it repeats.
pub fn duplicate_frontmatter_keys(content: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut duplicates = Vec::new();
    let mut dup_set = HashSet::new();
    if let Some(entries) = content_frontmatter_entries(content) {
        for entry in entries {
            if !seen.insert(entry.key.clone()) && dup_set.insert(entry.key.clone()) {
                duplicates.push(entry.key);
            }
        }
    }
    duplicates
}

/// A reader's reading of a key: its look-alike skeleton, unquoted and trimmed.
pub fn normalized_frontmatter_key(key: &str) -> String {
    look_alike_skeleton(key)
        .trim_matches(|c| c == '"' || c == '\'')
        .trim()
        .to_string()
}

pub fn look_alike_frontmatter_keys(content: &str, canonical: &str) -> Vec<String> {
    content_frontmatter_entries(content)
        .unwrap_or_default()
        .into_iter()
        .map(|entry| entry.key)
        .filter(|key| key != canonical && normalized_frontmatter_key(key) == canonical)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(header: &str) -> String {
        format!("---\n{header}\n---\n\nbody\n")
    }

    #[test]
    fn key_lines_become_entries() {
        let entries = frontmatter_entries("description: A note\nkind: fact");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].key, "description");
        assert_eq!(entries[0].value, "A note");
        assert_eq!(entries[1].key, "kind");
        assert_eq!(entries[1].value, "fact");
    }

    #[test]
    fn the_entry_keeps_the_original_line() {
        let entries = frontmatter_entries("  description:  A note  ");
        assert_eq!(entries[0].line, "  description:  A note  ");
        assert_eq!(entries[0].key, "description");
        assert_eq!(entries[0].value, "A note");
    }

    #[test]
    fn the_first_colon_splits_key_from_value() {
        let entries = frontmatter_entries("source: https://example.test/a");
        assert_eq!(entries[0].key, "source");
        assert_eq!(entries[0].value, "https://example.test/a");
    }

    #[test]
    fn a_leading_colon_is_not_an_entry() {
        assert!(frontmatter_entries(": no key").is_empty());
    }

    #[test]
    fn a_line_without_a_colon_is_not_an_entry() {
        assert!(frontmatter_entries("just text\n- list item").is_empty());
    }

    #[test]
    fn an_empty_value_is_kept() {
        let entries = frontmatter_entries("description:");
        assert_eq!(entries[0].key, "description");
        assert_eq!(entries[0].value, "");
    }

    #[test]
    fn a_trailing_carriage_return_is_stripped() {
        let entries = frontmatter_entries("description: A note\r\nkind: fact\r");
        assert_eq!(entries[0].line, "description: A note");
        assert_eq!(entries[1].line, "kind: fact");
    }

    #[test]
    fn entries_are_read_from_the_header_only() {
        let content = note("description: header");
        let entries = content_frontmatter_entries(&content).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].value, "header");
    }

    #[test]
    fn content_without_frontmatter_has_no_entries() {
        assert!(content_frontmatter_entries("plain body\n").is_none());
    }

    #[test]
    fn an_empty_header_has_no_entries() {
        assert_eq!(content_frontmatter_entries(&note("")), Some(vec![]));
    }

    #[test]
    fn a_repeated_key_is_reported_once() {
        let content = note("description: one\ndescription: two\ndescription: three");
        assert_eq!(duplicate_frontmatter_keys(&content), vec!["description"]);
    }

    #[test]
    fn several_duplicated_keys_keep_their_order() {
        let content = note("kind: a\ndescription: x\nkind: b\ndescription: y");
        assert_eq!(
            duplicate_frontmatter_keys(&content),
            vec!["kind", "description"]
        );
    }

    #[test]
    fn distinct_keys_are_not_duplicates() {
        let content = note("description: one\nkind: fact");
        assert!(duplicate_frontmatter_keys(&content).is_empty());
    }

    #[test]
    fn keys_are_compared_case_sensitively_for_duplicates() {
        let content = note("Description: one\ndescription: two");
        assert!(duplicate_frontmatter_keys(&content).is_empty());
    }

    #[test]
    fn a_key_normalizes_to_its_skeleton() {
        assert_eq!(normalized_frontmatter_key("Description"), "description");
        assert_eq!(normalized_frontmatter_key(" desc "), "desc");
    }

    #[test]
    fn surrounding_quotes_are_removed_when_normalizing() {
        assert_eq!(normalized_frontmatter_key("\"description\""), "description");
        assert_eq!(normalized_frontmatter_key("'description'"), "description");
    }

    #[test]
    fn a_cyrillic_spelling_reads_as_the_canonical_key() {
        let content = note("\u{0501}escription: value");
        assert_eq!(
            look_alike_frontmatter_keys(&content, "description"),
            vec!["\u{0501}escription"]
        );
    }

    #[test]
    fn an_exact_spelling_is_not_a_look_alike() {
        let content = note("description: value");
        assert!(look_alike_frontmatter_keys(&content, "description").is_empty());
    }

    #[test]
    fn content_without_frontmatter_has_no_look_alikes() {
        assert!(look_alike_frontmatter_keys("body only", "description").is_empty());
    }

    #[test]
    fn every_look_alike_spelling_is_listed() {
        let content = note("\u{0501}escription: a\nDescription: b\ndescription: c");
        assert_eq!(
            look_alike_frontmatter_keys(&content, "description").len(),
            2
        );
    }
}
