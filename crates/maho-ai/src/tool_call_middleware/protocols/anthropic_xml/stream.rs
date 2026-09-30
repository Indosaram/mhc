//! Port of senpi packages/ai/src/tool-call-middleware/protocols/anthropic-xml/stream.ts.

use serde_json::Value;

use super::invoke_match::find_next_invoke_match;
use super::invoke_protocol::{InvokeProtocolConfig, ANTHROPIC_XML_INVOKE_CONFIG};
use super::invoke_stream_helpers::{
    coerce_stream_parameters, emit_text, find_function_calls_close_tag, find_function_calls_open_tag, get_safe_stream_text_length,
    is_whitespace_or_function_calls_close_prefix, overflow_pending_fragment, report_error, report_truncated_invoke,
    should_emit_raw_tool_call_text_on_error,
};
use super::invoke_tag_scanner::{
    find_incomplete_invoke_open_tag, find_invoke_open_tag, is_potential_protocol_start, scan_invoke_block, scan_truncated_invoke_block,
};
use super::parse::parse_invoke_generated_text;
pub use super::stream_boundary::ANTHROPIC_XML_MAX_RETAINED_FRAGMENT_LENGTH;
use super::stream_boundary::{create_pending_fragment, PendingFragment, PendingFragmentKind};
use super::tool_resolver::ToolResolver;
use crate::tool_call_middleware::types::{ParserOptions, StreamParser, StreamParserEvent};
use crate::types::Tool;
use crate::utils::validation::validate_tool_arguments;

struct InvokeStreamParser {
    tools: Vec<Tool>,
    config: &'static InvokeProtocolConfig,
    options: Option<ParserOptions>,
    buffer: String,
    next_tool_call_index: usize,
    pending_fragment: Option<PendingFragment>,
}

impl InvokeStreamParser {
    fn resolver(&self) -> ToolResolver<'_> {
        ToolResolver::new(&self.tools)
    }

    fn overflow_retained_fragment(&mut self) -> Vec<StreamParserEvent> {
        let retained_fragment = std::mem::take(&mut self.buffer);
        self.pending_fragment = None;
        overflow_pending_fragment(self.options.as_ref(), self.config.label, &retained_fragment)
    }

    fn process_buffer(&mut self, consume_function_calls_residue: bool, allow_json_schema_fallback: bool) -> Vec<StreamParserEvent> {
        let mut events = Vec::new();
        self.pending_fragment = None;

        while !self.buffer.is_empty() {
            let resolver = self.resolver();
            let invoke_open_tag = find_invoke_open_tag(&self.buffer, 0);
            let function_calls_open_tag = find_function_calls_open_tag(&self.buffer, 0);

            if let Some(function_calls_open_tag) = &function_calls_open_tag
                && invoke_open_tag.as_ref().is_none_or(|invoke| function_calls_open_tag.index <= invoke.index)
            {
                if function_calls_open_tag.index > 0 {
                    emit_text(&mut events, &self.buffer[..function_calls_open_tag.index]);
                    self.buffer = self.buffer[function_calls_open_tag.index..].to_string();
                    continue;
                }

                let function_calls_close_tag = find_function_calls_close_tag(&self.buffer, function_calls_open_tag.length);
                let Some(function_calls_close_tag) = function_calls_close_tag else {
                    self.pending_fragment = Some(create_pending_fragment(PendingFragmentKind::FunctionCalls, &self.buffer));
                    break;
                };

                let wrapper_end = function_calls_close_tag.index + function_calls_close_tag.length;
                let wrapper_content = self.buffer[function_calls_open_tag.length..function_calls_close_tag.index].to_string();
                if !parse_invoke_generated_text(&wrapper_content, &self.tools, self.config, None).is_empty() {
                    self.buffer = format!("{}{}", wrapper_content, &self.buffer[wrapper_end..]);
                } else {
                    emit_text(&mut events, &self.buffer[..wrapper_end]);
                    self.buffer = self.buffer[wrapper_end..].to_string();
                }
                continue;
            }

            if let Some(invoke_open_tag) = invoke_open_tag {
                if invoke_open_tag.index > 0 {
                    emit_text(&mut events, &self.buffer[..invoke_open_tag.index]);
                    self.buffer = self.buffer[invoke_open_tag.index..].to_string();
                    continue;
                }

                let tool = resolver.resolve(&invoke_open_tag.tool_name).cloned();
                let block = scan_invoke_block(&self.buffer, &invoke_open_tag);
                if tool.is_none() {
                    let next_known_invoke = find_next_invoke_match(&self.buffer, invoke_open_tag.index + invoke_open_tag.length, |name| {
                        resolver.resolve(name).is_some()
                    });
                    if let Some(next_known_invoke) = &next_known_invoke
                        && (block.is_none()
                            || next_known_invoke.block.as_ref().is_some_and(|nb| Some(nb.end) == block.as_ref().map(|b| b.end)))
                    {
                        emit_text(&mut events, &self.buffer[..next_known_invoke.opening_tag.index]);
                        self.buffer = self.buffer[next_known_invoke.opening_tag.index..].to_string();
                        continue;
                    }
                }
                let Some(block) = block else {
                    self.pending_fragment = Some(create_pending_fragment(PendingFragmentKind::Invoke, &self.buffer));
                    break;
                };

                let original_call_text = self.buffer[..block.end].to_string();
                self.buffer = self.buffer[block.end..].to_string();

                let Some(tool) = tool else {
                    emit_text(&mut events, &original_call_text);
                    continue;
                };

                let index = self.next_tool_call_index;
                self.next_tool_call_index += 1;
                let id = format!("{}-{}", self.config.id_prefix, index);
                let arguments_record = block
                    .parameters
                    .as_deref()
                    .and_then(|params| coerce_stream_parameters(params, &tool, self.config, allow_json_schema_fallback));
                let Some(arguments_record) = arguments_record else {
                    report_error(
                        self.options.as_ref(),
                        &format!("Could not process streaming {} tool call, keeping original text.", self.config.label),
                        &original_call_text,
                    );
                    if should_emit_raw_tool_call_text_on_error(self.options.as_ref()) {
                        emit_text(&mut events, &original_call_text);
                    }
                    continue;
                };

                events.push(StreamParserEvent::ToolcallStart { index, name: tool.name.clone(), id: id.clone() });
                events.push(StreamParserEvent::ToolcallDelta {
                    index,
                    arguments_delta: Value::Object(arguments_record.clone()).to_string(),
                });
                events.push(StreamParserEvent::ToolcallEnd {
                    index,
                    name: tool.name.clone(),
                    id,
                    arguments: arguments_record,
                    incomplete: false,
                    error_message: None,
                });
                continue;
            }

            if consume_function_calls_residue && is_whitespace_or_function_calls_close_prefix(&self.buffer) {
                self.buffer.clear();
                break;
            }

            let safe_length = get_safe_stream_text_length(&self.buffer);
            if safe_length == 0 {
                if self.buffer.len() >= 128 {
                    self.pending_fragment = Some(create_pending_fragment(PendingFragmentKind::OpenTag, &self.buffer));
                }
                break;
            }
            emit_text(&mut events, &self.buffer[..safe_length]);
            self.buffer = self.buffer[safe_length..].to_string();
        }

        events
    }
}

impl StreamParser for InvokeStreamParser {
    fn feed(&mut self, text_delta: &str) -> Vec<StreamParserEvent> {
        if text_delta.is_empty() {
            return Vec::new();
        }

        let mut events = Vec::new();
        let mut offset = 0usize;
        while offset < text_delta.len() {
            let capacity = ANTHROPIC_XML_MAX_RETAINED_FRAGMENT_LENGTH.saturating_sub(self.buffer.len());
            if capacity == 0 {
                events.extend(self.overflow_retained_fragment());
                continue;
            }

            let next_offset = (offset + capacity).min(text_delta.len());
            let chunk = &text_delta[offset..next_offset];
            offset = next_offset;
            self.buffer.push_str(chunk);

            if let Some(pending_fragment) = &mut self.pending_fragment {
                if pending_fragment.matcher.feed(chunk) {
                    events.extend(self.process_buffer(false, false));
                }
            } else {
                events.extend(self.process_buffer(false, false));
            }

            if self.pending_fragment.is_some() && self.buffer.len() == ANTHROPIC_XML_MAX_RETAINED_FRAGMENT_LENGTH {
                events.extend(self.overflow_retained_fragment());
            }
        }
        events
    }

    fn finish(&mut self) -> Vec<StreamParserEvent> {
        let mut events = self.process_buffer(false, false);
        let function_calls_open_tag = find_function_calls_open_tag(&self.buffer, 0);
        if let Some(function_calls_open_tag) = &function_calls_open_tag
            && function_calls_open_tag.index == 0
            && find_function_calls_close_tag(&self.buffer, function_calls_open_tag.length).is_none()
        {
            self.buffer = self.buffer[function_calls_open_tag.length..].to_string();
            events.extend(self.process_buffer(true, true));
        }

        if self.buffer.is_empty() {
            return events;
        }

        let invoke_open_tag = find_invoke_open_tag(&self.buffer, 0).or_else(|| find_incomplete_invoke_open_tag(&self.buffer, 0));
        let tool = invoke_open_tag.as_ref().filter(|t| t.index == 0).and_then(|t| self.resolver().resolve(&t.tool_name).cloned());
        if let (Some(invoke_open_tag), Some(tool)) = (&invoke_open_tag, tool) {
            let scanned_block = scan_truncated_invoke_block(&self.buffer, invoke_open_tag);
            let coerced_arguments = coerce_stream_parameters(&scanned_block.parameters, &tool, self.config, true);
            let mut recovered_arguments = None;
            if scanned_block.is_structurally_complete
                && let Some(coerced) = &coerced_arguments
            {
                let tool_call = crate::types::ToolCall {
                    id: format!("{}-finish-recovery", self.config.protocol),
                    name: tool.name.clone(),
                    arguments: coerced.clone(),
                    incomplete: None,
                    error_message: None,
                    thought_signature: None,
                    namespace: None,
                };
                if let Ok(Value::Object(validated)) = validate_tool_arguments(&tool, &tool_call) {
                    recovered_arguments = Some(validated);
                }
            }

            let index = self.next_tool_call_index;
            self.next_tool_call_index += 1;
            let id = format!("{}-{}", self.config.id_prefix, index);
            let arguments_record = recovered_arguments.clone().or_else(|| coerced_arguments.clone()).unwrap_or_default();
            events.push(StreamParserEvent::ToolcallStart { index, name: tool.name.clone(), id: id.clone() });
            events.push(StreamParserEvent::ToolcallDelta {
                index,
                arguments_delta: Value::Object(arguments_record.clone()).to_string(),
            });
            if recovered_arguments.is_some() {
                events.push(StreamParserEvent::ToolcallEnd {
                    index,
                    name: tool.name.clone(),
                    id,
                    arguments: arguments_record,
                    incomplete: false,
                    error_message: None,
                });
            } else {
                events.push(StreamParserEvent::ToolcallEnd {
                    index,
                    name: tool.name.clone(),
                    id,
                    arguments: arguments_record,
                    incomplete: true,
                    error_message: Some("Tool call was truncated mid-arguments".to_string()),
                });
                report_truncated_invoke(self.options.as_ref(), self.config, self.buffer.len());
            }
        } else if invoke_open_tag.as_ref().is_some_and(|t| t.index == 0) {
            emit_text(&mut events, &self.buffer);
        } else if is_potential_protocol_start(&self.buffer) {
            report_truncated_invoke(self.options.as_ref(), self.config, self.buffer.len());
        } else {
            emit_text(&mut events, &self.buffer);
        }
        self.buffer.clear();
        events
    }
}

pub fn create_invoke_stream_parser(
    tools: Vec<Tool>,
    config: &'static InvokeProtocolConfig,
    options: Option<ParserOptions>,
) -> Box<dyn StreamParser + Send> {
    Box::new(InvokeStreamParser { tools, config, options, buffer: String::new(), next_tool_call_index: 0, pending_fragment: None })
}

pub fn create_anthropic_xml_stream_parser(tools: Vec<Tool>, options: Option<ParserOptions>) -> Box<dyn StreamParser + Send> {
    create_invoke_stream_parser(tools, &ANTHROPIC_XML_INVOKE_CONFIG, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_call_middleware::types::ParserOptions;
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
    fn feeds_plain_text_as_text_events() {
        let tools = vec![tool("t")];
        let mut parser = create_anthropic_xml_stream_parser(tools, None);
        let events = parser.feed("hello world");
        assert_eq!(events, vec![StreamParserEvent::Text { text: "hello world".into() }]);
        assert!(parser.finish().is_empty());
    }

    #[test]
    fn streams_a_complete_invoke_across_feeds() {
        let tools = vec![tool("get_weather")];
        let mut parser = create_anthropic_xml_stream_parser(tools, None);
        let mut events = parser.feed(r#"<invoke name="get_weather"><parameter name="city">Seoul</parameter>"#);
        events.extend(parser.feed("</invoke>"));
        assert!(matches!(events[0], StreamParserEvent::ToolcallStart { name: ref n, .. } if n == "get_weather"));
        assert!(matches!(&events[1], StreamParserEvent::ToolcallDelta { arguments_delta, .. } if arguments_delta.contains("Seoul")));
        assert!(matches!(&events[2], StreamParserEvent::ToolcallEnd { incomplete: false, .. }));
        assert!(parser.finish().is_empty());
    }

    #[test]
    fn finish_recovers_a_truncated_invoke_with_complete_parameters() {
        let tools = vec![tool("get_weather")];
        let mut parser = create_anthropic_xml_stream_parser(tools, None);
        parser.feed(r#"<invoke name="get_weather"><parameter name="city">Seoul</parameter>"#);
        let events = parser.finish();
        assert!(matches!(events[0], StreamParserEvent::ToolcallStart { .. }));
        assert!(matches!(&events[2], StreamParserEvent::ToolcallEnd { incomplete: false, .. }));
    }

    #[test]
    fn finish_marks_incomplete_when_parameters_are_unterminated() {
        let tools = vec![tool("get_weather")];
        let mut parser = create_anthropic_xml_stream_parser(tools, None);
        parser.feed(r#"<invoke name="get_weather"><parameter name="city">Seo"#);
        let events = parser.finish();
        assert!(matches!(&events[2], StreamParserEvent::ToolcallEnd { incomplete: true, error_message: Some(m), .. } if m == "Tool call was truncated mid-arguments"));
    }

    #[test]
    fn a_non_protocol_partial_tag_is_emitted_as_text_and_finish_is_empty() {
        // senpi: anthropic-xml-stream.test.ts "withholds a split invoke start tag from plain text";
        // "<div" is not a protocol start, so feed emits it immediately and finish has nothing left.
        let tools = vec![tool("get_weather")];
        let mut parser = create_anthropic_xml_stream_parser(tools, None);
        let events = parser.feed("<div");
        assert_eq!(events, vec![StreamParserEvent::Text { text: "<div".into() }]);
        assert!(parser.finish().is_empty());
    }

    #[test]
    fn unknown_invoke_falls_through_to_next_known_invoke() {
        let tools = vec![tool("get_weather")];
        let mut parser = create_anthropic_xml_stream_parser(tools, None);
        let events = parser.feed(
            r#"<invoke name="unknown"><parameter name="a">1</parameter></invoke><invoke name="get_weather"><parameter name="city">Busan</parameter></invoke>"#,
        );
        assert!(events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallStart { name, .. } if name == "get_weather")));
        assert!(parser.finish().is_empty());
    }

    #[test]
    fn function_calls_wrapper_around_a_known_invoke_is_unwrapped() {
        let tools = vec![tool("get_weather")];
        let mut parser = create_anthropic_xml_stream_parser(tools, None);
        let events =
            parser.feed(r#"<function_calls><invoke name="get_weather"><parameter name="city">Seoul</parameter></invoke></function_calls>"#);
        assert!(events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallStart { .. })));
        assert!(parser.finish().is_empty());
    }
    fn bash_tool() -> Tool {
        Tool { name: "Bash".into(), description: "Run a shell command".into(), parameters: json!({"type": "object", "required": ["command"], "properties": {"command": {"type": "string"}}}), freeform: None, constrained_sampling: None }
    }

    fn counter_tool() -> Tool {
        Tool { name: "Count".into(), description: "Count a value".into(), parameters: json!({"type": "object", "required": ["count"], "properties": {"count": {"type": "number"}}}), freeform: None, constrained_sampling: None }
    }

    fn noop_tool() -> Tool {
        Tool { name: "noop".into(), description: "Do nothing".into(), parameters: json!({"type": "object", "properties": {}}), freeform: None, constrained_sampling: None }
    }

    fn fixture_tools() -> Vec<Tool> {
        vec![
            Tool { name: "get_weather".into(), description: "Get weather".into(), parameters: json!({"type": "object", "required": ["city"], "properties": {"city": {"type": "string"}, "days": {"type": "integer"}}}), freeform: None, constrained_sampling: None },
            Tool { name: "todowrite".into(), description: "Write todos".into(), parameters: json!({"type": "object", "required": ["todos"], "properties": {"todos": {"type": "array", "minItems": 1, "items": {"type": "object", "required": ["content", "status", "priority"], "properties": {"content": {"type": "string"}, "status": {"type": "string"}, "priority": {"type": "string"}}}}}}), freeform: None, constrained_sampling: None },
            Tool { name: "get_location".into(), description: "Get location".into(), parameters: json!({"type": "object", "properties": {}}), freeform: None, constrained_sampling: None },
        ]
    }

    fn error_sink() -> (std::sync::Arc<std::sync::Mutex<Vec<String>>>, crate::tool_call_middleware::types::ParserErrorHandler) {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let handler: crate::tool_call_middleware::types::ParserErrorHandler = std::sync::Arc::new(move |message: &str, _metadata: Option<&std::collections::HashMap<String, Value>>| {
            sink.lock().expect("error sink").push(message.to_string());
        });
        (seen, handler)
    }

    fn options_with(emit_raw: bool, handler: crate::tool_call_middleware::types::ParserErrorHandler) -> ParserOptions {
        ParserOptions { emit_raw_tool_call_text_on_error: emit_raw, on_error: Some(handler) }
    }

    fn text_output(events: &[StreamParserEvent]) -> String {
        events.iter().filter_map(|event| if let StreamParserEvent::Text { text } = event { Some(text.as_str()) } else { None }).collect()
    }

    fn tool_call_ends(events: &[StreamParserEvent]) -> Vec<&StreamParserEvent> {
        events.iter().filter(|event| matches!(event, StreamParserEvent::ToolcallEnd { .. })).collect()
    }

    fn feed_all(parser: &mut Box<dyn StreamParser + Send>, input: &str) -> Vec<StreamParserEvent> {
        let mut events = parser.feed(input);
        events.extend(parser.finish());
        events
    }

    fn command_of(event: &StreamParserEvent) -> Option<&str> {
        let StreamParserEvent::ToolcallEnd { arguments, .. } = event else { return None };
        arguments.get("command").and_then(Value::as_str)
    }

    // Mirrors the TS randomChunkSplit helper (sizes 1..=8).
    fn random_chunk_split(text: &str, seed: u64) -> Vec<String> {
        let mut current = seed;
        let chars: Vec<char> = text.chars().collect();
        let mut chunks = Vec::new();
        let mut index = 0usize;
        while index < chars.len() {
            current = (current * 9301 + 49_297) % 233_280;
            let size = ((current as f64 / 233_280.0) * 8.0).floor() as usize + 1;
            let end = (index + size).min(chars.len());
            chunks.push(chars[index..end].iter().collect());
            index = end;
        }
        chunks
    }

    fn collect_bash_events(input: &str, seed: u64, options: Option<ParserOptions>) -> Vec<StreamParserEvent> {
        let mut parser = create_anthropic_xml_stream_parser(vec![bash_tool()], options);
        let mut events = Vec::new();
        for chunk in random_chunk_split(input, seed) {
            events.extend(parser.feed(&chunk));
        }
        events.extend(parser.finish());
        events
    }

    #[test]
    fn emits_a_complete_invoke_as_one_tool_call() {
        let mut parser = create_anthropic_xml_stream_parser(vec![bash_tool()], None);
        assert_eq!(
            feed_all(&mut parser, r#"<invoke name="Bash"><parameter name="command">echo hi</parameter></invoke>"#),
            vec![
                StreamParserEvent::ToolcallStart { index: 0, name: "Bash".into(), id: "anthropic-xml-tool-0".into() },
                StreamParserEvent::ToolcallDelta { index: 0, arguments_delta: r#"{"command":"echo hi"}"#.into() },
                StreamParserEvent::ToolcallEnd {
                    index: 0,
                    name: "Bash".into(),
                    id: "anthropic-xml-tool-0".into(),
                    arguments: json!({"command": "echo hi"}).as_object().expect("object").clone(),
                    incomplete: false,
                    error_message: None,
                },
            ]
        );
    }

    #[test]
    fn keeps_arbitrary_chunk_splits_stable() {
        let input = r#"Before <invoke name="Bash"><parameter name="command">printf '<ok>'
next</parameter></invoke> after"#;
        for seed in [0u64, 1, 7, 13, 21, 42] {
            let events = collect_bash_events(input, seed, None);
            let ends = tool_call_ends(&events);
            assert_eq!(ends.len(), 1, "seed {seed}");
            let StreamParserEvent::ToolcallEnd { index, name, id, arguments, .. } = ends[0] else { unreachable!() };
            assert_eq!((*index, name.as_str(), id.as_str()), (0, "Bash", "anthropic-xml-tool-0"), "seed {seed}");
            assert_eq!(arguments.get("command"), Some(&json!("printf '<ok>'\nnext")), "seed {seed}");
            assert_eq!(events.iter().filter(|e| matches!(e, StreamParserEvent::ToolcallStart { .. })).count(), 1, "seed {seed}");
            let text = text_output(&events);
            assert_eq!(text, "Before  after", "seed {seed}");
            assert!(!text.contains("<invoke"), "seed {seed}");
        }
    }

    #[test]
    fn withholds_a_split_invoke_start_tag_from_plain_text() {
        let mut parser = create_anthropic_xml_stream_parser(vec![bash_tool()], None);
        let first = parser.feed("prefix <inv");
        assert_eq!(first, vec![StreamParserEvent::Text { text: "prefix ".into() }]);
        assert!(!first.iter().any(|e| matches!(e, StreamParserEvent::ToolcallEnd { .. })));
        let mut remaining = parser.feed("oke name=\"Bash\"><parameter name=\"command\">ls</parameter></invoke> suffix");
        remaining.extend(parser.finish());
        assert_eq!(tool_call_ends(&remaining).len(), 1);
        let combined: Vec<StreamParserEvent> = first.into_iter().chain(remaining).collect();
        assert_eq!(text_output(&combined), "prefix  suffix");
    }

    #[test]
    fn drops_the_optional_function_calls_wrapper_while_preserving_prose() {
        let input = r#"Before <function_calls><invoke name="Bash"><parameter name="command">ls</parameter></invoke></function_calls> after"#;
        let events = collect_bash_events(input, 13, None);
        assert_eq!(tool_call_ends(&events).len(), 1);
        let text = text_output(&events);
        assert_eq!(text, "Before  after");
        assert!(!text.contains("function_calls"));
    }

    #[test]
    fn assigns_deterministic_ids_to_consecutive_known_invokes() {
        let mut parser = create_anthropic_xml_stream_parser(vec![bash_tool()], None);
        let input = r#"<invoke name="Bash"><parameter name="command">one</parameter></invoke><invoke name="Bash"><parameter name="command">two</parameter></invoke>"#;
        let events = feed_all(&mut parser, input);
        let observed: Vec<(usize, String, String)> = tool_call_ends(&events)
            .into_iter()
            .map(|event| {
                let StreamParserEvent::ToolcallEnd { index, id, arguments, .. } = event else { unreachable!() };
                (*index, id.clone(), arguments.get("command").and_then(Value::as_str).unwrap_or_default().to_string())
            })
            .collect();
        assert_eq!(
            observed,
            vec![
                (0, "anthropic-xml-tool-0".to_string(), "one".to_string()),
                (1, "anthropic-xml-tool-1".to_string(), "two".to_string()),
            ]
        );
    }

    #[test]
    fn preserves_prompt_like_invoke_markup_inside_a_parameter_value() {
        let command = "Show <invoke name=\"Other\"><parameter name=\"example\">raw</parameter></invoke> exactly as text";
        let input = format!("<invoke name=\"Bash\"><parameter name=\"command\">{command}</parameter></invoke>");
        let events = collect_bash_events(&input, 21, None);
        let ends = tool_call_ends(&events);
        assert_eq!(ends.len(), 1);
        let StreamParserEvent::ToolcallEnd { arguments, .. } = ends[0] else { unreachable!() };
        assert_eq!(arguments.get("command"), Some(&json!(command)));
        assert_eq!(text_output(&events), "");
    }

    #[test]
    fn keeps_a_complete_unknown_invoke_as_text_without_reporting_an_error() {
        let (seen, handler) = error_sink();
        let mut parser = create_anthropic_xml_stream_parser(vec![bash_tool()], Some(options_with(false, handler)));
        let unknown_call = r#"<invoke name="Other"><parameter name="command">echo no</parameter></invoke>"#;
        let events = feed_all(&mut parser, &format!("Before {unknown_call} after"));
        assert!(tool_call_ends(&events).is_empty());
        assert_eq!(text_output(&events), format!("Before {unknown_call} after"));
        assert!(seen.lock().expect("errors").is_empty());
    }

    #[test]
    fn suppresses_a_malformed_known_invoke_and_reports_the_original_text() {
        let (seen, handler) = error_sink();
        let mut parser = create_anthropic_xml_stream_parser(vec![counter_tool()], Some(options_with(false, handler)));
        let call = r#"<invoke name="Count"><parameter name="count">not-a-number</parameter></invoke>"#;
        let events = feed_all(&mut parser, &format!("Before {call} after"));
        assert_eq!(events, vec![StreamParserEvent::Text { text: "Before ".into() }, StreamParserEvent::Text { text: " after".into() }]);
        assert_eq!(seen.lock().expect("errors").as_slice(), ["Could not process streaming Anthropic XML tool call, keeping original text."]);
    }

    #[test]
    fn emits_a_malformed_complete_invoke_as_raw_text_when_enabled() {
        let (seen, handler) = error_sink();
        let mut parser = create_anthropic_xml_stream_parser(vec![counter_tool()], Some(options_with(true, handler)));
        let call = r#"<invoke name="Count"><parameter name="count">not-a-number</parameter></invoke>"#;
        let events = feed_all(&mut parser, &format!("Before {call} after"));
        assert_eq!(text_output(&events), format!("Before {call} after"));
        assert!(tool_call_ends(&events).is_empty());
        assert_eq!(seen.lock().expect("errors").len(), 1);
    }

    #[test]
    fn rejects_malformed_parameter_markup_for_a_no_parameter_tool() {
        let (seen, handler) = error_sink();
        let mut parser = create_anthropic_xml_stream_parser(vec![noop_tool()], Some(options_with(false, handler)));
        let call = r#"<invoke name="noop"><parameter name="ignored">unterminated</invoke>"#;
        let events = feed_all(&mut parser, &format!("Before {call} after"));
        assert!(tool_call_ends(&events).is_empty());
        assert_eq!(text_output(&events), "Before  after");
        assert_eq!(seen.lock().expect("errors").as_slice(), ["Could not process streaming Anthropic XML tool call, keeping original text."]);
    }

    #[test]
    fn flags_an_incomplete_known_invoke_at_finish_without_raw_text() {
        let (seen, handler) = error_sink();
        let mut parser = create_anthropic_xml_stream_parser(vec![bash_tool()], Some(options_with(true, handler)));
        let incomplete_call = r#"<invoke name="Bash"><parameter name="command">ls"#;
        let events = feed_all(&mut parser, &format!("Before {incomplete_call}"));
        assert_eq!(text_output(&events), "Before ");
        let ends = tool_call_ends(&events);
        assert_eq!(ends.len(), 1);
        let StreamParserEvent::ToolcallEnd { incomplete, arguments, error_message, .. } = ends[0] else { unreachable!() };
        assert!(*incomplete);
        assert!(arguments.is_empty());
        assert_eq!(error_message.as_deref(), Some("Tool call was truncated mid-arguments"));
        assert!(!seen.lock().expect("errors").iter().any(|message| message.contains(incomplete_call)));
    }

    #[test]
    fn flags_a_recognized_opening_tag_truncated_before_its_closing_quote_without_raw_text() {
        let (seen, handler) = error_sink();
        let mut parser = create_anthropic_xml_stream_parser(vec![bash_tool()], Some(options_with(true, handler)));
        let incomplete_call = r#"<invoke name="Bash""#;
        let events = feed_all(&mut parser, &format!("Before {incomplete_call}"));
        assert_eq!(text_output(&events), "Before ");
        let ends = tool_call_ends(&events);
        assert_eq!(ends.len(), 1);
        let StreamParserEvent::ToolcallEnd { incomplete, arguments, error_message, .. } = ends[0] else { unreachable!() };
        assert!(*incomplete);
        assert!(arguments.is_empty());
        assert_eq!(error_message.as_deref(), Some("Tool call was truncated mid-arguments"));
        assert!(!seen.lock().expect("errors").iter().any(|message| message.contains(incomplete_call)));
    }

    fn assert_anthropic_fixture(input: &str, expect: FixtureExpectation) {
        let (seen, handler) = error_sink();
        let mut parser = create_anthropic_xml_stream_parser(fixture_tools(), Some(options_with(true, handler)));
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
                assert!(!events.iter().any(|event| matches!(event, StreamParserEvent::ToolcallStart { .. } | StreamParserEvent::ToolcallDelta { .. })), "{input}");
                assert!(!text_output(&events).contains(input), "{input}");
                assert_eq!(seen.lock().expect("errors").len(), 1, "{input}");
                assert!(!seen.lock().expect("errors").iter().any(|message| message.contains(input)), "{input}");
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
        assert_anthropic_fixture(
            r#"<invoke name="get_weather"><parameter name="city">Seoul</parameter>"#,
            FixtureExpectation::Recovered { tool: "get_weather", arguments: json!({"city": "Seoul"}).as_object().expect("object").clone() },
        );
    }

    #[test]
    fn handles_a_truncation_fixture_with_a_proper_invoke_close_prefix_after_complete_parameters() {
        assert_anthropic_fixture(
            r#"<invoke name="get_weather"><parameter name="city">Seoul</parameter></inv"#,
            FixtureExpectation::Recovered { tool: "get_weather", arguments: json!({"city": "Seoul"}).as_object().expect("object").clone() },
        );
    }

    #[test]
    fn handles_a_truncation_fixture_where_a_mid_value_cut_leaves_the_parameter_unclosed() {
        assert_anthropic_fixture(r#"<invoke name="get_weather"><parameter name="city">Seo"#, FixtureExpectation::Incomplete { tool: "get_weather" });
    }

    #[test]
    fn handles_a_truncation_fixture_where_closed_parameters_violate_todowrite_min_items() {
        assert_anthropic_fixture(r#"<invoke name="todowrite"><parameter name="todos">[]</parameter>"#, FixtureExpectation::Incomplete { tool: "todowrite" });
    }

    #[test]
    fn handles_a_truncation_fixture_with_a_nameless_invoke_prefix_that_is_dropped() {
        assert_anthropic_fixture("<invoke na", FixtureExpectation::Dropped);
    }

    #[test]
    fn handles_a_truncation_fixture_where_an_unknown_invoke_remains_ordinary_text() {
        assert_anthropic_fixture(r#"<invoke name="unknown_tool"><parameter name="city">Seoul</parameter>"#, FixtureExpectation::Text);
    }

    #[test]
    fn recovers_a_complete_invoke_in_a_truncated_function_calls_wrapper_without_emitting_wrapper_markup() {
        let mut parser = create_anthropic_xml_stream_parser(fixture_tools(), None);
        let events = feed_all(&mut parser, r#"<function_calls><invoke name="get_weather"><parameter name="city">Seoul</parameter></invoke>"#);
        let ends = tool_call_ends(&events);
        assert_eq!(ends.len(), 1);
        let StreamParserEvent::ToolcallEnd { name, arguments, incomplete, .. } = ends[0] else { unreachable!() };
        assert_eq!(name, "get_weather");
        assert_eq!(arguments.get("city"), Some(&json!("Seoul")));
        assert!(!*incomplete);
        let text = text_output(&events);
        assert!(!text.contains("<function_calls>"));
        assert!(!text.contains("<invoke"));
    }

    #[test]
    fn recovers_and_flags_invokes_in_a_truncated_function_calls_wrapper_in_source_order() {
        let mut parser = create_anthropic_xml_stream_parser(fixture_tools(), None);
        let events = feed_all(
            &mut parser,
            concat!(
                r#"<function_calls><invoke name="get_weather"><parameter name="city">Seoul</parameter></invoke>"#,
                r#"<invoke name="get_weather"><parameter name="city">Seo"#
            ),
        );
        let ends = tool_call_ends(&events);
        assert_eq!(ends.len(), 2);
        let StreamParserEvent::ToolcallEnd { name: first_name, arguments: first_arguments, incomplete: first_incomplete, .. } = ends[0] else { unreachable!() };
        assert_eq!(first_name, "get_weather");
        assert_eq!(first_arguments.get("city"), Some(&json!("Seoul")));
        assert!(!*first_incomplete);
        let StreamParserEvent::ToolcallEnd { name: second_name, incomplete: second_incomplete, .. } = ends[1] else { unreachable!() };
        assert_eq!(second_name, "get_weather");
        assert!(*second_incomplete);
        let text = text_output(&events);
        assert!(!text.contains("<function_calls>"));
        assert!(!text.contains("<invoke"));
    }

    fn canonical_bash_tool() -> Tool {
        Tool { name: "bash".into(), description: "Run a shell command".into(), parameters: json!({"type": "object", "required": ["command"], "properties": {"command": {"type": "string"}}}), freeform: None, constrained_sampling: None }
    }

    #[test]
    fn edge_round_trips_encoded_delimiters_and_boundary_newlines_from_the_formatter() {
        let command = "\nprintf '</parameter> </invoke> & <tag>'\n";
        let mut args = serde_json::Map::new();
        args.insert("command".to_string(), json!(command));
        let input = super::super::format::anthropic_xml_format_tool_call("Bash", &args);
        let mut parser = create_anthropic_xml_stream_parser(vec![bash_tool()], None);
        let events = feed_all(&mut parser, &input);
        let ends = tool_call_ends(&events);
        assert_eq!(ends.len(), 1);
        assert_eq!(command_of(ends[0]), Some(command));
        assert_eq!(text_output(&events), "");
    }

    #[test]
    fn edge_canonicalizes_a_unique_case_insensitive_tool_name_match_to_the_declared_stream_tool_name() {
        let mut parser = create_anthropic_xml_stream_parser(vec![canonical_bash_tool()], None);
        assert_eq!(
            feed_all(&mut parser, r#"<invoke name="Bash"><parameter name="command">echo ulwqa</parameter></invoke>"#),
            vec![
                StreamParserEvent::ToolcallStart { index: 0, name: "bash".into(), id: "anthropic-xml-tool-0".into() },
                StreamParserEvent::ToolcallDelta { index: 0, arguments_delta: r#"{"command":"echo ulwqa"}"#.into() },
                StreamParserEvent::ToolcallEnd {
                    index: 0,
                    name: "bash".into(),
                    id: "anthropic-xml-tool-0".into(),
                    arguments: json!({"command": "echo ulwqa"}).as_object().expect("object").clone(),
                    incomplete: false,
                    error_message: None,
                },
            ]
        );
    }

    #[test]
    fn edge_preserves_an_unbalanced_nested_invoke_opening_tag_inside_a_parameter_value() {
        let command = "prefix <invoke name=\"Other\"> literally";
        let input = format!("<invoke name=\"Bash\"><parameter name=\"command\">{command}</parameter></invoke>");
        let mut parser = create_anthropic_xml_stream_parser(vec![bash_tool()], None);
        let events = feed_all(&mut parser, &input);
        let ends = tool_call_ends(&events);
        assert_eq!(ends.len(), 1);
        assert_eq!(command_of(ends[0]), Some(command));
        assert_eq!(text_output(&events), "");
    }

    #[test]
    fn edge_resynchronizes_after_an_unclosed_unknown_invoke_and_streams_a_later_known_invoke() {
        let unknown_prefix = "<invoke name=\"Unknown\"><parameter name=\"value\">unterminated\n";
        let known_call = r#"<invoke name="Bash"><parameter name="command">echo recovered</parameter></invoke>"#;
        let mut parser = create_anthropic_xml_stream_parser(vec![bash_tool()], None);
        let mut events = parser.feed(unknown_prefix);
        events.extend(parser.feed(known_call));
        events.extend(parser.finish());
        assert_eq!(text_output(&events), unknown_prefix);
        let ends = tool_call_ends(&events);
        assert_eq!(ends.len(), 1);
        assert_eq!(command_of(ends[0]), Some("echo recovered"));
    }

    #[test]
    fn edge_does_not_resynchronize_away_from_an_incomplete_known_invoke_in_the_stream() {
        let (seen, handler) = error_sink();
        let input = concat!(
            "<invoke name=\"Bash\"><parameter name=\"command\">unterminated\n",
            "<invoke name=\"Bash\"><parameter name=\"command\">echo hidden</parameter></invoke>"
        );
        let mut parser = create_anthropic_xml_stream_parser(vec![bash_tool()], Some(options_with(false, handler)));
        let events = feed_all(&mut parser, input);
        let ends = tool_call_ends(&events);
        assert_eq!(ends.len(), 1);
        assert!(matches!(ends[0], StreamParserEvent::ToolcallEnd { incomplete: true, .. }));
        assert!(!text_output(&events).contains("<invoke"));
        assert!(!seen.lock().expect("errors").iter().any(|message| message.contains(input)));
    }

    #[test]
    fn edge_does_not_stream_a_known_no_parameter_invoke_closed_by_a_later_nested_call() {
        let (seen, handler) = error_sink();
        let input = "<invoke name=\"noop\">unterminated\n<invoke name=\"noop\"></invoke>";
        let mut parser = create_anthropic_xml_stream_parser(vec![noop_tool()], Some(options_with(false, handler)));
        let events = feed_all(&mut parser, input);
        let ends = tool_call_ends(&events);
        assert_eq!(ends.len(), 1);
        assert!(matches!(ends[0], StreamParserEvent::ToolcallEnd { name, incomplete: true, .. } if name == "noop"));
        assert!(!text_output(&events).contains("<invoke"));
        assert!(!seen.lock().expect("errors").iter().any(|message| message.contains(input)));
    }

    #[test]
    fn edge_preserves_literal_function_calls_prose_when_it_does_not_wrap_a_recognized_invoke() {
        let input = "Before <function_calls>literal prose</function_calls> after";
        let mut parser = create_anthropic_xml_stream_parser(vec![bash_tool()], None);
        let events = feed_all(&mut parser, input);
        assert_eq!(text_output(&events), input);
        assert!(tool_call_ends(&events).is_empty());
    }

    #[test]
    fn edge_preserves_a_function_calls_wrapper_when_its_recognized_invoke_is_incomplete() {
        let (seen, handler) = error_sink();
        let input = "Before <function_calls><invoke name=\"Bash\">literal</function_calls> after";
        let mut parser = create_anthropic_xml_stream_parser(vec![bash_tool()], Some(options_with(false, handler)));
        let events = feed_all(&mut parser, input);
        assert_eq!(text_output(&events), input);
        assert!(tool_call_ends(&events).is_empty());
        assert!(seen.lock().expect("errors").is_empty());
    }

    #[test]
    fn edge_flags_a_recognized_opening_tag_truncated_before_the_closing_quote_without_raw_text() {
        let (seen, handler) = error_sink();
        let mut parser = create_anthropic_xml_stream_parser(vec![bash_tool()], Some(options_with(true, handler)));
        let incomplete_call = r#"<invoke name="Bash"#;
        let events = feed_all(&mut parser, &format!("Before {incomplete_call}"));
        assert_eq!(text_output(&events), "Before ");
        let ends = tool_call_ends(&events);
        assert_eq!(ends.len(), 1);
        assert!(matches!(ends[0], StreamParserEvent::ToolcallEnd { incomplete: true, arguments, .. } if arguments.is_empty()));
        assert!(!seen.lock().expect("errors").iter().any(|message| message.contains(incomplete_call)));
    }

    #[test]
    fn edge_consumes_a_truncated_function_calls_close_tag_prefix_after_a_complete_invoke() {
        let mut parser = create_anthropic_xml_stream_parser(vec![bash_tool()], None);
        let events = feed_all(
            &mut parser,
            r#"<function_calls><invoke name="Bash"><parameter name="command">ls</parameter></invoke></function_"#,
        );
        let ends = tool_call_ends(&events);
        assert_eq!(ends.len(), 1);
        assert_eq!(command_of(ends[0]), Some("ls"));
        assert!(matches!(ends[0], StreamParserEvent::ToolcallEnd { incomplete: false, .. }));
        assert_eq!(text_output(&events), "");
    }

    // senpi: anthropic-xml-stream-resource.test.ts "keeps nested candidate-close processing linear in one-character streams".
    // The TS case also counts calls into a mocked scanInvokeBlock; Rust has no equivalent module mock, so the
    // observable outcome (a recovered call after a one-character feed of a boundary-heavy input) is asserted here.
    #[test]
    fn resource_keeps_nested_candidate_close_processing_linear_in_one_character_streams() {
        let prefix = r#"<invoke name="Bash"><parameter name="command">"#;
        let suffix = "</parameter></invoke>";
        let target_length = 24 * 1024;
        let command_length = target_length - prefix.len() - suffix.len();
        let nested_pair_length = prefix.len() + suffix.len();
        let candidate_count = command_length / nested_pair_length;
        let command = format!(
            "{}{}{}",
            prefix.repeat(candidate_count),
            "x".repeat(command_length - candidate_count * nested_pair_length),
            suffix.repeat(candidate_count)
        );
        let input = format!("{prefix}{command}{suffix}");
        let mut parser = create_anthropic_xml_stream_parser(vec![bash_tool()], None);
        let mut events = Vec::new();
        for character in input.chars() {
            events.extend(parser.feed(&character.to_string()));
        }
        events.extend(parser.finish());
        let ends = tool_call_ends(&events);
        assert_eq!(ends.len(), 1);
        assert_eq!(command_of(ends[0]), Some(command.as_str()));
    }

    fn assert_overflow_bounds(prefix: &str, emit_raw: bool) {
        let (seen, handler) = error_sink();
        let mut parser = create_anthropic_xml_stream_parser(vec![bash_tool()], Some(options_with(emit_raw, handler)));
        let input = format!("{prefix}{}", "x".repeat(ANTHROPIC_XML_MAX_RETAINED_FRAGMENT_LENGTH - prefix.len()));
        let mut overflow_events = Vec::new();
        for character in input.chars() {
            overflow_events.extend(parser.feed(&character.to_string()));
        }
        let mut recovery_events = parser.feed(r#"<invoke name="Bash"><parameter name="command">echo recovered</parameter></invoke>"#);
        recovery_events.extend(parser.finish());

        assert_eq!(seen.lock().expect("errors").len(), 1, "{prefix} raw={emit_raw}");
        assert_eq!(
            seen.lock().expect("errors").as_slice(),
            [format!("Anthropic XML streaming fragment exceeded the {ANTHROPIC_XML_MAX_RETAINED_FRAGMENT_LENGTH}-character retained-input limit.")],
            "{prefix} raw={emit_raw}"
        );
        assert_eq!(text_output(&overflow_events), if emit_raw { input.as_str() } else { "" }, "{prefix} raw={emit_raw}");
        let ends = tool_call_ends(&recovery_events);
        assert_eq!(ends.len(), 1, "{prefix} raw={emit_raw}");
        let StreamParserEvent::ToolcallEnd { index, name, id, arguments, .. } = ends[0] else { unreachable!() };
        assert_eq!((*index, name.as_str(), id.as_str()), (0, "Bash", "anthropic-xml-tool-0"));
        assert_eq!(arguments.get("command"), Some(&json!("echo recovered")));
        assert_eq!(text_output(&recovery_events), "");
    }

    #[test]
    fn resource_bounds_one_byte_deltas_and_recovers_after_overflow_for_an_incomplete_invoke() {
        assert_overflow_bounds(r#"<invoke name="Bash"><parameter name="command">"#, false);
        assert_overflow_bounds(r#"<invoke name="Bash"><parameter name="command">"#, true);
    }

    #[test]
    fn resource_bounds_one_byte_deltas_and_recovers_after_overflow_for_a_function_calls_wrapper() {
        assert_overflow_bounds("<function_calls>", false);
        assert_overflow_bounds("<function_calls>", true);
    }

}
