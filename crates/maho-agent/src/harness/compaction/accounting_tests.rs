use maho_ai::types::{ContentBlock, Message, StopReason, UserContent, UserMessage};
use serde_json::json;

use super::compaction::{
    DEFAULT_COMPACTION_SETTINGS, estimate_context_tokens, estimate_tokens, find_cut_point,
    prepare_compaction,
};
use super::utils::serialize_conversation;
use crate::harness::session::types::{Entry, EntryKind};
use crate::types::AgentMessage;

fn user(text: &str) -> AgentMessage {
    AgentMessage::Llm(Message::User(UserMessage {
        content: UserContent::Text(text.to_owned()),
        timestamp: 0,
    }))
}

fn assistant(tokens: u64) -> AgentMessage {
    serde_json::from_value(json!({
        "role": "assistant", "content": [{"type": "text", "text": "abcd"}],
        "api": "anthropic-messages", "provider": "anthropic", "model": "test",
        "usage": {"input": tokens, "output": 0, "cacheRead": 0, "cacheWrite": 0,
            "totalTokens": tokens,
            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}},
        "stopReason": "stop", "timestamp": 0
    }))
    .expect("assistant fixture")
}

fn entry(id: &str, kind: EntryKind) -> Entry {
    Entry {
        id: id.to_owned(),
        parent_id: None,
        seq: 0,
        timestamp: 0,
        kind,
    }
}

fn message_entry(id: &str, message: AgentMessage) -> Entry {
    entry(
        id,
        EntryKind::Message {
            message,
            terminate: None,
        },
    )
}

fn compacted_path() -> Vec<Entry> {
    vec![
        message_entry("old", assistant(9000)),
        entry(
            "compact",
            EntryKind::Compaction {
                summary: "abcd".to_owned(),
                retained_tail: vec![assistant(8000)],
                tokens_before: 9000,
                details: None,
                usage: None,
                from_hook: false,
            },
        ),
        message_entry("new", user("abcd")),
    ]
}

#[test]
fn estimates_content_when_only_retained_usage_exists() {
    // Given
    let path = compacted_path();
    // When
    let preparation = prepare_compaction(&path, DEFAULT_COMPACTION_SETTINGS)
        .expect("prepare")
        .expect("applicable");
    // Then
    assert_eq!(preparation.tokens_before, 3);
}

#[test]
fn anchors_usage_when_reported_after_compaction() {
    // Given
    let mut path = compacted_path();
    path.push(message_entry("response", assistant(100)));
    path.push(message_entry("trailing", user("abcdefgh")));
    // When
    let preparation = prepare_compaction(&path, DEFAULT_COMPACTION_SETTINGS)
        .expect("prepare")
        .expect("applicable");
    // Then
    assert_eq!(preparation.tokens_before, 102);
}

#[test]
fn estimates_content_when_post_compaction_response_failed() {
    // Given
    let mut path = compacted_path();
    let mut failed = assistant(20000);
    let AgentMessage::Llm(Message::Assistant(response)) = &mut failed else {
        panic!("assistant fixture");
    };
    response.stop_reason = StopReason::Error;
    path.push(message_entry("failed", failed));
    // When
    let preparation = prepare_compaction(&path, DEFAULT_COMPACTION_SETTINGS)
        .expect("prepare")
        .expect("applicable");
    // Then
    assert_eq!(preparation.tokens_before, 3);
}

#[test]
fn preserves_usage_anchors_when_message_list_has_no_path_boundary() {
    // Given
    let messages = vec![assistant(8000), user("abcd")];
    // When
    let estimate = estimate_context_tokens(&messages);
    // Then
    assert_eq!(estimate.tokens, 8001);
}

#[test]
fn counts_utf16_units_when_estimating_message_text() {
    // Given
    let text = "\u{1f600}".repeat(4);
    let mut response = assistant(0);
    let AgentMessage::Llm(Message::Assistant(message)) = &mut response else {
        panic!("assistant fixture");
    };
    message.content = vec![ContentBlock::text(&text)];
    let messages = [user(&text), response];
    // When
    let estimates = messages.iter().map(estimate_tokens).collect::<Vec<_>>();
    // Then
    assert_eq!(estimates, vec![2, 2]);
}

#[test]
fn counts_utf16_units_when_estimating_thinking_and_tool_calls() {
    // Given
    let message: AgentMessage = serde_json::from_value(json!({
        "role": "assistant", "content": [
            {"type": "thinking", "thinking": "\u{1f600}"},
            {"type": "toolCall", "id": "call", "name": "\u{1f600}",
                "arguments": {"x": "\u{1f600}"}}
        ],
        "api": "anthropic-messages", "provider": "anthropic", "model": "test",
        "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0,
            "totalTokens": 0,
            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}},
        "stopReason": "stop", "timestamp": 0
    }))
    .expect("assistant fixture");
    // When
    let tokens = estimate_tokens(&message);
    // Then
    assert_eq!(tokens, 4);
}

#[test]
fn counts_utf16_units_when_estimating_custom_and_block_content() {
    // Given
    let text = "\u{1f600}".repeat(4);
    let fixtures = [
        json!({"role": "user", "content": [{"type": "text", "text": text}], "timestamp": 0}),
        json!({"role": "toolResult", "toolCallId": "call", "toolName": "read",
            "content": [{"type": "text", "text": text}], "isError": false, "timestamp": 0}),
        json!({"role": "custom", "customType": "test", "content": text,
            "display": false, "timestamp": 0}),
        json!({"role": "custom", "customType": "test",
            "content": [{"type": "text", "text": text}], "display": false, "timestamp": 0}),
        json!({"role": "branchSummary", "summary": text, "fromId": null, "timestamp": 0}),
        json!({"role": "compactionSummary", "summary": text, "tokensBefore": 0, "timestamp": 0}),
        json!({"role": "bashExecution", "command": "\u{1f600}\u{1f600}",
            "output": "\u{1f600}\u{1f600}", "cancelled": false, "truncated": false, "timestamp": 0}),
    ];
    let messages: Vec<AgentMessage> = fixtures
        .into_iter()
        .map(|fixture| serde_json::from_value(fixture).expect("message fixture"))
        .collect();
    // When
    let estimates = messages.iter().map(estimate_tokens).collect::<Vec<_>>();
    // Then
    assert_eq!(estimates, vec![2; 7]);
}

#[test]
fn selects_cut_point_when_non_bmp_text_reaches_retention_budget() {
    // Given
    let entries = vec![
        message_entry("first", user("abcd")),
        message_entry("second", user(&"\u{1f600}".repeat(4))),
    ];
    // When
    let cut = find_cut_point(&entries, 0, entries.len(), 2);
    // Then
    assert_eq!(cut.first_kept_entry_index, 1);
}

fn tool_result(text: &str) -> Message {
    serde_json::from_value(json!({
        "role": "toolResult", "toolCallId": "call", "toolName": "read",
        "content": [{"type": "text", "text": text}], "isError": false, "timestamp": 0
    }))
    .expect("tool-result fixture")
}

#[test]
fn truncates_tool_result_when_utf16_length_exceeds_limit() {
    // Given
    let text = "\u{1f600}".repeat(1001);
    // When
    let serialized = serialize_conversation(&[tool_result(&text)]);
    // Then
    assert_eq!(
        serialized,
        format!(
            "[Tool result]: {}\n\n[... 2 more characters truncated]",
            "\u{1f600}".repeat(1000)
        )
    );
}

#[test]
fn preserves_tool_result_when_utf16_length_equals_limit() {
    // Given
    let text = "\u{1f600}".repeat(1000);
    // When
    let serialized = serialize_conversation(&[tool_result(&text)]);
    // Then
    assert_eq!(serialized, format!("[Tool result]: {text}"));
}

#[test]
fn replaces_lone_surrogate_when_truncation_splits_pair() {
    // Given
    let prefix = "a".repeat(1999);
    let text = format!("{prefix}\u{1f600}z");
    // When
    let serialized = serialize_conversation(&[tool_result(&text)]);
    // Then
    assert_eq!(
        serialized,
        format!("[Tool result]: {prefix}\u{fffd}\n\n[... 2 more characters truncated]")
    );
}
