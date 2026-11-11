//! What a reflection run left out, recorded on the commit it produced.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-completion-repair.md`. The completion gate can refuse
//! a memory entry the agent wrote; the run is still committed, so the refusal has to be readable
//! later. It is recorded as one JSON trailer per dropped entry rather than prose, because a path can
//! contain a newline or ` | ` and a separator-based format would not read back.
//!
//! Reading the trailer off a real commit needs a git caller; this module owns the format and the
//! body scan, so the caller supplies the commit body and decides which commit to read.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::redact::redact_secret_like_material;

/// One memory entry the completion gate refused and removed from the run's commit. `path` is the
/// repository path itself (the agent chose it, so it can be secret-shaped): redact it before
/// printing. `reason` never carries the matched value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DroppedEntry {
    pub path: String,
    pub reason: String,
}

pub const DROPPED_TRAILER: &str = "Omo-Completion-Dropped";

pub fn describe_dropped_entries(dropped: &[DroppedEntry]) -> String {
    let items: Vec<String> = dropped
        .iter()
        .map(|entry| {
            format!(
                "{} ({})",
                redact_secret_like_material(&entry.path),
                redact_secret_like_material(&entry.reason)
            )
        })
        .collect();
    format!(
        "Dropped {} invalid memory entr{}: {}",
        dropped.len(),
        if dropped.len() == 1 { "y" } else { "ies" },
        items.join("; ")
    )
}

pub fn trailer_line(entry: &DroppedEntry) -> String {
    let mut object = Map::new();
    object.insert(
        "path".to_string(),
        Value::String(redact_secret_like_material(&entry.path)),
    );
    object.insert(
        "reason".to_string(),
        Value::String(redact_secret_like_material(&entry.reason)),
    );
    format!("{DROPPED_TRAILER}: {}", Value::Object(object))
}

pub fn parse_trailer_value(value: &str) -> Option<DroppedEntry> {
    let parsed: Value = serde_json::from_str(value).ok()?;
    let object = parsed.as_object()?;
    let path = object.get("path")?.as_str()?;
    let reason = object.get("reason")?.as_str()?;
    Some(DroppedEntry {
        path: path.to_string(),
        reason: reason.to_string(),
    })
}

/// The dropped entries recorded in a commit body.
///
/// A line that is not this trailer, or whose value does not parse, is skipped: a hand-edited or
/// truncated body must not turn into a wrong report of what was dropped.
pub fn dropped_entries_in_body(body: &str) -> Vec<DroppedEntry> {
    let prefix = format!("{DROPPED_TRAILER}: ");
    let mut entries = Vec::new();
    for line in body.split('\n') {
        let Some(value) = line.strip_prefix(prefix.as_str()) else {
            continue;
        };
        if let Some(entry) = parse_trailer_value(value) {
            entries.push(entry);
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, reason: &str) -> DroppedEntry {
        DroppedEntry {
            path: path.to_string(),
            reason: reason.to_string(),
        }
    }

    #[test]
    fn one_entry_reads_as_a_singular_sentence() {
        let text = describe_dropped_entries(&[entry("notes/a.md", "too long")]);
        assert_eq!(
            text,
            "Dropped 1 invalid memory entry: notes/a.md (too long)"
        );
    }

    #[test]
    fn several_entries_read_as_a_plural_sentence() {
        let text = describe_dropped_entries(&[
            entry("notes/a.md", "too long"),
            entry("notes/b.md", "no frontmatter"),
        ]);
        assert_eq!(
            text,
            "Dropped 2 invalid memory entries: notes/a.md (too long); notes/b.md (no frontmatter)"
        );
    }

    #[test]
    fn a_secret_shaped_path_is_redacted_in_the_sentence() {
        let text = describe_dropped_entries(&[entry(
            "notes/ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.md",
            "rejected",
        )]);
        assert!(!text.contains("ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert!(text.contains("notes/"));
    }

    #[test]
    fn a_path_with_a_pipe_survives_the_round_trip() {
        let original = entry("notes/a | b.md", "reason | with pipe");
        let line = trailer_line(&original);
        let body = format!("subject\n\n{line}\n");
        assert_eq!(dropped_entries_in_body(&body), vec![original]);
    }

    #[test]
    fn a_path_with_a_newline_survives_the_round_trip() {
        let original = entry("notes/a\nb.md", "line\nbreak");
        let body = format!("subject\n\n{}\n", trailer_line(&original));
        assert_eq!(dropped_entries_in_body(&body), vec![original]);
    }

    #[test]
    fn a_trailer_writes_its_values_redacted() {
        let line = trailer_line(&entry(
            "notes/ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.md",
            "rejected",
        ));
        assert!(!line.contains("ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert!(line.starts_with(DROPPED_TRAILER));
    }

    #[test]
    fn a_body_without_the_trailer_reports_nothing() {
        assert!(dropped_entries_in_body("subject\n\njust prose\n").is_empty());
    }

    #[test]
    fn a_malformed_trailer_is_skipped_not_fatal() {
        let body = format!(
            "{DROPPED_TRAILER}: not json\n{DROPPED_TRAILER}: {{\"path\":\"a.md\"}}\n{}",
            trailer_line(&entry("notes/ok.md", "kept"))
        );
        assert_eq!(
            dropped_entries_in_body(&body),
            vec![entry("notes/ok.md", "kept")]
        );
    }

    #[test]
    fn a_prose_line_that_mentions_the_key_is_not_a_trailer() {
        assert!(dropped_entries_in_body(&format!("see {DROPPED_TRAILER} for details")).is_empty());
    }

    #[test]
    fn parse_rejects_a_value_that_is_not_the_written_shape() {
        assert!(parse_trailer_value("{\"path\":\"a.md\"}").is_none());
        assert!(parse_trailer_value("[]").is_none());
        assert!(parse_trailer_value("").is_none());
    }
}
