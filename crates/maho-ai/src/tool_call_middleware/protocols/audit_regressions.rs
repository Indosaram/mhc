use super::anthropic_xml::{
    coerce_parameters::coerce_parameters, invoke_tag_syntax::InvokeParameter,
};
use super::gemma4::{gemma4_parse_generated_text, parse_gemma4_args};
use crate::types::Tool;
use serde_json::json;

fn tool(parameters: serde_json::Value) -> Tool {
    Tool {
        name: "t".into(),
        description: String::new(),
        parameters,
        output_schema: None,
        freeform: None,
        constrained_sampling: None,
    }
}

#[test]
fn gemma_requires_adjacent_terminator() {
    let text = "<|tool_call>call:t{x:1}garbage<tool_call|>";
    let calls = gemma4_parse_generated_text(text, &[], None);
    assert!(calls.is_empty());
}

#[test]
fn gemma_preserves_exponent_fraction_as_string() {
    let arguments = parse_gemma4_args("x:1.2e3", false);
    assert_eq!(arguments["x"], json!("1.2e3"));
}

#[test]
fn recovered_numbers_accept_js_radices() {
    let t = tool(json!({"type":"object","properties":{"x":{"type":"integer"}}}));
    for (raw, expected) in [("0x10", 16), ("0b10", 2), ("0o10", 8)] {
        let params = [InvokeParameter {
            name: "x".into(),
            raw_value: raw.into(),
        }];
        let arguments = coerce_parameters(&params, &t).expect("numeric arguments");
        assert_eq!(arguments["x"], json!(expected));
    }
}

#[test]
fn recovered_arguments_enforce_schema_constraints_without_extra_coercion() {
    for (schema, raw) in [
        (json!({"type":"string","pattern":"^a+$"}), "bbb"),
        (json!({"type":"number","exclusiveMinimum":2}), "2"),
        (json!({"type":"number","multipleOf":3}), "4"),
        (json!({"type":"array","uniqueItems":true}), "[1,1]"),
        (json!({"anyOf":[{"const":"a"},{"const":"b"}]}), "c"),
        (
            json!({"type":"object","additionalProperties":{"type":"number"}}),
            "{\"k\":\"2\"}",
        ),
    ] {
        let t = tool(json!({"type":"object","properties":{"x":schema}}));
        let params = [InvokeParameter {
            name: "x".into(),
            raw_value: raw.into(),
        }];
        let arguments = coerce_parameters(&params, &t);
        assert!(arguments.is_none(), "{raw}");
    }
}

#[test]
fn xml_scanner_accepts_js_whitespace() {
    let names = vec!["t".into()];
    let matched = super::xml_tool_tag_scanner::find_earliest_xml_tool_tag(
        "<\u{feff}t\u{a0}/\u{2028}>",
        &names,
    );
    assert!(matched.is_some_and(|tag| tag.self_closing));
}

#[test]
fn namespaced_partial_closes_are_retained() {
    assert!(
        super::anthropic_xml::invoke_stream_helpers::is_whitespace_or_function_calls_close_prefix(
            "</antml:fun"
        )
    );
    assert!(
        super::anthropic_xml::invoke_tag_syntax::is_whitespace_or_invoke_close_prefix(
            "</antml:inv"
        )
    );
}

#[test]
fn xml_resolver_strips_all_namespace_segments() {
    let tools = [tool(json!({"type":"object"}))];
    let resolver = super::anthropic_xml::tool_resolver::ToolResolver::new(&tools);
    let resolved = resolver.resolve("mcp__foo__bar__t");
    assert_eq!(resolved.map(|t| t.name.as_str()), Some("t"));
}

#[test]
fn invoke_stream_preserves_unicode_across_retained_capacity() {
    let mut parser = super::anthropic_xml::stream::create_anthropic_xml_stream_parser(vec![], None);
    let input = format!("{}😀é", "a".repeat(65535));
    let mut events = parser.feed(&input);
    events.extend(parser.finish());
    let text: String = events
        .into_iter()
        .filter_map(|event| match event {
            crate::tool_call_middleware::types::StreamParserEvent::Text { text } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(text, input);
}
