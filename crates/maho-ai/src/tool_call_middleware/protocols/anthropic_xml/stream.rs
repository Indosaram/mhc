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

                let tool = resolver.resolve(&invoke_open_tag.tool_name);
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
                    .and_then(|params| coerce_stream_parameters(params, tool, self.config, allow_json_schema_fallback));
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
        let resolver = self.resolver();
        let tool = invoke_open_tag.as_ref().filter(|t| t.index == 0).and_then(|t| resolver.resolve(&t.tool_name));
        if let (Some(invoke_open_tag), Some(tool)) = (&invoke_open_tag, tool) {
            let scanned_block = scan_truncated_invoke_block(&self.buffer, invoke_open_tag);
            let coerced_arguments = coerce_stream_parameters(&scanned_block.parameters, tool, self.config, true);
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
                if let Ok(Value::Object(validated)) = validate_tool_arguments(tool, &tool_call) {
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
    fn finish_emits_raw_text_for_a_non_protocol_partial_tag() {
        let tools = vec![tool("get_weather")];
        let mut parser = create_anthropic_xml_stream_parser(tools, None);
        parser.feed("<div");
        let events = parser.finish();
        assert_eq!(events, vec![StreamParserEvent::Text { text: "<div".into() }]);
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
}
