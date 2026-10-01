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
    use super::super::invoke_tag_syntax::{find_invoke_open_tag, InvokeOpenTagMatch, InvokeParameter};
    use super::super::invoke_tag_syntax::scan_invoke_block;

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
    fn open_tag(index: usize) -> InvokeOpenTagMatch {
        InvokeOpenTagMatch { index, length: 0, tool_name: String::new() }
    }

    fn find_invoke_close_index(text: &str, from_index: usize) -> isize {
        match scan_invoke_block(text, &open_tag(from_index)) {
            Some(block) => block.content_end as isize,
            None => -1,
        }
    }

    fn parse_parameter_tags(inner: &str) -> Option<Vec<InvokeParameter>> {
        let text = format!("{inner}</invoke>");
        scan_invoke_block(&text, &open_tag(0)).and_then(|block| block.parameters)
    }

    #[test]
    fn finds_double_quoted_invoke_names_and_ignores_a_leading_function_calls_wrapper() {
        let text = "<function_calls>\n<invoke name=\"Bash\">";
        let found = find_invoke_open_tag(text, 0).expect("match");
        assert_eq!(found.index, text.find("<invoke").expect("invoke"));
        assert_eq!(found.length, "<invoke name=\"Bash\">".len());
        assert_eq!(found.tool_name, "Bash");
    }

    #[test]
    fn finds_single_quoted_names_with_whitespace_around_the_tag_syntax() {
        let text = "prefix <  invoke  name = 'list_files'  >";
        let found = find_invoke_open_tag(text, 0).expect("match");
        assert_eq!(found.index, text.find('<').expect("angle"));
        assert_eq!(found.length, text.len() - "prefix ".len());
        assert_eq!(found.tool_name, "list_files");
    }

    #[test]
    fn starts_searching_at_the_requested_index() {
        let text = "<invoke name=\"first\"></invoke><invoke name=\"second\">";
        let from = text.find("</invoke>").expect("close") + "</invoke>".len();
        assert_eq!(find_invoke_open_tag(text, from).map(|found| found.tool_name), Some("second".to_string()));
    }

    #[test]
    fn finds_the_first_closing_invoke_tag_after_the_content_start() {
        let text = "<invoke name=\"Bash\">body</invoke> trailing";
        let close_index = find_invoke_close_index(text, "<invoke name=\"Bash\">".len());
        assert_eq!(close_index, text.find("</invoke>").expect("close") as isize);
    }

    #[test]
    fn ignores_prompt_like_invoke_text_inside_a_parameter_value() {
        let text = "<invoke name=\"Bash\"><parameter name=\"command\">show <invoke name=\"Other\">example</invoke> literally</parameter></invoke>";
        let close_index = find_invoke_close_index(text, "<invoke name=\"Bash\">".len());
        assert_eq!(close_index, text.rfind("</invoke>").expect("close") as isize);
    }

    #[test]
    fn preserves_parameter_order_and_captures_multiline_values_verbatim() {
        let inner = concat!(
            "\n<parameter name=\"command\">echo first</parameter>",
            "\n< parameter name = 'script' >line 1\n<value>\nline 2 > line 3</parameter>"
        );
        let parameters = parse_parameter_tags(inner).expect("parameters");
        assert_eq!(parameters.len(), 2);
        assert_eq!(parameters[0].name, "command");
        assert_eq!(parameters[0].raw_value, "echo first");
        assert_eq!(parameters[1].name, "script");
        assert_eq!(parameters[1].raw_value, "line 1\n<value>\nline 2 > line 3");
    }

    #[test]
    fn returns_an_empty_list_when_no_parameter_tags_are_present() {
        assert_eq!(parse_parameter_tags("plain invoke content"), Some(Vec::new()));
    }

    #[test]
    fn rejects_unclosed_parameter_markup_instead_of_returning_partial_parameters() {
        assert_eq!(parse_parameter_tags("<parameter name=\"ignored\">unterminated"), None);
    }

    #[test]
    fn withholds_a_partial_invoke_start_from_the_text_prefix() {
        let text = "prefix text <inv";
        assert_eq!(get_safe_invoke_text_length(text), text.find("<inv").expect("partial"));
    }

    #[test]
    fn withholds_partial_parameter_function_calls_and_invoke_starts() {
        for partial_start in ["<parameter", "<function_calls", "< invoke name=", "<invoke name=\"Bash", "<invoke name='Bash'"] {
            let text = format!("prefix {partial_start}");
            assert_eq!(get_safe_invoke_text_length(&text), text.find('<').expect("angle"), "{partial_start}");
        }
    }

    #[test]
    fn emits_an_already_invalid_closed_invoke_attribute_with_trailing_content() {
        let text = "prefix <invoke name=\"Bash\" prose";
        assert!(find_invoke_open_tag(text, 0).is_none());
        assert_eq!(get_safe_invoke_text_length(text), text.len());
    }

    #[test]
    fn returns_the_full_length_when_no_partial_protocol_start_trails_the_text() {
        let text = "ordinary text with < and > characters";
        assert_eq!(get_safe_invoke_text_length(text), text.len());
    }

    #[test]
    fn keeps_unsupported_namespace_and_tag_names_as_bounded_text() {
        for text in ["<foo:invoke", "<ant:invoke", "<tool>"] {
            assert_eq!(get_safe_invoke_text_length(text), text.len(), "{text}");
        }
    }

}
