use pretty_assertions::assert_eq;

use super::*;
use crate::journal::entries::{
    ProjectedToolCall, TextTranscriptEntry, ToolCallTranscriptEntry, TranscriptProjection,
};
use crate::journal::store::{TranscriptJournal, TranscriptJournalOptions};

#[test]
fn test_reflection_state_not_mutated_when_aborted_signal_on_snapshot_capture() {
    // given
    let temp_dir = tempfile::tempdir().unwrap();
    let journal = TranscriptJournal::new(TranscriptJournalOptions::new(temp_dir.path()));
    let message = TranscriptProjection::Assistant {
        message_id: "assistant-1".to_string(),
        text_blocks: vec!["first".to_string()],
        reasoning_blocks: Vec::new(),
        tool_calls: Vec::new(),
    };
    journal.reconcile(&[message]).unwrap();
    let is_aborted = || true;

    // when
    let failure = journal.capture_reflection_snapshot(Some(&is_aborted));

    // then
    assert!(failure.is_err());
    let state = journal.get_state().unwrap();
    assert_eq!(state.last_reflection_started_at, None);
}

#[test]
fn test_only_snapshot_rows_become_reflected_when_rows_appended_during_reflection_and_snapshot_succeeds()
 {
    // given
    let temp_dir = tempfile::tempdir().unwrap();
    let tick = std::sync::atomic::AtomicUsize::new(0);
    let now_fn = Box::new(move || {
        let t = tick.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("2026-08-09T12:00:{t:02}.000Z")
    });
    let mut options = TranscriptJournalOptions::new(temp_dir.path());
    options.now = Some(now_fn);
    let journal = TranscriptJournal::new(options);

    journal
        .reconcile(&[
            TranscriptProjection::User {
                message_id: "user-1".to_string(),
                text: "hello".to_string(),
            },
            TranscriptProjection::Assistant {
                message_id: "assistant-1".to_string(),
                text_blocks: vec!["one".to_string()],
                reasoning_blocks: Vec::new(),
                tool_calls: vec![ProjectedToolCall {
                    call_id: "call-1".to_string(),
                    name: Some("read".to_string()),
                    args_text: None,
                    result_text: None,
                    result_ok: None,
                }],
            },
        ])
        .unwrap();

    // when
    let snapshot = journal.capture_reflection_snapshot(None).unwrap();
    assert!(snapshot.is_some());
    let snapshot = snapshot.unwrap();
    assert_eq!(snapshot.end_message_id, "assistant-1");
    assert_eq!(snapshot.end_snapshot_line, 3);
    assert_eq!(
        snapshot
            .entries
            .iter()
            .map(|e| e.kind())
            .collect::<Vec<_>>(),
        vec!["user", "assistant", "tool_call"]
    );

    journal
        .reconcile(&[TranscriptProjection::Assistant {
            message_id: "assistant-2".to_string(),
            text_blocks: vec!["two".to_string()],
            reasoning_blocks: Vec::new(),
            tool_calls: Vec::new(),
        }])
        .unwrap();

    journal.finalize_reflection(&snapshot, true).unwrap();

    // then
    let state = journal.get_state().unwrap();
    assert_eq!(
        state.reflected_through_message_id,
        Some("assistant-1".to_string())
    );
    assert_eq!(state.total_completed_steps, 2);
    assert_eq!(state.reflected_completed_steps, 1);
    assert_eq!(state.steps_since_last_successful_reflection, 1);
    assert_eq!(
        state.last_reflection_started_at,
        Some("2026-08-09T12:00:01.000Z".to_string())
    );
    assert_eq!(
        state.last_reflection_succeeded_at,
        Some("2026-08-09T12:00:03.000Z".to_string())
    );
}

#[test]
fn test_cursor_remains_retryable_when_reflection_fails_on_captured_snapshot() {
    // given
    let temp_dir = tempfile::tempdir().unwrap();
    let journal = TranscriptJournal::new(TranscriptJournalOptions::new(temp_dir.path()));
    journal
        .reconcile(&[TranscriptProjection::Assistant {
            message_id: "assistant-1".to_string(),
            text_blocks: vec!["one".to_string()],
            reasoning_blocks: Vec::new(),
            tool_calls: Vec::new(),
        }])
        .unwrap();

    let snapshot = journal.capture_reflection_snapshot(None).unwrap();
    assert!(snapshot.is_some());
    let snapshot = snapshot.unwrap();

    // when
    journal.finalize_reflection(&snapshot, false).unwrap();

    // then
    let state = journal.get_state().unwrap();
    assert_eq!(state.reflected_through_message_id, None);
    assert_eq!(state.reflected_completed_steps, 0);
    assert_eq!(state.steps_since_last_successful_reflection, 1);
    assert!(journal.capture_reflection_snapshot(None).unwrap().is_some());
}

#[test]
fn test_parse_state_when_valid_json_then_tolerantly_extracts_counters() {
    // given
    let raw = r#"{
        "schema_version": "v3_assistant_steps",
        "total_completed_steps": 5,
        "reflected_completed_steps": 2,
        "steps_since_last_successful_reflection": 3,
        "reflected_through_message_id": "msg-42"
    }"#;

    // when
    let state = parse_state(raw).unwrap();

    // then
    assert_eq!(state.schema_version, REFLECTION_STATE_SCHEMA_VERSION);
    assert_eq!(state.total_completed_steps, 5);
    assert_eq!(state.reflected_completed_steps, 2);
    assert_eq!(state.steps_since_last_successful_reflection, 3);
    assert_eq!(
        state.reflected_through_message_id,
        Some("msg-42".to_string())
    );
}

#[test]
fn test_parse_state_when_corrupt_json_then_returns_error() {
    // given
    let raw = "not json at all";

    // when
    let result = parse_state(raw);

    // then
    assert!(result.is_err());
}

#[test]
fn test_is_canonical_entry_when_user_or_assistant_with_text_then_true() {
    // given
    let user_entry = TranscriptEntry::Text(TextTranscriptEntry::new(
        "user",
        "hello",
        "2026-08-09T12:00:00.000Z",
        "u1:user",
        "u1",
    ));
    let tool_entry = TranscriptEntry::ToolCall(ToolCallTranscriptEntry::new(
        Some("read".to_string()),
        None,
        None,
        None,
        "2026-08-09T12:00:00.000Z",
        "t1:tool",
        "t1",
    ));
    let empty_user = TranscriptEntry::Text(TextTranscriptEntry::new(
        "user",
        "   ",
        "2026-08-09T12:00:00.000Z",
        "u2:user",
        "u2",
    ));

    // then
    assert_eq!(is_canonical_entry(&user_entry), true);
    assert_eq!(is_canonical_entry(&tool_entry), false);
    assert_eq!(is_canonical_entry(&empty_user), false);
}
