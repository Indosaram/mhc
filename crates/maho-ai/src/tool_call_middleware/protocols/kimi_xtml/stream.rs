//! Port of senpi packages/ai/src/tool-call-middleware/protocols/kimi-xtml/stream.ts.

use serde_json::{Map, Value};

use super::markers::{get_partial_xtml_suffix, parse_xtml_attributes, XTML_ARGUMENT_CLOSE, XTML_ARGUMENT_OPEN, XTML_CALL_CLOSE, XTML_CALL_OPEN, XTML_SEP, XTML_TOOLS_CLOSE, XTML_TOOLS_OPEN};
use super::parse::{coerce_xtml_argument_value, CoercedXtmlValue};
use crate::tool_call_middleware::types::{ParserOptions, StreamParser, StreamParserEvent};
use crate::types::Tool;

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

const TEXT_BOUNDARY_TOKENS: [&str; 1] = [XTML_TOOLS_OPEN];
const TOOLS_BOUNDARY_TOKENS: [&str; 2] = [XTML_CALL_OPEN, XTML_TOOLS_CLOSE];

struct KimiXtmlStreamParser {
    tools: Vec<Tool>,
    options: Option<ParserOptions>,
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

impl KimiXtmlStreamParser {
    fn reset_call(&mut self) {
        self.call_name.clear();
        self.call_args.clear();
        self.call_started = false;
        self.call_invalid_reason = None;
        self.argument_key.clear();
        self.argument_type = None;
    }

    fn start_call(&mut self, events: &mut Vec<StreamParserEvent>, name: &str) {
        self.call_index += 1;
        self.call_name = name.to_string();
        self.call_started = true;
        events.push(StreamParserEvent::ToolcallStart { index: self.call_index as usize, name: name.to_string(), id: format!("kimi-xtml-tool-{}", self.call_index) });
    }

    fn end_call(&mut self, events: &mut Vec<StreamParserEvent>, incomplete: bool) {
        if !self.call_started {
            self.reset_call();
            return;
        }
        events.push(StreamParserEvent::ToolcallEnd {
            index: self.call_index as usize,
            name: self.call_name.clone(),
            id: format!("kimi-xtml-tool-{}", self.call_index),
            arguments: self.call_args.clone(),
            incomplete,
            error_message: if incomplete { self.call_invalid_reason.clone() } else { None },
        });
        self.reset_call();
        self.mode = ParserMode::Tools;
    }

    fn hold(&mut self, events: &mut Vec<StreamParserEvent>, tokens: &[&str], flush_as_text: bool) {
        let partial = get_partial_xtml_suffix(&self.buffer, tokens);
        if !flush_as_text {
            return;
        }
        let flushable = if partial.is_empty() { self.buffer.clone() } else { self.buffer[..self.buffer.len() - partial.len()].to_string() };
        if !flushable.is_empty() {
            events.push(StreamParserEvent::Text { text: flushable });
        }
        self.buffer = partial;
    }

    fn process(&mut self, events: &mut Vec<StreamParserEvent>) {
        loop {
            match self.mode {
                ParserMode::Text => {
                    let Some(start) = self.buffer.find(XTML_TOOLS_OPEN) else {
                        self.hold(events, &TEXT_BOUNDARY_TOKENS, true);
                        return;
                    };
                    if start > 0 {
                        events.push(StreamParserEvent::Text { text: self.buffer[..start].to_string() });
                    }
                    self.buffer = self.buffer[start + XTML_TOOLS_OPEN.len()..].to_string();
                    self.mode = ParserMode::Tools;
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
                    let Some(call_start) = call_start else {
                        self.hold(events, &TOOLS_BOUNDARY_TOKENS, false);
                        return;
                    };
                    self.buffer = self.buffer[call_start + XTML_CALL_OPEN.len()..].to_string();
                    self.mode = ParserMode::CallHeader;
                }
                ParserMode::CallHeader | ParserMode::ArgumentHeader => {
                    let Some(sep_index) = self.buffer.find(XTML_SEP) else {
                        self.hold(events, &[XTML_SEP], false);
                        return;
                    };
                    let attributes = parse_xtml_attributes(&self.buffer[..sep_index]);
                    self.buffer = self.buffer[sep_index + XTML_SEP.len()..].to_string();
                    if self.mode == ParserMode::CallHeader {
                        let name = attributes.get("tool").cloned().unwrap_or_default();
                        if !self.tools.iter().any(|candidate| candidate.name == name) {
                            if let Some(options) = &self.options {
                                options.report_error(&format!("kimi-xtml: call for unknown tool \"{name}\"."), Some(std::collections::HashMap::new()));
                            }
                            self.mode = ParserMode::DiscardCall;
                            continue;
                        }
                        self.start_call(events, &name);
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
                    let Some(arg_start) = arg_start else {
                        self.hold(events, &[XTML_ARGUMENT_OPEN, XTML_CALL_CLOSE], false);
                        return;
                    };
                    self.buffer = self.buffer[arg_start + XTML_ARGUMENT_OPEN.len()..].to_string();
                    self.mode = ParserMode::ArgumentHeader;
                }
                ParserMode::ArgumentValue => {
                    let Some(value_end) = self.buffer.find(XTML_ARGUMENT_CLOSE) else {
                        self.hold(events, &[XTML_ARGUMENT_CLOSE], false);
                        return;
                    };
                    let coerced = coerce_xtml_argument_value(&self.buffer[..value_end], self.argument_type.as_deref());
                    self.buffer = self.buffer[value_end + XTML_ARGUMENT_CLOSE.len()..].to_string();
                    match coerced {
                        CoercedXtmlValue::Err => {
                            let reason = format!("kimi-xtml: invalid value for argument \"{}\".", self.argument_key);
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
                    let Some(call_end) = self.buffer.find(XTML_CALL_CLOSE) else {
                        self.hold(events, &[XTML_CALL_CLOSE], false);
                        return;
                    };
                    self.buffer = self.buffer[call_end + XTML_CALL_CLOSE.len()..].to_string();
                    self.reset_call();
                    self.mode = ParserMode::Tools;
                }
            }
        }
    }
}

impl StreamParser for KimiXtmlStreamParser {
    fn feed(&mut self, text_delta: &str) -> Vec<StreamParserEvent> {
        self.buffer.push_str(text_delta);
        let mut events = Vec::new();
        self.process(&mut events);
        events
    }

    fn finish(&mut self) -> Vec<StreamParserEvent> {
        let mut events = Vec::new();
        if self.mode == ParserMode::Text {
            if !self.buffer.is_empty() {
                events.push(StreamParserEvent::Text { text: std::mem::take(&mut self.buffer) });
            }
            self.buffer.clear();
            return events;
        }
        if self.call_started {
            self.end_call(&mut events, true);
        }
        self.buffer.clear();
        self.mode = ParserMode::Text;
        events
    }
}

pub fn create_kimi_xtml_stream_parser(tools: Vec<Tool>, options: Option<ParserOptions>) -> Box<dyn StreamParser + Send> {
    Box::new(KimiXtmlStreamParser {
        tools,
        options,
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
    fn feeds_plain_text_when_no_tools_block_is_present() {
        let mut parser = create_kimi_xtml_stream_parser(vec![tool("t")], None);
        let events = parser.feed("hello world");
        assert_eq!(events, vec![StreamParserEvent::Text { text: "hello world".to_string() }]);
        assert!(parser.finish().is_empty());
    }

    #[test]
    fn parses_a_complete_call_across_feeds() {
        let mut parser = create_kimi_xtml_stream_parser(vec![tool("get_weather")], None);
        let text = format!(
            "before {XTML_TOOLS_OPEN}{XTML_CALL_OPEN}tool=\"get_weather\" index=\"1\"{XTML_SEP}{XTML_ARGUMENT_OPEN}key=\"city\" type=\"string\"{XTML_SEP}Seoul{XTML_ARGUMENT_CLOSE}{XTML_CALL_CLOSE}{XTML_TOOLS_CLOSE} after"
        );
        let mut events = parser.feed(&text);
        events.extend(parser.finish());
        assert!(events.iter().any(|e| matches!(e, StreamParserEvent::Text{text} if text == "before ")));
        assert!(events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallStart{name, ..} if name == "get_weather")));
        assert!(events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallEnd{name, arguments, incomplete: false, ..} if name == "get_weather" && arguments.get("city") == Some(&json!("Seoul")))));
        assert!(events.iter().any(|e| matches!(e, StreamParserEvent::Text{text} if text == " after")));
    }

    #[test]
    fn finish_marks_an_unterminated_call_incomplete() {
        let mut parser = create_kimi_xtml_stream_parser(vec![tool("get_weather")], None);
        parser.feed(&format!("{XTML_TOOLS_OPEN}{XTML_CALL_OPEN}tool=\"get_weather\" index=\"1\"{XTML_SEP}"));
        let events = parser.finish();
        assert!(events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallEnd{incomplete: true, ..})));
    }

    #[test]
    fn discards_calls_for_unknown_tools() {
        let mut parser = create_kimi_xtml_stream_parser(vec![tool("get_weather")], None);
        let text = format!("{XTML_TOOLS_OPEN}{XTML_CALL_OPEN}tool=\"unknown\" index=\"1\"{XTML_SEP}{XTML_CALL_CLOSE}{XTML_TOOLS_CLOSE}");
        let mut events = parser.feed(&text);
        events.extend(parser.finish());
        assert!(!events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallStart{..})));
    }
}
