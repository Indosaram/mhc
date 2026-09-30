//! Port of senpi packages/ai/src/tool-call-middleware/protocols/anthropic-xml/invoke-stream-helpers.ts.

use std::collections::HashMap;

use serde_json::Value;

use super::invoke_protocol::InvokeProtocolConfig;
use super::invoke_tag_scanner::get_safe_invoke_text_length;
use super::invoke_tag_syntax::InvokeParameter;
use super::stream_boundary::ANTHROPIC_XML_MAX_RETAINED_FRAGMENT_LENGTH;
use super::xml_entities::decode_xml_entities;
use crate::tool_call_middleware::types::{ParserOptions, StreamParserEvent};
use crate::types::Tool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FunctionCallsTag {
    pub index: usize,
    pub length: usize,
}

pub fn should_emit_raw_tool_call_text_on_error(options: Option<&ParserOptions>) -> bool {
    options.is_some_and(|options| options.emit_raw_tool_call_text_on_error)
}

pub fn emit_text(events: &mut Vec<StreamParserEvent>, text: &str) {
    if !text.is_empty() {
        events.push(StreamParserEvent::Text { text: text.to_string() });
    }
}

fn find_function_calls_tag(closing: bool, text: &str, from_index: usize) -> Option<FunctionCallsTag> {
    let haystack = &text[from_index.min(text.len())..];
    for (start, c) in haystack.char_indices() {
        if c != '<' {
            continue;
        }
        let mut cursor = start + c.len_utf8();
        let rest = &haystack[cursor..];
        let after_ws: String = rest.chars().take_while(|ch| ch.is_whitespace()).collect();
        cursor += after_ws.len();
        let rest = &haystack[cursor..];
        if closing {
            let Some(rest2) = rest.strip_prefix('/') else { continue };
            cursor += 1;
            let after_ws2: String = rest2.chars().take_while(|ch| ch.is_whitespace()).collect();
            cursor += after_ws2.len();
        }
        let rest = &haystack[cursor..];
        let rest_no_ns = rest.strip_prefix("antml:").unwrap_or(rest);
        let ns_len = rest.len() - rest_no_ns.len();
        let Some(after_name) = rest_no_ns.strip_prefix("function_calls") else { continue };
        let name_end = cursor + ns_len + "function_calls".len();
        let after_ws3: String = after_name.chars().take_while(|ch| ch.is_whitespace()).collect();
        let close_pos = name_end + after_ws3.len();
        if !haystack[close_pos..].starts_with('>') {
            continue;
        }
        let end = close_pos + 1;
        return Some(FunctionCallsTag { index: from_index + start, length: end - start });
    }
    None
}

pub fn find_function_calls_open_tag(text: &str, from_index: usize) -> Option<FunctionCallsTag> {
    find_function_calls_tag(false, text, from_index)
}

pub fn find_function_calls_close_tag(text: &str, from_index: usize) -> Option<FunctionCallsTag> {
    find_function_calls_tag(true, text, from_index)
}

const FUNCTION_CALLS_OPEN_PREFIXES: [&str; 2] = ["<function_calls>", "<function_calls>"];
const FUNCTION_CALLS_CLOSE_PREFIXES: [&str; 2] = ["</function_calls>", "</function_calls>"];

/// `FUNCTION_CALLS_COMPLETE_TAG.test(candidate)`: a complete (possibly closing) function_calls tag
/// anchored at the start of `candidate`.
fn is_complete_function_calls_tag(candidate: &str) -> bool {
    find_function_calls_open_tag(candidate, 0).is_some_and(|m| m.index == 0)
        || find_function_calls_close_tag(candidate, 0).is_some_and(|m| m.index == 0)
}

pub fn is_potential_function_calls_tag(candidate: &str) -> bool {
    if is_complete_function_calls_tag(candidate) {
        return false;
    }
    let compact_candidate: String = candidate.chars().filter(|c| !c.is_whitespace()).collect();
    FUNCTION_CALLS_OPEN_PREFIXES.iter().chain(FUNCTION_CALLS_CLOSE_PREFIXES.iter()).any(|prefix| prefix.starts_with(&compact_candidate))
}

pub fn is_whitespace_or_function_calls_close_prefix(text: &str) -> bool {
    let compact_text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    compact_text.is_empty()
        || FUNCTION_CALLS_CLOSE_PREFIXES.iter().any(|prefix| prefix.starts_with(&compact_text) && compact_text != *prefix)
}

pub fn get_safe_stream_text_length(text: &str) -> usize {
    let scanner_safe_length = get_safe_invoke_text_length(text);
    let Some(last_tag_index) = text.rfind('<') else { return scanner_safe_length };
    if is_potential_function_calls_tag(&text[last_tag_index..]) {
        scanner_safe_length.min(last_tag_index)
    } else {
        scanner_safe_length
    }
}

pub fn report_error(options: Option<&ParserOptions>, message: &str, tool_call: &str) {
    if let Some(options) = options {
        let mut metadata = HashMap::new();
        metadata.insert("toolCall".to_string(), Value::String(tool_call.to_string()));
        options.report_error(message, Some(metadata));
    }
}

pub fn overflow_pending_fragment(options: Option<&ParserOptions>, label: &str, retained_fragment: &str) -> Vec<StreamParserEvent> {
    report_error(
        options,
        &format!("{label} streaming fragment exceeded the {ANTHROPIC_XML_MAX_RETAINED_FRAGMENT_LENGTH}-character retained-input limit."),
        retained_fragment,
    );
    if should_emit_raw_tool_call_text_on_error(options) {
        vec![StreamParserEvent::Text { text: retained_fragment.to_string() }]
    } else {
        Vec::new()
    }
}

pub fn report_truncated_invoke(options: Option<&ParserOptions>, config: &InvokeProtocolConfig, retained_length: usize) {
    if let Some(options) = options {
        let mut metadata = HashMap::new();
        metadata.insert("protocol".to_string(), Value::String(config.protocol.to_string()));
        metadata.insert("retainedLength".to_string(), Value::Number(retained_length.into()));
        options.report_error(&format!("{} tool call truncated at finish", config.label), Some(metadata));
    }
}

pub fn coerce_stream_parameters(
    parameters: &[InvokeParameter],
    tool: &Tool,
    config: &InvokeProtocolConfig,
    allow_json_schema_fallback: bool,
) -> Option<serde_json::Map<String, Value>> {
    let coerced_parameters = (config.coerce)(parameters, tool);
    if coerced_parameters.is_some() || !allow_json_schema_fallback {
        return coerced_parameters;
    }

    let mut arguments_record = serde_json::Map::new();
    for parameter in parameters {
        if arguments_record.contains_key(&parameter.name) {
            return None;
        }
        let raw_value = decode_xml_entities(&parameter.raw_value);
        let value = serde_json::from_str(&raw_value).unwrap_or(Value::String(raw_value));
        arguments_record.insert(parameter.name.clone(), value);
    }
    Some(arguments_record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_text_skips_empty_strings() {
        let mut events = Vec::new();
        emit_text(&mut events, "");
        assert!(events.is_empty());
        emit_text(&mut events, "hi");
        assert_eq!(events, vec![StreamParserEvent::Text { text: "hi".into() }]);
    }

    #[test]
    fn finds_function_calls_open_and_close_tags_with_namespace() {
        let open = find_function_calls_open_tag("prefix <function_calls> rest", 0).expect("open");
        assert_eq!(open.index, 7);
        assert_eq!(&"prefix <function_calls> rest"[open.index..open.index + open.length], "<function_calls>");

        let close = find_function_calls_close_tag("<function_calls></function_calls>", 5).expect("close");
        assert_eq!(&"<function_calls></function_calls>"[close.index..close.index + close.length], "</function_calls>");
    }

    #[test]
    fn is_potential_function_calls_tag_true_only_for_incomplete_prefixes() {
        assert!(is_potential_function_calls_tag("<function_ca"));
        assert!(is_potential_function_calls_tag("</function_calls"));
        assert!(!is_potential_function_calls_tag("<function_calls>"));
        assert!(!is_potential_function_calls_tag("<div>"));
    }

    #[test]
    fn is_whitespace_or_function_calls_close_prefix_detects_partial_close() {
        assert!(is_whitespace_or_function_calls_close_prefix("   "));
        assert!(is_whitespace_or_function_calls_close_prefix("</function_calls"));
        assert!(!is_whitespace_or_function_calls_close_prefix("</function_calls>"));
        assert!(!is_whitespace_or_function_calls_close_prefix("xyz"));
    }

    #[test]
    fn get_safe_stream_text_length_trims_trailing_function_calls_prefix() {
        assert_eq!(get_safe_stream_text_length("hello </function_ca"), 6);
        assert_eq!(get_safe_stream_text_length("hello world"), 11);
    }

    #[test]
    fn coerce_stream_parameters_falls_back_to_json_schema_when_configured() {
        use crate::tool_call_middleware::protocols::anthropic_xml::invoke_protocol::ANTHROPIC_XML_INVOKE_CONFIG;
        let tool = Tool { name: "t".into(), description: String::new(), parameters: serde_json::json!({}), freeform: None, constrained_sampling: None };
        let params = vec![InvokeParameter { name: "a".into(), raw_value: "1".into() }];
        // ANTHROPIC_XML_INVOKE_CONFIG.coerce requires the tool to declare "a" in its schema, so
        // this exercises the schema-based rejection returning None and then, with fallback
        // enabled, falling through to the raw JSON-or-string coercion.
        assert_eq!(coerce_stream_parameters(&params, &tool, &ANTHROPIC_XML_INVOKE_CONFIG, false), None);
        let fallback = coerce_stream_parameters(&params, &tool, &ANTHROPIC_XML_INVOKE_CONFIG, true).expect("fallback");
        assert_eq!(fallback.get("a"), Some(&Value::Number(1.into())));
    }

    #[test]
    fn coerce_stream_parameters_fallback_rejects_duplicate_names() {
        use crate::tool_call_middleware::protocols::anthropic_xml::invoke_protocol::ANTHROPIC_XML_INVOKE_CONFIG;
        let tool = Tool { name: "t".into(), description: String::new(), parameters: serde_json::json!({}), freeform: None, constrained_sampling: None };
        let params = vec![
            InvokeParameter { name: "a".into(), raw_value: "1".into() },
            InvokeParameter { name: "a".into(), raw_value: "2".into() },
        ];
        assert_eq!(coerce_stream_parameters(&params, &tool, &ANTHROPIC_XML_INVOKE_CONFIG, true), None);
    }
}
