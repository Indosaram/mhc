//! Port of senpi packages/ai/src/tool-call-middleware/protocols/anthropic-xml/recovery-stream.ts.

use std::collections::HashMap;

use serde_json::Value;

use super::invoke_protocol::InvokeProtocolConfig;
use super::invoke_stream_helpers::find_function_calls_open_tag;
use super::invoke_tag_scanner::{find_invoke_open_tag, is_potential_protocol_start, scan_invoke_block};
use super::recovery_wrapper_state::{RecoveryWrapperAction, RecoveryWrapperState};
use super::stream_boundary::{create_pending_fragment, PendingFragmentKind, StreamBoundaryMatcher, ANTHROPIC_XML_MAX_RETAINED_FRAGMENT_LENGTH};
use super::tool_resolver::ToolResolver;
use crate::tool_call_middleware::types::{ParserOptions, StreamParser, StreamParserEvent};
use crate::types::Tool;

const MAX_PARTIAL_TAG_VALIDATION_LENGTH: usize = 128;

pub trait RecoveryStreamParser: StreamParser {
    fn interrupt(&mut self) -> Vec<StreamParserEvent>;
}

type WrapperResolver = Box<dyn Fn(&str) -> Option<Tool> + Send>;

enum RecoveryState {
    Idle { tag: String },
    Wrapper { scanner: RecoveryWrapperState<WrapperResolver> },
    Active {
        tool: Tool,
        index: usize,
        id: String,
        close_matcher: Box<dyn StreamBoundaryMatcher>,
        wrapper: Option<RecoveryWrapperState<WrapperResolver>>,
        source: String,
    },
    Finished,
}

struct InvokeRecoveryStreamParser {
    tools: Vec<Tool>,
    config: &'static InvokeProtocolConfig,
    options: Option<ParserOptions>,
    state: RecoveryState,
    next_tool_call_index: usize,
}

fn exceeds_retained_limit(retained_length: usize, incoming_len: usize) -> bool {
    retained_length + incoming_len > ANTHROPIC_XML_MAX_RETAINED_FRAGMENT_LENGTH
}

impl InvokeRecoveryStreamParser {
    fn resolver(&self) -> ToolResolver<'_> {
        ToolResolver::new(&self.tools)
    }

    fn report_overflow(&self, retained_length: usize) {
        if let Some(options) = &self.options {
            let mut metadata = HashMap::new();
            metadata.insert("protocol".to_string(), Value::String(self.config.protocol.to_string()));
            metadata.insert("retainedLength".to_string(), Value::Number(retained_length.into()));
            options.report_error("ANTML recovery fragment exceeded the retained-input limit.", Some(metadata));
        }
    }

    fn make_resolver_fn(&self) -> WrapperResolver {
        let tools = self.tools.clone();
        Box::new(move |name: &str| ToolResolver::new(&tools).resolve(name).cloned())
    }

    fn start_known_invoke(
        &mut self,
        events: &mut Vec<StreamParserEvent>,
        opening: &str,
        tool: Tool,
        wrapper: Option<RecoveryWrapperState<WrapperResolver>>,
    ) {
        let index = self.next_tool_call_index;
        self.next_tool_call_index += 1;
        let id = format!("recovered-antml-{index}");
        events.push(StreamParserEvent::ToolcallStart { index, name: tool.name.clone(), id: id.clone() });
        let close_matcher = create_pending_fragment(PendingFragmentKind::Invoke, opening).matcher;
        self.state = RecoveryState::Active { tool, index, id, close_matcher, wrapper, source: opening.to_string() };
    }

    fn restore_after_active(&mut self, wrapper: Option<RecoveryWrapperState<WrapperResolver>>) {
        self.state = match wrapper {
            Some(scanner) => RecoveryState::Wrapper { scanner },
            None => RecoveryState::Idle { tag: String::new() },
        };
    }

    fn finish_active(&mut self, events: &mut Vec<StreamParserEvent>) {
        let RecoveryState::Active { tool, index, id, wrapper, source, .. } = std::mem::replace(&mut self.state, RecoveryState::Finished)
        else {
            unreachable!()
        };
        let opening = find_invoke_open_tag(&source, 0).expect("active state always starts from a matched invoke open tag");
        let block = scan_invoke_block(&source, &opening);
        let arguments_record = if block.as_ref().is_some_and(|b| b.end == source.len()) {
            block.unwrap().parameters.and_then(|p| (self.config.coerce)(&p, &tool))
        } else {
            None
        };
        let arguments_value = arguments_record.clone().unwrap_or_default();
        events.push(StreamParserEvent::ToolcallDelta { index, arguments_delta: Value::Object(arguments_value.clone()).to_string() });
        if arguments_record.is_some() {
            events.push(StreamParserEvent::ToolcallEnd {
                index,
                name: tool.name.clone(),
                id,
                arguments: arguments_value,
                incomplete: false,
                error_message: None,
            });
        } else {
            if let Some(options) = &self.options {
                let mut metadata = HashMap::new();
                metadata.insert("protocol".to_string(), Value::String(self.config.protocol.to_string()));
                metadata.insert("toolName".to_string(), Value::String(tool.name.clone()));
                options.report_error("Recovered ANTML tool call arguments failed validation.", Some(metadata));
            }
            events.push(StreamParserEvent::ToolcallEnd {
                index,
                name: tool.name.clone(),
                id,
                arguments: arguments_value,
                incomplete: true,
                error_message: Some("Recovered tool call arguments failed validation".to_string()),
            });
        }
        self.restore_after_active(wrapper);
    }

    fn overflow_active(&mut self, events: &mut Vec<StreamParserEvent>) {
        let RecoveryState::Active { tool, index, id, wrapper, source, .. } = std::mem::replace(&mut self.state, RecoveryState::Finished)
        else {
            unreachable!()
        };
        self.report_overflow(source.len());
        events.push(StreamParserEvent::ToolcallDelta { index, arguments_delta: "{}".to_string() });
        events.push(StreamParserEvent::ToolcallEnd {
            index,
            name: tool.name.clone(),
            id,
            arguments: serde_json::Map::new(),
            incomplete: true,
            error_message: Some("Tool call stream ended before completion".to_string()),
        });
        self.restore_after_active(wrapper);
    }

    fn handle_idle_tag(&mut self, events: &mut Vec<StreamParserEvent>, tag: String) {
        let invoke = find_invoke_open_tag(&tag, 0);
        if let Some(invoke) = &invoke
            && invoke.index == 0
            && invoke.length == tag.len()
        {
            if let Some(tool) = self.resolver().resolve(&invoke.tool_name).cloned() {
                self.start_known_invoke(events, &tag, tool, None);
            } else {
                super::invoke_stream_helpers::emit_text(events, &tag);
                self.state = RecoveryState::Idle { tag: String::new() };
            }
            return;
        }
        let wrapper = find_function_calls_open_tag(&tag, 0);
        if let Some(wrapper) = &wrapper
            && wrapper.index == 0
            && wrapper.length == tag.len()
        {
            let resolver_fn = self.make_resolver_fn();
            self.state = RecoveryState::Wrapper { scanner: RecoveryWrapperState::new(tag, resolver_fn) };
            return;
        }
        super::invoke_stream_helpers::emit_text(events, &tag);
        self.state = RecoveryState::Idle { tag: String::new() };
    }

    fn feed_idle_character(&mut self, events: &mut Vec<StreamParserEvent>, character: char) {
        let RecoveryState::Idle { tag } = &mut self.state else { unreachable!() };
        if tag.is_empty() && character != '<' {
            super::invoke_stream_helpers::emit_text(events, &character.to_string());
            return;
        }
        if character == '<' && !tag.is_empty() {
            let old_tag = std::mem::take(tag);
            super::invoke_stream_helpers::emit_text(events, &old_tag);
            let RecoveryState::Idle { tag } = &mut self.state else { unreachable!() };
            tag.push('<');
            return;
        }
        if exceeds_retained_limit(tag.len(), character.len_utf8()) {
            let tag_len = tag.len();
            self.report_overflow(tag_len);
            let old_tag = std::mem::take(tag);
            super::invoke_stream_helpers::emit_text(events, &old_tag);
            self.state = RecoveryState::Idle { tag: String::new() };
            self.feed_idle_character(events, character);
            return;
        }
        tag.push(character);
        if character == '>' {
            let RecoveryState::Idle { tag } = std::mem::replace(&mut self.state, RecoveryState::Idle { tag: String::new() }) else {
                unreachable!()
            };
            self.handle_idle_tag(events, tag);
        } else if tag.len() == ANTHROPIC_XML_MAX_RETAINED_FRAGMENT_LENGTH {
            let tag_len = tag.len();
            self.report_overflow(tag_len);
            let old_tag = std::mem::take(tag);
            super::invoke_stream_helpers::emit_text(events, &old_tag);
            self.state = RecoveryState::Idle { tag: String::new() };
        } else if tag.len() <= MAX_PARTIAL_TAG_VALIDATION_LENGTH && !is_potential_protocol_start(tag) {
            let old_tag = std::mem::take(tag);
            super::invoke_stream_helpers::emit_text(events, &old_tag);
        }
    }

    fn feed_wrapper_character(&mut self, events: &mut Vec<StreamParserEvent>, character: char) {
        let RecoveryState::Wrapper { scanner } = &mut self.state else { unreachable!() };
        let actions = scanner.feed(character);
        for action in actions {
            match action {
                RecoveryWrapperAction::Text { text } => {
                    super::invoke_stream_helpers::emit_text(events, &text);
                }
                RecoveryWrapperAction::Closed { text } => {
                    super::invoke_stream_helpers::emit_text(events, &text);
                    self.state = RecoveryState::Idle { tag: String::new() };
                }
                RecoveryWrapperAction::Known { text_before, opening, tool } => {
                    super::invoke_stream_helpers::emit_text(events, &text_before);
                    let tool = tool.clone();
                    let RecoveryState::Wrapper { scanner } = std::mem::replace(&mut self.state, RecoveryState::Finished) else {
                        unreachable!()
                    };
                    self.start_known_invoke(events, &opening, tool, Some(scanner));
                    return;
                }
                RecoveryWrapperAction::Overflow { text, retained_length, retains_wrapper, next_character } => {
                    self.report_overflow(retained_length);
                    super::invoke_stream_helpers::emit_text(events, &text);
                    if retains_wrapper {
                        if let Some(next_character) = next_character {
                            self.feed_wrapper_character(events, next_character);
                        }
                    } else {
                        self.state = RecoveryState::Idle { tag: String::new() };
                        if let Some(next_character) = next_character {
                            self.feed_idle_character(events, next_character);
                        }
                    }
                    return;
                }
            }
        }
    }

    fn feed_active_character(&mut self, events: &mut Vec<StreamParserEvent>, character: char) {
        let RecoveryState::Active { source, .. } = &self.state else { unreachable!() };
        if exceeds_retained_limit(source.len(), character.len_utf8()) {
            self.overflow_active(events);
            match &mut self.state {
                RecoveryState::Wrapper { .. } => self.feed_wrapper_character(events, character),
                RecoveryState::Idle { .. } => self.feed_idle_character(events, character),
                _ => {}
            }
            return;
        }
        let RecoveryState::Active { close_matcher, source, .. } = &mut self.state else { unreachable!() };
        source.push(character);
        let matched = close_matcher.feed(&character.to_string());
        if matched {
            self.finish_active(events);
        } else if source.len() == ANTHROPIC_XML_MAX_RETAINED_FRAGMENT_LENGTH {
            self.overflow_active(events);
        }
    }
}

impl StreamParser for InvokeRecoveryStreamParser {
    fn feed(&mut self, text_delta: &str) -> Vec<StreamParserEvent> {
        let mut events = Vec::new();
        for character in text_delta.chars() {
            if matches!(self.state, RecoveryState::Finished) {
                break;
            }
            match &self.state {
                RecoveryState::Idle { .. } => self.feed_idle_character(&mut events, character),
                RecoveryState::Wrapper { .. } => self.feed_wrapper_character(&mut events, character),
                RecoveryState::Active { .. } => self.feed_active_character(&mut events, character),
                RecoveryState::Finished => {}
            }
        }
        events
    }

    fn finish(&mut self) -> Vec<StreamParserEvent> {
        let mut events = Vec::new();
        match std::mem::replace(&mut self.state, RecoveryState::Finished) {
            RecoveryState::Idle { tag } => super::invoke_stream_helpers::emit_text(&mut events, &tag),
            RecoveryState::Wrapper { scanner } => super::invoke_stream_helpers::emit_text(&mut events, &scanner.finish()),
            _ => {}
        }
        events
    }
}

impl RecoveryStreamParser for InvokeRecoveryStreamParser {
    fn interrupt(&mut self) -> Vec<StreamParserEvent> {
        let mut events = Vec::new();
        match &self.state {
            RecoveryState::Idle { tag } => {
                super::invoke_stream_helpers::emit_text(&mut events, tag);
                self.state = RecoveryState::Idle { tag: String::new() };
            }
            RecoveryState::Wrapper { scanner } => {
                super::invoke_stream_helpers::emit_text(&mut events, &scanner.finish());
                self.state = RecoveryState::Idle { tag: String::new() };
            }
            _ => {}
        }
        events
    }
}

pub fn create_invoke_recovery_stream_parser(
    tools: Vec<Tool>,
    config: &'static InvokeProtocolConfig,
    options: Option<ParserOptions>,
) -> Box<dyn RecoveryStreamParser + Send> {
    Box::new(InvokeRecoveryStreamParser { tools, config, options, state: RecoveryState::Idle { tag: String::new() }, next_tool_call_index: 0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_call_middleware::protocols::anthropic_xml::invoke_protocol::ANTHROPIC_XML_INVOKE_CONFIG;
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
    fn recovers_a_bare_invoke_leaked_outside_any_wrapper() {
        let tools = vec![tool("get_weather")];
        let mut parser = create_invoke_recovery_stream_parser(tools, &ANTHROPIC_XML_INVOKE_CONFIG, None);
        let mut events = parser.feed(r#"<invoke name="get_weather"><parameter name="city">Seoul</parameter></invoke>"#);
        events.extend(parser.finish());
        assert!(matches!(events[0], StreamParserEvent::ToolcallStart { .. }));
        assert!(matches!(&events[2], StreamParserEvent::ToolcallEnd { incomplete: false, .. }));
    }

    #[test]
    fn recovers_an_invoke_wrapped_in_function_calls() {
        let tools = vec![tool("get_weather")];
        let mut parser = create_invoke_recovery_stream_parser(tools, &ANTHROPIC_XML_INVOKE_CONFIG, None);
        let mut events =
            parser.feed(r#"<function_calls><invoke name="get_weather"><parameter name="city">Seoul</parameter></invoke></function_calls>"#);
        events.extend(parser.finish());
        assert!(events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallStart { .. })));
        assert!(events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallEnd { incomplete: false, .. })));
    }

    #[test]
    fn plain_text_passes_through_untouched() {
        let tools = vec![tool("get_weather")];
        let mut parser = create_invoke_recovery_stream_parser(tools, &ANTHROPIC_XML_INVOKE_CONFIG, None);
        let mut events = parser.feed("hello world");
        events.extend(parser.finish());
        assert_eq!(events, vec![StreamParserEvent::Text { text: "hello world".into() }]);
    }

    #[test]
    fn interrupt_flushes_idle_tag_as_text() {
        let tools = vec![tool("get_weather")];
        let mut parser = create_invoke_recovery_stream_parser(tools, &ANTHROPIC_XML_INVOKE_CONFIG, None);
        parser.feed("<inv");
        let events = parser.interrupt();
        assert_eq!(events, vec![StreamParserEvent::Text { text: "<inv".into() }]);
    }

    #[test]
    fn unknown_tool_invoke_is_emitted_as_text() {
        let tools: Vec<Tool> = Vec::new();
        let mut parser = create_invoke_recovery_stream_parser(tools, &ANTHROPIC_XML_INVOKE_CONFIG, None);
        let mut events = parser.feed(r#"<invoke name="unknown"></invoke>"#);
        events.extend(parser.finish());
        let text: String = events
            .iter()
            .filter_map(|e| if let StreamParserEvent::Text { text } = e { Some(text.as_str()) } else { None })
            .collect();
        assert_eq!(text, r#"<invoke name="unknown"></invoke>"#);
    }
}
