//! Port of senpi packages/ai/src/tool-call-middleware/protocols/kimi-xtml/recovery-stream.ts.

use serde_json::{Map, Value};

use crate::tool_call_middleware::protocols::anthropic_xml::recovery_stream::RecoveryStreamParser;
use crate::tool_call_middleware::recovery_code_mask::{create_recovery_code_mask, RecoveryCodeMask, RecoveryCodeMaskSegment};
use crate::tool_call_middleware::types::{ParserOptions, StreamParser, StreamParserEvent};
use crate::types::Tool;

use super::markers::{
    get_partial_xtml_suffix, match_xtml_channel_marker, parse_xtml_attributes, XTML_ARGUMENT_CLOSE, XTML_ARGUMENT_OPEN, XTML_CALL_CLOSE,
    XTML_CALL_OPEN, XTML_CLOSE_PREFIX, XTML_OPEN_PREFIX, XTML_SEP, XTML_TOOLS_CLOSE, XTML_TOOLS_OPEN,
};
use super::parse::{coerce_xtml_argument_value, CoercedXtmlValue};

#[derive(Clone, Copy, PartialEq, Eq)]
enum ParserMode {
    Text,
    Tools,
    CallHeader,
    CallBody,
    ArgumentHeader,
    ArgumentValue,
    DiscardCall,
}

const MARKER_PREFIXES: [&str; 3] = [XTML_OPEN_PREFIX, XTML_CLOSE_PREFIX, XTML_SEP];
const STRUCTURAL_MARKER_OPENS: [&str; 2] = [XTML_CALL_OPEN, XTML_ARGUMENT_OPEN];

fn find_marker_start(text: &str) -> Option<usize> {
    MARKER_PREFIXES.iter().filter_map(|prefix| text.find(prefix)).min()
}

fn could_extend_marker(buffer: &str, marker: &str) -> bool {
    if marker.ends_with(XTML_SEP) {
        return false;
    }
    let rest = &buffer[marker.len()..];
    rest.is_empty() || XTML_SEP.starts_with(rest)
}

fn starts_complete_structural_marker(buffer: &str) -> bool {
    STRUCTURAL_MARKER_OPENS.iter().any(|open| buffer.starts_with(open))
}

fn starts_partial_structural_marker(buffer: &str) -> bool {
    STRUCTURAL_MARKER_OPENS.iter().any(|open| buffer.len() < open.len() && open.starts_with(buffer))
}

struct XtmlRecoveryStreamParser {
    tools: Vec<Tool>,
    options: Option<ParserOptions>,
    mask: RecoveryCodeMask,
    buffer: String,
    mode: ParserMode,
    call_index: i64,
    call_name: String,
    call_args: Map<String, Value>,
    call_started: bool,
    call_invalid_reason: Option<String>,
    argument_key: String,
    argument_type: Option<String>,
}

impl XtmlRecoveryStreamParser {
    fn reset_call(&mut self) {
        self.call_name.clear();
        self.call_args.clear();
        self.call_started = false;
        self.call_invalid_reason = None;
        self.argument_key.clear();
        self.argument_type = None;
    }

    fn end_call(&mut self, events: &mut Vec<StreamParserEvent>, incomplete: bool) {
        if !self.call_started {
            self.reset_call();
            return;
        }
        events.push(StreamParserEvent::ToolcallEnd {
            index: self.call_index as usize,
            name: self.call_name.clone(),
            id: format!("recovered-xtml-{}", self.call_index),
            arguments: self.call_args.clone(),
            incomplete,
            error_message: if incomplete { self.call_invalid_reason.clone() } else { None },
        });
        self.reset_call();
        self.mode = ParserMode::Tools;
    }

    fn process_text(&mut self, events: &mut Vec<StreamParserEvent>) -> bool {
        let Some(marker_index) = find_marker_start(&self.buffer) else {
            let partial = get_partial_xtml_suffix(&self.buffer, &MARKER_PREFIXES);
            let flushable = if partial.is_empty() { self.buffer.clone() } else { self.buffer[..self.buffer.len() - partial.len()].to_string() };
            if !flushable.is_empty() {
                events.push(StreamParserEvent::Text { text: flushable });
            }
            self.buffer = partial;
            return false;
        };
        if marker_index > 0 {
            events.push(StreamParserEvent::Text { text: self.buffer[..marker_index].to_string() });
            self.buffer = self.buffer[marker_index..].to_string();
            return true;
        }
        if starts_complete_structural_marker(&self.buffer) {
            self.mode = ParserMode::Tools;
            return true;
        }
        if starts_partial_structural_marker(&self.buffer) {
            return false;
        }
        if let Some(marker) = match_xtml_channel_marker(&self.buffer) {
            if could_extend_marker(&self.buffer, &marker) {
                return false;
            }
            self.buffer = self.buffer[marker.len()..].to_string();
            if marker == XTML_TOOLS_OPEN {
                self.mode = ParserMode::Tools;
            }
            return true;
        }
        let take = self.buffer.chars().take(2).map(char::len_utf8).sum::<usize>().min(self.buffer.len());
        events.push(StreamParserEvent::Text { text: self.buffer[..take].to_string() });
        self.buffer = self.buffer[take..].to_string();
        true
    }

    fn process(&mut self, events: &mut Vec<StreamParserEvent>) {
        loop {
            match self.mode {
                ParserMode::Text => {
                    if !self.process_text(events) {
                        return;
                    }
                }
                ParserMode::Tools => {
                    let call_start = self.buffer.find(XTML_CALL_OPEN);
                    let tools_end = self.buffer.find(XTML_TOOLS_CLOSE);
                    if let Some(tools_end) = tools_end
                        && call_start.is_none_or(|call_start| tools_end < call_start)
                    {
                        self.buffer = self.buffer[tools_end + XTML_TOOLS_CLOSE.len()..].to_string();
                        self.mode = ParserMode::Text;
                        continue;
                    }
                    let Some(call_start) = call_start else { return };
                    self.buffer = self.buffer[call_start + XTML_CALL_OPEN.len()..].to_string();
                    self.mode = ParserMode::CallHeader;
                }
                ParserMode::CallHeader | ParserMode::ArgumentHeader => {
                    let Some(sep_index) = self.buffer.find(XTML_SEP) else { return };
                    let attributes = parse_xtml_attributes(&self.buffer[..sep_index]);
                    self.buffer = self.buffer[sep_index + XTML_SEP.len()..].to_string();
                    if self.mode == ParserMode::CallHeader {
                        let name = attributes.get("tool").cloned().unwrap_or_default();
                        if !self.tools.iter().any(|candidate| candidate.name == name) {
                            if let Some(options) = &self.options {
                                options.report_error(&format!("kimi-xtml recovery: call for unknown tool \"{name}\"."), Some(std::collections::HashMap::new()));
                            }
                            self.mode = ParserMode::DiscardCall;
                            continue;
                        }
                        self.call_index += 1;
                        self.call_name = name.clone();
                        self.call_started = true;
                        events.push(StreamParserEvent::ToolcallStart { index: self.call_index as usize, name, id: format!("recovered-xtml-{}", self.call_index) });
                        self.mode = ParserMode::CallBody;
                    } else {
                        self.argument_key = attributes.get("key").cloned().unwrap_or_default();
                        self.argument_type = attributes.get("type").cloned();
                        self.mode = ParserMode::ArgumentValue;
                    }
                }
                ParserMode::CallBody => {
                    let arg_start = self.buffer.find(XTML_ARGUMENT_OPEN);
                    let call_end = self.buffer.find(XTML_CALL_CLOSE);
                    if let Some(call_end) = call_end
                        && arg_start.is_none_or(|arg_start| call_end < arg_start)
                    {
                        self.buffer = self.buffer[call_end + XTML_CALL_CLOSE.len()..].to_string();
                        let incomplete = self.call_invalid_reason.is_some();
                        self.end_call(events, incomplete);
                        continue;
                    }
                    let Some(arg_start) = arg_start else { return };
                    self.buffer = self.buffer[arg_start + XTML_ARGUMENT_OPEN.len()..].to_string();
                    self.mode = ParserMode::ArgumentHeader;
                }
                ParserMode::ArgumentValue => {
                    let Some(value_end) = self.buffer.find(XTML_ARGUMENT_CLOSE) else { return };
                    let coerced = coerce_xtml_argument_value(&self.buffer[..value_end], self.argument_type.as_deref());
                    self.buffer = self.buffer[value_end + XTML_ARGUMENT_CLOSE.len()..].to_string();
                    match coerced {
                        CoercedXtmlValue::Err => {
                            let reason = format!("kimi-xtml recovery: invalid value for argument \"{}\".", self.argument_key);
                            self.call_invalid_reason = Some(reason.clone());
                            if let Some(options) = &self.options {
                                options.report_error(&reason, Some(std::collections::HashMap::from([("toolCall".to_string(), Value::String(self.call_name.clone()))])));
                            }
                        }
                        CoercedXtmlValue::Ok(value) => {
                            if !self.argument_key.is_empty() {
                                self.call_args.insert(self.argument_key.clone(), value);
                                events.push(StreamParserEvent::ToolcallDelta { index: self.call_index as usize, arguments_delta: Value::Object(self.call_args.clone()).to_string() });
                            }
                        }
                    }
                    self.mode = ParserMode::CallBody;
                }
                ParserMode::DiscardCall => {
                    let Some(call_end) = self.buffer.find(XTML_CALL_CLOSE) else { return };
                    self.buffer = self.buffer[call_end + XTML_CALL_CLOSE.len()..].to_string();
                    self.reset_call();
                    self.mode = ParserMode::Tools;
                }
            }
        }
    }

    fn consume(&mut self, segments: Vec<RecoveryCodeMaskSegment>) -> Vec<StreamParserEvent> {
        let mut events = Vec::new();
        for segment in segments {
            if segment.scan {
                self.buffer.push_str(&segment.text);
                self.process(&mut events);
                continue;
            }
            if self.mode == ParserMode::Text && !self.buffer.is_empty() {
                events.push(StreamParserEvent::Text { text: std::mem::take(&mut self.buffer) });
            }
            if self.mode == ParserMode::Text {
                events.push(StreamParserEvent::Text { text: segment.text });
            } else {
                self.buffer.push_str(&segment.text);
            }
        }
        events
    }
}

impl StreamParser for XtmlRecoveryStreamParser {
    fn feed(&mut self, text_delta: &str) -> Vec<StreamParserEvent> {
        let segments = self.mask.feed(text_delta, None);
        self.consume(segments)
    }

    fn finish(&mut self) -> Vec<StreamParserEvent> {
        let segments = self.mask.finish();
        let mut events = self.consume(segments);
        if self.mode == ParserMode::Text {
            let mut rest = self.buffer.as_str();
            while let Some(marker) = match_xtml_channel_marker(rest) {
                if marker.is_empty() {
                    break;
                }
                rest = &rest[marker.len()..];
            }
            let mut rest = rest.to_string();
            if XTML_SEP.starts_with(rest.as_str()) && !rest.is_empty() {
                rest = String::new();
            }
            if !rest.is_empty() {
                events.push(StreamParserEvent::Text { text: rest });
            }
        } else if self.call_started {
            self.end_call(&mut events, true);
        }
        self.buffer.clear();
        self.mode = ParserMode::Text;
        events
    }
}

impl RecoveryStreamParser for XtmlRecoveryStreamParser {
    fn interrupt(&mut self) -> Vec<StreamParserEvent> {
        let mut events = Vec::new();
        if self.mode == ParserMode::Text && !self.buffer.is_empty() {
            events.push(StreamParserEvent::Text { text: std::mem::take(&mut self.buffer) });
        }
        events
    }
}

pub fn create_xtml_recovery_stream_parser(tools: Vec<Tool>, options: Option<ParserOptions>) -> Box<dyn RecoveryStreamParser + Send> {
    Box::new(XtmlRecoveryStreamParser {
        tools,
        options,
        mask: create_recovery_code_mask(),
        buffer: String::new(),
        mode: ParserMode::Text,
        call_index: -1,
        call_name: String::new(),
        call_args: Map::new(),
        call_started: false,
        call_invalid_reason: None,
        argument_key: String::new(),
        argument_type: None,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn tool(name: &str) -> Tool {
        Tool { name: name.into(), description: "d".into(), parameters: json!({"type": "object"}), freeform: None, constrained_sampling: None }
    }

    #[test]
    fn recovers_a_leaked_call_outside_any_tools_wrapper() {
        let mut parser = create_xtml_recovery_stream_parser(vec![tool("get_weather")], None);
        let text = format!(
            "before {XTML_TOOLS_OPEN}{XTML_CALL_OPEN}tool=\"get_weather\" index=\"1\"{XTML_SEP}{XTML_ARGUMENT_OPEN}key=\"city\" type=\"string\"{XTML_SEP}Seoul{XTML_ARGUMENT_CLOSE}{XTML_CALL_CLOSE}{XTML_TOOLS_CLOSE} after"
        );
        let mut events = parser.feed(&text);
        events.extend(parser.finish());
        assert!(events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallStart{name, ..} if name == "get_weather")));
        assert!(events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallEnd{incomplete: false, arguments, ..} if arguments.get("city") == Some(&json!("Seoul")))));
    }

    #[test]
    fn plain_text_passes_through_untouched() {
        let mut parser = create_xtml_recovery_stream_parser(vec![tool("get_weather")], None);
        let mut events = parser.feed("hello world");
        events.extend(parser.finish());
        let text: String = events.iter().filter_map(|e| if let StreamParserEvent::Text { text } = e { Some(text.as_str()) } else { None }).collect();
        assert_eq!(text, "hello world");
    }

    #[test]
    fn backtick_fenced_content_is_not_scanned_for_markers() {
        let mut parser = create_xtml_recovery_stream_parser(vec![tool("get_weather")], None);
        let text = format!("```\n{XTML_TOOLS_OPEN}not a real call{XTML_TOOLS_CLOSE}\n```");
        let mut events = parser.feed(&text);
        events.extend(parser.finish());
        assert!(!events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallStart { .. })));
    }

    #[test]
    fn interrupt_flushes_buffered_text() {
        let mut parser = create_xtml_recovery_stream_parser(vec![tool("get_weather")], None);
        parser.feed("partial text");
        let events = parser.interrupt();
        assert_eq!(events, vec![StreamParserEvent::Text { text: "partial text".into() }]);
    }
}
