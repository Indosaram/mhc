//! Port of senpi packages/ai/src/api/github-copilot-headers.ts.

use crate::types::{ContentBlock, Message, UserContent};
use std::collections::BTreeMap;

/// Copilot expects X-Initiator to indicate whether the request is user-initiated or
/// agent-initiated (e.g. follow-up after assistant/tool messages).
pub fn infer_copilot_initiator(messages: &[Message]) -> &'static str {
    match messages.last() {
        Some(last) if last.role() != "user" => "agent",
        _ => "user",
    }
}

/// Copilot requires Copilot-Vision-Request header when sending images.
pub fn has_copilot_vision_input(messages: &[Message]) -> bool {
    messages.iter().any(|message| {
        let content = match message {
            Message::User(user) => match &user.content {
                UserContent::Text(_) => return false,
                UserContent::Blocks(blocks) => blocks,
            },
            Message::ToolResult(result) => &result.content,
            _ => return false,
        };
        content.iter().any(|block| matches!(block, ContentBlock::Image(_)))
    })
}

pub fn build_copilot_dynamic_headers(messages: &[Message], has_images: bool) -> BTreeMap<String, String> {
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    headers.insert("X-Initiator".into(), infer_copilot_initiator(messages).to_owned());
    headers.insert("Openai-Intent".into(), "conversation-edits".into());

    if has_images {
        headers.insert("Copilot-Vision-Request".into(), "true".into());
    }

    headers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AssistantMessage, ImageContent, StopReason, ToolResultMessage, Usage, UserMessage,
    };

    fn user(content: UserContent) -> Message {
        Message::User(UserMessage { content, timestamp: 0 })
    }

    fn assistant() -> Message {
        Message::Assistant(Box::new(AssistantMessage {
            content: vec![ContentBlock::text("hi")],
            api: "openai-completions".into(),
            provider: "github-copilot".into(),
            model: "gpt".into(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            stop_details: None,
            deferred: None,
            error_message: None,
            abort_source: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 0,
        }))
    }

    #[test]
    fn initiator_is_agent_unless_the_last_message_is_from_the_user() {
        assert_eq!(infer_copilot_initiator(&[]), "user");
        assert_eq!(infer_copilot_initiator(&[user(UserContent::Text("hi".into()))]), "user");
        assert_eq!(infer_copilot_initiator(&[user(UserContent::Text("hi".into())), assistant()]), "agent");
    }

    #[test]
    fn vision_input_covers_user_and_tool_result_images() {
        let image = ContentBlock::Image(ImageContent { data: "AA==".into(), mime_type: "image/png".into() });
        assert!(!has_copilot_vision_input(&[user(UserContent::Text("hi".into()))]));
        assert!(has_copilot_vision_input(&[user(UserContent::Blocks(vec![image.clone()]))]));
        assert!(has_copilot_vision_input(&[Message::ToolResult(ToolResultMessage {
            tool_call_id: "c".into(),
            tool_name: "t".into(),
            content: vec![image],
            details: None,
            usage: None,
            added_tool_names: None,
            is_error: false,
            timestamp: 0,
        })]));
    }

    #[test]
    fn dynamic_headers_add_the_vision_marker_only_with_images() {
        let messages = vec![user(UserContent::Text("hi".into()))];
        let plain = build_copilot_dynamic_headers(&messages, false);
        assert_eq!(plain.get("X-Initiator").map(String::as_str), Some("user"));
        assert_eq!(plain.get("Openai-Intent").map(String::as_str), Some("conversation-edits"));
        assert!(!plain.contains_key("Copilot-Vision-Request"));

        let with_images = build_copilot_dynamic_headers(&messages, true);
        assert_eq!(with_images.get("Copilot-Vision-Request").map(String::as_str), Some("true"));
    }
}
