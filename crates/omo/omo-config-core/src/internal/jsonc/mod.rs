use serde_json::Value;

pub mod edit;
pub mod format;
pub mod parse;
pub mod scan;

pub use edit::{ModificationOptions, document_value, modify};
pub use format::{
    FormattingOptions, JsoncEdit, JsoncEditError, apply_edit, apply_edits, compute_indent_level,
    format_range, get_eol,
};
pub use parse::{JsonNode, JsoncParseError, NodeKind, find_node_at_location, parse_tree, to_value};
pub use scan::{Scanner, Token, TokenKind, is_eol};

#[derive(Debug, Clone, PartialEq)]
pub struct JsoncParseResult {
    pub data: Option<Value>,
    pub errors: Vec<JsoncParseError>,
}

pub fn strip_bom(content: &str) -> &str {
    content.strip_prefix('\u{feff}').unwrap_or(content)
}

pub fn parse_jsonc_safe(content: &str) -> JsoncParseResult {
    let text = strip_bom(content);
    let errors = parse::parse_errors(text);
    if !errors.is_empty() {
        return JsoncParseResult { data: None, errors };
    }
    match parse::parse_tree(text) {
        Ok(node) => JsoncParseResult {
            data: Some(parse::to_value(&node)),
            errors: Vec::new(),
        },
        Err(error) => JsoncParseResult {
            data: None,
            errors: vec![error],
        },
    }
}

#[cfg(test)]
mod parity_5138_tests {
    use super::parse_jsonc_safe;

    #[test]
    fn collects_recovered_errors_in_order_with_token_spans() {
        let result = parse_jsonc_safe("{\"a\":,\"b\":,\"c\" 1}");
        assert!(result.data.is_none());
        let errors: Vec<_> = result.errors.iter().map(|error| {
            (error.message.as_str(), error.offset, error.length)
        }).collect();
        assert_eq!(errors, vec![("ValueExpected", 5, 1), ("ValueExpected", 10, 1), ("ColonExpected", 15, 1)]);
    }

    #[test]
    fn scanner_errors_keep_specific_names_and_recovery_order() {
        let cases = [
            ("\"unfinished", vec![("UnexpectedEndOfString", 0, 11)]),
            ("\"a\u{1}b\"", vec![("InvalidCharacter", 0, 5)]),
            (r#""\q""#, vec![("InvalidEscapeCharacter", 0, 4)]),
            (r#""\u12x""#, vec![("InvalidUnicode", 0, 7)]),
            ("1.", vec![("UnexpectedEndOfNumber", 0, 2)]),
            ("1e+", vec![("UnexpectedEndOfNumber", 0, 3)]),
            ("/* open", vec![("UnexpectedEndOfComment", 0, 7), ("ValueExpected", 7, 0)]),
            ("wat", vec![("InvalidSymbol", 0, 3), ("ValueExpected", 3, 0)]),
        ];
        for (text, expected) in cases {
            let result = parse_jsonc_safe(text);
            assert!(result.data.is_none(), "{text:?}");
            let actual: Vec<_> = result.errors.iter().map(|error| {
                (error.message.as_str(), error.offset, error.length)
            }).collect();
            assert_eq!(actual, expected, "{text:?}");
        }
    }

    #[test]
    fn multi_error_files_join_every_recovered_message_like_the_loader() {
        let result = parse_jsonc_safe("{\"a\":,\"b\":,\"c\" 1}");
        let joined = result.errors.iter().map(|error| error.message.as_str()).collect::<Vec<_>>().join(", ");
        assert_eq!(joined, "ValueExpected, ValueExpected, ColonExpected");
    }

    #[test]
    fn recovery_reports_missing_commas_and_closing_delimiters() {
        let result = parse_jsonc_safe("[1 2, {\"a\":");
        let actual: Vec<_> = result.errors.iter().map(|error| error.message.as_str()).collect();
        assert_eq!(actual, ["CommaExpected", "ValueExpected", "CloseBraceExpected", "CloseBracketExpected"]);
    }

    #[test]
    fn errors_use_utf16_positions_after_stripping_bom() {
        let result = parse_jsonc_safe("\u{feff}{\"😀\":,\"b\":}");
        assert_eq!(result.errors.iter().map(|error| (error.offset, error.length)).collect::<Vec<_>>(), [(6, 1), (11, 1)]);
    }

    #[test]
    fn allowed_comments_and_trailing_commas_remain_valid() {
        let result = parse_jsonc_safe("// comment\n{\"a\": [1,], /* fine */}");
        assert!(result.errors.is_empty());
        assert_eq!(result.data, Some(serde_json::json!({"a": [1]})));
    }
}
