//! Port of senpi packages/ai/src/tool-call-middleware/protocols/anthropic-xml/invoke-tag-scanner.ts.

pub use super::invoke_tag_syntax::{
    find_incomplete_invoke_open_tag, find_invoke_open_tag, is_potential_protocol_start, scan_invoke_block,
    InvokeBlockMatch, InvokeOpenTagMatch, InvokeParameter,
};
use super::invoke_tag_syntax::{find_parameter_boundary, find_parameter_markup_pub, find_parameter_open_tag_at, ParameterBoundary};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruncatedInvokeBlockMatch {
    pub parameters: Vec<InvokeParameter>,
    pub is_structurally_complete: bool,
}

// Scans an invoke whose closing tag may be missing at stream end. A result is
// structurally complete only when every parameter closed and the residue is
// whitespace or a proper prefix of the invoke closing tag.
pub fn scan_truncated_invoke_block(text: &str, opening_tag: &InvokeOpenTagMatch) -> TruncatedInvokeBlockMatch {
    let mut parameters: Vec<InvokeParameter> = Vec::new();
    let mut cursor = opening_tag.index + opening_tag.length;

    loop {
        if cursor > text.len() {
            return TruncatedInvokeBlockMatch { parameters, is_structurally_complete: false };
        }
        let remainder = &text[cursor.min(text.len())..];
        if super::invoke_tag_syntax::is_whitespace_or_invoke_close_prefix(remainder) {
            return TruncatedInvokeBlockMatch { parameters, is_structurally_complete: true };
        }

        let leading_ws_len = remainder.chars().take_while(|c| c.is_whitespace()).map(|c| c.len_utf8()).sum::<usize>();
        let Some(parameter_markup) = find_parameter_markup_pub(text, cursor + leading_ws_len) else {
            return TruncatedInvokeBlockMatch { parameters, is_structurally_complete: false };
        };
        if parameter_markup.index != cursor + leading_ws_len {
            return TruncatedInvokeBlockMatch { parameters, is_structurally_complete: false };
        }

        let char_index = text[..parameter_markup.index].chars().count();
        let Some(parameter_open) = find_parameter_open_tag_at(text, char_index) else {
            return TruncatedInvokeBlockMatch { parameters, is_structurally_complete: false };
        };

        let value_start_char = char_index + parameter_open.length;
        let Some(boundary) = find_parameter_boundary(text, value_start_char) else {
            return TruncatedInvokeBlockMatch { parameters, is_structurally_complete: false };
        };
        let ParameterBoundary::ParameterClose(close) = boundary else {
            return TruncatedInvokeBlockMatch { parameters, is_structurally_complete: false };
        };

        let chars: Vec<char> = text.chars().collect();
        let raw_value: String = chars[value_start_char..close.index].iter().collect();
        parameters.push(InvokeParameter { name: parameter_open.name, raw_value });

        let close_end_char = close.index + close.length;
        cursor = text.char_indices().nth(close_end_char).map(|(b, _)| b).unwrap_or(text.len());
    }
}

pub fn get_safe_invoke_text_length(text: &str) -> usize {
    match text.rfind('<') {
        None => text.len(),
        Some(last_tag_index) => {
            if is_potential_protocol_start(&text[last_tag_index..]) {
                last_tag_index
            } else {
                text.len()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_truncated_invoke_block_is_complete_when_residue_is_whitespace() {
        let text = r#"<invoke name="t"><parameter name="a">1</parameter>  "#;
        let opening = find_invoke_open_tag(text, 0).expect("open");
        let result = scan_truncated_invoke_block(text, &opening);
        assert!(result.is_structurally_complete);
        assert_eq!(result.parameters.len(), 1);
    }

    #[test]
    fn scan_truncated_invoke_block_is_complete_with_partial_close_tag_prefix() {
        let text = r#"<invoke name="t"><parameter name="a">1</parameter></inv"#;
        let opening = find_invoke_open_tag(text, 0).expect("open");
        let result = scan_truncated_invoke_block(text, &opening);
        assert!(result.is_structurally_complete);
    }

    #[test]
    fn scan_truncated_invoke_block_is_incomplete_with_unclosed_parameter() {
        let text = r#"<invoke name="t"><parameter name="a">1"#;
        let opening = find_invoke_open_tag(text, 0).expect("open");
        let result = scan_truncated_invoke_block(text, &opening);
        assert!(!result.is_structurally_complete);
    }

    #[test]
    fn get_safe_invoke_text_length_trims_trailing_potential_tag_prefix() {
        assert_eq!(get_safe_invoke_text_length("hello <inv"), 6);
        assert_eq!(get_safe_invoke_text_length("hello <div>"), 11);
        assert_eq!(get_safe_invoke_text_length("no tags"), 7);
    }
}
