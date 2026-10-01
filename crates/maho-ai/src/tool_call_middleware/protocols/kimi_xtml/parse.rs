//! Port of senpi packages/ai/src/tool-call-middleware/protocols/kimi-xtml/parse.ts.

use serde_json::{Map, Value};

use super::markers::{parse_xtml_attributes, XTML_ARGUMENT_CLOSE, XTML_ARGUMENT_OPEN, XTML_CALL_CLOSE, XTML_CALL_OPEN, XTML_SEP, XTML_TOOLS_CLOSE, XTML_TOOLS_OPEN};
use crate::tool_call_middleware::types::{ParsedToolCall, ParserOptions};
use crate::types::Tool;

pub enum CoercedXtmlValue {
    Ok(Value),
    Err,
}

pub fn coerce_xtml_argument_value(raw: &str, value_type: Option<&str>) -> CoercedXtmlValue {
    match value_type {
        None | Some("string") => CoercedXtmlValue::Ok(Value::String(raw.to_string())),
        Some("number") => match raw.trim().parse::<f64>() {
            Ok(parsed) if !parsed.is_nan() => CoercedXtmlValue::Ok(crate::utils::js::json_number(parsed)),
            _ => CoercedXtmlValue::Err,
        },
        Some("boolean") => match raw {
            "true" => CoercedXtmlValue::Ok(Value::Bool(true)),
            "false" => CoercedXtmlValue::Ok(Value::Bool(false)),
            _ => CoercedXtmlValue::Err,
        },
        Some("object") | Some("array") => serde_json::from_str::<Value>(raw).map_or(CoercedXtmlValue::Err, CoercedXtmlValue::Ok),
        Some(_) => CoercedXtmlValue::Ok(Value::String(raw.to_string())),
    }
}

fn parse_call_body(body: &str, tool: &Tool, options: Option<&ParserOptions>) -> Option<Map<String, Value>> {
    let mut args = Map::new();
    let mut rest = body;

    loop {
        rest = rest.trim_start();
        let Some(after_open) = rest.strip_prefix(XTML_ARGUMENT_OPEN) else { break };
        let Some(header_end) = after_open.find(XTML_SEP) else { break };
        let attributes = parse_xtml_attributes(&after_open[..header_end]);
        let key = attributes.get("key").cloned();
        let value_start = header_end + XTML_SEP.len();
        let Some(value_end) = after_open[value_start..].find(XTML_ARGUMENT_CLOSE).map(|i| i + value_start) else { break };
        let Some(key) = key else { break };

        let raw_value = &after_open[value_start..value_end];
        match coerce_xtml_argument_value(raw_value, attributes.get("type").map(String::as_str)) {
            CoercedXtmlValue::Err => {
                if let Some(options) = options {
                    options.report_error(
                        &format!("kimi-xtml: invalid value for argument \"{key}\" on tool \"{}\".", tool.name),
                        Some(std::collections::HashMap::from([("toolCall".to_string(), Value::String(after_open[..value_end + XTML_ARGUMENT_CLOSE.len()].to_string()))])),
                    );
                }
                return None;
            }
            CoercedXtmlValue::Ok(value) => {
                args.insert(key, value);
            }
        }

        rest = &after_open[value_end + XTML_ARGUMENT_CLOSE.len()..];
    }

    Some(args)
}

pub fn parse_kimi_xtml_generated_text(text: &str, tools: &[Tool], options: Option<&ParserOptions>) -> Vec<ParsedToolCall> {
    let mut parsed = Vec::new();
    let mut cursor = 0usize;

    while cursor < text.len() {
        let Some(block_start) = text[cursor..].find(XTML_TOOLS_OPEN).map(|i| i + cursor) else { break };
        let body_start = block_start + XTML_TOOLS_OPEN.len();
        let block_end = text[body_start..].find(XTML_TOOLS_CLOSE).map(|i| i + body_start);
        let block = match block_end {
            Some(end) => &text[body_start..end],
            None => &text[body_start..],
        };
        cursor = match block_end {
            Some(end) => end + XTML_TOOLS_CLOSE.len(),
            None => text.len(),
        };

        let mut call_cursor = 0usize;
        while call_cursor < block.len() {
            let Some(call_start) = block[call_cursor..].find(XTML_CALL_OPEN).map(|i| i + call_cursor) else { break };
            let header_start = call_start + XTML_CALL_OPEN.len();
            let Some(header_end) = block[header_start..].find(XTML_SEP).map(|i| i + header_start) else { break };
            let attributes = parse_xtml_attributes(&block[header_start..header_end]);
            let body_start = header_end + XTML_SEP.len();
            let body_end = block[body_start..].find(XTML_CALL_CLOSE).map(|i| i + body_start);
            let call_body = match body_end {
                Some(end) => &block[body_start..end],
                None => &block[body_start..],
            };
            call_cursor = match body_end {
                Some(end) => end + XTML_CALL_CLOSE.len(),
                None => block.len(),
            };

            let name = attributes.get("tool").cloned();
            let tool = name.as_deref().and_then(|n| tools.iter().find(|candidate| candidate.name == n));
            let Some(tool) = tool else {
                if let Some(options) = options {
                    options.report_error(
                        &format!("kimi-xtml: call for unknown tool \"{}\".", name.as_deref().unwrap_or("")),
                        Some(std::collections::HashMap::from([("toolCall".to_string(), Value::String(call_body.to_string()))])),
                    );
                }
                continue;
            };

            if let Some(args) = parse_call_body(call_body, tool, options) {
                parsed.push(ParsedToolCall { name: tool.name.clone(), arguments: args });
            }
        }
    }

    parsed
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn tool(name: &str) -> Tool {
        Tool { name: name.into(), description: "d".into(), parameters: json!({"type": "object"}), freeform: None, constrained_sampling: None }
    }

    #[test]
    fn coerces_number_boolean_object_and_array_values() {
        assert!(matches!(coerce_xtml_argument_value("3.5", Some("number")), CoercedXtmlValue::Ok(Value::Number(_))));
        assert!(matches!(coerce_xtml_argument_value("not-a-number", Some("number")), CoercedXtmlValue::Err));
        assert!(matches!(coerce_xtml_argument_value("true", Some("boolean")), CoercedXtmlValue::Ok(Value::Bool(true))));
        assert!(matches!(coerce_xtml_argument_value("maybe", Some("boolean")), CoercedXtmlValue::Err));
        assert!(matches!(coerce_xtml_argument_value("{\"a\":1}", Some("object")), CoercedXtmlValue::Ok(Value::Object(_))));
        assert!(matches!(coerce_xtml_argument_value("not json", Some("array")), CoercedXtmlValue::Err));
    }

    #[test]
    fn parses_a_single_call_with_string_and_number_arguments() {
        let text = format!(
            "{XTML_TOOLS_OPEN}{XTML_CALL_OPEN}tool=\"get_weather\" index=\"1\"{XTML_SEP}{XTML_ARGUMENT_OPEN}key=\"city\" type=\"string\"{XTML_SEP}Seoul{XTML_ARGUMENT_CLOSE}{XTML_ARGUMENT_OPEN}key=\"days\" type=\"number\"{XTML_SEP}3{XTML_ARGUMENT_CLOSE}{XTML_CALL_CLOSE}{XTML_TOOLS_CLOSE}"
        );
        let parsed = parse_kimi_xtml_generated_text(&text, &[tool("get_weather")], None);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "get_weather");
        assert_eq!(parsed[0].arguments.get("city"), Some(&json!("Seoul")));
        assert_eq!(parsed[0].arguments.get("days"), Some(&json!(3)));
    }

    #[test]
    fn skips_calls_for_unknown_tools_and_reports_the_error() {
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called_clone = called.clone();
        let options = ParserOptions { emit_raw_tool_call_text_on_error: false, on_error: Some(std::sync::Arc::new(move |_msg: &str, _meta| called_clone.store(true, std::sync::atomic::Ordering::SeqCst))) };
        let text = format!("{XTML_TOOLS_OPEN}{XTML_CALL_OPEN}tool=\"unknown\" index=\"1\"{XTML_SEP}{XTML_CALL_CLOSE}{XTML_TOOLS_CLOSE}");
        let parsed = parse_kimi_xtml_generated_text(&text, &[tool("get_weather")], Some(&options));
        assert_eq!(parsed, vec![]);
        assert!(called.load(std::sync::atomic::Ordering::SeqCst));
    }
    fn weather_tool() -> Tool {
        Tool {
            name: "get_weather".into(),
            description: "Get weather for a city".into(),
            parameters: json!({"type": "object", "required": ["city"], "properties": {"city": {"type": "string"}, "count": {"type": "number"}, "flag": {"type": "boolean"}}}),
            freeform: None,
            constrained_sampling: None,
        }
    }

    fn catalog_tool() -> Tool {
        Tool {
            name: "search_catalog".into(),
            description: "Search a nested catalog".into(),
            parameters: json!({"type": "object", "required": ["filters", "tags"], "properties": {"filters": {"type": "object", "required": ["category"], "properties": {"category": {"type": "string"}}}, "tags": {"type": "array", "items": {"type": "string"}}}}),
            freeform: None,
            constrained_sampling: None,
        }
    }

    fn error_sink() -> (std::sync::Arc<std::sync::Mutex<Vec<String>>>, crate::tool_call_middleware::types::ParserErrorHandler) {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let handler: crate::tool_call_middleware::types::ParserErrorHandler = std::sync::Arc::new(move |message: &str, _metadata: Option<&std::collections::HashMap<String, Value>>| {
            sink.lock().expect("error sink").push(message.to_string());
        });
        (seen, handler)
    }

    fn options_with(handler: crate::tool_call_middleware::types::ParserErrorHandler) -> ParserOptions {
        ParserOptions { emit_raw_tool_call_text_on_error: false, on_error: Some(handler) }
    }

    #[test]
    fn parses_a_full_xtml_tools_block_with_typed_arguments() {
        let text = concat!(
            "<|open|>tools<|sep|>",
            "<|open|>call tool=\"get_weather\" index=\"1\"<|sep|>",
            "<|open|>argument key=\"city\" type=\"string\"<|sep|>Seoul<|close|>argument<|sep|>",
            "<|open|>argument key=\"count\" type=\"number\"<|sep|>3<|close|>argument<|sep|>",
            "<|open|>argument key=\"flag\" type=\"boolean\"<|sep|>true<|close|>argument<|sep|>",
            "<|close|>call<|sep|>",
            "<|close|>tools<|sep|>"
        );
        let result = parse_kimi_xtml_generated_text(text, &[weather_tool()], None);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "get_weather");
        assert_eq!(result[0].arguments.get("city"), Some(&json!("Seoul")));
        assert_eq!(result[0].arguments.get("count").and_then(Value::as_f64), Some(3.0));
        assert_eq!(result[0].arguments.get("flag"), Some(&json!(true)));
    }

    #[test]
    fn parses_object_and_array_arguments_from_strict_json_values() {
        let text = concat!(
            "<|open|>tools<|sep|>",
            "<|open|>call tool=\"search_catalog\" index=\"1\"<|sep|>",
            "<|open|>argument key=\"filters\" type=\"object\"<|sep|>{\"category\":\"books\"}<|close|>argument<|sep|>",
            "<|open|>argument key=\"tags\" type=\"array\"<|sep|>[\"fiction\",\"award\"]<|close|>argument<|sep|>",
            "<|close|>call<|sep|>",
            "<|close|>tools<|sep|>"
        );
        let result = parse_kimi_xtml_generated_text(text, &[catalog_tool()], None);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].arguments.get("filters"), Some(&json!({"category": "books"})));
        assert_eq!(result[0].arguments.get("tags"), Some(&json!(["fiction", "award"])));
    }

    #[test]
    fn parses_multiple_calls_inside_one_tools_block() {
        let text = concat!(
            "<|open|>tools<|sep|>",
            "<|open|>call tool=\"get_weather\" index=\"1\"<|sep|>",
            "<|open|>argument key=\"city\" type=\"string\"<|sep|>Seoul<|close|>argument<|sep|>",
            "<|close|>call<|sep|>",
            "<|open|>call tool=\"get_weather\" index=\"2\"<|sep|>",
            "<|open|>argument key=\"city\" type=\"string\"<|sep|>Busan<|close|>argument<|sep|>",
            "<|close|>call<|sep|>",
            "<|close|>tools<|sep|>"
        );
        let result = parse_kimi_xtml_generated_text(text, &[weather_tool()], None);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].arguments.get("city"), Some(&json!("Seoul")));
        assert_eq!(result[1].arguments.get("city"), Some(&json!("Busan")));
    }

    #[test]
    fn accepts_single_quoted_and_unquoted_header_attributes() {
        let text = concat!(
            "<|open|>tools<|sep|>",
            "<|open|>call tool='get_weather' index='1'<|sep|>",
            "<|open|>argument key=city type=string<|sep|>Seoul<|close|>argument<|sep|>",
            "<|close|>call<|sep|>",
            "<|close|>tools<|sep|>"
        );
        let result = parse_kimi_xtml_generated_text(text, &[weather_tool()], None);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].arguments.get("city"), Some(&json!("Seoul")));
    }

    #[test]
    fn ignores_narrative_text_outside_tools_blocks() {
        let text = concat!(
            "Let me check that for you.\n",
            "<|open|>tools<|sep|>",
            "<|open|>call tool=\"get_weather\" index=\"1\"<|sep|>",
            "<|open|>argument key=\"city\" type=\"string\"<|sep|>Seoul<|close|>argument<|sep|>",
            "<|close|>call<|sep|>",
            "<|close|>tools<|sep|>",
            "\nOne moment."
        );
        let result = parse_kimi_xtml_generated_text(text, &[weather_tool()], None);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].arguments.get("city"), Some(&json!("Seoul")));
    }

    #[test]
    fn treats_a_missing_type_attribute_as_a_raw_string() {
        let text = concat!(
            "<|open|>tools<|sep|>",
            "<|open|>call tool=\"get_weather\" index=\"1\"<|sep|>",
            "<|open|>argument key=\"city\"<|sep|>Seoul<|close|>argument<|sep|>",
            "<|close|>call<|sep|>",
            "<|close|>tools<|sep|>"
        );
        let result = parse_kimi_xtml_generated_text(text, &[weather_tool()], None);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].arguments.get("city"), Some(&json!("Seoul")));
    }

    #[test]
    fn treats_an_unknown_type_attribute_as_a_raw_string() {
        let text = concat!(
            "<|open|>tools<|sep|>",
            "<|open|>call tool=\"get_weather\" index=\"1\"<|sep|>",
            "<|open|>argument key=\"city\" type=\"mystery\"<|sep|>Seoul<|close|>argument<|sep|>",
            "<|close|>call<|sep|>",
            "<|close|>tools<|sep|>"
        );
        let result = parse_kimi_xtml_generated_text(text, &[weather_tool()], None);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].arguments.get("city"), Some(&json!("Seoul")));
    }

    #[test]
    fn rejects_malformed_json_in_an_object_argument_and_reports_the_error() {
        let (seen, handler) = error_sink();
        let options = options_with(handler);
        let text = concat!(
            "<|open|>tools<|sep|>",
            "<|open|>call tool=\"search_catalog\" index=\"1\"<|sep|>",
            "<|open|>argument key=\"filters\" type=\"object\"<|sep|>{category:books}<|close|>argument<|sep|>",
            "<|close|>call<|sep|>",
            "<|close|>tools<|sep|>"
        );
        let result = parse_kimi_xtml_generated_text(text, &[catalog_tool()], Some(&options));
        assert!(result.is_empty());
        assert_eq!(seen.lock().expect("errors").len(), 1);
    }

    #[test]
    fn parses_a_complete_call_even_when_the_closing_tools_marker_is_missing() {
        let text = concat!(
            "<|open|>tools<|sep|>",
            "<|open|>call tool=\"get_weather\" index=\"1\"<|sep|>",
            "<|open|>argument key=\"city\" type=\"string\"<|sep|>Seoul<|close|>argument<|sep|>",
            "<|close|>call<|sep|>"
        );
        let result = parse_kimi_xtml_generated_text(text, &[weather_tool()], None);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].arguments.get("city"), Some(&json!("Seoul")));
    }

}
