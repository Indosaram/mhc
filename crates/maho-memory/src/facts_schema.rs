//! On-disk facts queue: where batches live and how far a conversation has been read.
//!
//! Spec: `docs/cleanroom/specs/memory-facts-queue-layout.md`. Every path is derivable from its contents
//! alone because nothing here appends to a log, and entry names carry the enqueue timestamp so a
//! re-enqueue of the same range lands on a new file instead of overwriting a batch being extracted.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Bumped when the stored shape changes incompatibly; a file with another version is treated as absent.
pub const FACTS_QUEUE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsQueueRange {
    pub start_message_id: String,
    pub end_message_id: String,
    pub start_line: u64,
    pub end_snapshot_line: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsQueueEntry {
    pub version: u32,
    pub identity: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "conversationId")]
    pub conversation_id: String,
    pub range: FactsQueueRange,
    #[serde(rename = "enqueuedAt")]
    pub enqueued_at: String,
}

/// Enqueue and consume are separate watermarks: a batch can be enqueued before any extractor runs, and
/// consuming one batch must not make the next range look already enqueued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsCursor {
    pub version: u32,
    pub enqueued_through_message_id: Option<String>,
    pub enqueued_through_snapshot_line: i64,
    pub consumed_through_message_id: Option<String>,
    pub consumed_through_snapshot_line: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsConsumedRecord {
    pub end_message_id: String,
    pub end_snapshot_line: i64,
    #[serde(rename = "consumedAt")]
    pub consumed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsConsumedWatermark {
    pub version: u32,
    pub consumed: BTreeMap<String, FactsConsumedRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactsQueueLayout {
    pub queue_dir: PathBuf,
    pub parked_dir: PathBuf,
    pub cursor_dir: PathBuf,
    pub consumed_path: PathBuf,
    pub failures_path: PathBuf,
}

impl FactsQueueLayout {
    pub fn new(queue_dir: PathBuf) -> Self {
        Self {
            cursor_dir: queue_dir.join("cursor"),
            consumed_path: queue_dir.join("consumed.json"),
            failures_path: queue_dir.join("failures.json"),
            parked_dir: queue_dir.join("parked"),
            queue_dir,
        }
    }

    /// Hashed, not named: a conversation id is caller-controlled text and may contain `/` or `..`,
    /// either of which would move the file out of the cursor directory.
    pub fn cursor_path(&self, conversation_id: &str) -> PathBuf {
        self.cursor_dir
            .join(format!("{}.json", hash_prefix(conversation_id, 12)))
    }

    /// The timestamp is part of the name so a re-enqueue is a new file: overwriting would discard a
    /// batch an extractor may be reading.
    pub fn entry_path(&self, conversation_id: &str, end_message_id: &str, at: &str) -> PathBuf {
        self.queue_dir.join(format!(
            "{}-{}-{}.json",
            queue_timestamp(at),
            hash_prefix(conversation_id, 12),
            hash_prefix(end_message_id, 8)
        ))
    }
}

/// `YYYYMMDDTHHMMSSmmmZ`; colons are not portable in a file name on Windows.
pub fn queue_timestamp(at: &str) -> String {
    at.replace(['-', ':', '.'], "")
}

/// `-1` rather than `0` for the snapshot lines: line 0 is a real position, so `0` would claim the
/// first line is already handled and skip it.
pub fn initial_cursor() -> FactsCursor {
    FactsCursor {
        version: FACTS_QUEUE_VERSION,
        enqueued_through_message_id: None,
        enqueued_through_snapshot_line: -1,
        consumed_through_message_id: None,
        consumed_through_snapshot_line: -1,
    }
}

/// A corrupt cursor falls back to the initial state rather than erroring: that re-reads some
/// already-extracted messages, which costs work, while failing would stall extraction permanently.
pub fn parse_cursor(raw: &str) -> FactsCursor {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return initial_cursor();
    };
    if !is_current_version(&value) {
        return initial_cursor();
    }
    let message_id = |key: &str| {
        value
            .get(key)
            .and_then(|value| value.as_str())
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let snapshot_line = |key: &str| {
        value
            .get(key)
            .and_then(|value| value.as_i64())
            .filter(|line| *line >= 0)
            .unwrap_or(-1)
    };
    FactsCursor {
        version: FACTS_QUEUE_VERSION,
        enqueued_through_message_id: message_id("enqueued_through_message_id"),
        enqueued_through_snapshot_line: snapshot_line("enqueued_through_snapshot_line"),
        consumed_through_message_id: message_id("consumed_through_message_id"),
        consumed_through_snapshot_line: snapshot_line("consumed_through_snapshot_line"),
    }
}

fn is_current_version(value: &serde_json::Value) -> bool {
    value.is_object()
        && value.get("version").and_then(|value| value.as_u64())
            == Some(u64::from(FACTS_QUEUE_VERSION))
}

/// A single bad record is skipped rather than discarding the file: the other conversations' progress is
/// still true, and losing it would re-extract every one of them.
pub fn parse_consumed(raw: &str) -> FactsConsumedWatermark {
    let empty = FactsConsumedWatermark {
        version: FACTS_QUEUE_VERSION,
        consumed: BTreeMap::new(),
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return empty;
    };
    if !is_current_version(&value) {
        return empty;
    }
    let Some(records) = value.get("consumed").and_then(|value| value.as_object()) else {
        return empty;
    };
    let mut consumed = BTreeMap::new();
    for (conversation_id, item) in records {
        let Some(end_message_id) = item
            .get("end_message_id")
            .and_then(|value| value.as_str())
            .filter(|text| !text.is_empty())
        else {
            continue;
        };
        let Some(consumed_at) = item
            .get("consumedAt")
            .and_then(|value| value.as_str())
            .filter(|text| !text.is_empty())
        else {
            continue;
        };
        consumed.insert(
            conversation_id.clone(),
            FactsConsumedRecord {
                end_message_id: end_message_id.to_owned(),
                end_snapshot_line: item
                    .get("end_snapshot_line")
                    .and_then(|value| value.as_i64())
                    .filter(|line| *line >= 0)
                    .unwrap_or(-1),
                consumed_at: consumed_at.to_owned(),
            },
        );
    }
    FactsConsumedWatermark {
        version: FACTS_QUEUE_VERSION,
        consumed,
    }
}

/// All-or-nothing: a batch missing its range or conversation cannot be safely extracted, and one
/// missing its identity would be written into the wrong store.
pub fn parse_queue_entry(raw: &str) -> Option<FactsQueueEntry> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    if !is_current_version(&value) {
        return None;
    }
    let text = |key: &str| {
        value
            .get(key)
            .and_then(|value| value.as_str())
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let range = value.get("range")?.as_object()?;
    let range_text = |key: &str| {
        range
            .get(key)
            .and_then(|value| value.as_str())
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    Some(FactsQueueEntry {
        version: FACTS_QUEUE_VERSION,
        identity: text("identity")?,
        session_id: text("sessionId")?,
        conversation_id: text("conversationId")?,
        range: FactsQueueRange {
            start_message_id: range_text("start_message_id")?,
            end_message_id: range_text("end_message_id")?,
            start_line: range.get("start_line").and_then(|value| value.as_u64())?,
            end_snapshot_line: range
                .get("end_snapshot_line")
                .and_then(|value| value.as_u64())?,
        },
        enqueued_at: text("enqueuedAt")?,
    })
}

/// Searched from the end because a message can span several lines and its last line is the boundary.
pub fn canonical_position(
    entries: &[super::cursor::TranscriptEntry],
    message_id: Option<&str>,
) -> i64 {
    let Some(target) = message_id else {
        return -1;
    };
    for (index, entry) in entries.iter().enumerate().rev() {
        if entry.is_canonical() && entry.source_message_id == target {
            return index as i64;
        }
    }
    -1
}

pub fn queue_conversation_hash(conversation_id: &str) -> String {
    hash_prefix(conversation_id, 12)
}

pub fn queue_entry_suffix(conversation_id: &str, end_message_id: &str) -> String {
    format!(
        "-{}-{}.json",
        queue_conversation_hash(conversation_id),
        hash_prefix(end_message_id, 8)
    )
}

fn hash_prefix(value: &str, length: usize) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    hex[..length.min(hex.len())].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::TranscriptEntry;
    use std::path::Path;

    fn entry(kind: &str, message: &str, text: Option<&str>) -> TranscriptEntry {
        TranscriptEntry {
            kind: kind.to_owned(),
            text: text.map(str::to_owned),
            captured_at: "2026-01-01T00:00:00Z".to_owned(),
            source_line_id: format!("line-{message}"),
            source_message_id: message.to_owned(),
        }
    }

    fn queue_entry_json() -> String {
        r#"{
            "version": 1,
            "identity": "default",
            "sessionId": "s1",
            "conversationId": "c1",
            "range": {
                "start_message_id": "m1",
                "end_message_id": "m2",
                "start_line": 0,
                "end_snapshot_line": 5
            },
            "enqueuedAt": "2026-01-01T00:00:00.000Z"
        }"#
        .to_owned()
    }

    #[test]
    fn the_layout_places_every_file_under_the_queue_dir() {
        let layout = FactsQueueLayout::new(PathBuf::from("/q"));
        assert_eq!(layout.cursor_dir, PathBuf::from("/q/cursor"));
        assert_eq!(layout.parked_dir, PathBuf::from("/q/parked"));
        assert_eq!(layout.consumed_path, PathBuf::from("/q/consumed.json"));
        assert_eq!(layout.failures_path, PathBuf::from("/q/failures.json"));
    }

    #[test]
    fn a_colon_free_timestamp_is_a_portable_file_name() {
        assert_eq!(
            queue_timestamp("2026-01-01T12:34:56.789Z"),
            "20260101T123456789Z"
        );
        assert!(!queue_timestamp("2026-01-01T12:34:56.789Z").contains(':'));
    }

    #[test]
    fn an_entry_path_carries_the_time_and_both_hashes() {
        let layout = FactsQueueLayout::new(PathBuf::from("/q"));
        let path = layout.entry_path("c1", "m2", "2026-01-01T00:00:00Z");
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("20260101T000000Z-"), "{name}");
        assert!(name.ends_with(".json"));
        assert_eq!(
            name,
            format!(
                "20260101T000000Z-{}-{}.json",
                queue_conversation_hash("c1"),
                &hash_prefix("m2", 8)
            )
        );
    }

    #[test]
    fn a_re_enqueue_of_the_same_range_lands_on_a_new_file() {
        let layout = FactsQueueLayout::new(PathBuf::from("/q"));
        let first = layout.entry_path("c1", "m2", "2026-01-01T00:00:00Z");
        let second = layout.entry_path("c1", "m2", "2026-01-01T00:00:01Z");
        assert_ne!(first, second);
    }

    #[test]
    fn a_conversation_id_cannot_escape_the_cursor_dir() {
        let layout = FactsQueueLayout::new(PathBuf::from("/q"));
        let path = layout.cursor_path("../../etc/passwd");
        assert_eq!(path.parent(), Some(Path::new("/q/cursor")));
        assert!(!path.to_string_lossy().contains(".."));
    }

    #[test]
    fn a_fresh_cursor_starts_before_the_first_line() {
        let cursor = initial_cursor();
        assert_eq!(cursor.enqueued_through_snapshot_line, -1);
        assert_eq!(cursor.consumed_through_snapshot_line, -1);
        assert_eq!(cursor.enqueued_through_message_id, None);
    }

    #[test]
    fn a_cursor_round_trips() {
        let cursor = FactsCursor {
            version: FACTS_QUEUE_VERSION,
            enqueued_through_message_id: Some("m2".to_owned()),
            enqueued_through_snapshot_line: 7,
            consumed_through_message_id: Some("m1".to_owned()),
            consumed_through_snapshot_line: 3,
        };
        let raw = serde_json::to_string(&cursor).expect("serializes");
        let parsed = parse_cursor(&raw);
        assert_eq!(parsed, cursor);
    }

    #[test]
    fn a_corrupt_cursor_falls_back_to_the_initial_state() {
        for raw in [
            "",
            "not json",
            "[]",
            r#"{"version":2}"#,
            r#"{"version":"1"}"#,
        ] {
            assert_eq!(parse_cursor(raw), initial_cursor(), "raw: {raw}");
        }
    }

    #[test]
    fn a_cursor_ignores_negative_and_empty_fields() {
        let parsed = parse_cursor(
            r#"{"version":1,"enqueued_through_snapshot_line":-5,"enqueued_through_message_id":""}"#,
        );
        assert_eq!(parsed.enqueued_through_snapshot_line, -1);
        assert_eq!(parsed.enqueued_through_message_id, None);
    }

    #[test]
    fn the_two_watermarks_are_independent() {
        let raw = r#"{"version":1,"enqueued_through_message_id":"m9","enqueued_through_snapshot_line":9,"consumed_through_message_id":"m1","consumed_through_snapshot_line":1}"#;
        let parsed = parse_cursor(raw);
        assert_eq!(parsed.enqueued_through_message_id.as_deref(), Some("m9"));
        assert_eq!(parsed.consumed_through_message_id.as_deref(), Some("m1"));
    }

    #[test]
    fn a_consumed_watermark_keeps_well_formed_records_only() {
        let raw = r#"{"version":1,"consumed":{
            "good":{"end_message_id":"m2","end_snapshot_line":4,"consumedAt":"t"},
            "noEnd":{"end_snapshot_line":4,"consumedAt":"t"},
            "noTime":{"end_message_id":"m3","end_snapshot_line":4},
            "emptyId":{"end_message_id":"","end_snapshot_line":4,"consumedAt":"t"}
        }}"#;
        let parsed = parse_consumed(raw);
        assert_eq!(parsed.consumed.len(), 1);
        assert!(parsed.consumed.contains_key("good"));
    }

    #[test]
    fn a_corrupt_consumed_file_is_an_empty_watermark() {
        assert!(parse_consumed("nope").consumed.is_empty());
        assert!(
            parse_consumed(r#"{"version":9,"consumed":{}}"#)
                .consumed
                .is_empty()
        );
    }

    #[test]
    fn a_queue_entry_parses_every_scheduling_field() {
        let entry = parse_queue_entry(&queue_entry_json()).expect("parses");
        assert_eq!(entry.identity, "default");
        assert_eq!(entry.session_id, "s1");
        assert_eq!(entry.conversation_id, "c1");
        assert_eq!(entry.range.start_message_id, "m1");
        assert_eq!(entry.range.end_message_id, "m2");
        assert_eq!(entry.range.end_snapshot_line, 5);
        assert_eq!(entry.enqueued_at, "2026-01-01T00:00:00.000Z");
    }

    #[test]
    fn a_queue_entry_missing_any_required_field_is_refused() {
        for field in ["identity", "sessionId", "conversationId", "enqueuedAt"] {
            let raw = queue_entry_json()
                .lines()
                .filter(|line| !line.contains(&format!("\"{field}\"")))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(parse_queue_entry(&raw).is_none(), "field: {field}");
        }
    }

    #[test]
    fn a_queue_entry_with_a_future_version_is_refused() {
        let raw = queue_entry_json().replace("\"version\": 1", "\"version\": 2");
        assert!(parse_queue_entry(&raw).is_none());
    }

    #[test]
    fn a_queue_entry_without_a_range_is_refused() {
        let raw = queue_entry_json().replace(
            r#""range": {
                "start_message_id": "m1",
                "end_message_id": "m2",
                "start_line": 0,
                "end_snapshot_line": 5
            },"#,
            "",
        );
        assert!(parse_queue_entry(&raw).is_none());
    }

    #[test]
    fn the_last_matching_canonical_line_is_the_position() {
        let entries = vec![
            entry("user", "m1", Some("hi")),
            entry("assistant", "m2", Some("one")),
            entry("assistant", "m2", Some("two")),
            entry("assistant", "m3", Some("three")),
        ];
        assert_eq!(canonical_position(&entries, Some("m2")), 2);
        assert_eq!(canonical_position(&entries, Some("m3")), 3);
    }

    #[test]
    fn an_unknown_or_absent_message_id_has_position_minus_one() {
        let entries = vec![entry("assistant", "m1", Some("one"))];
        assert_eq!(canonical_position(&entries, Some("gone")), -1);
        assert_eq!(canonical_position(&entries, None), -1);
    }

    #[test]
    fn a_non_canonical_entry_is_not_a_position() {
        let entries = vec![
            entry("assistant", "m1", Some("one")),
            entry("user", "m2", Some("hi")),
        ];
        assert_eq!(canonical_position(&entries, Some("m2")), -1);
    }

    #[test]
    fn the_entry_suffix_is_the_two_hashes() {
        assert_eq!(
            queue_entry_suffix("c1", "m2"),
            format!(
                "-{}-{}.json",
                queue_conversation_hash("c1"),
                &hash_prefix("m2", 8)
            )
        );
    }
}
