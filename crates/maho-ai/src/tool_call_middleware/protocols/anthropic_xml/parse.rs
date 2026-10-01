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
    use std::collections::HashMap;

    fn tool(name: &str) -> Tool {
        Tool {
            name: name.into(),
            description: "d".into(),
            parameters: json!({"type": "object", "properties": {"city": {"type": "string"}}}),
            freeform: None,
            constrained_sampling: None,
        }
    }

    fn inspect_tool() -> Tool {
        Tool {
            name: "inspect".into(),
            description: "d".into(),
            parameters: json!({"type": "object", "properties": {"label": {"type": "string"}, "count": {"type": "number"}}}),
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
    fn reports_invalid_coercion_and_skips_the_malformed_call() {
        // senpi: anthropic-xml-parser.test.ts "reports invalid coercion and skips the malformed call".
        let text = [
            r#"<invoke name="inspect"><parameter name="label">bad</parameter><parameter name="count">not-a-number</parameter></invoke>"#,
            r#"<invoke name="inspect"><parameter name="label">good</parameter><parameter name="count">2</parameter></invoke>"#,
        ]
        .join("\n");

        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_clone = seen.clone();
        let options = ParserOptions {
            emit_raw_tool_call_text_on_error: false,
            on_error: Some(std::sync::Arc::new(move |message: &str, _metadata| {
                seen_clone.lock().unwrap().push(message.to_string());
            })),
        };
        let calls = parse_anthropic_xml_generated_text(&text, &[inspect_tool()], Some(&options));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "inspect");
        assert_eq!(calls[0].arguments.get("label"), Some(&Value::String("good".into())));
        assert_eq!(calls[0].arguments.get("count"), Some(&json!(2)));
        assert_eq!(seen.lock().unwrap().as_slice(), ["Could not process anthropic-xml tool call, keeping original text."]);
    }

    #[test]
    fn an_undeclared_parameter_is_coerced_as_an_unknown_value_and_kept() {
        // senpi's coerceParameters falls back to coerceUnknownValue for a property the tool schema
        // does not declare, so the call survives; it is not a coercion failure.
        let text = r#"<invoke name="get_weather"><parameter name="unknown_param">x</parameter></invoke>"#;
        let calls = parse_anthropic_xml_generated_text(text, &[tool("get_weather")], None);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments.get("unknown_param"), Some(&Value::String("x".into())));
    }

    #[test]
    fn breaks_on_unclosed_invoke_block() {
        let text = r#"<invoke name="get_weather">"#;
        assert!(parse_anthropic_xml_generated_text(text, &[tool("get_weather")], None).is_empty());
    }
    fn bash_tool() -> Tool {
        Tool { name: "Bash".into(), description: "Run a shell command".into(), parameters: json!({"type": "object", "required": ["command"], "properties": {"command": {"type": "string"}}}), freeform: None, constrained_sampling: None }
    }

    fn canonical_bash_tool() -> Tool {
        Tool { name: "bash".into(), description: "Run a shell command".into(), parameters: json!({"type": "object", "required": ["command"], "properties": {"command": {"type": "string"}}}), freeform: None, constrained_sampling: None }
    }

    fn noop_tool() -> Tool {
        Tool { name: "noop".into(), description: "Do nothing".into(), parameters: json!({"type": "object", "properties": {}}), freeform: None, constrained_sampling: None }
    }

    fn optional_tool() -> Tool {
        Tool { name: "optional".into(), description: "Accept an optional note".into(), parameters: json!({"type": "object", "properties": {"note": {"type": "string"}}}), freeform: None, constrained_sampling: None }
    }

    fn flag_tool() -> Tool {
        Tool { name: "Flag".into(), description: "Toggle a flag".into(), parameters: json!({"type": "object", "required": ["enabled"], "properties": {"enabled": {"type": "boolean"}}}), freeform: None, constrained_sampling: None }
    }

    fn xml_sensitive_tool() -> Tool {
        Tool { name: "probe<&\"".into(), description: "Probe XML-sensitive values".into(), parameters: json!({"type": "object", "required": ["message"], "properties": {"message": {"type": "string"}}}), freeform: None, constrained_sampling: None }
    }

    fn error_messages(handler: impl Fn(&str, Option<&HashMap<String, Value>>) + Send + Sync + 'static) -> (std::sync::Arc<std::sync::Mutex<Vec<String>>>, ParserOptions) {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let options = ParserOptions {
            emit_raw_tool_call_text_on_error: false,
            on_error: Some(std::sync::Arc::new(move |message: &str, metadata: Option<&HashMap<String, Value>>| {
                handler(message, metadata);
                sink.lock().expect("error sink").push(message.to_string());
            })),
        };
        (seen, options)
    }

    #[test]
    fn parses_one_bare_invoke_with_schema_typed_parameters() {
        let text = r#"<invoke name="inspect"><parameter name="label">sample</parameter><parameter name="count">3.5</parameter></invoke>"#;
        let calls = parse_anthropic_xml_generated_text(text, &[inspect_tool()], None);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments.get("label"), Some(&json!("sample")));
        assert_eq!(calls[0].arguments.get("count"), Some(&json!(3.5)));
    }

    #[test]
    fn parses_a_pretty_printed_boolean_parameter() {
        let text = "<invoke name=\"Flag\"><parameter name=\"enabled\">\ntrue\n</parameter></invoke>";
        let calls = parse_anthropic_xml_generated_text(text, &[flag_tool()], None);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments.get("enabled"), Some(&json!(true)));
    }

    #[test]
    fn parses_multiple_invokes_in_order() {
        let text = [
            "before",
            r#"<invoke name="Bash"><parameter name="command">echo first</parameter></invoke>"#,
            "between",
            r#"<invoke name="Bash"><parameter name="command">echo second</parameter></invoke>"#,
            "after",
        ]
        .join(" ");
        let calls = parse_anthropic_xml_generated_text(&text, &[bash_tool()], None);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].arguments.get("command"), Some(&json!("echo first")));
        assert_eq!(calls[1].arguments.get("command"), Some(&json!("echo second")));
    }

    #[test]
    fn canonicalizes_a_unique_case_insensitive_tool_name_match_to_the_declared_tool_name() {
        let text = r#"<invoke name="Bash"><parameter name="command">echo ulwqa</parameter></invoke>"#;
        let calls = parse_anthropic_xml_generated_text(text, &[canonical_bash_tool()], None);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "bash");
        assert_eq!(calls[0].arguments.get("command"), Some(&json!("echo ulwqa")));
    }

    #[test]
    fn does_not_guess_an_ambiguous_case_insensitive_tool_name_match() {
        let text = r#"<invoke name="BaSh"><parameter name="command">echo ulwqa</parameter></invoke>"#;
        assert!(parse_anthropic_xml_generated_text(text, &[canonical_bash_tool(), bash_tool()], None).is_empty());
    }

    #[test]
    fn accepts_an_optional_function_calls_wrapper() {
        let text = "<function_calls>\n<invoke name=\"noop\"></invoke>\n</function_calls>";
        let calls = parse_anthropic_xml_generated_text(text, &[noop_tool()], None);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "noop");
        assert!(calls[0].arguments.is_empty());
    }

    #[test]
    fn preserves_embedded_angle_brackets_and_multiline_string_values() {
        let command = "printf '<value> & keep'\nline two";
        let text = format!("<invoke name=\"Bash\"><parameter name=\"command\">{command}</parameter></invoke>");
        let calls = parse_anthropic_xml_generated_text(&text, &[bash_tool()], None);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments.get("command"), Some(&json!(command)));
    }

    #[test]
    fn round_trips_xml_delimiters_and_boundary_newlines_from_the_formatter() {
        let message = "\n\nalpha &#10; </parameter> beta </invoke> & <tag>\r\nmiddle\n\n";
        let mut args = serde_json::Map::new();
        args.insert("message".to_string(), json!(message));
        let text = crate::tool_call_middleware::protocols::anthropic_xml::format::anthropic_xml_format_tool_call("probe<&\"", &args);
        assert!(text.contains("<parameter name=\"message\">&#10;&#10;alpha"));
        assert!(text.contains("middle&#10;&#10;</parameter>"));
        let calls = parse_anthropic_xml_generated_text(&text, &[xml_sensitive_tool()], None);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "probe<&\"");
        assert_eq!(calls[0].arguments.get("message"), Some(&json!(message)));
    }

    #[test]
    fn preserves_prompt_like_invoke_markup_inside_a_parameter_value() {
        let command = "Show <invoke name=\"Other\"><parameter name=\"example\">raw</parameter></invoke> exactly as text";
        let text = format!("<invoke name=\"Bash\"><parameter name=\"command\">{command}</parameter></invoke>");
        let calls = parse_anthropic_xml_generated_text(&text, &[bash_tool()], None);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments.get("command"), Some(&json!(command)));
    }

    #[test]
    fn preserves_an_unbalanced_nested_invoke_opening_tag_inside_a_parameter_value() {
        let command = "prefix <invoke name=\"Other\"> literally";
        let text = format!("<invoke name=\"Bash\"><parameter name=\"command\">{command}</parameter></invoke>");
        let calls = parse_anthropic_xml_generated_text(&text, &[bash_tool()], None);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments.get("command"), Some(&json!(command)));
    }

    #[test]
    fn skips_unknown_invokes_and_continues_with_later_known_invokes() {
        let text = [
            r#"<invoke name="Unknown"><parameter name="value">ignored</parameter></invoke>"#,
            r#"<invoke name="Bash"><parameter name="command">echo known</parameter></invoke>"#,
        ]
        .join("\n");
        let calls = parse_anthropic_xml_generated_text(&text, &[bash_tool()], None);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments.get("command"), Some(&json!("echo known")));
    }

    #[test]
    fn resynchronizes_after_an_unclosed_unknown_invoke_and_parses_a_later_known_invoke() {
        let text = concat!(
            "<invoke name=\"Unknown\"><parameter name=\"value\">unterminated\n",
            "<invoke name=\"Bash\"><parameter name=\"command\">echo recovered</parameter></invoke>"
        );
        let calls = parse_anthropic_xml_generated_text(text, &[bash_tool()], None);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments.get("command"), Some(&json!("echo recovered")));
    }

    #[test]
    fn does_not_resynchronize_away_from_an_incomplete_known_invoke() {
        let text = concat!(
            "<invoke name=\"Bash\"><parameter name=\"command\">unterminated\n",
            "<invoke name=\"Bash\"><parameter name=\"command\">echo hidden</parameter></invoke>"
        );
        assert!(parse_anthropic_xml_generated_text(text, &[bash_tool()], None).is_empty());
    }

    #[test]
    fn does_not_complete_a_known_no_parameter_invoke_with_a_later_nested_close_tag() {
        let text = "<invoke name=\"noop\">unterminated\n<invoke name=\"noop\"></invoke>";
        assert!(parse_anthropic_xml_generated_text(text, &[noop_tool()], None).is_empty());
    }

    #[test]
    fn returns_an_empty_argument_object_for_a_known_invoke_without_parameters() {
        let calls = parse_anthropic_xml_generated_text("<invoke name=\"noop\">\n</invoke>", &[noop_tool()], None);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "noop");
        assert!(calls[0].arguments.is_empty());
    }

    #[test]
    fn reports_malformed_parameter_markup_for_a_no_parameter_tool() {
        let (seen, options) = error_messages(|_, _| {});
        let text = "<invoke name=\"noop\"><parameter name=\"ignored\">unterminated</invoke>";
        assert!(parse_anthropic_xml_generated_text(text, &[noop_tool()], Some(&options)).is_empty());
        assert_eq!(seen.lock().expect("errors").as_slice(), ["Could not process anthropic-xml tool call, keeping original text."]);
    }

    #[test]
    fn reports_malformed_parameter_markup_for_an_optional_parameter_tool() {
        let (seen, options) = error_messages(|_, _| {});
        let text = "<invoke name=\"optional\"><parameter name=\"ignored\">unterminated</invoke>";
        assert!(parse_anthropic_xml_generated_text(text, &[optional_tool()], Some(&options)).is_empty());
        assert_eq!(seen.lock().expect("errors").as_slice(), ["Could not process anthropic-xml tool call, keeping original text."]);
    }

    #[test]
    fn stops_cleanly_at_a_truncated_invoke_after_returning_complete_calls() {
        let text = "<invoke name=\"noop\"></invoke><invoke name=\"Bash\"><parameter name=\"command\">echo incomplete";
        let calls = parse_anthropic_xml_generated_text(text, &[noop_tool(), bash_tool()], None);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "noop");
    }

}
