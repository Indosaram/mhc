//! Port of senpi packages/ai/src/tool-call-middleware/protocols/antml/stream.ts.

use super::config::ANTML_INVOKE_CONFIG;
use crate::tool_call_middleware::protocols::anthropic_xml::stream::create_invoke_stream_parser;
use crate::tool_call_middleware::types::{ParserOptions, StreamParser};
use crate::types::Tool;

pub fn create_antml_stream_parser(tools: Vec<Tool>, options: Option<ParserOptions>) -> Box<dyn StreamParser + Send> {
    create_invoke_stream_parser(tools, &ANTML_INVOKE_CONFIG, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_call_middleware::types::{ParserOptions, StreamParser, StreamParserEvent};
    use serde_json::{json, Value};

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
    fn streams_an_antml_invoke_wrapped_in_function_calls() {
        let tools = vec![tool("get_weather")];
        let mut parser = create_antml_stream_parser(tools, None);
        let events =
            parser.feed(r#"<function_calls><invoke name="get_weather"><parameter name="city">Seoul</parameter></invoke></function_calls>"#);
        assert!(events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallStart { name, .. } if name == "get_weather")));
        assert!(parser.finish().is_empty());
    }
    fn bash_tool() -> Tool {
        Tool { name: "Bash".into(), description: "Run a shell command".into(), parameters: json!({"type": "object", "required": ["command"], "properties": {"command": {"type": "string"}}}), freeform: None, constrained_sampling: None }
    }

    fn edit_tool() -> Tool {
        Tool {
            name: "Edit".into(),
            description: "Edit a file".into(),
            parameters: json!({"type": "object", "required": ["file_path", "edits"], "properties": {"file_path": {"type": "string"}, "edits": {"type": "array", "items": {"type": "object", "required": ["oldText", "newText"], "properties": {"oldText": {"type": "string"}, "newText": {"type": "string"}}}}}}),
            freeform: None,
            constrained_sampling: None,
        }
    }

    fn fixture_tools() -> Vec<Tool> {
        vec![
            Tool { name: "get_weather".into(), description: "Get weather".into(), parameters: json!({"type": "object", "required": ["city"], "properties": {"city": {"type": "string"}, "days": {"type": "integer"}}}), freeform: None, constrained_sampling: None },
            Tool { name: "todowrite".into(), description: "Write todos".into(), parameters: json!({"type": "object", "required": ["todos"], "properties": {"todos": {"type": "array", "minItems": 1, "items": {"type": "object", "required": ["content", "status", "priority"], "properties": {"content": {"type": "string"}, "status": {"type": "string"}, "priority": {"type": "string"}}}}}}), freeform: None, constrained_sampling: None },
            Tool { name: "get_location".into(), description: "Get location".into(), parameters: json!({"type": "object", "properties": {}}), freeform: None, constrained_sampling: None },
        ]
    }

    fn error_sink() -> (std::sync::Arc<std::sync::Mutex<Vec<String>>>, impl Fn(&str, Option<&std::collections::HashMap<String, Value>>) + Send + Sync + 'static) {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let handler = move |message: &str, _metadata: Option<&std::collections::HashMap<String, Value>>| {
            sink.lock().expect("error sink").push(message.to_string());
        };
        (seen, handler)
    }

    fn options_with(emit_raw: bool, handler: impl Fn(&str, Option<&std::collections::HashMap<String, Value>>) + Send + Sync + 'static) -> ParserOptions {
        ParserOptions { emit_raw_tool_call_text_on_error: emit_raw, on_error: Some(std::sync::Arc::new(handler)) }
    }

    fn text_output(events: &[StreamParserEvent]) -> String {
        events.iter().filter_map(|event| if let StreamParserEvent::Text { text } = event { Some(text.as_str()) } else { None }).collect()
    }

    fn tool_call_ends(events: &[StreamParserEvent]) -> Vec<&StreamParserEvent> {
        events.iter().filter(|event| matches!(event, StreamParserEvent::ToolcallEnd { .. })).collect()
    }

    fn is_toolcall_event(event: &StreamParserEvent) -> bool {
        matches!(event, StreamParserEvent::ToolcallStart { .. } | StreamParserEvent::ToolcallDelta { .. } | StreamParserEvent::ToolcallEnd { .. })
    }

    fn feed_all(parser: &mut Box<dyn StreamParser + Send>, input: &str) -> Vec<StreamParserEvent> {
        let mut events = parser.feed(input);
        events.extend(parser.finish());
        events
    }

    #[test]
    fn emits_a_complete_invoke_as_one_tool_call_with_antml_ids() {
        let mut parser = create_antml_stream_parser(vec![bash_tool()], None);
        assert_eq!(
            feed_all(&mut parser, r#"<invoke name="Bash"><parameter name="command">echo hi</parameter></invoke>"#),
            vec![
                StreamParserEvent::ToolcallStart { index: 0, name: "Bash".into(), id: "antml-tool-0".into() },
                StreamParserEvent::ToolcallDelta { index: 0, arguments_delta: r#"{"command":"echo hi"}"#.into() },
                StreamParserEvent::ToolcallEnd {
                    index: 0,
                    name: "Bash".into(),
                    id: "antml-tool-0".into(),
                    arguments: json!({"command": "echo hi"}).as_object().expect("object").clone(),
                    incomplete: false,
                    error_message: None,
                },
            ]
        );
    }

    #[test]
    fn drops_the_function_calls_wrapper_while_preserving_surrounding_prose() {
        let input = r#"Before <function_calls><invoke name="Bash"><parameter name="command">ls</parameter></invoke></function_calls> after"#;
        let mut parser = create_antml_stream_parser(vec![bash_tool()], None);
        let events = feed_all(&mut parser, input);
        assert_eq!(tool_call_ends(&events).len(), 1);
        let text = text_output(&events);
        assert_eq!(text, "Before  after");
        assert!(!text.contains("function_calls"));
    }

    #[test]
    fn parses_bare_and_antml_namespaced_tags_identically_across_split_boundaries() {
        let bare = r#"<function_calls><invoke name="Bash"><parameter name="command">echo hi</parameter></invoke></function_calls>"#;
        let namespaced = r#"<antml:function_calls><antml:invoke name="Bash"><antml:parameter name="command">echo hi</antml:parameter></antml:invoke></antml:function_calls>"#;
        let mut expected_parser = create_antml_stream_parser(vec![bash_tool()], None);
        let expected_events = feed_all(&mut expected_parser, bare);

        let namespaced_chars: Vec<char> = namespaced.chars().collect();
        for split in 0..=namespaced_chars.len() {
            let head: String = namespaced_chars[..split].iter().collect();
            let tail: String = namespaced_chars[split..].iter().collect();
            let mut parser = create_antml_stream_parser(vec![bash_tool()], None);
            let mut events = parser.feed(&head);
            events.extend(parser.feed(&tail));
            events.extend(parser.finish());
            assert_eq!(events, expected_events, "split {split}");
        }
    }

    #[test]
    fn keeps_unsupported_namespace_and_tag_names_as_bounded_text() {
        for input in [
            r#"<foo:invoke name="Bash"><foo:parameter name="command">echo hi</foo:parameter></foo:invoke>"#,
            r#"<ant:invoke name="Bash"><ant:parameter name="command">echo hi</ant:parameter></ant:invoke>"#,
            "before <tool>after",
        ] {
            let mut parser = create_antml_stream_parser(vec![bash_tool()], None);
            let events = feed_all(&mut parser, input);
            assert_eq!(text_output(&events), input, "{input}");
            assert!(!events.iter().any(is_toolcall_event), "{input}");
        }
    }

    #[test]
    fn repairs_sloppy_nested_arguments_in_the_streaming_path() {
        let input = concat!(
            "<invoke name=\"Edit\">",
            "<parameter name=\"path\">f.py</parameter>",
            "<parameter name=\"edits\">[{\"oldText\":\"a\",\"newText\":\"b\",\"requireUnique\":true}]</parameter>",
            "</invoke>"
        );
        let mut parser = create_antml_stream_parser(vec![edit_tool()], None);
        let events = feed_all(&mut parser, input);
        let ends = tool_call_ends(&events);
        assert_eq!(ends.len(), 1);
        let StreamParserEvent::ToolcallEnd { name, arguments, .. } = ends[0] else { unreachable!() };
        assert_eq!(name, "Edit");
        assert_eq!(arguments.get("file_path"), Some(&json!("f.py")));
        assert_eq!(arguments.get("edits"), Some(&json!([{"oldText": "a", "newText": "b"}])));
    }

    #[test]
    fn suppresses_an_unrecoverably_malformed_known_invoke_and_reports_it() {
        let (seen, handler) = error_sink();
        let count_tool = Tool { name: "Count".into(), description: "Count".into(), parameters: json!({"type": "object", "required": ["count"], "properties": {"count": {"type": "number"}}}), freeform: None, constrained_sampling: None };
        let mut parser = create_antml_stream_parser(vec![count_tool], Some(options_with(false, handler)));
        let call = r#"<invoke name="Count"><parameter name="count">not-a-number</parameter></invoke>"#;
        let events = feed_all(&mut parser, &format!("Before {call} after"));
        assert!(tool_call_ends(&events).is_empty());
        assert_eq!(text_output(&events), "Before  after");
        assert_eq!(seen.lock().expect("errors").as_slice(), ["Could not process streaming antml tool call, keeping original text."]);
    }

    #[test]
    fn flags_an_incomplete_known_invoke_at_finish() {
        let (seen, handler) = error_sink();
        let mut parser = create_antml_stream_parser(vec![bash_tool()], Some(options_with(false, handler)));
        let events = feed_all(&mut parser, r#"<invoke name="Bash"><parameter name="command">ls"#);
        let ends = tool_call_ends(&events);
        assert_eq!(ends.len(), 1);
        let StreamParserEvent::ToolcallEnd { incomplete, error_message, .. } = ends[0] else { unreachable!() };
        assert!(*incomplete);
        assert_eq!(error_message.as_deref(), Some("Tool call was truncated mid-arguments"));
        assert_eq!(seen.lock().expect("errors").as_slice(), ["antml tool call truncated at finish"]);
    }

    fn assert_antml_fixture(input: &str, expect: FixtureExpectation) {
        let (seen, handler) = error_sink();
        let mut parser = create_antml_stream_parser(fixture_tools(), Some(options_with(true, handler)));
        let events = feed_all(&mut parser, input);
        let ends = tool_call_ends(&events);
        match expect {
            FixtureExpectation::Recovered { tool, arguments } => {
                assert_eq!(ends.len(), 1, "{input}");
                let StreamParserEvent::ToolcallEnd { name, arguments: observed, incomplete, .. } = ends[0] else { unreachable!() };
                assert_eq!(name, tool);
                assert_eq!(observed, &arguments);
                assert!(!*incomplete, "{input}");
            }
            FixtureExpectation::Incomplete { tool } => {
                assert_eq!(ends.len(), 1, "{input}");
                let StreamParserEvent::ToolcallEnd { name, incomplete, .. } = ends[0] else { unreachable!() };
                assert_eq!(name, tool);
                assert!(*incomplete);
                assert!(!text_output(&events).contains("<invoke"), "{input}");
                assert!(!seen.lock().expect("errors").iter().any(|message| message.contains(input)), "{input}");
            }
            FixtureExpectation::Dropped => {
                assert!(ends.is_empty(), "{input}");
                assert!(!events.iter().any(is_toolcall_event), "{input}");
                assert!(!text_output(&events).contains(input), "{input}");
                assert_eq!(seen.lock().expect("errors").len(), 1, "{input}");
            }
            FixtureExpectation::Text => {
                assert!(ends.is_empty(), "{input}");
                assert!(text_output(&events).contains(input), "{input}");
            }
        }
    }

    enum FixtureExpectation {
        Recovered { tool: &'static str, arguments: serde_json::Map<String, Value> },
        Incomplete { tool: &'static str },
        Dropped,
        Text,
    }

    #[test]
    fn handles_a_truncation_fixture_with_a_closed_parameter_and_only_the_invoke_close_missing() {
        assert_antml_fixture(
            r#"<invoke name="get_weather"><parameter name="city">Seoul</parameter>"#,
            FixtureExpectation::Recovered { tool: "get_weather", arguments: json!({"city": "Seoul"}).as_object().expect("object").clone() },
        );
    }

    #[test]
    fn handles_a_truncation_fixture_with_a_proper_invoke_close_prefix_after_complete_parameters() {
        assert_antml_fixture(
            r#"<invoke name="get_weather"><parameter name="city">Seoul</parameter></inv"#,
            FixtureExpectation::Recovered { tool: "get_weather", arguments: json!({"city": "Seoul"}).as_object().expect("object").clone() },
        );
    }

    #[test]
    fn handles_a_truncation_fixture_where_a_mid_value_cut_leaves_the_parameter_unclosed() {
        assert_antml_fixture(r#"<invoke name="get_weather"><parameter name="city">Seo"#, FixtureExpectation::Incomplete { tool: "get_weather" });
    }

    #[test]
    fn handles_a_truncation_fixture_where_closed_parameters_violate_todowrite_min_items() {
        assert_antml_fixture(r#"<invoke name="todowrite"><parameter name="todos">[]</parameter>"#, FixtureExpectation::Incomplete { tool: "todowrite" });
    }

    #[test]
    fn handles_a_truncation_fixture_with_a_nameless_invoke_prefix_that_is_dropped() {
        assert_antml_fixture("<invoke na", FixtureExpectation::Dropped);
    }

    #[test]
    fn handles_a_truncation_fixture_where_an_unknown_invoke_remains_ordinary_text() {
        assert_antml_fixture(r#"<invoke name="unknown_tool"><parameter name="city">Seoul</parameter>"#, FixtureExpectation::Text);
    }

}
