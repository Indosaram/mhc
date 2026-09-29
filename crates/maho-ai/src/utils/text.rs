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

    #[test]
    fn joins_only_text_blocks() {
        let blocks = vec![
            ContentBlock::text("a"),
            ContentBlock::Image(crate::types::ImageContent { data: "x".into(), mime_type: "image/png".into() }),
            ContentBlock::text("b"),
        ];
        assert_eq!(content_text(&blocks, "\n"), "a\nb");
        assert_eq!(user_content_text(&UserContent::Text("s".into()), "\n"), "s");
        assert_eq!(to_radix_36(0), "0");
        assert_eq!(to_radix_36(35), "z");
        assert_eq!(to_radix_36(36), "10");
    }
}
