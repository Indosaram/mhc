//! Port of senpi packages/ai/src/utils/estimate.ts.

use crate::types::{ContentBlock, Context, Message, StopReason, Tool, Usage, UserContent};
use crate::utils::js::utf16_len;
use serde::Serialize;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUsageEstimate {
    pub tokens: u64,
    pub usage_tokens: u64,
    pub trailing_tokens: u64,
    pub last_usage_index: Option<usize>,
}

const CHARS_PER_TOKEN: usize = 4;
const ESTIMATED_IMAGE_CHARS: usize = 4800;

pub fn calculate_context_tokens(usage: &Usage) -> u64 {
    if usage.total_tokens != 0 { usage.total_tokens } else { usage.input + usage.output + usage.cache_read + usage.cache_write }
}

fn json_len(value: &impl Serialize) -> usize {
    serde_json::to_string(value).map_or("[unserializable]".len(), |json| utf16_len(&json))
}

fn tokens_for_chars(chars: usize) -> u64 {
    chars.div_ceil(CHARS_PER_TOKEN) as u64
}

fn blocks_chars(blocks: &[ContentBlock]) -> usize {
    blocks
        .iter()
        .map(|block| match block {
            ContentBlock::Text(text) => utf16_len(&text.text),
            _ => ESTIMATED_IMAGE_CHARS,
        })
        .sum()
}

pub fn estimate_text_tokens(text: &str) -> u64 {
    tokens_for_chars(utf16_len(text))
}

pub fn estimate_text_and_image_content_tokens(content: &UserContent) -> u64 {
    tokens_for_chars(match content {
        UserContent::Text(text) => utf16_len(text),
        UserContent::Blocks(blocks) => blocks_chars(blocks),
    })
}

pub fn estimate_message_tokens(message: &Message) -> u64 {
    match message {
        Message::User(user) => estimate_text_and_image_content_tokens(&user.content),
        Message::ToolResult(result) => tokens_for_chars(blocks_chars(&result.content)),
        Message::ConfigurationUpdate(_) => 0,
        Message::Assistant(assistant) => tokens_for_chars(
            assistant
                .content
                .iter()
                .map(|block| match block {
                    ContentBlock::Text(text) => utf16_len(&text.text),
                    ContentBlock::Thinking(thinking) => utf16_len(&thinking.thinking),
                    ContentBlock::ToolCall(call) => utf16_len(&call.name) + json_len(&call.arguments),
                    ContentBlock::ProviderNative(native) => utf16_len(&native.subtype) + json_len(&native.raw),
                    // Assistant content never carries images in TS; count like the fallthrough branch.
                    ContentBlock::Image(image) => json_len(image),
                })
                .sum(),
        ),
    }
}

fn message_timestamp(message: &Message) -> i64 {
    match message {
        Message::User(m) => m.timestamp,
        Message::Assistant(m) => m.timestamp,
        Message::ToolResult(m) => m.timestamp,
        Message::ConfigurationUpdate(m) => m.timestamp,
    }
}

fn last_assistant_usage(messages: &[Message]) -> Option<(Usage, usize)> {
    let mut latest_prefix_timestamp = i64::MIN;
    let mut usage_info = None;
    for (index, message) in messages.iter().enumerate() {
        if let Message::Assistant(assistant) = message
            && assistant.timestamp >= latest_prefix_timestamp
            && !matches!(assistant.stop_reason, StopReason::Aborted | StopReason::Error)
            && calculate_context_tokens(&assistant.usage) > 0
        {
            usage_info = Some((assistant.usage, index));
        }
        latest_prefix_timestamp = latest_prefix_timestamp.max(message_timestamp(message));
    }
    usage_info
}

pub fn estimate_messages(messages: &[Message]) -> ContextUsageEstimate {
    if let Some((usage, index)) = last_assistant_usage(messages) {
        let usage_tokens = calculate_context_tokens(&usage);
        let trailing_tokens = messages[index + 1..].iter().map(estimate_message_tokens).sum();
        return ContextUsageEstimate { tokens: usage_tokens + trailing_tokens, usage_tokens, trailing_tokens, last_usage_index: Some(index) };
    }
    let tokens = messages.iter().map(estimate_message_tokens).sum();
    ContextUsageEstimate { tokens, usage_tokens: 0, trailing_tokens: tokens, last_usage_index: None }
}

fn estimate_tools_tokens(tools: &[&Tool]) -> u64 {
    if tools.is_empty() { 0 } else { tokens_for_chars(json_len(&tools)) }
}

pub fn estimate_context_tokens(context: &Context) -> ContextUsageEstimate {
    let estimate = estimate_messages(&context.messages);
    let tools = context.tools.as_deref().unwrap_or_default();
    let added = match estimate.last_usage_index {
        Some(index) => {
            let names: HashSet<&str> = context.messages[index + 1..]
                .iter()
                .filter_map(|message| match message {
                    Message::ToolResult(result) => result.added_tool_names.as_ref(),
                    _ => None,
                })
                .flatten()
                .map(String::as_str)
                .collect();
            estimate_tools_tokens(&tools.iter().filter(|tool| names.contains(tool.name.as_str())).collect::<Vec<_>>())
        }
        None => {
            context.system_prompt.as_deref().filter(|p| !p.is_empty()).map_or(0, estimate_text_tokens)
                + estimate_tools_tokens(&tools.iter().collect::<Vec<_>>())
        }
    };
    ContextUsageEstimate { tokens: estimate.tokens + added, trailing_tokens: estimate.trailing_tokens + added, ..estimate }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn messages(value: serde_json::Value) -> Vec<Message> {
        serde_json::from_value(value).expect("messages")
    }

    fn assistant(total: u64, timestamp: i64, stop: &str) -> serde_json::Value {
        json!({"role": "assistant", "content": [{"type": "text", "text": "12345678"}], "api": "a", "provider": "p", "model": "m",
            "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": total,
                "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}},
            "stopReason": stop, "timestamp": timestamp})
    }

    #[test]
    fn estimates_from_last_usable_assistant_usage() {
        let list = messages(json!([
            {"role": "user", "content": "hello world!", "timestamp": 1},
            assistant(100, 2, "stop"),
            {"role": "user", "content": [{"type": "text", "text": "abcde"}, {"type": "image", "data": "x", "mimeType": "image/png"}], "timestamp": 3},
            assistant(500, 4, "error"),
        ]));
        let estimate = estimate_messages(&list);
        assert_eq!(estimate, ContextUsageEstimate { tokens: 100 + 1202 + 2, usage_tokens: 100, trailing_tokens: 1204, last_usage_index: Some(1) });
    }

    #[test]
    fn stale_usage_before_a_newer_prefix_is_ignored_and_context_prefix_counts() {
        let list = messages(json!([{"role": "user", "content": "abcd", "timestamp": 10}, assistant(100, 5, "stop")]));
        let context = Context {
            system_prompt: Some("12345678".into()),
            messages: list,
            tools: Some(vec![serde_json::from_value(json!({"name": "t", "description": "d", "parameters": {}})).expect("tool")]),
        };
        let estimate = estimate_context_tokens(&context);
        assert_eq!(estimate.last_usage_index, None);
        let tools_tokens = tokens_for_chars(r#"[{"name":"t","description":"d","parameters":{}}]"#.len());
        assert_eq!(estimate.tokens, 1 + 2 + 2 + tools_tokens);
    }
}
