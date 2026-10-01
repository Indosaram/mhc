//! Port of senpi packages/ai/src/tool-call-middleware/protocols/antml/recovery-stream.ts.

use super::config::ANTML_INVOKE_CONFIG;
use crate::tool_call_middleware::protocols::anthropic_xml::recovery_stream::{create_invoke_recovery_stream_parser, RecoveryStreamParser};
use crate::tool_call_middleware::types::{ParserOptions, StreamParser, StreamParserEvent};
use crate::types::Tool;

const MISSING_ANGLE_INVOKE_PREFIX: &str = "antml:invoke";
const STRAY_FUNCTION_RESULTS_CLOSE: &str = "</function_results>";
const MAX_MISSING_ANGLE_OPENING_LENGTH: usize = 128;

enum PendingState {
    None,
    Opening(String),
    Trailer(String),
}

fn is_opening_boundary(character: Option<char>) -> bool {
    match character {
        None => true,
        Some(c) => c != '<' && c != '/' && !(c.is_ascii_alphanumeric() || c == '_'),
    }
}

struct AntmlInvokeRecoveryStreamParser {
    inner: Box<dyn RecoveryStreamParser + Send>,
    pending: PendingState,
    previous_character: Option<char>,
    active_missing_angle_invoke: bool,
    synthetic_angle_pending: bool,
}

impl AntmlInvokeRecoveryStreamParser {
    fn collect_events(&mut self, events: &mut Vec<StreamParserEvent>, inner_events: Vec<StreamParserEvent>) {
        for event in inner_events {
            if self.synthetic_angle_pending {
                if let StreamParserEvent::Text { text } = &event
                    && text.starts_with(&format!("<{MISSING_ANGLE_INVOKE_PREFIX}"))
                {
                    self.synthetic_angle_pending = false;
                    let text = &text[1..];
                    if !text.is_empty() {
                        events.push(StreamParserEvent::Text { text: text.to_string() });
                    }
                    continue;
                }
                if matches!(event, StreamParserEvent::ToolcallStart { .. }) {
                    self.synthetic_angle_pending = false;
                    self.active_missing_angle_invoke = true;
                }
            }
            if self.active_missing_angle_invoke
                && let StreamParserEvent::ToolcallEnd { incomplete, .. } = &event
            {
                self.active_missing_angle_invoke = false;
                if !incomplete {
                    self.pending = PendingState::Trailer(String::new());
                }
            }
            events.push(event);
        }
    }

    fn collect_inner(&mut self, events: &mut Vec<StreamParserEvent>, input: &str, synthetic_angle: bool) {
        self.synthetic_angle_pending = self.synthetic_angle_pending || synthetic_angle;
        let inner_events = self.inner.feed(input);
        self.collect_events(events, inner_events);
    }

    fn flush_pending(&mut self, events: &mut Vec<StreamParserEvent>) {
        let value = match std::mem::replace(&mut self.pending, PendingState::None) {
            PendingState::None => return,
            PendingState::Opening(value) | PendingState::Trailer(value) => value,
        };
        self.collect_inner(events, &value, false);
    }

    fn feed_character(&mut self, events: &mut Vec<StreamParserEvent>, character: char, opening_boundary: bool) {
        match std::mem::replace(&mut self.pending, PendingState::None) {
            PendingState::Trailer(mut value) => {
                value.push(character);
                let candidate = value.trim_start().to_string();
                if candidate.is_empty() || STRAY_FUNCTION_RESULTS_CLOSE.starts_with(&candidate) {
                    if candidate == STRAY_FUNCTION_RESULTS_CLOSE {
                        self.pending = PendingState::None;
                    } else if value.len() >= MAX_MISSING_ANGLE_OPENING_LENGTH {
                        self.pending = PendingState::Trailer(value);
                        self.flush_pending(events);
                    } else {
                        self.pending = PendingState::Trailer(value);
                    }
                    return;
                }
                let leading_whitespace = value[..value.len() - candidate.len()].to_string();
                self.pending = PendingState::None;
                self.collect_inner(events, &leading_whitespace, false);
                if character == 'a' && opening_boundary {
                    self.pending = PendingState::Opening(character.to_string());
                } else {
                    self.collect_inner(events, &candidate, false);
                }
            }
            PendingState::Opening(mut value) => {
                value.push(character);
                if value.len() <= MISSING_ANGLE_INVOKE_PREFIX.len() && !MISSING_ANGLE_INVOKE_PREFIX.starts_with(&value) {
                    self.pending = PendingState::Opening(value);
                    self.flush_pending(events);
                    return;
                }
                if value.len() > MISSING_ANGLE_INVOKE_PREFIX.len() && character == '>' {
                    let opening = value;
                    self.pending = PendingState::None;
                    self.collect_inner(events, &format!("<{opening}"), true);
                    return;
                }
                if value.len() >= MAX_MISSING_ANGLE_OPENING_LENGTH {
                    self.pending = PendingState::Opening(value);
                    self.flush_pending(events);
                    return;
                }
                self.pending = PendingState::Opening(value);
            }
            PendingState::None => {
                if character == 'a' && opening_boundary {
                    self.pending = PendingState::Opening(character.to_string());
                    return;
                }
                self.collect_inner(events, &character.to_string(), false);
            }
        }
    }
}

impl StreamParser for AntmlInvokeRecoveryStreamParser {
    fn feed(&mut self, text_delta: &str) -> Vec<StreamParserEvent> {
        let mut events = Vec::new();
        for character in text_delta.chars() {
            let opening_boundary = is_opening_boundary(self.previous_character);
            self.previous_character = Some(character);
            self.feed_character(&mut events, character, opening_boundary);
        }
        events
    }

    fn finish(&mut self) -> Vec<StreamParserEvent> {
        let mut events = Vec::new();
        self.flush_pending(&mut events);
        let inner_events = self.inner.finish();
        self.collect_events(&mut events, inner_events);
        events
    }
}

impl RecoveryStreamParser for AntmlInvokeRecoveryStreamParser {
    fn interrupt(&mut self) -> Vec<StreamParserEvent> {
        let mut events = Vec::new();
        self.flush_pending(&mut events);
        let inner_events = self.inner.interrupt();
        self.collect_events(&mut events, inner_events);
        self.previous_character = None;
        events
    }
}

pub fn create_antml_invoke_recovery_stream_parser(tools: Vec<Tool>, options: Option<ParserOptions>) -> Box<dyn RecoveryStreamParser + Send> {
    Box::new(AntmlInvokeRecoveryStreamParser {
        inner: create_invoke_recovery_stream_parser(tools, &ANTML_INVOKE_CONFIG, options),
        pending: PendingState::None,
        previous_character: None,
        active_missing_angle_invoke: false,
        synthetic_angle_pending: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// senpi emits one text event per idle character, so tests assert on the concatenation.
    fn text_of(events: &[StreamParserEvent]) -> String {
        events.iter().filter_map(|event| if let StreamParserEvent::Text { text } = event { Some(text.as_str()) } else { None }).collect()
    }

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
    fn recovers_an_antml_invoke_wrapped_in_function_calls() {
        let tools = vec![tool("get_weather")];
        let mut parser = create_antml_invoke_recovery_stream_parser(tools, None);
        let mut events =
            parser.feed(r#"<function_calls><invoke name="get_weather"><parameter name="city">Seoul</parameter></invoke></function_calls>"#);
        events.extend(parser.finish());
        assert!(events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallStart { .. })));
        assert!(events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallEnd { incomplete: false, .. })));
    }

    #[test]
    fn recovers_an_invoke_missing_its_leading_angle_bracket() {
        let tools = vec![tool("get_weather")];
        let mut parser = create_antml_invoke_recovery_stream_parser(tools, None);
        let mut events = parser.feed(r#"antml:invoke name="get_weather"><parameter name="city">Seoul</parameter></invoke>"#);
        events.extend(parser.finish());
        assert!(events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallStart { .. })));
    }

    #[test]
    fn plain_text_passes_through_untouched() {
        let tools = vec![tool("get_weather")];
        let mut parser = create_antml_invoke_recovery_stream_parser(tools, None);
        let mut events = parser.feed("hello world");
        events.extend(parser.finish());
        // senpi's idle path emits one text event per character; the concatenation is what callers see.
        assert_eq!(text_of(&events), "hello world");
        assert!(events.iter().all(|event| matches!(event, StreamParserEvent::Text { .. })));
    }

    #[test]
    fn a_bare_lowercase_a_that_is_not_an_opening_boundary_passes_through() {
        let tools = vec![tool("get_weather")];
        let mut parser = create_antml_invoke_recovery_stream_parser(tools, None);
        let mut events = parser.feed("banana");
        events.extend(parser.finish());
        assert_eq!(text_of(&events), "banana");
    }

    // ---- port of packages/ai/test/tool-call-middleware/invoke-recovery-parser.test.ts ----

    /// The suite's `bashTool`: `command` is required and has `minLength: 3`.
    fn bash_tool() -> Tool {
        Tool {
            name: "Bash".into(),
            description: "Run a command".into(),
            parameters: json!({
                "type": "object",
                "properties": { "command": { "type": "string", "minLength": 3 } },
                "required": ["command"],
            }),
            freeform: None,
            constrained_sampling: None,
        }
    }

    fn arguments(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        value.as_object().expect("object arguments").clone()
    }

    fn tool_call_events(events: &[StreamParserEvent]) -> Vec<StreamParserEvent> {
        events
            .iter()
            .filter(|event| {
                matches!(event, StreamParserEvent::ToolcallStart { .. } | StreamParserEvent::ToolcallDelta { .. } | StreamParserEvent::ToolcallEnd { .. })
            })
            .cloned()
            .collect()
    }

    #[test]
    fn emits_start_at_the_known_invoke_opening_boundary() {
        let mut parser = create_antml_invoke_recovery_stream_parser(vec![bash_tool()], None);
        let opening = parser.feed("<antml:invoke name=\"Bash\">");
        assert_eq!(opening, vec![StreamParserEvent::ToolcallStart { index: 0, name: "Bash".into(), id: "recovered-antml-0".into() }]);

        let mut completion = parser.feed("<antml:parameter name=\"command\">echo recovered</antml:parameter></antml:invoke>");
        completion.extend(parser.finish());
        assert_eq!(
            completion,
            vec![
                StreamParserEvent::ToolcallDelta { index: 0, arguments_delta: r#"{"command":"echo recovered"}"#.into() },
                StreamParserEvent::ToolcallEnd {
                    index: 0,
                    name: "Bash".into(),
                    id: "recovered-antml-0".into(),
                    arguments: arguments(json!({"command": "echo recovered"})),
                    incomplete: false,
                    error_message: None,
                },
            ]
        );
    }

    #[test]
    fn keeps_unknown_and_ambiguous_tools_as_text() {
        let unknown = r#"<function_calls><invoke name="Missing"></invoke></function_calls>"#;
        let ambiguous = r#"<invoke name="bash"></invoke>"#;
        let mut unknown_parser = create_antml_invoke_recovery_stream_parser(vec![bash_tool()], None);
        let mut ambiguous_tools = vec![bash_tool()];
        let mut upper = bash_tool();
        upper.name = "BASH".into();
        ambiguous_tools.push(upper);
        let mut ambiguous_parser = create_antml_invoke_recovery_stream_parser(ambiguous_tools, None);

        let mut unknown_events = unknown_parser.feed(unknown);
        unknown_events.extend(unknown_parser.finish());
        let mut ambiguous_events = ambiguous_parser.feed(ambiguous);
        ambiguous_events.extend(ambiguous_parser.finish());

        assert_eq!(text_of(&unknown_events), unknown);
        assert_eq!(text_of(&ambiguous_events), ambiguous);
        let mut both = unknown_events.clone();
        both.extend(ambiguous_events);
        assert_eq!(tool_call_events(&both), Vec::new());
    }

    #[test]
    fn finalizes_schema_failure_after_start_as_incomplete() {
        let mut parser = create_antml_invoke_recovery_stream_parser(vec![bash_tool()], None);
        let mut events = parser.feed("<invoke name=\"Bash\">");
        events.extend(parser.feed("<parameter name=\"command\">42</parameter></invoke>"));

        assert_eq!(
            events,
            vec![
                StreamParserEvent::ToolcallStart { index: 0, name: "Bash".into(), id: "recovered-antml-0".into() },
                StreamParserEvent::ToolcallDelta { index: 0, arguments_delta: "{}".into() },
                StreamParserEvent::ToolcallEnd {
                    index: 0,
                    name: "Bash".into(),
                    id: "recovered-antml-0".into(),
                    arguments: serde_json::Map::new(),
                    incomplete: true,
                    error_message: Some("Recovered tool call arguments failed validation".into()),
                },
            ]
        );
    }

    #[test]
    fn never_ends_or_makes_a_missing_invoke_close_executable() {
        let mut parser = create_antml_invoke_recovery_stream_parser(vec![bash_tool()], None);
        let mut events = parser.feed("<invoke name=\"Bash\"><parameter name=\"command\">echo no-close</parameter>");
        events.extend(parser.finish());

        assert_eq!(events, vec![StreamParserEvent::ToolcallStart { index: 0, name: "Bash".into(), id: "recovered-antml-0".into() }]);
    }

    #[test]
    fn preserves_nested_prompt_like_invokes_inside_an_active_parameter() {
        let mut parser = create_antml_invoke_recovery_stream_parser(vec![bash_tool()], None);
        let mut events =
            parser.feed("<invoke name=\"Bash\"><parameter name=\"command\">echo <invoke name=\"X\"></invoke></parameter></invoke>");
        events.extend(parser.finish());

        assert_eq!(
            events,
            vec![
                StreamParserEvent::ToolcallStart { index: 0, name: "Bash".into(), id: "recovered-antml-0".into() },
                StreamParserEvent::ToolcallDelta { index: 0, arguments_delta: r#"{"command":"echo <invoke name=\"X\"></invoke>"}"#.into() },
                StreamParserEvent::ToolcallEnd {
                    index: 0,
                    name: "Bash".into(),
                    id: "recovered-antml-0".into(),
                    arguments: arguments(json!({"command": "echo <invoke name=\"X\"></invoke>"})),
                    incomplete: false,
                    error_message: None,
                },
            ]
        );
    }

    #[test]
    fn consumes_the_wrapper_close_after_a_recovered_call() {
        let mut parser = create_antml_invoke_recovery_stream_parser(vec![bash_tool()], None);
        let mut events = parser
            .feed("<function_calls><invoke name=\"Bash\"><parameter name=\"command\">echo wrapped</parameter></invoke></function_calls>");
        events.extend(parser.finish());

        assert_eq!(text_of(&events), "");
        assert_eq!(
            tool_call_events(&events),
            vec![
                StreamParserEvent::ToolcallStart { index: 0, name: "Bash".into(), id: "recovered-antml-0".into() },
                StreamParserEvent::ToolcallDelta { index: 0, arguments_delta: r#"{"command":"echo wrapped"}"#.into() },
                StreamParserEvent::ToolcallEnd {
                    index: 0,
                    name: "Bash".into(),
                    id: "recovered-antml-0".into(),
                    arguments: arguments(json!({"command": "echo wrapped"})),
                    incomplete: false,
                    error_message: None,
                },
            ]
        );
    }

    #[test]
    fn preserves_unknown_invokes_after_a_recovered_wrapper_call() {
        let mut parser = create_antml_invoke_recovery_stream_parser(vec![bash_tool()], None);
        let mut events = parser.feed(
            "<function_calls><invoke name=\"Bash\"><parameter name=\"command\">one</parameter></invoke><invoke name=\"Missing\"></invoke></function_calls>",
        );
        events.extend(parser.finish());

        assert_eq!(text_of(&events), r#"<invoke name="Missing"></invoke>"#);
        assert_eq!(tool_call_events(&events).len(), 3);
    }

    #[test]
    fn preserves_tail_text_after_a_recovered_wrapper_call() {
        let mut parser = create_antml_invoke_recovery_stream_parser(vec![bash_tool()], None);
        let mut events = parser.feed(
            "<function_calls><invoke name=\"Bash\"><parameter name=\"command\">one</parameter></invoke>TAIL</function_calls>",
        );
        events.extend(parser.finish());

        assert_eq!(text_of(&events), "TAIL");
        assert_eq!(tool_call_events(&events).len(), 3);
    }
}
