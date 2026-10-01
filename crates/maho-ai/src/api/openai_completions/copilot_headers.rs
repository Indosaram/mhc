//! Port of senpi packages/ai/src/api/github-copilot-headers.ts.
//!
//! Node-private copy: `src/api/github_copilot_headers.rs` is owned by node 12-misc.

use crate::types::{Message, UserContent};
use std::collections::BTreeMap;

pub fn infer_copilot_initiator(messages: &[Message]) -> &'static str {
    match messages.last() {
        Some(last) if last.role() != "user" => "agent",
        _ => "user",
    }
}

fn content_has_image(content: &[crate::types::ContentBlock]) -> bool {
    content.iter().any(|block| matches!(block, crate::types::ContentBlock::Image(_)))
}

pub fn has_copilot_vision_input(messages: &[Message]) -> bool {
    messages.iter().any(|message| match message {
        Message::User(user) => match &user.content {
            UserContent::Text(_) => false,
            UserContent::Blocks(blocks) => content_has_image(blocks),
        },
        Message::ToolResult(result) => content_has_image(&result.content),
        _ => false,
    })
}

pub fn build_copilot_dynamic_headers(messages: &[Message], has_images: bool) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert("X-Initiator".to_owned(), infer_copilot_initiator(messages).to_owned());
    headers.insert("Openai-Intent".to_owned(), "conversation-edits".to_owned());
    if has_images {
        headers.insert("Copilot-Vision-Request".to_owned(), "true".to_owned());
    }
    headers
}
