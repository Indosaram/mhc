//! Port of senpi packages/coding-agent/src/core/user-text-blocks.ts (the text-joining half; the
//! ask-user frame check `keepsTextBlocksVerbatim` stays with the ask-user port).

use maho_ai::types::TextContent;

/// The one text a user message's text blocks spell: the blocks joined by a newline.
pub fn join_user_text_blocks<S: AsRef<str>>(blocks: &[S]) -> String {
    blocks
        .iter()
        .map(AsRef::as_ref)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The text a queued user message is tracked by, read on both enqueue and delivery (senpi#3588):
/// `user_message_text(&user_text_content(text, blocks)) == text` for every queued `text`.
pub fn user_message_text(content: &[TextContent]) -> String {
    join_user_text_blocks(
        &content
            .iter()
            .map(|part| part.text.as_str())
            .collect::<Vec<_>>(),
    )
}

/// Counterpart of `userMessageText` for a plain string content: returned as-is.
pub fn user_message_text_of_string(content: &str) -> String {
    content.to_owned()
}

fn block(text: &str) -> TextContent {
    TextContent {
        text: text.to_owned(),
        audience: None,
        text_signature: None,
    }
}

/// The text content of a user message. Text that arrived as several blocks keeps them while they still
/// spell the final text exactly; text that was rewritten becomes one block (senpi#2920).
pub fn user_text_content(text: &str, text_blocks: Option<&[String]>) -> Vec<TextContent> {
    if let Some(blocks) = text_blocks
        && blocks.len() > 1
        && join_user_text_blocks(blocks) == text
    {
        return blocks.iter().map(|part| block(part)).collect();
    }
    vec![block(text)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_text_through_content() {
        let blocks = vec!["label".to_owned(), "words".to_owned()];
        assert_eq!(
            user_message_text(&user_text_content("label\nwords", Some(&blocks))),
            "label\nwords"
        );
        assert_eq!(user_text_content("label\nwords", Some(&blocks)).len(), 2);
        assert_eq!(user_text_content("rewritten", Some(&blocks)).len(), 1);
        assert_eq!(
            user_message_text(&user_text_content("plain", None)),
            "plain"
        );
        assert_eq!(user_message_text_of_string("a"), "a");
    }
}
