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
        memory_usage: runtime.join("memory-usage"),
        memory_maintenance: runtime.join("memory-maintenance"),
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

#[test]
fn test_parse_queue_header_valid_equals_entry_with_cleared_entries() {
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
            },
            {
                "kind": "assistant",
                "text": "hi there",
                "captured_at": "2026-08-16T00:00:01.000Z",
                "source_line_id": "m1:assistant",
                "source_message_id": "m1"
            },
            {
                "kind": "tool_call",
                "name": "lookup",
                "argsText": "{}",
                "resultText": "found",
                "resultOk": true,
                "captured_at": "2026-08-16T00:00:02.000Z",
                "source_line_id": "m2:tool_call",
                "source_message_id": "m2"
            }
        ]
    })
    .to_string();

    let mut expected = parse_queue_entry(&valid_json).expect("valid queue entry");
    assert!(!expected.entries.is_empty());
    expected.entries.clear();

    let header = parse_queue_header(valid_json.as_bytes()).expect("valid queue header");
    assert_eq!(header, expected);

    // Also verify field order on disk: visitor accepts entries preceding header fields.
    let reordered_json = serde_json::json!({
        "entries": [
            {
                "kind": "user",
                "text": "hello",
                "captured_at": "2026-08-16T00:00:00.000Z",
                "source_line_id": "m1:user",
                "source_message_id": "m1"
            }
        ],
        "enqueuedAt": "2026-08-16T00:00:00.000Z",
        "range": {
            "start_message_id": "m1",
            "end_message_id": "m2",
            "start_line": 0,
            "end_snapshot_line": 2
        },
        "conversationId": "conv-1",
        "sessionId": "session-1",
        "identity": "facts-agent",
        "version": 1
    })
    .to_string();

    let mut reordered_expected = parse_queue_entry(&reordered_json).expect("valid reordered entry");
    reordered_expected.entries.clear();
    let reordered_header =
        parse_queue_header(reordered_json.as_bytes()).expect("valid reordered header");
    assert_eq!(reordered_header, reordered_expected);
}

#[test]
fn test_parse_queue_header_and_entry_reject_invalid_second_entry() {
    let invalid_second_entry_json = serde_json::json!({
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
            },
            {
                "kind": "user",
                // missing text field
                "captured_at": "2026-08-16T00:00:01.000Z",
                "source_line_id": "m2:user",
                "source_message_id": "m2"
            }
        ]
    })
    .to_string();

    assert_eq!(parse_queue_entry(&invalid_second_entry_json), None);
    assert_eq!(
        parse_queue_header(invalid_second_entry_json.as_bytes()),
        None
    );

    // Also test where 2nd entry is invalid tool_call (non-boolean resultOk)
    let invalid_tool_call_json = serde_json::json!({
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
            },
            {
                "kind": "tool_call",
                "captured_at": "2026-08-16T00:00:01.000Z",
                "source_line_id": "m2:tool",
                "source_message_id": "m2",
                "resultOk": "not_a_bool"
            }
        ]
    })
    .to_string();

    assert_eq!(parse_queue_entry(&invalid_tool_call_json), None);
    assert_eq!(
        parse_queue_header(invalid_tool_call_json.as_bytes()),
        None
    );
}

#[test]
fn test_parse_queue_header_wrong_version_and_missing_range() {
    let wrong_version = serde_json::json!({
        "version": 2,
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
        "entries": []
    })
    .to_string();

    assert_eq!(parse_queue_entry(&wrong_version), None);
    assert_eq!(parse_queue_header(wrong_version.as_bytes()), None);

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
    assert_eq!(parse_queue_header(missing_range.as_bytes()), None);

    let missing_range_field = serde_json::json!({
        "version": 1,
        "identity": "facts-agent",
        "sessionId": "session-1",
        "conversationId": "conv-1",
        "range": {
            "start_message_id": "m1",
            "end_message_id": "m2",
            "start_line": 0
            // missing end_snapshot_line
        },
        "enqueuedAt": "2026-08-16T00:00:00.000Z",
        "entries": []
    })
    .to_string();

    assert_eq!(parse_queue_entry(&missing_range_field), None);
    assert_eq!(parse_queue_header(missing_range_field.as_bytes()), None);

    let empty_start_id = serde_json::json!({
        "version": 1,
        "identity": "facts-agent",
        "sessionId": "session-1",
        "conversationId": "conv-1",
        "range": {
            "start_message_id": "",
            "end_message_id": "m2",
            "start_line": 0,
            "end_snapshot_line": 2
        },
        "enqueuedAt": "2026-08-16T00:00:00.000Z",
        "entries": []
    })
    .to_string();

    assert_eq!(parse_queue_entry(&empty_start_id), None);
    assert_eq!(parse_queue_header(empty_start_id.as_bytes()), None);

    let missing_entries = serde_json::json!({
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
        "enqueuedAt": "2026-08-16T00:00:00.000Z"
    })
    .to_string();

    assert_eq!(parse_queue_entry(&missing_entries), None);
    assert_eq!(parse_queue_header(missing_entries.as_bytes()), None);
}

#[test]
fn test_parse_queue_header_large_synthetic_file() {
    use std::io::Cursor;

    let prefix = br#"{"version":1,"identity":"agent-perf","sessionId":"sess-perf","conversationId":"conv-perf","range":{"start_message_id":"m0","end_message_id":"m9999","start_line":0,"end_snapshot_line":10000},"enqueuedAt":"2026-08-16T00:00:00.000Z","entries":["#;
    let suffix = br#"]}"#;

    let entry_body = format!(
        r#"{{"kind":"user","text":"{}","captured_at":"2026-08-16T00:00:00.000Z","source_line_id":"l:u","source_message_id":"m1"}}"#,
        "x".repeat(1900)
    );
    let entry_bytes = entry_body.as_bytes();

    let count = 11_000;
    let mut payload =
        Vec::with_capacity(prefix.len() + suffix.len() + (entry_bytes.len() + 1) * count);
    payload.extend_from_slice(prefix);
    for i in 0..count {
        if i > 0 {
            payload.push(b',');
        }
        payload.extend_from_slice(entry_bytes);
    }
    payload.extend_from_slice(suffix);

    let total_mb = payload.len() as f64 / (1024.0 * 1024.0);
    assert!(
        total_mb >= 20.0,
        "payload must be at least 20 MB, got {total_mb:.2} MB"
    );

    let header =
        parse_queue_header(Cursor::new(payload)).expect("accepted large synthetic file");
    assert_eq!(header.version, 1);
    assert_eq!(header.identity, "agent-perf");
    assert_eq!(header.session_id, "sess-perf");
    assert_eq!(header.conversation_id, "conv-perf");
    assert_eq!(header.range.start_message_id, "m0");
    assert_eq!(header.range.end_message_id, "m9999");
    assert_eq!(header.range.start_line, 0);
    assert_eq!(header.range.end_snapshot_line, 10000);
    assert!(header.entries.is_empty());
}
