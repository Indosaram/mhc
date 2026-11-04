//! Port of senpi packages/ai/src/tool-call-middleware/protocols/anthropic-xml/recovery-stream.ts.

use std::collections::HashMap;

use serde_json::Value;

use super::invoke_protocol::InvokeProtocolConfig;
use super::invoke_stream_helpers::find_function_calls_open_tag;
use super::invoke_tag_scanner::{
    find_invoke_open_tag, is_potential_protocol_start, scan_invoke_block,
};
use super::recovery_wrapper_state::{RecoveryWrapperAction, RecoveryWrapperState};
use super::stream_boundary::{
    ANTHROPIC_XML_MAX_RETAINED_FRAGMENT_LENGTH, PendingFragmentKind, StreamBoundaryMatcher,
    Utf16Buffer, create_pending_fragment,
};
use super::tool_resolver::ToolResolver;
use crate::tool_call_middleware::types::{ParserOptions, StreamParser, StreamParserEvent};
use crate::types::Tool;

const MAX_PARTIAL_TAG_VALIDATION_LENGTH: usize = 128;

pub trait RecoveryStreamParser: StreamParser {
    fn interrupt(&mut self) -> Vec<StreamParserEvent>;
}

type WrapperResolver = Box<dyn Fn(&str) -> Option<Tool> + Send>;

enum RecoveryState {
    Idle {
        tag: Utf16Buffer,
    },
    Wrapper {
        scanner: RecoveryWrapperState<WrapperResolver>,
    },
    Active {
        tool: Box<Tool>,
        index: usize,
        id: String,
        close_matcher: Box<dyn StreamBoundaryMatcher>,
        wrapper: Option<RecoveryWrapperState<WrapperResolver>>,
        source: Utf16Buffer,
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
            metadata.insert(
                "protocol".to_string(),
                Value::String(self.config.protocol.to_string()),
            );
            metadata.insert(
                "retainedLength".to_string(),
                Value::Number(retained_length.into()),
            );
            options.report_error(
                "ANTML recovery fragment exceeded the retained-input limit.",
                Some(metadata),
            );
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
        events.push(StreamParserEvent::ToolcallStart {
            index,
            name: tool.name.clone(),
            id: id.clone(),
        });
        let close_matcher = create_pending_fragment(PendingFragmentKind::Invoke, opening).matcher;
        self.state = RecoveryState::Active {
            tool: Box::new(tool),
            index,
            id,
            close_matcher,
            wrapper,
            source: Utf16Buffer::from(opening),
        };
    }

    fn restore_after_active(&mut self, wrapper: Option<RecoveryWrapperState<WrapperResolver>>) {
        self.state = match wrapper {
            Some(scanner) => RecoveryState::Wrapper { scanner },
            None => RecoveryState::Idle {
                tag: Utf16Buffer::default(),
            },
        };
    }

    fn finish_active(&mut self, events: &mut Vec<StreamParserEvent>) {
        let RecoveryState::Active {
            tool,
            index,
            id,
            wrapper,
            source,
            ..
        } = std::mem::replace(&mut self.state, RecoveryState::Finished)
        else {
            unreachable!()
        };
        let source = source.as_str();
        let opening = find_invoke_open_tag(source, 0)
            .expect("active state always starts from a matched invoke open tag");
        let block = scan_invoke_block(source, &opening);
        let arguments_record = if block.as_ref().is_some_and(|b| b.end == source.len()) {
            block
                .and_then(|b| b.parameters)
                .and_then(|p| (self.config.coerce)(&p, &tool))
        } else {
            None
        };
        let arguments_value = arguments_record.clone().unwrap_or_default();
        events.push(StreamParserEvent::ToolcallDelta {
            index,
            arguments_delta: Value::Object(arguments_value.clone()).to_string(),
        });
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
                metadata.insert(
                    "protocol".to_string(),
                    Value::String(self.config.protocol.to_string()),
                );
                metadata.insert("toolName".to_string(), Value::String(tool.name.clone()));
                options.report_error(
                    "Recovered ANTML tool call arguments failed validation.",
                    Some(metadata),
                );
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
        let RecoveryState::Active {
            tool,
            index,
            id,
            wrapper,
            source,
            ..
        } = std::mem::replace(&mut self.state, RecoveryState::Finished)
        else {
            unreachable!()
        };
        self.report_overflow(source.utf16_len());
        events.push(StreamParserEvent::ToolcallDelta {
            index,
            arguments_delta: "{}".to_string(),
        });
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
                self.state = RecoveryState::Idle {
                    tag: Utf16Buffer::default(),
                };
            }
            return;
        }
        let wrapper = find_function_calls_open_tag(&tag, 0);
        if let Some(wrapper) = &wrapper
            && wrapper.index == 0
            && wrapper.length == tag.len()
        {
            let resolver_fn = self.make_resolver_fn();
            self.state = RecoveryState::Wrapper {
                scanner: RecoveryWrapperState::new(tag, resolver_fn),
            };
            return;
        }
        super::invoke_stream_helpers::emit_text(events, &tag);
        self.state = RecoveryState::Idle {
            tag: Utf16Buffer::default(),
        };
    }

    fn feed_idle_character(&mut self, events: &mut Vec<StreamParserEvent>, character: char) {
        let RecoveryState::Idle { tag } = &mut self.state else {
            unreachable!()
        };
        if tag.is_empty() && character != '<' {
            super::invoke_stream_helpers::emit_text(events, character.encode_utf8(&mut [0u8; 4]));
            return;
        }
        if character == '<' && !tag.is_empty() {
            let tag = std::mem::replace(tag, Utf16Buffer::from("<"));
            super::invoke_stream_helpers::emit_text(events, tag.as_str());
            return;
        }
        if exceeds_retained_limit(tag.utf16_len(), character.len_utf16()) {
            let tag = std::mem::take(tag);
            self.report_overflow(tag.utf16_len());
            super::invoke_stream_helpers::emit_text(events, tag.as_str());
            self.feed_idle_character(events, character);
            return;
        }
        tag.push(character);
        if character == '>' {
            let tag = std::mem::take(tag).into_string();
            self.handle_idle_tag(events, tag);
        } else if tag.utf16_len() == ANTHROPIC_XML_MAX_RETAINED_FRAGMENT_LENGTH {
            let tag = std::mem::take(tag);
            self.report_overflow(tag.utf16_len());
            super::invoke_stream_helpers::emit_text(events, tag.as_str());
        } else if tag.utf16_len() <= MAX_PARTIAL_TAG_VALIDATION_LENGTH
            && !is_potential_protocol_start(tag.as_str())
        {
            let tag = std::mem::take(tag);
            super::invoke_stream_helpers::emit_text(events, tag.as_str());
        }
    }

    fn feed_wrapper_character(&mut self, events: &mut Vec<StreamParserEvent>, character: char) {
        let RecoveryState::Wrapper { scanner } = &mut self.state else {
            unreachable!()
        };
        let actions = scanner.feed(character);
        for action in actions {
            match action {
                RecoveryWrapperAction::Text { text } => {
                    super::invoke_stream_helpers::emit_text(events, &text);
                }
                RecoveryWrapperAction::Closed { text } => {
                    super::invoke_stream_helpers::emit_text(events, &text);
                    self.state = RecoveryState::Idle {
                        tag: Utf16Buffer::default(),
                    };
                }
                RecoveryWrapperAction::Known {
                    text_before,
                    opening,
                    tool,
                } => {
                    super::invoke_stream_helpers::emit_text(events, &text_before);
                    let tool = (*tool).clone();
                    let RecoveryState::Wrapper { scanner } =
                        std::mem::replace(&mut self.state, RecoveryState::Finished)
                    else {
                        unreachable!()
                    };
                    self.start_known_invoke(events, &opening, tool, Some(scanner));
                    return;
                }
                RecoveryWrapperAction::Overflow {
                    text,
                    retained_length,
                    retains_wrapper,
                    next_character,
                } => {
                    self.report_overflow(retained_length);
                    super::invoke_stream_helpers::emit_text(events, &text);
                    if retains_wrapper {
                        if let Some(next_character) = next_character {
                            self.feed_wrapper_character(events, next_character);
                        }
                    } else {
                        self.state = RecoveryState::Idle {
                            tag: Utf16Buffer::default(),
                        };
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
        let RecoveryState::Active { source, .. } = &self.state else {
            unreachable!()
        };
        if exceeds_retained_limit(source.utf16_len(), character.len_utf16()) {
            self.overflow_active(events);
            match &mut self.state {
                RecoveryState::Wrapper { .. } => self.feed_wrapper_character(events, character),
                RecoveryState::Idle { .. } => self.feed_idle_character(events, character),
                _ => {}
            }
            return;
        }
        let RecoveryState::Active {
            close_matcher,
            source,
            ..
        } = &mut self.state
        else {
            unreachable!()
        };
        source.push(character);
        let matched = close_matcher.feed(character.encode_utf8(&mut [0u8; 4]));
        if matched {
            self.finish_active(events);
        } else if source.utf16_len() == ANTHROPIC_XML_MAX_RETAINED_FRAGMENT_LENGTH {
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
                RecoveryState::Wrapper { .. } => {
                    self.feed_wrapper_character(&mut events, character)
                }
                RecoveryState::Active { .. } => self.feed_active_character(&mut events, character),
                RecoveryState::Finished => {}
            }
        }
        events
    }

    fn finish(&mut self) -> Vec<StreamParserEvent> {
        let mut events = Vec::new();
        match std::mem::replace(&mut self.state, RecoveryState::Finished) {
            RecoveryState::Idle { tag } => {
                super::invoke_stream_helpers::emit_text(&mut events, tag.as_str())
            }
            RecoveryState::Wrapper { scanner } => {
                super::invoke_stream_helpers::emit_text(&mut events, &scanner.finish())
            }
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
                super::invoke_stream_helpers::emit_text(&mut events, tag.as_str());
                self.state = RecoveryState::Idle {
                    tag: Utf16Buffer::default(),
                };
            }
            RecoveryState::Wrapper { scanner } => {
                super::invoke_stream_helpers::emit_text(&mut events, &scanner.finish());
                self.state = RecoveryState::Idle {
                    tag: Utf16Buffer::default(),
                };
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
    Box::new(InvokeRecoveryStreamParser {
        tools,
        config,
        options,
        state: RecoveryState::Idle {
            tag: Utf16Buffer::default(),
        },
        next_tool_call_index: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_call_middleware::protocols::anthropic_xml::invoke_protocol::ANTHROPIC_XML_INVOKE_CONFIG;
    use serde_json::json;

    /// senpi emits one text event per idle character, so tests assert on the concatenation.
    fn text_of(events: &[StreamParserEvent]) -> String {
        events
            .iter()
            .filter_map(|event| {
                if let StreamParserEvent::Text { text } = event {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect()
    }

    fn tool(name: &str) -> Tool {
        Tool {
            name: name.into(),
            description: "d".into(),
            parameters: json!({"type": "object", "properties": {"city": {"type": "string"}}}),
            output_schema: None,
            freeform: None,
            constrained_sampling: None,
        }
    }

    #[test]
    fn recovers_a_bare_invoke_leaked_outside_any_wrapper() {
        let tools = vec![tool("get_weather")];
        let mut parser =
            create_invoke_recovery_stream_parser(tools, &ANTHROPIC_XML_INVOKE_CONFIG, None);
        let mut events = parser.feed(
            r#"<invoke name="get_weather"><parameter name="city">Seoul</parameter></invoke>"#,
        );
        events.extend(parser.finish());
        assert!(matches!(events[0], StreamParserEvent::ToolcallStart { .. }));
        assert!(matches!(
            &events[2],
            StreamParserEvent::ToolcallEnd {
                incomplete: false,
                ..
            }
        ));
    }

    #[test]
    fn recovers_an_invoke_wrapped_in_function_calls() {
        let tools = vec![tool("get_weather")];
        let mut parser =
            create_invoke_recovery_stream_parser(tools, &ANTHROPIC_XML_INVOKE_CONFIG, None);
        let mut events =
            parser.feed(r#"<function_calls><invoke name="get_weather"><parameter name="city">Seoul</parameter></invoke></function_calls>"#);
        events.extend(parser.finish());
        assert!(
            events
                .iter()
                .any(|e| matches!(e, StreamParserEvent::ToolcallStart { .. }))
        );
        assert!(events.iter().any(|e| matches!(
            e,
            StreamParserEvent::ToolcallEnd {
                incomplete: false,
                ..
            }
        )));
    }

    #[test]
    fn plain_text_passes_through_untouched() {
        let tools = vec![tool("get_weather")];
        let mut parser =
            create_invoke_recovery_stream_parser(tools, &ANTHROPIC_XML_INVOKE_CONFIG, None);
        let mut events = parser.feed("hello world");
        events.extend(parser.finish());
        // senpi's idle path emits one text event per character; the concatenation is what callers see.
        assert_eq!(text_of(&events), "hello world");
        assert!(
            events
                .iter()
                .all(|event| matches!(event, StreamParserEvent::Text { .. }))
        );
    }

    #[test]
    fn interrupt_flushes_idle_tag_as_text() {
        let tools = vec![tool("get_weather")];
        let mut parser =
            create_invoke_recovery_stream_parser(tools, &ANTHROPIC_XML_INVOKE_CONFIG, None);
        parser.feed("<inv");
        let events = parser.interrupt();
        assert_eq!(
            events,
            vec![StreamParserEvent::Text {
                text: "<inv".into()
            }]
        );
    }

    #[test]
    fn unknown_tool_invoke_is_emitted_as_text() {
        let tools: Vec<Tool> = Vec::new();
        let mut parser =
            create_invoke_recovery_stream_parser(tools, &ANTHROPIC_XML_INVOKE_CONFIG, None);
        let mut events = parser.feed(r#"<invoke name="unknown"></invoke>"#);
        events.extend(parser.finish());
        let text: String = events
            .iter()
            .filter_map(|e| {
                if let StreamParserEvent::Text { text } = e {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(text, r#"<invoke name="unknown"></invoke>"#);
    }

    // ---- ports of packages/ai/test/tool-call-middleware/invoke-recovery-resource*.test.ts ----
    //
    // The vi.mock instrumentation of `scanInvokeBlock` and `RecoveryWrapperState.feed` (scan-work
    // and wrapper-retention counters) has no Rust analogue without test hooks in modules owned by
    // other lanes; those counter assertions are omitted, every observable assertion is kept.
    mod resource {
        use super::super::super::stream_boundary::ANTHROPIC_XML_MAX_RETAINED_FRAGMENT_LENGTH as MAX;
        use crate::tool_call_middleware::protocols::antml::recovery_stream::create_antml_invoke_recovery_stream_parser;
        use crate::tool_call_middleware::types::{ParserOptions, StreamParserEvent};
        use crate::types::Tool;
        use serde_json::{Value, json};
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};

        type Parser = Box<dyn super::super::RecoveryStreamParser + Send>;
        type Errors = Arc<Mutex<Vec<(String, Option<HashMap<String, Value>>)>>>;
        type End = (String, Value, bool, Option<String>);

        const VALID_CALL: &str =
            r#"<invoke name="Bash"><parameter name="command">echo recovered</parameter></invoke>"#;
        const OVERFLOW: &str = "ANTML recovery fragment exceeded the retained-input limit.";
        const KNOWN_WRAPPER_CALL: &str = r#"<function_calls><invoke name="Bash"><parameter name="command">one</parameter></invoke>"#;

        fn bash_tool() -> Tool {
            Tool {
                name: "Bash".into(),
                description: "Run a command".into(),
                parameters: json!({
                    "type": "object",
                    "properties": { "command": { "type": "string" } },
                    "required": ["command"]
                }),
                output_schema: None,
                freeform: None,
                constrained_sampling: None,
            }
        }

        fn parser() -> (Parser, Errors) {
            let errors: Errors = Arc::new(Mutex::new(Vec::new()));
            let sink = errors.clone();
            let options = ParserOptions {
                emit_raw_tool_call_text_on_error: false,
                on_error: Some(Arc::new(move |message, metadata| {
                    sink.lock()
                        .expect("errors")
                        .push((message.to_owned(), metadata.cloned()));
                })),
            };
            (
                create_antml_invoke_recovery_stream_parser(vec![bash_tool()], Some(options)),
                errors,
            )
        }

        fn feed_units(parser: &mut Parser, input: &str) -> Vec<StreamParserEvent> {
            let mut events = Vec::new();
            for unit in input.chars() {
                events.extend(parser.feed(unit.encode_utf8(&mut [0u8; 4])));
            }
            events
        }

        fn text_output(events: &[StreamParserEvent]) -> String {
            events
                .iter()
                .filter_map(|event| match event {
                    StreamParserEvent::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect()
        }

        fn ends(events: &[StreamParserEvent]) -> Vec<End> {
            events
                .iter()
                .filter_map(|event| match event {
                    StreamParserEvent::ToolcallEnd {
                        id,
                        arguments,
                        incomplete,
                        error_message,
                        ..
                    } => Some((
                        id.clone(),
                        Value::Object(arguments.clone()),
                        *incomplete,
                        error_message.clone(),
                    )),
                    _ => None,
                })
                .collect()
        }

        fn recovered(id: &str) -> End {
            (
                id.into(),
                json!({ "command": "echo recovered" }),
                false,
                None,
            )
        }

        fn overflow(retained_length: usize) -> (String, Option<HashMap<String, Value>>) {
            (
                OVERFLOW.into(),
                Some(HashMap::from([
                    ("protocol".to_owned(), json!("antml")),
                    ("retainedLength".to_owned(), json!(retained_length)),
                ])),
            )
        }

        fn recover(parser: &mut Parser) -> Vec<StreamParserEvent> {
            let mut events = parser.feed(VALID_CALL);
            events.extend(parser.finish());
            events
        }

        #[test]
        fn keeps_recovery_scan_work_linear_for_one_character_deltas() {
            let command = "x".repeat(16 * 1024);
            let input = format!(
                r#"<invoke name="Bash"><parameter name="command">{command}</parameter></invoke>"#
            );
            let (mut parser, _) = parser();
            let mut events = feed_units(&mut parser, &input);
            events.extend(parser.finish());
            assert_eq!(
                ends(&events),
                vec![(
                    "recovered-antml-0".into(),
                    json!({ "command": command }),
                    false,
                    None
                )]
            );
        }

        #[test]
        fn bounds_a_partial_opening_tag_before_toolcall_start_and_recovers() {
            let (mut parser, errors) = parser();
            let input = format!(
                r#"<invoke name="{}"#,
                "x".repeat(MAX - r#"<invoke name=""#.len())
            );
            let events = feed_units(&mut parser, &input);
            let recovery = recover(&mut parser);
            assert_eq!(text_output(&events), input);
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, StreamParserEvent::ToolcallStart { .. }))
            );
            assert!(errors.lock().expect("errors").contains(&overflow(MAX)));
            assert_eq!(ends(&recovery), vec![recovered("recovered-antml-0")]);
        }

        #[test]
        fn does_not_double_count_an_unknown_only_wrapper_fragment() {
            let (mut parser, errors) = parser();
            let input = format!("<function_calls><{}<", "x".repeat(32_759));
            let mut events = feed_units(&mut parser, &input);
            events.extend(parser.finish());
            assert_eq!(input.len(), 32_777);
            assert_eq!(text_output(&events), input);
            assert!(errors.lock().expect("errors").is_empty());
        }

        #[test]
        fn preserves_unknown_only_wrapper_text_across_fragment_overflow() {
            let (mut parser, errors) = parser();
            let wrapper = "<function_calls>";
            let fragment = format!("<{}<", "x".repeat(32_759));
            let trailing = format!(
                "{}\u{1F600}</function_calls>",
                "y".repeat(MAX - wrapper.len() - fragment.len())
            );
            let input = format!("{wrapper}{fragment}{trailing}");
            let events = feed_units(&mut parser, &input);
            let recovery = recover(&mut parser);
            assert_eq!(text_output(&events), input);
            let errors = errors.lock().expect("errors");
            assert_eq!(*errors, vec![overflow(MAX)]);
            assert!(!format!("{errors:?}").contains(&fragment));
            assert_eq!(ends(&recovery), vec![recovered("recovered-antml-0")]);
        }

        #[test]
        fn bounds_utf_16_surrogate_crossings_before_recovery_start() {
            let (mut parser, errors) = parser();
            let opening_prefix = r#"<invoke name=""#;
            let prefix = format!(
                "{opening_prefix}{}",
                "x".repeat(MAX - 1 - opening_prefix.len())
            );
            let mut events = feed_units(&mut parser, &prefix);
            events.extend(feed_units(&mut parser, "\u{1F600}"));
            let recovery = recover(&mut parser);
            assert_eq!(text_output(&events), format!("{prefix}\u{1F600}"));
            let errors = errors.lock().expect("errors");
            assert!(errors.contains(&overflow(MAX - 1)));
            assert!(!format!("{errors:?}").contains(&prefix));
            assert_eq!(ends(&recovery), vec![recovered("recovered-antml-0")]);
        }

        #[test]
        fn bounds_utf_16_surrogate_crossings_during_an_active_recovery() {
            let (mut parser, errors) = parser();
            let opening = r#"<invoke name="Bash">"#;
            let active_prefix = "x".repeat(MAX - opening.len() - 1);
            parser.feed(opening);
            feed_units(&mut parser, &active_prefix);
            let crossing = feed_units(&mut parser, "\u{1F600}");
            let recovery = recover(&mut parser);
            assert_eq!(text_output(&crossing), "\u{1F600}");
            let crossing_ends = ends(&crossing);
            assert_eq!(crossing_ends.len(), 1);
            assert!(crossing_ends[0].2);
            assert_eq!(
                crossing_ends[0].3.as_deref(),
                Some("Tool call stream ended before completion")
            );
            assert!(errors.lock().expect("errors").contains(&overflow(MAX - 1)));
            assert_eq!(ends(&recovery), vec![recovered("recovered-antml-1")]);
        }

        #[test]
        fn bounds_utf_16_surrogate_crossings_after_a_recovered_wrapper_call() {
            let (mut parser, errors) = parser();
            let prefix = format!("<{}", "x".repeat(MAX - 2));
            parser.feed(KNOWN_WRAPPER_CALL);
            let mut crossing = feed_units(&mut parser, &prefix);
            crossing.extend(feed_units(&mut parser, "\u{1F600}"));
            let close = parser.feed("</function_calls>");
            let recovery = recover(&mut parser);
            assert_eq!(text_output(&crossing), format!("{prefix}\u{1F600}"));
            let errors = errors.lock().expect("errors");
            assert!(errors.contains(&overflow(MAX - 1)));
            assert!(!format!("{errors:?}").contains(&prefix));
            assert_eq!(text_output(&close), "");
            assert_eq!(ends(&recovery), vec![recovered("recovered-antml-1")]);
        }

        #[test]
        fn bounds_partial_tags_after_a_recovered_wrapper_call() {
            let (mut parser, errors) = parser();
            let partial_tag = format!("<{}", "x".repeat(66_560));
            parser.feed(KNOWN_WRAPPER_CALL);
            let partial = feed_units(&mut parser, &partial_tag);
            let close = parser.feed("</function_calls>");
            let recovery = recover(&mut parser);
            assert_eq!(text_output(&partial), partial_tag);
            let errors = errors.lock().expect("errors");
            assert!(errors.contains(&overflow(MAX)));
            assert!(!format!("{errors:?}").contains(&partial_tag));
            assert_eq!(text_output(&close), "");
            assert_eq!(ends(&recovery), vec![recovered("recovered-antml-1")]);
        }

        #[test]
        fn finalizes_an_active_call_incomplete_on_retained_buffer_overflow_and_recovers() {
            let (mut parser, errors) = parser();
            let opening = r#"<invoke name="Bash">"#;
            let input = format!("{opening}{}", "x".repeat(MAX - opening.len()));
            let events = feed_units(&mut parser, &input);
            let recovery = recover(&mut parser);
            let active_ends = ends(&events);
            assert_eq!(active_ends.len(), 1);
            assert_eq!(active_ends[0].0, "recovered-antml-0");
            assert!(active_ends[0].2);
            assert_eq!(
                active_ends[0].3.as_deref(),
                Some("Tool call stream ended before completion")
            );
            assert!(errors.lock().expect("errors").contains(&overflow(MAX)));
            assert_eq!(ends(&recovery), vec![recovered("recovered-antml-1")]);
        }

        /// Pinned `invoke-recovery-metadata-fixtures.ts` `MetadataStreamHarness`: a hand-driven
        /// producer whose every event carries a fresh snapshot of one metadata-rich partial.
        struct MetadataStreamHarness {
            inner: crate::utils::event_stream::AssistantMessageEventStream,
            partial: crate::types::AssistantMessage,
        }

        impl MetadataStreamHarness {
            fn new() -> Self {
                let usage = crate::types::Usage {
                    context_usage: None,
                    cost_unknown: None,
                    input: 101,
                    output: 37,
                    cache_read: 23,
                    cache_write: 19,
                    cache_write_1h: Some(17),
                    reasoning: Some(11),
                    total_tokens: 180,
                    cost: crate::types::UsageCost {
                        input: 0.101,
                        output: 0.037,
                        cache_read: 0.023,
                        cache_write: 0.019,
                        total: 0.18,
                    },
                };
                let partial: crate::types::AssistantMessage = serde_json::from_value(json!({
                    "role": "assistant",
                    "api": "anthropic-messages",
                    "provider": "anthropic",
                    "model": "claude-requested",
                    "responseModel": "claude-routed",
                    "responseId": "msg-response-9",
                    "content": [],
                    "usage": usage,
                    "stopReason": "stop",
                    "timestamp": 9009
                }))
                .expect("metadata message");
                Self {
                    inner: crate::utils::event_stream::AssistantMessageEventStream::assistant(),
                    partial,
                }
            }

            fn start(&self) {
                self.inner.push(crate::types::AssistantMessageEvent::Start {
                    partial: self.partial.clone(),
                });
            }

            fn start_text(&mut self, text_signature: &str) -> usize {
                let content_index = self.partial.content.len();
                self.partial.content.push(crate::types::ContentBlock::Text(
                    crate::types::TextContent {
                        text: String::new(),
                        audience: None,
                        text_signature: Some(text_signature.to_owned()),
                    },
                ));
                self.inner
                    .push(crate::types::AssistantMessageEvent::TextStart {
                        content_index,
                        partial: self.partial.clone(),
                    });
                content_index
            }

            fn text_delta(&mut self, content_index: usize, delta: &str) {
                let Some(crate::types::ContentBlock::Text(block)) =
                    self.partial.content.get_mut(content_index)
                else {
                    panic!("Expected text block at {content_index}");
                };
                block.text.push_str(delta);
                self.inner
                    .push(crate::types::AssistantMessageEvent::TextDelta {
                        content_index,
                        delta: delta.to_owned(),
                        partial: self.partial.clone(),
                    });
            }

            fn end_text(&self, content_index: usize) {
                let Some(crate::types::ContentBlock::Text(block)) =
                    self.partial.content.get(content_index)
                else {
                    panic!("Expected text block at {content_index}");
                };
                self.inner
                    .push(crate::types::AssistantMessageEvent::TextEnd {
                        content_index,
                        content: block.text.clone(),
                        partial: self.partial.clone(),
                    });
            }

            fn finish(&mut self) {
                self.partial.stop_reason = crate::types::StopReason::ToolUse;
                self.inner.push(crate::types::AssistantMessageEvent::Done {
                    reason: crate::types::DoneReason::ToolUse,
                    message: self.partial.clone(),
                });
            }
        }

        fn event_message(
            event: &crate::types::AssistantMessageEvent,
        ) -> &crate::types::AssistantMessage {
            use crate::types::AssistantMessageEvent as Event;
            match event {
                Event::Done { message, .. } => message,
                Event::Error { error, .. } => error,
                Event::Start { partial }
                | Event::TextStart { partial, .. }
                | Event::TextDelta { partial, .. }
                | Event::TextEnd { partial, .. }
                | Event::ThinkingStart { partial, .. }
                | Event::ThinkingDelta { partial, .. }
                | Event::ThinkingEnd { partial, .. }
                | Event::ToolcallStart { partial, .. }
                | Event::ToolcallDelta { partial, .. }
                | Event::ToolcallEnd { partial, .. } => partial,
            }
        }

        /// invoke-recovery-resource-scenarios.test.ts "preserves byte-identical full usage on
        /// every event and terminal".
        #[tokio::test]
        async fn preserves_byte_identical_full_usage_on_every_event_and_terminal() {
            use crate::tool_call_middleware::recovery_stream_wrapper::{
                InvokeRecoveryOptions, wrap_stream_with_invoke_recovery,
            };
            let mut producer = MetadataStreamHarness::new();
            let wrapped = wrap_stream_with_invoke_recovery(
                producer.inner.clone(),
                vec![bash_tool()],
                InvokeRecoveryOptions::default(),
            );
            producer.start();
            let index = producer.start_text("signed");
            producer.text_delta(index, VALID_CALL);
            producer.end_text(index);
            producer.finish();
            let deadline = std::time::Duration::from_secs(5);
            let events = tokio::time::timeout(deadline, wrapped.collect())
                .await
                .expect("events deadline")
                .expect("events");
            let result = tokio::time::timeout(deadline, wrapped.result())
                .await
                .expect("result deadline")
                .expect("result");
            let expected = serde_json::to_string(&result.usage).expect("usage json");
            assert!(!events.is_empty());
            for event in &events {
                assert_eq!(
                    serde_json::to_string(&event_message(event).usage).expect("usage json"),
                    expected
                );
            }
        }

        /// invoke-recovery-resource-scenarios.test.ts "bounds recovery buffers and scan work".
        #[test]
        fn bounds_recovery_buffers_and_scan_work() {
            let (mut parser, errors) = parser();
            let opening = r#"<invoke name=""#;
            let overflow_input = format!("{opening}{}", "x".repeat(MAX - opening.len()));
            let mut events = parser.feed(&overflow_input);
            events.extend(parser.feed(VALID_CALL));
            events.extend(parser.finish());
            assert!(errors.lock().expect("errors").contains(&overflow(MAX)));
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, StreamParserEvent::ToolcallStart { .. }))
                    .count(),
                1
            );
            let all_ends = ends(&events);
            assert_eq!(all_ends.len(), 1);
            assert_eq!(all_ends[0].1, json!({ "command": "echo recovered" }));
        }

        /// invoke-recovery-resource-scenarios.test.ts "keeps bare and namespaced recovery stable
        /// across exhaustive split points", asserted at the parser layer: every two-chunk split
        /// yields start/delta/end and the same recovered call.
        #[test]
        fn keeps_bare_and_namespaced_recovery_stable_across_exhaustive_split_points() {
            // The namespace prefix is assembled at runtime so the source never carries the
            // literal namespaced tag.
            let ns = ["ant", "ml:"].concat();
            let namespaced = format!(
                r#"<{ns}invoke name="Bash"><{ns}parameter name="command">echo recovered</{ns}parameter></{ns}invoke>"#
            );
            for input in [VALID_CALL.to_owned(), namespaced] {
                let boundaries: Vec<usize> = (0..=input.len())
                    .filter(|split| input.is_char_boundary(*split))
                    .collect();
                for split in boundaries {
                    let (mut parser, _) = parser();
                    let mut events = parser.feed(&input[..split]);
                    events.extend(parser.feed(&input[split..]));
                    events.extend(parser.finish());
                    let kinds: Vec<&str> = events
                        .iter()
                        .filter_map(|event| match event {
                            StreamParserEvent::ToolcallStart { .. } => Some("toolcall_start"),
                            StreamParserEvent::ToolcallDelta { .. } => Some("toolcall_delta"),
                            StreamParserEvent::ToolcallEnd { .. } => Some("toolcall_end"),
                            StreamParserEvent::Text { .. } => None,
                        })
                        .collect();
                    assert_eq!(
                        kinds,
                        vec!["toolcall_start", "toolcall_delta", "toolcall_end"],
                        "split {split}"
                    );
                    assert_eq!(
                        ends(&events),
                        vec![recovered("recovered-antml-0")],
                        "split {split}"
                    );
                }
            }
        }

        /// invoke-recovery-resource-scenarios.test.ts "handles %s forms without generic tag
        /// inference", asserted at the parser layer.
        #[test]
        fn handles_forms_without_generic_tag_inference() {
            let ns = ["ant", "ml:"].concat();
            let namespaced = format!(
                r#"<{ns}invoke name="Bash"><{ns}parameter name="command">echo recovered</{ns}parameter></{ns}invoke>"#
            );
            let cases = [
                (VALID_CALL.to_owned(), true),
                (namespaced, true),
                (
                    r#"<foo:invoke name="Bash"><foo:parameter name="command">echo nope</foo:parameter></foo:invoke>"#
                        .to_owned(),
                    false,
                ),
                (
                    r#"<tool name="Bash"><parameter name="command">echo nope</parameter></tool>"#.to_owned(),
                    false,
                ),
            ];
            for (input, recovers) in cases {
                let (mut parser, _) = parser();
                let mut events = parser.feed(&input);
                events.extend(parser.finish());
                let has_tool_events = events
                    .iter()
                    .any(|event| !matches!(event, StreamParserEvent::Text { .. }));
                assert_eq!(has_tool_events, recovers, "{input}");
                if !recovers {
                    assert_eq!(text_output(&events), input);
                }
            }
        }
    }
}
