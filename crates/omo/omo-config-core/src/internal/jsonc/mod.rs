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
