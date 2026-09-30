//! Port of senpi packages/ai/src/tool-call-middleware/protocols/anthropic-xml/invoke-match.ts.

use super::invoke_tag_scanner::{find_invoke_open_tag, scan_invoke_block, InvokeBlockMatch, InvokeOpenTagMatch};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvokeMatch {
    pub opening_tag: InvokeOpenTagMatch,
    pub block: Option<InvokeBlockMatch>,
}

pub fn find_next_invoke_match(text: &str, from_index: usize, matches_tool_name: impl Fn(&str) -> bool) -> Option<InvokeMatch> {
    let mut cursor = from_index;
    while cursor < text.len() {
        let opening_tag = find_invoke_open_tag(text, cursor)?;
        if matches_tool_name(&opening_tag.tool_name) {
            let block = scan_invoke_block(text, &opening_tag);
            return Some(InvokeMatch { opening_tag, block });
        }
        cursor = opening_tag.index + opening_tag.length;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_first_invoke_matching_the_predicate() {
        let text = r#"<invoke name="skip"></invoke><invoke name="target"><parameter name="a">1</parameter></invoke>"#;
        let m = find_next_invoke_match(text, 0, |name| name == "target").expect("match");
        assert_eq!(m.opening_tag.tool_name, "target");
        assert!(m.block.is_some());
    }

    #[test]
    fn returns_none_when_no_invoke_matches() {
        let text = r#"<invoke name="skip"></invoke>"#;
        assert!(find_next_invoke_match(text, 0, |name| name == "target").is_none());
    }

    #[test]
    fn returns_match_with_none_block_when_invoke_is_unclosed() {
        let text = r#"<invoke name="target">"#;
        let m = find_next_invoke_match(text, 0, |name| name == "target").expect("match");
        assert!(m.block.is_none());
    }
}
