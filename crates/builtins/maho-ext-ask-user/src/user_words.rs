//! Port of `ask-user/user-words.ts` (senpi 903b8336).
//!
//! Where an answer's own words go (senpi#2920): inline for people (hooks, the transcript card), or
//! out of the model-facing text behind a reference that names the label block they follow, so they
//! reach the model as a user turn and never inside a tool result or beside a harness label.

use maho_ai::types::ContentBlock;
use std::cell::RefCell;

/// A user's own words carried out of a tool result (`ToolResultUserWord` in core
/// `tool-result-user-words.ts`): the label block text and the words that follow it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResultUserWord {
    pub label: String,
    pub text: String,
}

/// `WordPlacement`: how a comment and a typed answer enter the model-facing text.
pub trait WordPlacement {
    fn comment(&self, text: &str) -> String;
    fn typed(&self, header: &str, text: &str) -> String;
}

/// `INLINE_WORDS`: the words stay where they are.
#[derive(Debug, Clone, Copy, Default)]
pub struct InlineWords;

pub const INLINE_WORDS: InlineWords = InlineWords;

impl WordPlacement for InlineWords {
    fn comment(&self, text: &str) -> String {
        text.to_owned()
    }
    fn typed(&self, _header: &str, text: &str) -> String {
        text.to_owned()
    }
}

fn reference(label: &str) -> String {
    format!("(see [{label}] below)")
}

/// `separatedWords(requestId)`: each placed word is recorded under a unique label and replaced by
/// a reference to that label.
#[derive(Debug, Default)]
pub struct SeparatedWords {
    request_id: String,
    words: RefCell<Vec<ToolResultUserWord>>,
}

pub fn separated_words(request_id: &str) -> SeparatedWords {
    SeparatedWords {
        request_id: request_id.to_owned(),
        words: RefCell::new(Vec::new()),
    }
}

impl SeparatedWords {
    pub fn words(&self) -> Vec<ToolResultUserWord> {
        self.words.borrow().clone()
    }

    pub fn into_words(self) -> Vec<ToolResultUserWord> {
        self.words.into_inner()
    }

    fn place(&self, base: String, text: &str) -> String {
        let mut words = self.words.borrow_mut();
        let prefix = format!("{base} (");
        let taken = words
            .iter()
            .filter(|word| word.label == base || word.label.starts_with(&prefix))
            .count();
        let label = if taken == 0 {
            base
        } else {
            format!("{base} ({})", taken + 1)
        };
        let placed = reference(&label);
        words.push(ToolResultUserWord {
            label,
            text: text.to_owned(),
        });
        placed
    }
}

impl WordPlacement for SeparatedWords {
    fn comment(&self, text: &str) -> String {
        self.place(
            format!("The user's comment for question {}", self.request_id),
            text,
        )
    }
    fn typed(&self, header: &str, text: &str) -> String {
        self.place(
            format!(
                "The user's answer to {header} for question {}",
                self.request_id
            ),
            text,
        )
    }
}

/// `resolveUserWordReferences`: puts each word back where its reference stands, for display.
pub fn resolve_user_word_references(text: &str, words: &[ToolResultUserWord]) -> String {
    words.iter().fold(text.to_owned(), |resolved, word| {
        resolved.replace(&reference(&word.label), &word.text)
    })
}

/// `readUserWordBlocks`: the label and word blocks after a later answer's frame block, as
/// `userWordBlocks` wrote them. Image blocks are skipped; an odd text count or a label block not
/// shaped `[label]` yields `None`.
pub fn read_user_word_blocks(content: &[ContentBlock]) -> Option<Vec<ToolResultUserWord>> {
    let texts: Vec<&str> = content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect();
    if !texts.len().is_multiple_of(2) {
        return None;
    }
    let mut words = Vec::with_capacity(texts.len() / 2);
    for pair in texts.chunks(2) {
        // /^\[(.+)\]$/s: at least one character between the brackets, newlines allowed.
        let label = pair[0]
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
            .filter(|label| !label.is_empty())?;
        words.push(ToolResultUserWord {
            label: label.to_owned(),
            text: pair[1].to_owned(),
        });
    }
    Some(words)
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ai::types::{ImageContent, TextContent};

    fn text(value: &str) -> ContentBlock {
        ContentBlock::Text(TextContent {
            text: value.into(),
            audience: None,
            text_signature: None,
        })
    }

    #[test]
    fn inline_words_keep_text_in_place() {
        assert_eq!(INLINE_WORDS.comment("hi"), "hi");
        assert_eq!(INLINE_WORDS.typed("Header", "typed"), "typed");
    }

    #[test]
    fn separated_words_reference_unique_labels() {
        let placement = separated_words("r1");
        assert_eq!(
            placement.comment("note"),
            "(see [The user's comment for question r1] below)"
        );
        assert_eq!(
            placement.comment("again"),
            "(see [The user's comment for question r1 (2)] below)"
        );
        assert_eq!(
            placement.typed("Color", "teal"),
            "(see [The user's answer to Color for question r1] below)"
        );
        assert_eq!(
            placement.typed("Color", "navy"),
            "(see [The user's answer to Color for question r1 (2)] below)"
        );
        assert_eq!(
            placement.typed("Color", "red"),
            "(see [The user's answer to Color for question r1 (3)] below)"
        );
        let words = placement.into_words();
        assert_eq!(words.len(), 5);
        assert_eq!(words[1].label, "The user's comment for question r1 (2)");
        assert_eq!(words[1].text, "again");
    }

    #[test]
    fn resolve_puts_words_back_for_display() {
        let placement = separated_words("r");
        let body = format!(
            "Q: {}\nComment: {}",
            placement.typed("Q", "yes"),
            placement.comment("because")
        );
        assert_eq!(
            resolve_user_word_references(&body, &placement.words()),
            "Q: yes\nComment: because"
        );
    }

    #[test]
    fn read_user_word_blocks_pairs_labels_and_words() {
        let content = vec![
            text("[The user's comment for question r]"),
            ContentBlock::Image(ImageContent {
                data: String::new(),
                mime_type: "image/png".into(),
            }),
            text("multi\nline"),
            text("[a\nb]"),
            text("second"),
        ];
        assert_eq!(
            read_user_word_blocks(&content),
            Some(vec![
                ToolResultUserWord {
                    label: "The user's comment for question r".into(),
                    text: "multi\nline".into()
                },
                ToolResultUserWord {
                    label: "a\nb".into(),
                    text: "second".into()
                },
            ])
        );
        assert_eq!(read_user_word_blocks(&[]), Some(vec![]));
    }

    #[test]
    fn read_user_word_blocks_rejects_malformed_layouts() {
        assert_eq!(read_user_word_blocks(&[text("[label]")]), None);
        assert_eq!(read_user_word_blocks(&[text("label"), text("words")]), None);
        assert_eq!(read_user_word_blocks(&[text("[]"), text("words")]), None);
    }
}
