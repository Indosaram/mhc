//! Port of senpi packages/ai/src/utils/visible-text.ts.

use crate::types::{AssistantMessage, ContentBlock};
use unicode_general_category::{GeneralCategory, get_general_category};

pub fn has_visible_text(text: &str) -> bool {
    let stripped: String = text.chars().filter(|c| get_general_category(*c) != GeneralCategory::Format).collect();
    !super::js::trim(&stripped).is_empty()
}

pub fn has_visible_assistant_content(message: &AssistantMessage) -> bool {
    message.content.iter().any(|block| match block {
        ContentBlock::ToolCall(_) => true,
        ContentBlock::Text(text) => has_visible_text(&text.text),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{StopReason, TextContent, ThinkingContent, ToolCall, Usage};

    fn assistant(content: Vec<ContentBlock>) -> AssistantMessage {
        AssistantMessage {
            content,
            api: "openai-completions".into(),
            provider: "test".into(),
            model: "test-model".into(),
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
            timestamp: 1,
        }
    }

    /// One row per `it.each(["\u200b", ...])("treats format-character-only text %j as invisible")`
    /// case in visible-text.test.ts, title carried verbatim (the `%j` placeholder substituted).
    #[test]
    fn treats_format_character_only_text_as_invisible() {
        let cases: &[(&str, &str)] = &[
            ("treats format-character-only text \"\\u200b\" as invisible", "\u{200b}"),
            ("treats format-character-only text \"\\u200c\" as invisible", "\u{200c}"),
            ("treats format-character-only text \"\\u200d\" as invisible", "\u{200d}"),
            ("treats format-character-only text \"\\u2060\" as invisible", "\u{2060}"),
            ("treats format-character-only text \"\\ufeff\" as invisible", "\u{feff}"),
        ];
        for (title, text) in cases {
            assert!(!has_visible_text(text), "case: {title}");
        }
    }

    #[test]
    fn treats_whitespace_only_text_as_invisible() {
        assert!(!has_visible_text(" \t\n\r"));
    }

    #[test]
    fn keeps_emoji_zwj_sequences_visible() {
        assert!(has_visible_text("\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}"));
    }

    #[test]
    fn keeps_letters_mixed_with_zero_width_spaces_visible() {
        assert!(has_visible_text("\u{200b}visible\u{200b}"));
    }

    #[test]
    fn treats_an_empty_string_as_invisible() {
        assert!(!has_visible_text(""));
    }

    #[test]
    fn accepts_visible_text_or_a_tool_call_and_rejects_thinking_plus_invisible_text() {
        assert!(has_visible_assistant_content(&assistant(vec![ContentBlock::Text(TextContent {
            text: "answer".into(),
            ..Default::default()
        })])));
        assert!(has_visible_assistant_content(&assistant(vec![ContentBlock::ToolCall(ToolCall {
            id: "call-1".into(),
            name: "read".into(),
            arguments: [("path".to_owned(), serde_json::json!("README.md"))].into_iter().collect(),
            ..Default::default()
        })])));
        assert!(!has_visible_assistant_content(&assistant(vec![
            ContentBlock::Text(TextContent { text: "\u{200b}".into(), ..Default::default() }),
            ContentBlock::Thinking(ThinkingContent { thinking: "private".into(), ..Default::default() }),
        ])));
    }
}
