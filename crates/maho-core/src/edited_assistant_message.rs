//! Port of senpi packages/coding-agent/src/core/edited-assistant-message.ts.

use maho_ai::types::{AssistantMessage, ContentBlock, StopReason, TextContent};
use maho_ai::utils::text::content_text;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssistantEditReason {
    Empty,
    NotAssistant,
    NotFound,
    StaleLeaf,
}

impl AssistantEditReason {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::NotAssistant => "not_assistant",
            Self::NotFound => "not_found",
            Self::StaleLeaf => "stale_leaf",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssistantEditError {
    pub reason: AssistantEditReason,
    pub message: String,
}

impl std::fmt::Display for AssistantEditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AssistantEditError {}

impl AssistantEditError {
    fn new(reason: AssistantEditReason, message: impl Into<String>) -> Self {
        Self { reason, message: message.into() }
    }
}

/// Thrown by every tree mutation while a response is streaming; outranks the stale-leaf guard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStreamingError;

impl std::fmt::Display for SessionStreamingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Wait for the current response to finish before navigating the session tree.")
    }
}

impl std::error::Error for SessionStreamingError {}

impl SessionStreamingError {
    pub const fn code(&self) -> &'static str {
        "streaming"
    }
}

/// Optimistic-concurrency guard: the caller's leaf token must equal the session's current leaf.
pub fn assert_expected_leaf(expected_leaf_id: Option<&str>, current_leaf_id: Option<&str>) -> Result<(), AssistantEditError> {
    if expected_leaf_id.is_none() || expected_leaf_id == current_leaf_id {
        return Ok(());
    }
    Err(AssistantEditError::new(
        AssistantEditReason::StaleLeaf,
        format!(
            "Session leaf moved (expected {}, now {})",
            expected_leaf_id.unwrap_or_default(),
            current_leaf_id.unwrap_or("root")
        ),
    ))
}

pub fn assistant_text_equals(original: &AssistantMessage, text: &str) -> bool {
    content_text(&original.content, "").trim() == text.trim()
}

/// Text replaces every content block; model identity and usage stay so per-path cost and context
/// estimates remain accurate.
pub fn build_edited_assistant_message(original: &AssistantMessage, text: &str) -> Result<AssistantMessage, AssistantEditError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(AssistantEditError::new(AssistantEditReason::Empty, "Edited assistant response cannot be empty"));
    }
    let mut edited = original.clone();
    edited.content = vec![ContentBlock::Text(TextContent { text: trimmed.to_owned(), audience: None, text_signature: None })];
    edited.stop_reason = StopReason::Stop;
    edited.timestamp = chrono::Utc::now().timestamp_millis();
    Ok(edited)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assistant(content: serde_json::Value) -> AssistantMessage {
        serde_json::from_value(json!({
            "content": content, "api": "faux", "provider": "faux", "model": "faux-1",
            "usage": { "input": 1, "output": 2, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 3,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } },
            "stopReason": "toolUse", "timestamp": 0
        }))
        .expect("assistant")
    }

    #[test]
    fn a_stale_leaf_is_rejected() {
        let error = assert_expected_leaf(Some("a"), Some("b")).expect_err("stale");
        assert_eq!(error.reason.code(), "stale_leaf");
        assert!(assert_expected_leaf(Some("a"), Some("a")).is_ok());
    }

    #[test]
    fn the_streaming_error_code_is_stable() {
        assert_eq!(SessionStreamingError.code(), "streaming");
    }

    #[test]
    fn an_empty_edit_is_rejected() {
        let message = assistant(json!([{ "type": "text", "text": "hi" }]));
        assert_eq!(build_edited_assistant_message(&message, "").expect_err("empty").reason.code(), "empty");
    }

    #[test]
    fn editing_replaces_content_keeps_identity_and_sets_stop() {
        let message = assistant(json!([{ "type": "toolCall", "id": "1", "name": "edit", "arguments": {} }]));
        let edited = build_edited_assistant_message(&message, "new text").expect("edited");
        assert_eq!(edited.content.len(), 1);
        assert!(matches!(edited.content[0], ContentBlock::Text(_)));
        assert_eq!(edited.stop_reason, StopReason::Stop);
        assert_eq!(edited.model, "faux-1");
        assert_eq!(edited.usage.input, 1);
    }

    #[test]
    fn text_equality_ignores_whitespace() {
        let message = assistant(json!([{ "type": "text", "text": "hi" }]));
        assert!(assistant_text_equals(&message, " hi "));
    }
}
