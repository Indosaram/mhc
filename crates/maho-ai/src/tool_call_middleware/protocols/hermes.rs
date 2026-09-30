//! Port of senpi packages/ai/src/tool-call-middleware/protocols/hermes.ts.

use serde_json::{Map, Value};

use super::json_mix::{create_json_mix_stream_parser, format_json_mix_tool_call, parse_json_mix_generated_text, JsonMixOptions};
use crate::tool_call_middleware::types::{ParsedToolCall, ParserOptions, StreamParser, ToolResultContent};
use crate::types::Tool;

const TOOL_CALL_START: &str = "<tool_call>";
const TOOL_CALL_END: &str = "</tool_call>";

fn render_tool_definition(tool: &Tool) -> String {
    format!(
        "{{\"type\": \"function\", \"function\": {{\"name\": {}, \"description\": {}, \"parameters\": {}}}}}",
        Value::String(tool.name.clone()),
        Value::String(tool.description.clone()),
        tool.parameters
    )
}

pub fn hermes_format_tools_system_prompt(tools: &[Tool]) -> String {
    if tools.is_empty() {
        return String::new();
    }

    let tools_rendered = tools.iter().map(render_tool_definition).collect::<Vec<_>>().join("\n");

    format!(
        "You are a function calling AI model. You are provided with function signatures within <tools></tools> XML tags. You may call one or more functions to assist with the user query. Don't make assumptions about what values to plug into functions. Here are the available tools: <tools> {tools_rendered} </tools>\nUse the following pydantic model json schema for each tool call you will make: {{\"properties\": {{\"name\": {{\"title\": \"Name\", \"type\": \"string\"}}, \"arguments\": {{\"title\": \"Arguments\", \"type\": \"object\"}}}}, \"required\": [\"name\", \"arguments\"], \"title\": \"FunctionCall\", \"type\": \"object\"}}\nFor each function call return a json object with function name and arguments within <tool_call></tool_call> XML tags as follows:\n<tool_call>\n{{\"name\": \"<function-name>\", \"arguments\": <args-dict>}}\n</tool_call>"
    )
}

pub fn hermes_format_tool_response(tool_name: &str, _tool_call_id: &str, content: &[ToolResultContent]) -> String {
    let text_content = content.iter().filter_map(ToolResultContent::as_text).map(|c| c.text.as_str()).collect::<Vec<_>>().join("\n");
    let mut wrapper = Map::new();
    wrapper.insert("name".to_string(), Value::String(tool_name.to_string()));
    wrapper.insert("content".to_string(), Value::String(text_content));
    format!("<tool_response>{}</tool_response>", Value::Object(wrapper))
}

pub fn hermes_format_tool_call(name: &str, args: &Map<String, Value>) -> String {
    format_json_mix_tool_call(name, args, TOOL_CALL_START, TOOL_CALL_END)
}

pub fn hermes_parse_generated_text(text: &str, tools: &[Tool], options: Option<&ParserOptions>) -> Vec<ParsedToolCall> {
    parse_json_mix_generated_text(text, tools, TOOL_CALL_START, TOOL_CALL_END, options)
}

fn hermes_tool_call_id(index: usize) -> String {
    format!("hermes-tool-{index}")
}

pub fn hermes_create_stream_parser(tools: Vec<Tool>, options: Option<ParserOptions>) -> Box<dyn StreamParser + Send> {
    create_json_mix_stream_parser(tools, JsonMixOptions { tool_call_start: TOOL_CALL_START, tool_call_end: TOOL_CALL_END, create_tool_call_id: hermes_tool_call_id }, options)
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_call_middleware::types::{ParserOptions, StreamParserEvent};
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn tool(name: &str, description: &str, parameters: Value) -> Tool {
        Tool { name: name.into(), description: description.into(), parameters, freeform: None, constrained_sampling: None }
    }

    fn weather_tool() -> Tool {
        tool(
            "get_weather",
            "Get weather for a location",
            json!({"type": "object", "required": ["city"], "properties": {"city": {"type": "string"}, "unit": {"type": "string"}}}),
        )
    }

    fn clock_tool() -> Tool {
        tool("get_time", "Get time for a timezone", json!({"type": "object", "required": ["timezone"], "properties": {"timezone": {"type": "string"}}}))
    }

    fn read_tool() -> Tool {
        tool("read", "Read a file", json!({"type": "object", "required": ["path"], "properties": {"path": {"type": "string"}}}))
    }

    fn fixture_tools() -> Vec<Tool> {
        vec![
            tool(
                "get_weather",
                "Get weather",
                json!({"type": "object", "required": ["city"], "properties": {"city": {"type": "string"}, "days": {"type": "integer"}}}),
            ),
            tool(
                "todowrite",
                "Write todos",
                json!({"type": "object", "required": ["todos"], "properties": {"todos": {"type": "array", "minItems": 1, "items": {"type": "object", "required": ["content", "status", "priority"], "properties": {"content": {"type": "string"}, "status": {"type": "string"}, "priority": {"type": "string"}}}}}}),
            ),
            tool("get_location", "Get location", json!({"type": "object", "properties": {}})),
        ]
    }

    fn error_collector() -> (Arc<Mutex<Vec<String>>>, impl Fn(&str, Option<&HashMap<String, Value>>) + Send + Sync + 'static) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let handler = move |message: &str, _metadata: Option<&HashMap<String, Value>>| {
            sink.lock().expect("error sink").push(message.to_string());
        };
        (seen, handler)
    }

    fn options_with(emit_raw: bool, handler: impl Fn(&str, Option<&HashMap<String, Value>>) + Send + Sync + 'static) -> ParserOptions {
        ParserOptions { emit_raw_tool_call_text_on_error: emit_raw, on_error: Some(Arc::new(handler)) }
    }

    fn text_of(events: &[StreamParserEvent]) -> String {
        events.iter().filter_map(|event| if let StreamParserEvent::Text { text } = event { Some(text.as_str()) } else { None }).collect()
    }

    fn parse(text: &str, tools: &[Tool]) -> Vec<ParsedToolCall> {
        hermes_parse_generated_text(text, tools, None)
    }

    fn feed_all(parser: &mut Box<dyn StreamParser + Send>, input: &str) -> Vec<StreamParserEvent> {
        let mut events = parser.feed(input);
        events.extend(parser.finish());
        events
    }

    #[test]
    fn parses_a_single_tool_call_when_hermes_markup_contains_valid_json() {
        let text = r#"<tool_call>{"name":"get_weather","arguments":{"city":"Seoul"}}</tool_call>"#;
        assert_eq!(parse(text, &[weather_tool()]), vec![ParsedToolCall { name: "get_weather".into(), arguments: json!({"city": "Seoul"}).as_object().expect("object").clone() }]);
    }

    #[test]
    fn parses_multiple_consecutive_tool_calls_when_several_hermes_blocks_are_present() {
        let text = concat!(
            r#"<tool_call>{"name":"get_weather","arguments":{"city":"Seoul",}}</tool_call>"#,
            r#"<tool_call>{"name":"get_time","arguments":{"timezone":"Asia/Seoul"}}</tool_call>"#
        );
        let calls = parse(text, &[weather_tool(), clock_tool()]);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "get_weather");
        assert_eq!(calls[0].arguments.get("city"), Some(&Value::String("Seoul".into())));
        assert_eq!(calls[1].name, "get_time");
        assert_eq!(calls[1].arguments.get("timezone"), Some(&Value::String("Asia/Seoul".into())));
    }

    #[test]
    fn ignores_surrounding_text_when_tool_call_is_embedded_between_text_segments() {
        let text = concat!(
            "Before tool call. ",
            r#"<tool_call>{"name":"get_weather","arguments":{"city":"Busan"}}</tool_call>"#,
            " After tool call."
        );
        let calls = parse(text, &[weather_tool()]);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments.get("city"), Some(&Value::String("Busan".into())));
    }

    #[test]
    fn skips_malformed_json_gracefully_when_a_hermes_block_cannot_be_parsed() {
        let text = r#"<tool_call>{"name":"get_weather","arguments":{"city":"Seoul"</tool_call>"#;
        assert!(parse(text, &[weather_tool()]).is_empty());
    }

    #[test]
    fn reports_malformed_json_parse_failures_through_on_error() {
        let (seen, handler) = error_collector();
        let options = options_with(false, handler);
        let calls = hermes_parse_generated_text("before <tool_call>{invalid}</tool_call> after", &[weather_tool()], Some(&options));
        assert!(calls.is_empty());
        assert!(!seen.lock().expect("errors").is_empty());
    }

    #[test]
    fn recovers_common_qwen_style_malformed_tool_call_json() {
        let text = "<tool_call>\n\"name': \"read\", \"arguments\": {\"path\": \"/tmp/example.txt\"}}\n</tool_call>";
        let calls = parse(text, &[read_tool()]);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read");
        assert_eq!(calls[0].arguments.get("path"), Some(&Value::String("/tmp/example.txt".into())));
    }

    #[test]
    fn streams_text_and_tool_calls_when_text_surrounds_a_valid_tool_call() {
        let mut parser = hermes_create_stream_parser(vec![weather_tool()], None);
        let events = parser.feed(r#"Before <tool_call>{"name":"get_weather","arguments":{"city":"Seoul"}}</tool_call> after"#);
        assert_eq!(
            events,
            vec![
                StreamParserEvent::Text { text: "Before ".into() },
                StreamParserEvent::ToolcallStart { index: 0, name: "get_weather".into(), id: "hermes-tool-0".into() },
                StreamParserEvent::ToolcallDelta { index: 0, arguments_delta: r#"{"city":"Seoul"}"#.into() },
                StreamParserEvent::ToolcallEnd {
                    index: 0,
                    name: "get_weather".into(),
                    id: "hermes-tool-0".into(),
                    arguments: json!({"city": "Seoul"}).as_object().expect("object").clone(),
                    incomplete: false,
                    error_message: None,
                },
                StreamParserEvent::Text { text: " after".into() },
            ]
        );
        assert!(parser.finish().is_empty());
    }

    #[test]
    fn handles_tool_call_start_tag_split_across_streaming_chunk_boundaries() {
        let mut parser = hermes_create_stream_parser(vec![weather_tool()], None);
        assert_eq!(parser.feed("prefix <tool"), vec![StreamParserEvent::Text { text: "prefix ".into() }]);
        assert_eq!(
            parser.feed(r#"_call>{"name":"get_weather","arguments":{"city":"Seoul"}}"#),
            vec![
                StreamParserEvent::ToolcallStart { index: 0, name: "get_weather".into(), id: "hermes-tool-0".into() },
                StreamParserEvent::ToolcallDelta { index: 0, arguments_delta: r#"{"city":"Seoul"}"#.into() },
            ]
        );
        assert_eq!(
            parser.feed("</tool_call> suffix"),
            vec![
                StreamParserEvent::ToolcallEnd {
                    index: 0,
                    name: "get_weather".into(),
                    id: "hermes-tool-0".into(),
                    arguments: json!({"city": "Seoul"}).as_object().expect("object").clone(),
                    incomplete: false,
                    error_message: None,
                },
                StreamParserEvent::Text { text: " suffix".into() },
            ]
        );
        assert!(parser.finish().is_empty());
    }

    #[test]
    fn handles_tool_call_end_tag_split_across_streaming_chunk_boundaries() {
        let mut parser = hermes_create_stream_parser(vec![weather_tool()], None);
        assert_eq!(
            parser.feed(r#"<tool_call>{"name":"get_weather","arguments":{"city":"Seoul"}}</tool"#),
            vec![
                StreamParserEvent::ToolcallStart { index: 0, name: "get_weather".into(), id: "hermes-tool-0".into() },
                StreamParserEvent::ToolcallDelta { index: 0, arguments_delta: r#"{"city":"Seoul"}"#.into() },
            ]
        );
        assert_eq!(
            parser.feed("_call> suffix"),
            vec![
                StreamParserEvent::ToolcallEnd {
                    index: 0,
                    name: "get_weather".into(),
                    id: "hermes-tool-0".into(),
                    arguments: json!({"city": "Seoul"}).as_object().expect("object").clone(),
                    incomplete: false,
                    error_message: None,
                },
                StreamParserEvent::Text { text: " suffix".into() },
            ]
        );
        assert!(parser.finish().is_empty());
    }

    #[test]
    fn emits_malformed_hermes_tool_call_markup_as_text_when_json_is_invalid_and_raw_fallback_is_enabled() {
        let mut parser = hermes_create_stream_parser(vec![weather_tool()], Some(ParserOptions { emit_raw_tool_call_text_on_error: true, on_error: None }));
        let events = feed_all(&mut parser, r#"prefix <tool_call>{"name":"get_weather","arguments":{"city":"Seoul"</tool_call> suffix"#);
        assert_eq!(
            events,
            vec![
                StreamParserEvent::Text { text: "prefix ".into() },
                StreamParserEvent::Text { text: r#"<tool_call>{"name":"get_weather","arguments":{"city":"Seoul"</tool_call>"#.into() },
                StreamParserEvent::Text { text: " suffix".into() },
            ]
        );
    }

    #[test]
    fn suppresses_malformed_hermes_tool_markup_by_default_and_reports_on_error() {
        let (seen, handler) = error_collector();
        let mut parser = hermes_create_stream_parser(vec![weather_tool()], Some(options_with(false, handler)));
        let events = feed_all(&mut parser, r#"prefix <tool_call>{"name":"get_weather","arguments":{"city":"Seoul"</tool_call> suffix"#);
        assert_eq!(
            events,
            vec![StreamParserEvent::Text { text: "prefix ".into() }, StreamParserEvent::Text { text: " suffix".into() }]
        );
        assert!(!seen.lock().expect("errors").is_empty());
    }

    #[test]
    fn emits_malformed_hermes_tool_markup_when_raw_fallback_is_explicitly_enabled() {
        let mut parser = hermes_create_stream_parser(vec![weather_tool()], Some(ParserOptions { emit_raw_tool_call_text_on_error: true, on_error: None }));
        let events = feed_all(&mut parser, r#"prefix <tool_call>{"name":"get_weather","arguments":{"city":"Seoul"</tool_call> suffix"#);
        assert_eq!(
            events,
            vec![
                StreamParserEvent::Text { text: "prefix ".into() },
                StreamParserEvent::Text { text: r#"<tool_call>{"name":"get_weather","arguments":{"city":"Seoul"</tool_call>"#.into() },
                StreamParserEvent::Text { text: " suffix".into() },
            ]
        );
    }

    #[test]
    fn flags_unfinished_hermes_tool_markup_at_finish_by_default() {
        let (seen, handler) = error_collector();
        let mut parser = hermes_create_stream_parser(vec![weather_tool()], Some(options_with(false, handler)));
        assert_eq!(parser.feed(r#"prefix <tool_call>{"name":"get_weather""#), vec![StreamParserEvent::Text { text: "prefix ".into() }]);
        assert_eq!(
            parser.finish(),
            vec![
                StreamParserEvent::ToolcallStart { index: 0, name: "get_weather".into(), id: "hermes-tool-0".into() },
                StreamParserEvent::ToolcallEnd {
                    index: 0,
                    name: "get_weather".into(),
                    id: "hermes-tool-0".into(),
                    arguments: Map::new(),
                    incomplete: true,
                    error_message: Some("Tool call was truncated mid-arguments".into()),
                },
            ]
        );
        assert_eq!(seen.lock().expect("errors").as_slice(), ["Could not complete streaming JSON tool call at finish."]);
    }

    #[test]
    fn never_emits_unfinished_hermes_markup_at_finish_when_raw_fallback_is_enabled() {
        let mut parser = hermes_create_stream_parser(vec![weather_tool()], Some(ParserOptions { emit_raw_tool_call_text_on_error: true, on_error: None }));
        assert_eq!(parser.feed(r#"prefix <tool_call>{"name":"get_weather""#), vec![StreamParserEvent::Text { text: "prefix ".into() }]);
        assert_eq!(
            parser.finish(),
            vec![
                StreamParserEvent::ToolcallStart { index: 0, name: "get_weather".into(), id: "hermes-tool-0".into() },
                StreamParserEvent::ToolcallEnd {
                    index: 0,
                    name: "get_weather".into(),
                    id: "hermes-tool-0".into(),
                    arguments: Map::new(),
                    incomplete: true,
                    error_message: Some("Tool call was truncated mid-arguments".into()),
                },
            ]
        );
    }

    #[test]
    fn recovers_or_flags_a_complete_json_object_with_the_terminator_missing() {
        let (seen, handler) = error_collector();
        let mut parser = hermes_create_stream_parser(fixture_tools(), Some(options_with(true, handler)));
        let events = feed_all(&mut parser, r#"<tool_call>{"name":"get_weather","arguments":{"city":"Seoul"}}"#);
        let ends: Vec<_> = events.iter().filter(|e| matches!(e, StreamParserEvent::ToolcallEnd { .. })).collect();
        assert_eq!(ends.len(), 1);
        let StreamParserEvent::ToolcallEnd { name, arguments, incomplete, .. } = ends[0] else { unreachable!() };
        assert_eq!(name, "get_weather");
        assert_eq!(arguments.get("city"), Some(&Value::String("Seoul".into())));
        assert!(!*incomplete);
        assert!(seen.lock().expect("errors").is_empty());
    }

    #[test]
    fn recovers_or_flags_a_complete_json_object_with_the_terminator_prefix() {
        let (seen, handler) = error_collector();
        let mut parser = hermes_create_stream_parser(fixture_tools(), Some(options_with(true, handler)));
        let events = feed_all(&mut parser, r#"<tool_call>{"name":"get_weather","arguments":{"city":"Seoul"}}</tool_"#);
        let ends: Vec<_> = events.iter().filter(|e| matches!(e, StreamParserEvent::ToolcallEnd { .. })).collect();
        assert_eq!(ends.len(), 1);
        let StreamParserEvent::ToolcallEnd { name, arguments, incomplete, .. } = ends[0] else { unreachable!() };
        assert_eq!(name, "get_weather");
        assert_eq!(arguments.get("city"), Some(&Value::String("Seoul".into())));
        assert!(!*incomplete);
        assert!(seen.lock().expect("errors").is_empty());
    }

    #[test]
    fn recovers_or_flags_a_mid_value_json_cut_as_incomplete() {
        let (seen, handler) = error_collector();
        let mut parser = hermes_create_stream_parser(fixture_tools(), Some(options_with(true, handler)));
        let events = feed_all(&mut parser, r#"<tool_call>{"name":"get_weather","arguments":{"city":"Seo"#);
        let ends: Vec<_> = events.iter().filter(|e| matches!(e, StreamParserEvent::ToolcallEnd { .. })).collect();
        assert_eq!(ends.len(), 1);
        let StreamParserEvent::ToolcallEnd { name, incomplete, .. } = ends[0] else { unreachable!() };
        assert_eq!(name, "get_weather");
        assert!(*incomplete);
        assert!(!text_of(&events).contains("<tool_call>"));
        assert!(!seen.lock().expect("errors").is_empty());
    }

    #[test]
    fn recovers_or_flags_a_complete_json_that_violates_todowrite_min_items() {
        let (seen, handler) = error_collector();
        let mut parser = hermes_create_stream_parser(fixture_tools(), Some(options_with(true, handler)));
        let events = feed_all(&mut parser, r#"<tool_call>{"name":"todowrite","arguments":{"todos":[]}}"#);
        let ends: Vec<_> = events.iter().filter(|e| matches!(e, StreamParserEvent::ToolcallEnd { .. })).collect();
        assert_eq!(ends.len(), 1);
        let StreamParserEvent::ToolcallEnd { name, incomplete, .. } = ends[0] else { unreachable!() };
        assert_eq!(name, "todowrite");
        assert!(*incomplete);
        assert!(!seen.lock().expect("errors").is_empty());
    }

    #[test]
    fn drops_or_flags_a_json_tool_call_fragment_with_no_resolvable_name() {
        let (seen, handler) = error_collector();
        let mut parser = hermes_create_stream_parser(fixture_tools(), Some(options_with(true, handler)));
        let input = r#"<tool_call>{"na"#;
        let events = feed_all(&mut parser, input);
        assert!(!events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallStart { .. } | StreamParserEvent::ToolcallDelta { .. } | StreamParserEvent::ToolcallEnd { .. })));
        assert!(!text_of(&events).contains(input));
        assert!(!seen.lock().expect("errors").is_empty());
    }

    #[test]
    fn keeps_unknown_truncated_names_as_raw_text_only_when_raw_fallback_is_disabled() {
        let (seen, handler) = error_collector();
        let input = r#"<tool_call>{"name":"unknown_tool","arguments":{"city":"Seoul"}}"#;
        let mut parser = hermes_create_stream_parser(fixture_tools(), Some(options_with(false, handler)));
        let events = feed_all(&mut parser, input);
        assert!(!events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallStart { .. } | StreamParserEvent::ToolcallDelta { .. } | StreamParserEvent::ToolcallEnd { .. })));
        assert_eq!(text_of(&events), "");
        assert_eq!(seen.lock().expect("errors").as_slice(), ["Could not complete streaming JSON tool call at finish."]);
    }

    #[test]
    fn keeps_unknown_truncated_names_as_raw_text_only_when_raw_fallback_is_enabled() {
        let (seen, handler) = error_collector();
        let input = r#"<tool_call>{"name":"unknown_tool","arguments":{"city":"Seoul"}}"#;
        let mut parser = hermes_create_stream_parser(fixture_tools(), Some(options_with(true, handler)));
        let events = feed_all(&mut parser, input);
        assert!(!events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallStart { .. } | StreamParserEvent::ToolcallDelta { .. } | StreamParserEvent::ToolcallEnd { .. })));
        assert_eq!(text_of(&events), input);
        assert_eq!(seen.lock().expect("errors").as_slice(), ["Could not complete streaming JSON tool call at finish."]);
    }

    #[test]
    fn terminates_an_already_started_malformed_complete_call_as_incomplete() {
        let mut parser = hermes_create_stream_parser(vec![weather_tool()], None);
        assert_eq!(
            parser.feed(r#"<tool_call>{"name":"get_weather","arguments":{"city":"Seoul"} malformed}</tool_call>"#),
            vec![
                StreamParserEvent::ToolcallStart { index: 0, name: "get_weather".into(), id: "hermes-tool-0".into() },
                StreamParserEvent::ToolcallDelta { index: 0, arguments_delta: r#"{"city":"Seoul"}"#.into() },
                StreamParserEvent::ToolcallEnd {
                    index: 0,
                    name: "get_weather".into(),
                    id: "hermes-tool-0".into(),
                    arguments: json!({"city": "Seoul"}).as_object().expect("object").clone(),
                    incomplete: true,
                    error_message: Some("Tool call arguments could not be parsed".into()),
                },
            ]
        );
    }

    #[test]
    fn streams_recovered_qwen_style_malformed_tool_call_json_as_a_tool_call_instead_of_raw_text() {
        let mut parser = hermes_create_stream_parser(vec![read_tool()], None);
        let events = parser.feed("<tool_call>\n\"name': \"read\", \"arguments\": {\"path\": \"/tmp/example.txt\"}}\n</tool_call>");
        assert_eq!(
            events,
            vec![
                StreamParserEvent::ToolcallStart { index: 0, name: "read".into(), id: "hermes-tool-0".into() },
                StreamParserEvent::ToolcallDelta { index: 0, arguments_delta: r#"{"path":"/tmp/example.txt"}"#.into() },
                StreamParserEvent::ToolcallEnd {
                    index: 0,
                    name: "read".into(),
                    id: "hermes-tool-0".into(),
                    arguments: json!({"path": "/tmp/example.txt"}).as_object().expect("object").clone(),
                    incomplete: false,
                    error_message: None,
                },
            ]
        );
        assert!(parser.finish().is_empty());
    }
}

