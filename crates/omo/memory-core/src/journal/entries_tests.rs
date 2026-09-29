use pretty_assertions::assert_eq;

use super::*;

#[test]
fn test_canonical_and_contextual_rows_use_stable_ids_when_assistant_trajectory_projected() {
    // given
    let captured_at = "2026-08-09T12:00:00.000Z";
    let long_args = "x".repeat(301);
    let message = TranscriptProjection::Assistant {
        message_id: "msg-1".to_string(),
        text_blocks: vec!["first".to_string(), "".to_string(), "second".to_string()],
        reasoning_blocks: vec![
            ProjectedReasoning::text("consider this"),
            ProjectedReasoning::redacted(),
        ],
        tool_calls: vec![ProjectedToolCall {
            call_id: "call-1".to_string(),
            name: Some("read".to_string()),
            args_text: Some(long_args),
            result_text: Some("done".to_string()),
            result_ok: Some(true),
        }],
    };

    // when
    let entries = project_transcript_entries(&message, captured_at);

    // then
    assert_eq!(
        entries,
        vec![
            TranscriptEntry::Text(TextTranscriptEntry {
                kind: "assistant".to_string(),
                text: "first\nsecond".to_string(),
                captured_at: captured_at.to_string(),
                source_line_id: "msg-1:assistant".to_string(),
                source_message_id: "msg-1".to_string(),
            }),
            TranscriptEntry::Text(TextTranscriptEntry {
                kind: "reasoning".to_string(),
                text: "consider this".to_string(),
                captured_at: captured_at.to_string(),
                source_line_id: "msg-1:reasoning:0".to_string(),
                source_message_id: "msg-1".to_string(),
            }),
            TranscriptEntry::Text(TextTranscriptEntry {
                kind: "reasoning".to_string(),
                text: "[REDACTED REASONING]".to_string(),
                captured_at: captured_at.to_string(),
                source_line_id: "msg-1:reasoning:1".to_string(),
                source_message_id: "msg-1".to_string(),
            }),
            TranscriptEntry::ToolCall(ToolCallTranscriptEntry {
                kind: "tool_call".to_string(),
                name: Some("read".to_string()),
                args_text: Some("x".repeat(300)),
                result_text: Some("done".to_string()),
                result_ok: Some(true),
                captured_at: captured_at.to_string(),
                source_line_id: "msg-1:tool:call-1".to_string(),
                source_message_id: "msg-1".to_string(),
            }),
        ]
    );
}

#[test]
fn test_has_no_canonical_assistant_row_when_tool_only_assistant_message_projected() {
    // given
    let captured_at = "2026-08-09T12:00:00.000Z";
    let message = TranscriptProjection::Assistant {
        message_id: "msg-2".to_string(),
        text_blocks: vec!["   ".to_string()],
        reasoning_blocks: Vec::new(),
        tool_calls: vec![ProjectedToolCall {
            call_id: "call-2".to_string(),
            name: Some("bash".to_string()),
            args_text: None,
            result_text: None,
            result_ok: None,
        }],
    };

    // when
    let entries = project_transcript_entries(&message, captured_at);

    // then
    assert_eq!(
        entries,
        vec![TranscriptEntry::ToolCall(ToolCallTranscriptEntry {
            kind: "tool_call".to_string(),
            name: Some("bash".to_string()),
            args_text: None,
            result_text: None,
            result_ok: None,
            captured_at: captured_at.to_string(),
            source_line_id: "msg-2:tool:call-2".to_string(),
            source_message_id: "msg-2".to_string(),
        })]
    );
}
