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

    #[test]
    fn format_chars_and_whitespace_are_invisible() {
        assert!(!has_visible_text("\u{200B}\u{FEFF} \n"));
        assert!(has_visible_text("\u{200B}a"));
    }
}
