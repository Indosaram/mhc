//! Port of senpi packages/ai/src/utils/text.ts.

use crate::types::{ContentBlock, UserContent};

/// Extract and join text from message content.
pub fn content_text(content: &[ContentBlock], separator: &str) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(separator)
}

pub fn user_content_text(content: &UserContent, separator: &str) -> String {
    match content {
        UserContent::Text(text) => text.clone(),
        UserContent::Blocks(blocks) => content_text(blocks, separator),
    }
}

/// `Number.prototype.toString(36)` for non-negative integers.
pub fn to_radix_36(mut value: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while value > 0 {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    out.reverse();
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port of text.test.ts's shared `content` fixture (assistant content mixing thinking, text,
    /// a tool call, and provider-native blocks).
    fn content() -> Vec<ContentBlock> {
        vec![
            ContentBlock::Thinking(crate::types::ThinkingContent { thinking: "reasoning".into(), ..Default::default() }),
            ContentBlock::text("first"),
            ContentBlock::ToolCall(crate::types::ToolCall { id: "1".into(), name: "read".into(), ..Default::default() }),
            ContentBlock::ProviderNative(crate::types::ProviderNativeContent {
                subtype: "web_search_call".into(),
                raw: serde_json::json!({ "id": "ws_1" }),
            }),
            ContentBlock::text("second"),
        ]
    }

    #[test]
    fn extracts_assistant_text_blocks() {
        assert_eq!(content_text(&content(), "\n"), "first\nsecond");
    }

    #[test]
    fn supports_custom_separators() {
        assert_eq!(content_text(&content(), ""), "firstsecond");
    }

    #[test]
    fn passes_string_content_through() {
        assert_eq!(user_content_text(&UserContent::Text("hello".into()), "\n"), "hello");
    }

    #[test]
    fn extracts_text_from_tool_result_content() {
        let tool_result_content = vec![
            ContentBlock::text("first"),
            ContentBlock::Image(crate::types::ImageContent { data: "...".into(), mime_type: "image/png".into() }),
            ContentBlock::text("second"),
        ];
        assert_eq!(content_text(&tool_result_content, ""), "firstsecond");
    }

    #[test]
    fn to_radix_36_matches_number_prototype_to_string_36() {
        assert_eq!(to_radix_36(0), "0");
        assert_eq!(to_radix_36(35), "z");
        assert_eq!(to_radix_36(36), "10");
    }
}
