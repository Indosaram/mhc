//! Port of senpi packages/ai/src/tool-call-middleware/protocols/anthropic-xml/parse.ts.

use std::collections::HashMap;

use serde_json::Value;

use super::invoke_match::find_next_invoke_match;
use super::invoke_protocol::{InvokeProtocolConfig, ANTHROPIC_XML_INVOKE_CONFIG};
use super::invoke_tag_scanner::{find_invoke_open_tag, scan_invoke_block};
use super::tool_resolver::ToolResolver;
use crate::tool_call_middleware::types::{ParsedToolCall, ParserOptions};
use crate::types::Tool;

pub fn parse_invoke_generated_text(
    text: &str,
    tools: &[Tool],
    config: &InvokeProtocolConfig,
    options: Option<&ParserOptions>,
) -> Vec<ParsedToolCall> {
    if text.is_empty() || tools.is_empty() {
        return Vec::new();
    }

    let resolver = ToolResolver::new(tools);
    let mut parsed_tool_calls = Vec::new();
    let mut cursor = 0usize;

    while cursor < text.len() {
        let Some(opening_tag) = find_invoke_open_tag(text, cursor) else { break };

        let tool = resolver.resolve(&opening_tag.tool_name);
        let block = scan_invoke_block(text, &opening_tag);
        if tool.is_none() {
            let next_known_invoke =
                find_next_invoke_match(text, opening_tag.index + opening_tag.length, |name| resolver.resolve(name).is_some());
            if let Some(next_known_invoke) = &next_known_invoke
                && (block.is_none()
                    || next_known_invoke.block.as_ref().is_some_and(|nb| Some(nb.end) == block.as_ref().map(|b| b.end)))
            {
                cursor = next_known_invoke.opening_tag.index;
                continue;
            }
        }
        let Some(block) = block else { break };

        let original_call_text = &text[opening_tag.index..block.end];
        cursor = block.end;

        let Some(tool) = tool else { continue };

        let arguments_record = block.parameters.as_deref().and_then(|params| (config.coerce)(params, tool));
        let Some(arguments_record) = arguments_record else {
            if let Some(options) = options {
                let mut metadata = HashMap::new();
                metadata.insert("toolCall".to_string(), Value::String(original_call_text.to_string()));
                options.report_error(&format!("Could not process {} tool call, keeping original text.", config.protocol), Some(metadata));
            }
            continue;
        };

        parsed_tool_calls.push(ParsedToolCall { name: tool.name.clone(), arguments: arguments_record });
    }

    parsed_tool_calls
}

pub fn parse_anthropic_xml_generated_text(text: &str, tools: &[Tool], options: Option<&ParserOptions>) -> Vec<ParsedToolCall> {
    parse_invoke_generated_text(text, tools, &ANTHROPIC_XML_INVOKE_CONFIG, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str) -> Tool {
        Tool {
            name: name.into(),
            description: "d".into(),
            parameters: json!({"type": "object", "properties": {"city": {"type": "string"}}}),
            freeform: None,
            constrained_sampling: None,
        }
    }

    #[test]
    fn parses_a_single_well_formed_invoke() {
        let text = r#"<invoke name="get_weather"><parameter name="city">Seoul</parameter></invoke>"#;
        let calls = parse_anthropic_xml_generated_text(text, &[tool("get_weather")], None);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_weather");
        assert_eq!(calls[0].arguments.get("city"), Some(&Value::String("Seoul".into())));
    }

    #[test]
    fn empty_text_or_no_tools_yields_no_calls() {
        assert!(parse_anthropic_xml_generated_text("", &[tool("t")], None).is_empty());
        assert!(parse_anthropic_xml_generated_text("<invoke name=\"t\"></invoke>", &[], None).is_empty());
    }

    #[test]
    fn skips_invoke_for_unknown_tool_and_continues_to_next_known_one() {
        let text = r#"<invoke name="unknown"><parameter name="a">1</parameter></invoke><invoke name="get_weather"><parameter name="city">Busan</parameter></invoke>"#;
        let calls = parse_anthropic_xml_generated_text(text, &[tool("get_weather")], None);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_weather");
    }

    #[test]
    fn reports_error_and_skips_when_coercion_fails() {
        let text = r#"<invoke name="get_weather"><parameter name="unknown_param">x</parameter></invoke>"#;
        let calls_no_opts = parse_anthropic_xml_generated_text(text, &[tool("get_weather")], None);
        assert!(calls_no_opts.is_empty());

        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_clone = seen.clone();
        let options = ParserOptions {
            emit_raw_tool_call_text_on_error: false,
            on_error: Some(std::sync::Arc::new(move |message: &str, _metadata| {
                seen_clone.lock().unwrap().push(message.to_string());
            })),
        };
        let calls = parse_anthropic_xml_generated_text(text, &[tool("get_weather")], Some(&options));
        assert!(calls.is_empty());
        assert_eq!(seen.lock().unwrap().as_slice(), ["Could not process anthropic-xml tool call, keeping original text."]);
    }

    #[test]
    fn breaks_on_unclosed_invoke_block() {
        let text = r#"<invoke name="get_weather">"#;
        assert!(parse_anthropic_xml_generated_text(text, &[tool("get_weather")], None).is_empty());
    }
}
