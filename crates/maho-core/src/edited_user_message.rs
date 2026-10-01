//! Port of senpi packages/coding-agent/src/core/edited-user-message.ts.

use maho_ai::types::{ContentBlock, UserContent, UserMessage};
use maho_ai::utils::text::content_text;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserEditReason {
    Empty,
    NotUser,
    NotFound,
    StaleLeaf,
}

impl UserEditReason {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::NotUser => "not_user",
            Self::NotFound => "not_found",
            Self::StaleLeaf => "stale_leaf",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserEditError {
    pub reason: UserEditReason,
    pub message: String,
}

impl std::fmt::Display for UserEditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for UserEditError {}

impl UserEditError {
    fn new(reason: UserEditReason, message: impl Into<String>) -> Self {
        Self { reason, message: message.into() }
    }
}

/// Optimistic-concurrency guard for the user-edit surface: the caller's leaf token must equal the
/// session's current leaf.
pub fn assert_expected_user_leaf(expected_leaf_id: Option<&str>, current_leaf_id: Option<&str>) -> Result<(), UserEditError> {
    if expected_leaf_id.is_none() || expected_leaf_id == current_leaf_id {
        return Ok(());
    }
    Err(UserEditError::new(
        UserEditReason::StaleLeaf,
        format!(
            "Session leaf moved (expected {}, now {})",
            expected_leaf_id.unwrap_or_default(),
            current_leaf_id.unwrap_or("root")
        ),
    ))
}

fn user_content_text(content: &UserContent) -> String {
    match content {
        UserContent::Text(text) => text.clone(),
        UserContent::Blocks(blocks) => content_text(blocks, ""),
    }
}

pub fn user_text_equals(original: &UserMessage, text: &str) -> bool {
    user_content_text(&original.content).trim() == text.trim()
}

/// Text replaces every text block; every other block (the user's own attachments) is carried over.
pub fn build_edited_user_message(original: &UserMessage, text: &str) -> Result<UserMessage, UserEditError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(UserEditError::new(UserEditReason::Empty, "Edited user message cannot be empty"));
    }
    let attachments: Vec<ContentBlock> = match &original.content {
        UserContent::Blocks(blocks) => blocks.iter().filter(|block| !matches!(block, ContentBlock::Text(_))).cloned().collect(),
        UserContent::Text(_) => Vec::new(),
    };
    let mut content = vec![ContentBlock::Text(maho_ai::types::TextContent { text: trimmed.to_owned(), audience: None, text_signature: None })];
    content.extend(attachments);
    Ok(UserMessage { content: UserContent::Blocks(content), timestamp: chrono::Utc::now().timestamp_millis() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user(content: serde_json::Value) -> UserMessage {
        serde_json::from_value(json!({ "content": content, "timestamp": 0 })).expect("user")
    }

    #[test]
    fn a_stale_leaf_is_rejected() {
        assert!(assert_expected_user_leaf(Some("a"), Some("a")).is_ok());
        assert!(assert_expected_user_leaf(None, Some("b")).is_ok());
        let error = assert_expected_user_leaf(Some("a"), Some("b")).expect_err("stale");
        assert_eq!(error.reason.code(), "stale_leaf");
    }

    #[test]
    fn text_equality_ignores_surrounding_whitespace() {
        let message = user(json!([{ "type": "text", "text": "hi" }]));
        assert!(user_text_equals(&message, "  hi  "));
        assert!(!user_text_equals(&message, "bye"));
    }

    #[test]
    fn an_empty_edit_is_rejected() {
        let message = user(json!([{ "type": "text", "text": "hi" }]));
        assert_eq!(build_edited_user_message(&message, "   ").expect_err("empty").reason.code(), "empty");
    }

    #[test]
    fn attachments_are_carried_over() {
        let message = user(json!([
            { "type": "text", "text": "old" },
            { "type": "image", "mimeType": "image/png", "data": "AAAA" }
        ]));
        let edited = build_edited_user_message(&message, "new").expect("edited");
        let UserContent::Blocks(blocks) = edited.content else { panic!("blocks") };
        assert_eq!(blocks.len(), 2);
        assert!(matches!(blocks[0], ContentBlock::Text(_)));
        assert!(matches!(blocks[1], ContentBlock::Image(_)));
    }
}
