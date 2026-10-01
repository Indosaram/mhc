//! `tools/output/transcript/event-log.test.ts`

use std::fs;

use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

use crate::tools::output::transcript::event_log::{read_event_log_transcript, read_event_log_transcript_result};
use crate::tools::output::transcript::read_bounded::MAX_TRANSCRIPT_SOURCE_BYTES;
use crate::tools::output::types::TranscriptEntry;

/// Writes `logs/st_1.jsonl` into a fresh temp state dir; the returned guard keeps the dir alive.
fn write_log(lines: &[String]) -> (TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let logs = dir.path().join("logs");
    fs::create_dir_all(&logs).expect("create logs dir");
    fs::write(logs.join("st_1.jsonl"), lines.join("\n") + "\n").expect("write log");
    let state_dir = dir.path().to_string_lossy().into_owned();
    (dir, state_dir)
}

#[test]
fn given_a_log_with_assistant_tool_and_child_error_events_when_read_then_all_three_are_lifted_in_order() {
    // given
    let (_guard, state_dir) = write_log(&[
        json!({ "type": "assistant_message", "payload": { "text": "working on it" } }).to_string(),
        json!({ "type": "tool_execution", "payload": { "tool": "bash", "is_error": false } }).to_string(),
        json!({ "type": "child_error", "payload": { "message": "upstream gateway timeout", "stop_reason": "error" } })
            .to_string(),
    ]);

    // when
    let entries = read_event_log_transcript(&state_dir, "st_1").expect("read transcript");

    // then the error breadcrumb is part of the transcript
    assert_eq!(
        entries,
        vec![
            TranscriptEntry::Assistant {
                text: "working on it".to_string(),
            },
            TranscriptEntry::Tool {
                tool: "bash".to_string(),
                is_error: false,
            },
            TranscriptEntry::Error {
                message: "upstream gateway timeout".to_string(),
            },
        ]
    );
}

#[test]
fn given_a_large_event_log_when_read_then_head_and_tail_events_survive_with_source_truncation() {
    let (_guard, state_dir) = write_log(&[
        json!({ "type": "assistant_message", "payload": { "text": "first" } }).to_string(),
        json!({ "type": "ignored", "payload": { "text": "x".repeat(MAX_TRANSCRIPT_SOURCE_BYTES) } }).to_string(),
        json!({ "type": "assistant_message", "payload": { "text": "last" } }).to_string(),
    ]);

    let result = read_event_log_transcript_result(&state_dir, "st_1").expect("read transcript result");

    assert_eq!(result.truncated, Some(true));
    assert_eq!(
        result.entries,
        vec![
            TranscriptEntry::Assistant {
                text: "first".to_string(),
            },
            TranscriptEntry::Assistant {
                text: "last".to_string(),
            },
        ]
    );
}

#[test]
fn given_a_missing_log_when_read_then_transcript_is_empty_and_not_truncated() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_dir = dir.path().to_string_lossy().into_owned();

    let result = read_event_log_transcript_result(&state_dir, "st_missing").expect("read transcript result");

    assert_eq!(result.truncated, Some(false));
    assert_eq!(result.entries, Vec::<TranscriptEntry>::new());
}
