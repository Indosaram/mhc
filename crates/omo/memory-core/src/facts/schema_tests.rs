use std::collections::BTreeMap;
use std::path::PathBuf;

use pretty_assertions::assert_eq;

use super::*;
use crate::identity::MemoryIdentityPaths;

fn test_identity_paths(root: PathBuf) -> MemoryIdentityPaths {
    let runtime = root.join("runtime");
    MemoryIdentityPaths {
        repo: root.join("repo"),
        locks: runtime.join("locks"),
        transcripts: runtime.join("transcripts"),
        reflection: runtime.join("reflection"),
        reflection_sessions: runtime.join("reflection-sessions"),
        worktrees: runtime.join("worktrees"),
        viewers: runtime.join("viewers"),
        push_queue: runtime.join("push-queue"),
        facts_queue: runtime.join("facts-queue"),
        facts: runtime.join("facts"),
        notices: runtime.join("notices"),
        tool_receipts: runtime.join("tool-receipts"),
        recall: runtime.join("recall"),
        recall_ledger: runtime.join("recall").join("ledger"),
        recall_pending: runtime.join("recall").join("pending"),
        runtime,
        root,
    }
}

fn sample_transcript_entry(kind: &str, message_id: &str, text: &str) -> TranscriptEntry {
    serde_json::from_value(serde_json::json!({
        "kind": kind,
        "text": text,
        "captured_at": "2026-08-16T00:00:00.000Z",
        "source_line_id": format!("{message_id}:{kind}"),
        "source_message_id": message_id,
    }))
    .expect("valid transcript entry")
}

#[test]
fn test_initial_cursor_fields() {
    let cursor = initial_cursor();
    assert_eq!(cursor.version, FACTS_QUEUE_VERSION);
    assert_eq!(cursor.enqueued_through_message_id, None);
    assert_eq!(cursor.enqueued_through_snapshot_line, -1);
    assert_eq!(cursor.consumed_through_message_id, None);
    assert_eq!(cursor.consumed_through_snapshot_line, -1);
}

#[test]
fn test_parse_cursor_round_trip_and_defaults() {
    let valid_json = serde_json::json!({
        "version": 1,
        "enqueued_through_message_id": "m5",
        "enqueued_through_snapshot_line": 10,
        "consumed_through_message_id": "m3",
        "consumed_through_snapshot_line": 6,
    })
    .to_string();

    let parsed = parse_cursor(&valid_json);
    assert_eq!(parsed.version, 1);
    assert_eq!(parsed.enqueued_through_message_id, Some("m5".to_string()));
    assert_eq!(parsed.enqueued_through_snapshot_line, 10);
    assert_eq!(parsed.consumed_through_message_id, Some("m3".to_string()));
    assert_eq!(parsed.consumed_through_snapshot_line, 6);

    let corrupt = parse_cursor("{ invalid json");
    assert_eq!(corrupt, initial_cursor());

    let wrong_version = parse_cursor(r#"{"version":2}"#);
    assert_eq!(wrong_version, initial_cursor());

    let negative_lines = parse_cursor(
        r#"{"version":1,"enqueued_through_snapshot_line":-5,"consumed_through_snapshot_line":-10}"#,
    );
    assert_eq!(negative_lines.enqueued_through_snapshot_line, -1);
    assert_eq!(negative_lines.consumed_through_snapshot_line, -1);
}

#[test]
fn test_parse_consumed_round_trip_and_fallback() {
    let valid_json = serde_json::json!({
        "version": 1,
        "consumed": {
            "conversation-alpha": {
                "end_message_id": "m9",
                "end_snapshot_line": 20,
                "consumedAt": "2026-08-16T00:00:00.000Z",
            }
        }
    })
    .to_string();

    let parsed = parse_consumed(&valid_json);
    assert_eq!(parsed.version, 1);
    let record = parsed.consumed.get("conversation-alpha").expect("found");
    assert_eq!(record.end_message_id, "m9");
    assert_eq!(record.end_snapshot_line, 20);
    assert_eq!(record.consumed_at, "2026-08-16T00:00:00.000Z");

    let corrupt = parse_consumed("{ invalid json");
    assert_eq!(corrupt.version, 1);
    assert_eq!(corrupt.consumed, BTreeMap::new());

    let wrong_version = parse_consumed(r#"{"version":99,"consumed":{}}"#);
    assert_eq!(wrong_version.consumed, BTreeMap::new());
}

#[test]
fn test_parse_queue_entry_valid_and_invalid() {
    let valid_json = serde_json::json!({
        "version": 1,
        "identity": "facts-agent",
        "sessionId": "session-1",
        "conversationId": "conv-1",
        "range": {
            "start_message_id": "m1",
            "end_message_id": "m2",
            "start_line": 0,
            "end_snapshot_line": 2
        },
        "enqueuedAt": "2026-08-16T00:00:00.000Z",
        "entries": [
            {
                "kind": "user",
                "text": "hello",
                "captured_at": "2026-08-16T00:00:00.000Z",
                "source_line_id": "m1:user",
                "source_message_id": "m1"
            }
        ]
    })
    .to_string();

    let parsed = parse_queue_entry(&valid_json);
    assert_eq!(parsed.is_some(), true);
    let entry = parsed.expect("parsed entry");
    assert_eq!(entry.version, 1);
    assert_eq!(entry.identity, "facts-agent");
    assert_eq!(entry.conversation_id, "conv-1");
    assert_eq!(entry.range.start_message_id, "m1");
    assert_eq!(entry.range.end_message_id, "m2");
    assert_eq!(entry.range.start_line, 0);
    assert_eq!(entry.range.end_snapshot_line, 2);
    assert_eq!(entry.entries.len(), 1);

    assert_eq!(parse_queue_entry("{ not json"), None);
    assert_eq!(parse_queue_entry(r#"{"version":2}"#), None);

    let missing_range = serde_json::json!({
        "version": 1,
        "identity": "facts-agent",
        "sessionId": "session-1",
        "conversationId": "conv-1",
        "enqueuedAt": "2026-08-16T00:00:00.000Z",
        "entries": []
    })
    .to_string();
    assert_eq!(parse_queue_entry(&missing_range), None);
}

#[test]
fn test_canonical_position() {
    let entries = vec![
        sample_transcript_entry("user", "m1", "first"),
        sample_transcript_entry("assistant", "m1", "reply"),
        sample_transcript_entry("user", "m2", "second"),
        sample_transcript_entry("assistant", "m2", "second reply"),
    ];

    assert_eq!(canonical_position(&entries, Some("m1")), 1);
    assert_eq!(canonical_position(&entries, Some("m2")), 3);
    assert_eq!(canonical_position(&entries, Some("m999")), -1);
    assert_eq!(canonical_position(&entries, None), -1);

    let empty: Vec<TranscriptEntry> = Vec::new();
    assert_eq!(canonical_position(&empty, Some("m1")), -1);
}

#[test]
fn test_queue_timestamp() {
    let at = "2026-08-16T00:00:00.000Z";
    let formatted = queue_timestamp(at);
    assert_eq!(formatted, "20260816T000000000Z");
}

#[test]
fn test_facts_queue_paths() {
    let root = PathBuf::from("/tmp/memory-test");
    let identity = test_identity_paths(root.clone());
    let layout = facts_queue_paths(&identity);

    assert_eq!(layout.queue_dir, root.join("runtime").join("facts-queue"));
    assert_eq!(
        layout.cursor_dir,
        root.join("runtime").join("facts-queue").join("cursor")
    );
    assert_eq!(
        layout.consumed_path,
        root.join("runtime")
            .join("facts-queue")
            .join("consumed.json")
    );
    assert_eq!(
        layout.failures_path,
        root.join("runtime")
            .join("facts-queue")
            .join("failures.json")
    );

    let cursor_path = layout.cursor_path("conversation-alpha");
    assert_eq!(
        cursor_path.extension().and_then(|s| s.to_str()),
        Some("json")
    );

    let entry_path = layout.entry_path("conversation-alpha", "m2", "2026-08-16T00:00:00.000Z");
    let filename = entry_path
        .file_name()
        .and_then(|s| s.to_str())
        .expect("filename");
    assert_eq!(filename.starts_with("20260816T000000000Z-"), true);
    assert_eq!(filename.ends_with(".json"), true);
}
