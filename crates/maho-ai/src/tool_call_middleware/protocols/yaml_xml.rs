//! Port of senpi packages/ai/src/tool-call-middleware/protocols/yaml-xml.ts.

use serde_json::{Map, Value};

use super::xml_tool_tag_scanner::{find_earliest_xml_tool_tag, get_safe_xml_text_length};
use crate::tool_call_middleware::types::{ParsedToolCall, ParserOptions, StreamParser, StreamParserEvent, ToolResultContent};
use crate::types::Tool;
use crate::utils::validation::validate_tool_arguments;

fn normalize_yaml_content(yaml_content: &str) -> String {
    let trimmed_leading_newline = yaml_content.strip_prefix('\n').unwrap_or(yaml_content);
    let lines: Vec<&str> = trimmed_leading_newline.split('\n').collect();
    let non_empty_lines: Vec<&str> = lines.iter().filter(|line| !line.trim().is_empty()).copied().collect();
    if non_empty_lines.is_empty() {
        return String::new();
    }

    let min_indent = non_empty_lines.iter().map(|line| line.len() - line.trim_start_matches(' ').len()).min().unwrap_or(0);
    if min_indent > 0 {
        lines.iter().map(|line| if line.len() >= min_indent { &line[min_indent..] } else { *line }).collect::<Vec<_>>().join("\n")
    } else {
        trimmed_leading_newline.to_string()
    }
}

fn yaml_value_to_json(value: serde_yaml::Value) -> Option<Value> {
    serde_json::to_value(value).ok()
}

/// `Some(None)` mirrors TS `null` (invalid/non-object YAML); `Some(Some(map))` mirrors a parsed
/// mapping; `None` mirrors a YAML parse error.
fn parse_yaml_mapping(yaml_content: &str) -> Option<Option<Map<String, Value>>> {
    let normalized = normalize_yaml_content(yaml_content);
    if normalized.trim().is_empty() {
        return Some(Some(Map::new()));
    }

    let parsed: serde_yaml::Value = match serde_yaml::from_str(&normalized) {
        Ok(value) => value,
        Err(_) => return None,
    };

    if parsed.is_null() {
        return Some(Some(Map::new()));
    }

    match yaml_value_to_json(parsed) {
        Some(Value::Object(map)) => Some(Some(map)),
        Some(_) => Some(None),
        None => None,
    }
}

/// `</\s*name\s*>` scanned from `content_start`; returns the absolute end index of the match.
fn find_closing_tag_end(text: &str, content_start: usize, tool_name: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = content_start;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let mut cursor = i + 1;
        if cursor >= bytes.len() || bytes[cursor] != b'/' {
            i += 1;
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if !text[cursor..].starts_with(tool_name) {
            i += 1;
            continue;
        }
        cursor += tool_name.len();
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor < bytes.len() && bytes[cursor] == b'>' {
            return Some(cursor + 1);
        }
        i += 1;
    }
    None
}

/// The first `</\s*name\s*>` match within `text` (start, end), or `None`.
fn find_closing_tag_match(text: &str, tool_name: &str) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let mut cursor = i + 1;
        if cursor >= bytes.len() || bytes[cursor] != b'/' {
            i += 1;
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if !text[cursor..].starts_with(tool_name) {
            i += 1;
            continue;
        }
        cursor += tool_name.len();
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor < bytes.len() && bytes[cursor] == b'>' {
            return Some((i, cursor + 1));
        }
        i += 1;
    }
    None
}

fn should_emit_raw_tool_call_text_on_error(options: Option<&ParserOptions>) -> bool {
    options.is_some_and(|o| o.emit_raw_tool_call_text_on_error)
}

pub fn yaml_xml_format_tools_system_prompt(tools: &[Tool]) -> String {
    if tools.is_empty() {
        return String::new();
    }

    let tools_rendered: Vec<Value> = tools
        .iter()
        .map(|tool| {
            let mut map = Map::new();
            map.insert("name".to_string(), Value::String(tool.name.clone()));
            map.insert("description".to_string(), Value::String(tool.description.clone()));
            map.insert("parameters".to_string(), tool.parameters.clone());
            Value::Object(map)
        })
        .collect();
    let tools_rendered = Value::Array(tools_rendered).to_string();

    format!(
        "# Tools\n\nYou may call one or more functions to assist with the user query.\n\nYou are provided with function signatures within <tools></tools> XML tags:\n<tools>{tools_rendered}</tools>\n\n# Format\n\nUse exactly one XML element whose tag name is the function name.\nInside the XML element, specify parameters using YAML syntax (key: value pairs).\n\n# Example\n<get_weather>\ncity: Seoul\nunit: celsius\n</get_weather>"
    )
}

pub fn yaml_xml_format_tool_call(name: &str, args: &Map<String, Value>) -> String {
    let yaml_body = serde_yaml::to_string(args).unwrap_or_default().trim_end().to_string();
    format!("<{name}>\n{}\n</{name}>", if yaml_body.is_empty() { "null" } else { &yaml_body })
}

pub fn yaml_xml_format_tool_response(tool_name: &str, _tool_call_id: &str, content: &[ToolResultContent]) -> String {
    let text_content = content.iter().filter_map(ToolResultContent::as_text).map(|entry| entry.text.as_str()).collect::<Vec<_>>().join("\n");
    let mut lines = vec!["<tool_response>".to_string(), format!("tool_name: {tool_name}"), "result: |-".to_string()];
    lines.extend(text_content.split('\n').map(|line| format!("  {line}")));
    lines.push("</tool_response>".to_string());
    lines.join("\n")
}

pub fn parse_yaml_xml_generated_text(text: &str, tools: &[Tool], options: Option<&ParserOptions>) -> Vec<ParsedToolCall> {
    if tools.is_empty() || text.is_empty() {
        return Vec::new();
    }

    let mut parsed_tool_calls = Vec::new();
    let tool_names: Vec<String> = tools.iter().map(|tool| tool.name.clone()).collect();
    let mut cursor = 0usize;

    while cursor < text.len() {
        let Some(opening_tag) = find_earliest_xml_tool_tag(&text[cursor..], &tool_names) else { break };

        let absolute_index = cursor + opening_tag.index;
        if opening_tag.self_closing {
            parsed_tool_calls.push(ParsedToolCall { name: opening_tag.name.clone(), arguments: Map::new() });
            cursor = absolute_index + opening_tag.tag.len();
            continue;
        }

        let content_start = absolute_index + opening_tag.tag.len();
        let Some(closing_tag_end) = find_closing_tag_end(text, content_start, &opening_tag.name) else { break };

        let closing_tag_text = &text[content_start..closing_tag_end];
        let Some((closing_tag_index, _)) = find_closing_tag_match(closing_tag_text, &opening_tag.name) else {
            cursor = closing_tag_end;
            continue;
        };

        let yaml_content = &closing_tag_text[..closing_tag_index];
        match parse_yaml_mapping(yaml_content) {
            Some(Some(parsed_arguments)) => parsed_tool_calls.push(ParsedToolCall { name: opening_tag.name.clone(), arguments: parsed_arguments }),
            Some(None) | None => {
                if let Some(options) = options {
                    options.report_error(
                        "Could not process YAML XML tool call, keeping original text.",
                        Some(std::collections::HashMap::from([("toolCall".to_string(), Value::String(text[absolute_index..closing_tag_end].to_string()))])),
                    );
                }
            }
        }

        cursor = closing_tag_end;
    }

    parsed_tool_calls
}

struct YamlXmlToolState {
    id: String,
    index: usize,
    name: String,
    last_arguments_snapshot: Option<String>,
}

struct YamlXmlStreamParser {
    tools: Vec<Tool>,
    tool_names: Vec<String>,
    options: Option<ParserOptions>,
    buffer: String,
    current_tool_state: Option<YamlXmlToolState>,
    next_tool_call_index: usize,
}

impl YamlXmlStreamParser {
    fn emit_snapshot(&mut self, events: &mut Vec<StreamParserEvent>, yaml_content: &str) {
        let Some(Some(parsed_arguments)) = parse_yaml_mapping(yaml_content) else { return };
        let Some(tool_state) = &mut self.current_tool_state else { return };

        let snapshot = Value::Object(parsed_arguments).to_string();
        if snapshot == "{}" || Some(&snapshot) == tool_state.last_arguments_snapshot.as_ref() {
            return;
        }

        if tool_state.last_arguments_snapshot.is_none() {
            events.push(StreamParserEvent::ToolcallStart { index: tool_state.index, name: tool_state.name.clone(), id: tool_state.id.clone() });
        }

        tool_state.last_arguments_snapshot = Some(snapshot.clone());
        events.push(StreamParserEvent::ToolcallDelta { index: tool_state.index, arguments_delta: snapshot });
    }

    fn process_buffer(&mut self) -> Vec<StreamParserEvent> {
        let mut events = Vec::new();

        while !self.buffer.is_empty() {
            if let Some(tool_state) = &self.current_tool_state {
                let tool_name = tool_state.name.clone();
                let closing_tag_match = find_closing_tag_match(&self.buffer, &tool_name);

                let Some((closing_start, closing_end)) = closing_tag_match else {
                    let buffer = self.buffer.clone();
                    self.emit_snapshot(&mut events, &buffer);
                    break;
                };

                let yaml_content = self.buffer[..closing_start].to_string();
                let original_call_text = format!("<{tool_name}>{yaml_content}{}", &self.buffer[closing_start..closing_end]);
                let parsed = parse_yaml_mapping(&yaml_content);
                self.buffer = self.buffer[closing_end..].to_string();

                match parsed {
                    Some(Some(parsed_arguments)) => {
                        self.emit_snapshot(&mut events, &yaml_content);
                        let tool_state = self.current_tool_state.as_mut().expect("checked above");
                        if tool_state.last_arguments_snapshot.is_none() {
                            events.push(StreamParserEvent::ToolcallStart { index: tool_state.index, name: tool_state.name.clone(), id: tool_state.id.clone() });
                        }
                        events.push(StreamParserEvent::ToolcallEnd {
                            index: tool_state.index,
                            name: tool_state.name.clone(),
                            id: tool_state.id.clone(),
                            arguments: parsed_arguments,
                            incomplete: false,
                            error_message: None,
                        });
                        self.current_tool_state = None;
                    }
                    Some(None) | None => {
                        if let Some(options) = &self.options {
                            options.report_error(
                                "Could not process YAML XML tool call, keeping original text.",
                                Some(std::collections::HashMap::from([("toolCall".to_string(), Value::String(original_call_text.clone()))])),
                            );
                        }
                        let tool_state = self.current_tool_state.as_ref().expect("checked above");
                        if tool_state.last_arguments_snapshot.is_some() {
                            events.push(StreamParserEvent::ToolcallEnd {
                                index: tool_state.index,
                                name: tool_state.name.clone(),
                                id: tool_state.id.clone(),
                                arguments: Map::new(),
                                incomplete: true,
                                error_message: Some("Tool call arguments could not be parsed".to_string()),
                            });
                        }
                        if should_emit_raw_tool_call_text_on_error(self.options.as_ref()) {
                            events.push(StreamParserEvent::Text { text: original_call_text });
                        }
                        self.current_tool_state = None;
                    }
                }
                continue;
            }

            let opening_tag = find_earliest_xml_tool_tag(&self.buffer, &self.tool_names);
            let Some(opening_tag) = opening_tag else {
                let text_length = get_safe_xml_text_length(&self.buffer, &self.tool_names);
                if text_length == 0 {
                    break;
                }
                events.push(StreamParserEvent::Text { text: self.buffer[..text_length].to_string() });
                self.buffer = self.buffer[text_length..].to_string();
                continue;
            };

            if opening_tag.index > 0 {
                events.push(StreamParserEvent::Text { text: self.buffer[..opening_tag.index].to_string() });
            }
            self.buffer = self.buffer[opening_tag.index + opening_tag.tag.len()..].to_string();

            if opening_tag.self_closing {
                let id = format!("yaml-xml-tool-{}", self.next_tool_call_index);
                let index = self.next_tool_call_index;
                self.next_tool_call_index += 1;
                events.push(StreamParserEvent::ToolcallStart { index, name: opening_tag.name.clone(), id: id.clone() });
                events.push(StreamParserEvent::ToolcallEnd { index, name: opening_tag.name.clone(), id, arguments: Map::new(), incomplete: false, error_message: None });
                continue;
            }

            self.current_tool_state = Some(YamlXmlToolState {
                id: format!("yaml-xml-tool-{}", self.next_tool_call_index),
                index: self.next_tool_call_index,
                name: opening_tag.name.clone(),
                last_arguments_snapshot: None,
            });
            self.next_tool_call_index += 1;
        }

        events
    }
}

impl StreamParser for YamlXmlStreamParser {
    fn feed(&mut self, text_delta: &str) -> Vec<StreamParserEvent> {
        if text_delta.is_empty() {
            return Vec::new();
        }
        self.buffer.push_str(text_delta);
        self.process_buffer()
    }

    fn finish(&mut self) -> Vec<StreamParserEvent> {
        let mut events = self.process_buffer();

        if self.current_tool_state.is_none() && !self.buffer.is_empty() {
            events.push(StreamParserEvent::Text { text: std::mem::take(&mut self.buffer) });
        }

        if let Some(tool_state) = self.current_tool_state.take() {
            let parsed_arguments = parse_yaml_mapping(&self.buffer);
            let tool = self.tools.iter().find(|candidate| candidate.name == tool_state.name);

            let validated_arguments = match (&parsed_arguments, tool) {
                (Some(Some(parsed)), Some(tool)) => {
                    let tool_call = crate::types::ToolCall {
                        id: tool_state.id.clone(),
                        name: tool_state.name.clone(),
                        arguments: parsed.clone(),
                        incomplete: None,
                        error_message: None,
                        thought_signature: None,
                        namespace: None,
                    };
                    match validate_tool_arguments(tool, &tool_call) {
                        Ok(Value::Object(validated)) => Some(validated),
                        Ok(_) | Err(_) => None,
                    }
                }
                _ => None,
            };

            if let Some(validated_arguments) = validated_arguments {
                let buffer = self.buffer.clone();
                self.emit_snapshot(&mut events, &buffer);
                if tool_state.last_arguments_snapshot.is_none() {
                    events.push(StreamParserEvent::ToolcallStart { index: tool_state.index, name: tool_state.name.clone(), id: tool_state.id.clone() });
                }
                events.push(StreamParserEvent::ToolcallEnd {
                    index: tool_state.index,
                    name: tool_state.name.clone(),
                    id: tool_state.id.clone(),
                    arguments: validated_arguments,
                    incomplete: false,
                    error_message: None,
                });
            } else {
                if tool_state.last_arguments_snapshot.is_none() {
                    events.push(StreamParserEvent::ToolcallStart { index: tool_state.index, name: tool_state.name.clone(), id: tool_state.id.clone() });
                }
                let fallback_arguments = match parsed_arguments {
                    Some(Some(parsed)) => parsed,
                    _ => Map::new(),
                };
                events.push(StreamParserEvent::ToolcallEnd {
                    index: tool_state.index,
                    name: tool_state.name.clone(),
                    id: tool_state.id.clone(),
                    arguments: fallback_arguments,
                    incomplete: true,
                    error_message: Some("Tool call was truncated mid-arguments".to_string()),
                });
                if let Some(options) = &self.options {
                    options.report_error(
                        "Could not complete streaming YAML XML tool call at finish.",
                        Some(std::collections::HashMap::from([
                            ("protocol".to_string(), Value::String("yaml-xml".to_string())),
                            ("retainedLength".to_string(), Value::from(self.buffer.len())),
                        ])),
                    );
                }
            }

            self.buffer.clear();
        }

        events
    }
}

pub fn create_yaml_xml_stream_parser(tools: Vec<Tool>, options: Option<ParserOptions>) -> Box<dyn StreamParser + Send> {
    let tool_names = tools.iter().map(|tool| tool.name.clone()).collect();
    Box::new(YamlXmlStreamParser { tools, tool_names, options, buffer: String::new(), current_tool_state: None, next_tool_call_index: 0 })
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_call_middleware::types::StreamParserEvent;
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

    fn write_file_tool() -> Tool {
        tool(
            "write_file",
            "Write a file",
            json!({"type": "object", "required": ["file_path", "contents"], "properties": {"file_path": {"type": "string"}, "contents": {"type": "string"}}}),
        )
    }

    fn get_location_tool() -> Tool {
        tool("get_location", "Get location", json!({"type": "object", "properties": {}}))
    }

    fn fixture_tools() -> Vec<Tool> {
        vec![
            tool("get_weather", "Get weather", json!({"type": "object", "required": ["city"], "properties": {"city": {"type": "string"}, "days": {"type": "integer"}}})),
            tool(
                "todowrite",
                "Write todos",
                json!({"type": "object", "required": ["todos"], "properties": {"todos": {"type": "array", "minItems": 1, "items": {"type": "object", "required": ["content", "status", "priority"], "properties": {"content": {"type": "string"}, "status": {"type": "string"}, "priority": {"type": "string"}}}}}}),
            ),
            get_location_tool(),
        ]
    }

    fn error_collector() -> (Arc<Mutex<Vec<String>>>, crate::tool_call_middleware::types::ParserErrorHandler) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let handler: crate::tool_call_middleware::types::ParserErrorHandler = Arc::new(move |message: &str, _metadata: Option<&HashMap<String, Value>>| {
            sink.lock().expect("error sink").push(message.to_string());
        });
        (seen, handler)
    }

    fn options_with(emit_raw: bool, handler: crate::tool_call_middleware::types::ParserErrorHandler) -> ParserOptions {
        ParserOptions { emit_raw_tool_call_text_on_error: emit_raw, on_error: Some(handler) }
    }

    fn text_of(events: &[StreamParserEvent]) -> String {
        events.iter().filter_map(|event| if let StreamParserEvent::Text { text } = event { Some(text.as_str()) } else { None }).collect()
    }

    fn is_toolcall_event(event: &StreamParserEvent) -> bool {
        matches!(event, StreamParserEvent::ToolcallStart { .. } | StreamParserEvent::ToolcallDelta { .. } | StreamParserEvent::ToolcallEnd { .. })
    }

    fn feed_all(parser: &mut Box<dyn StreamParser + Send>, input: &str) -> Vec<StreamParserEvent> {
        let mut events = parser.feed(input);
        events.extend(parser.finish());
        events
    }

    fn end_of(events: &[StreamParserEvent]) -> Option<&StreamParserEvent> {
        events.iter().find(|event| matches!(event, StreamParserEvent::ToolcallEnd { .. }))
    }

    fn arg<'a>(call: &'a ParsedToolCall, key: &str) -> Option<&'a Value> {
        call.arguments.get(key)
    }

    // Deterministic chunking that mirrors the TS randomChunkSplit helper.
    fn random_chunk_split(text: &str, min_size: usize, max_size: usize, seed: u64) -> Vec<String> {
        let mut current = seed;
        let mut chunks = Vec::new();
        let chars: Vec<char> = text.chars().collect();
        let mut index = 0usize;
        while index < chars.len() {
            current = (current * 9301 + 49_297) % 233_280;
            let size = ((current as f64 / 233_280.0) * (max_size - min_size + 1) as f64).floor() as usize + min_size;
            let end = (index + size).min(chars.len());
            chunks.push(chars[index..end].iter().collect());
            index = end;
        }
        chunks
    }

    #[test]
    fn formats_object_arguments_as_yaml_inside_an_xml_tag() {
        let mut args = Map::new();
        args.insert("city".to_string(), Value::String("Seoul".into()));
        args.insert("unit".to_string(), Value::String("celsius".into()));
        let formatted = yaml_xml_format_tool_call("get_weather", &args);
        assert!(formatted.contains("<get_weather>"));
        assert!(formatted.contains("city: Seoul"));
        assert!(formatted.contains("unit: celsius"));
        assert!(formatted.contains("</get_weather>"));
    }

    #[test]
    fn parses_a_yaml_mapping_wrapped_in_an_xml_tool_tag() {
        let calls = parse_yaml_xml_generated_text("<get_weather>\ncity: Seoul\nunit: celsius\n</get_weather>", &[weather_tool()], None);
        assert_eq!(calls.len(), 1);
        assert_eq!(arg(&calls[0], "city"), Some(&Value::String("Seoul".into())));
        assert_eq!(arg(&calls[0], "unit"), Some(&Value::String("celsius".into())));
    }

    #[test]
    fn parses_yaml_multiline_blocks() {
        let text = "<write_file>\nfile_path: /tmp/example.txt\ncontents: |\n  First line\n  Second line\n</write_file>";
        let calls = parse_yaml_xml_generated_text(text, &[write_file_tool()], None);
        assert_eq!(calls.len(), 1);
        assert_eq!(arg(&calls[0], "file_path"), Some(&Value::String("/tmp/example.txt".into())));
        assert_eq!(arg(&calls[0], "contents"), Some(&Value::String("First line\nSecond line\n".into())));
    }

    #[test]
    fn treats_self_closing_tags_as_empty_argument_objects() {
        let calls = parse_yaml_xml_generated_text("<get_weather />", &[weather_tool()], None);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_weather");
        assert!(calls[0].arguments.is_empty());
    }

    #[test]
    fn parses_self_closing_tags_with_surrounding_text() {
        let calls = parse_yaml_xml_generated_text("Getting your location now... <get_location/> Done!", &[get_location_tool()], None);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_location");
        assert!(calls[0].arguments.is_empty());
    }

    #[test]
    fn does_not_parse_tool_tags_that_appear_inside_a_yaml_block_scalar_body() {
        let text = "<write_file>\nfile_path: /tmp/test.txt\ncontents: |\n  The text contains <get_location/> tag\n</write_file>";
        let calls = parse_yaml_xml_generated_text(text, &[write_file_tool(), get_location_tool()], None);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "write_file");
        assert_eq!(arg(&calls[0], "contents"), Some(&Value::String("The text contains <get_location/> tag\n".into())));
    }

    #[test]
    fn parses_multiple_tool_calls_where_the_second_starts_after_the_first_ends() {
        let text = "<write_file>\nfile_path: test.txt\ncontents: normal content\n</write_file>\n<get_weather>\nlocation: Seoul\n</get_weather>";
        let calls = parse_yaml_xml_generated_text(text, &[write_file_tool(), weather_tool()], None);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "write_file");
        assert_eq!(arg(&calls[0], "file_path"), Some(&Value::String("test.txt".into())));
        assert_eq!(calls[1].name, "get_weather");
        assert_eq!(arg(&calls[1], "location"), Some(&Value::String("Seoul".into())));
    }

    #[test]
    fn reports_invalid_yaml_through_on_error() {
        let (seen, handler) = error_collector();
        let options = options_with(false, handler);
        let calls = parse_yaml_xml_generated_text("<get_weather>\n[invalid: yaml:\n</get_weather>", &[weather_tool()], Some(&options));
        assert!(calls.is_empty());
        assert!(!seen.lock().expect("errors").is_empty());
    }

    #[test]
    fn emits_toolcall_events_when_a_yaml_xml_call_completes() {
        let mut parser = create_yaml_xml_stream_parser(vec![weather_tool()], None);
        assert_eq!(
            feed_all(&mut parser, "<get_weather>\ncity: Seoul\n</get_weather>"),
            vec![
                StreamParserEvent::ToolcallStart { index: 0, name: "get_weather".into(), id: "yaml-xml-tool-0".into() },
                StreamParserEvent::ToolcallDelta { index: 0, arguments_delta: r#"{"city":"Seoul"}"#.into() },
                StreamParserEvent::ToolcallEnd {
                    index: 0,
                    name: "get_weather".into(),
                    id: "yaml-xml-tool-0".into(),
                    arguments: json!({"city": "Seoul"}).as_object().expect("object").clone(),
                    incomplete: false,
                    error_message: None,
                },
            ]
        );
    }

    #[test]
    fn parses_self_closing_tags_with_surrounding_text_in_the_stream() {
        let mut parser = create_yaml_xml_stream_parser(vec![get_location_tool()], None);
        assert_eq!(
            feed_all(&mut parser, "prefix <get_location /> suffix"),
            vec![
                StreamParserEvent::Text { text: "prefix ".into() },
                StreamParserEvent::ToolcallStart { index: 0, name: "get_location".into(), id: "yaml-xml-tool-0".into() },
                StreamParserEvent::ToolcallEnd {
                    index: 0,
                    name: "get_location".into(),
                    id: "yaml-xml-tool-0".into(),
                    arguments: Map::new(),
                    incomplete: false,
                    error_message: None,
                },
                StreamParserEvent::Text { text: " suffix".into() },
            ]
        );
    }

    #[test]
    fn parses_tool_calls_split_across_multiple_chunks() {
        let mut parser = create_yaml_xml_stream_parser(vec![weather_tool()], None);
        let mut events = Vec::new();
        for chunk in ["<get_wea", "ther>\n", "location: Ber", "lin\n", "</get_weather>"] {
            events.extend(parser.feed(chunk));
        }
        events.extend(parser.finish());
        let StreamParserEvent::ToolcallEnd { name, arguments, .. } = end_of(&events).expect("toolcall end") else { unreachable!() };
        assert_eq!(name, "get_weather");
        assert_eq!(arguments.get("location"), Some(&Value::String("Berlin".into())));
    }

    #[test]
    fn parses_self_closing_tags_split_across_multiple_chunks() {
        let mut parser = create_yaml_xml_stream_parser(vec![get_location_tool()], None);
        let mut events = parser.feed("<get_loca");
        events.extend(parser.feed("tion/>"));
        events.extend(parser.finish());
        assert_eq!(
            events,
            vec![
                StreamParserEvent::ToolcallStart { index: 0, name: "get_location".into(), id: "yaml-xml-tool-0".into() },
                StreamParserEvent::ToolcallEnd {
                    index: 0,
                    name: "get_location".into(),
                    id: "yaml-xml-tool-0".into(),
                    arguments: Map::new(),
                    incomplete: false,
                    error_message: None,
                },
            ]
        );
    }

    #[test]
    fn parses_multiline_yaml_values_split_across_multiple_chunks() {
        let mut parser = create_yaml_xml_stream_parser(vec![write_file_tool()], None);
        let mut events = Vec::new();
        for chunk in ["<write_file>\n", "file_path: /tmp/test.txt\n", "contents: |\n", "  Line one\n", "  Line two\n", "</write_file>"] {
            events.extend(parser.feed(chunk));
        }
        events.extend(parser.finish());
        let StreamParserEvent::ToolcallEnd { name, arguments, .. } = end_of(&events).expect("toolcall end") else { unreachable!() };
        assert_eq!(name, "write_file");
        assert_eq!(arguments.get("file_path"), Some(&Value::String("/tmp/test.txt".into())));
        assert_eq!(arguments.get("contents"), Some(&Value::String("Line one\nLine two\n".into())));
    }

    #[test]
    fn suppresses_invalid_yaml_tool_markup_by_default_and_reports_on_error() {
        let (seen, handler) = error_collector();
        let mut parser = create_yaml_xml_stream_parser(vec![weather_tool()], Some(options_with(false, handler)));
        assert_eq!(
            feed_all(&mut parser, "prefix <get_weather>\n[invalid: yaml:\n</get_weather> suffix"),
            vec![StreamParserEvent::Text { text: "prefix ".into() }, StreamParserEvent::Text { text: " suffix".into() }]
        );
        assert!(!seen.lock().expect("errors").is_empty());
    }

    #[test]
    fn emits_invalid_yaml_tool_markup_when_raw_fallback_is_enabled() {
        let mut parser = create_yaml_xml_stream_parser(vec![weather_tool()], Some(ParserOptions { emit_raw_tool_call_text_on_error: true, on_error: None }));
        assert_eq!(
            feed_all(&mut parser, "prefix <get_weather>\n[invalid: yaml:\n</get_weather> suffix"),
            vec![
                StreamParserEvent::Text { text: "prefix ".into() },
                StreamParserEvent::Text { text: "<get_weather>\n[invalid: yaml:\n</get_weather>".into() },
                StreamParserEvent::Text { text: " suffix".into() },
            ]
        );
    }

    #[test]
    fn force_completes_unfinished_yaml_tool_calls_at_finish_only_when_the_arguments_validate() {
        let mut parser = create_yaml_xml_stream_parser(vec![weather_tool()], None);
        assert_eq!(
            feed_all(&mut parser, "<get_weather>\ncity: Seoul"),
            vec![
                StreamParserEvent::ToolcallStart { index: 0, name: "get_weather".into(), id: "yaml-xml-tool-0".into() },
                StreamParserEvent::ToolcallDelta { index: 0, arguments_delta: r#"{"city":"Seoul"}"#.into() },
                StreamParserEvent::ToolcallEnd {
                    index: 0,
                    name: "get_weather".into(),
                    id: "yaml-xml-tool-0".into(),
                    arguments: json!({"city": "Seoul"}).as_object().expect("object").clone(),
                    incomplete: false,
                    error_message: None,
                },
            ]
        );
    }

    #[test]
    fn flags_unfinished_invalid_yaml_tool_calls_at_finish_by_default() {
        let (seen, handler) = error_collector();
        let mut parser = create_yaml_xml_stream_parser(vec![weather_tool()], Some(options_with(false, handler)));
        assert_eq!(
            feed_all(&mut parser, "<get_weather>\n[invalid: yaml:"),
            vec![
                StreamParserEvent::ToolcallStart { index: 0, name: "get_weather".into(), id: "yaml-xml-tool-0".into() },
                StreamParserEvent::ToolcallEnd {
                    index: 0,
                    name: "get_weather".into(),
                    id: "yaml-xml-tool-0".into(),
                    arguments: Map::new(),
                    incomplete: true,
                    error_message: Some("Tool call was truncated mid-arguments".into()),
                },
            ]
        );
        assert_eq!(seen.lock().expect("errors").as_slice(), ["Could not complete streaming YAML XML tool call at finish."]);
    }

    #[test]
    fn never_emits_raw_unfinished_yaml_tool_markup_at_finish_when_raw_fallback_is_enabled() {
        let mut parser = create_yaml_xml_stream_parser(vec![weather_tool()], Some(ParserOptions { emit_raw_tool_call_text_on_error: true, on_error: None }));
        assert_eq!(
            feed_all(&mut parser, "<get_weather>\n[invalid: yaml:"),
            vec![
                StreamParserEvent::ToolcallStart { index: 0, name: "get_weather".into(), id: "yaml-xml-tool-0".into() },
                StreamParserEvent::ToolcallEnd {
                    index: 0,
                    name: "get_weather".into(),
                    id: "yaml-xml-tool-0".into(),
                    arguments: Map::new(),
                    incomplete: true,
                    error_message: Some("Tool call was truncated mid-arguments".into()),
                },
            ]
        );
    }

    #[test]
    fn handles_a_truncation_fixture_with_a_parseable_yaml_mapping_and_the_closing_tag_missing() {
        let (seen, handler) = error_collector();
        let mut parser = create_yaml_xml_stream_parser(fixture_tools(), Some(options_with(true, handler)));
        let input = "<get_weather>\ncity: Seoul";
        let events = feed_all(&mut parser, input);
        let ends: Vec<_> = events.iter().filter(|e| matches!(e, StreamParserEvent::ToolcallEnd { .. })).collect();
        assert_eq!(ends.len(), 1);
        let StreamParserEvent::ToolcallEnd { name, arguments, incomplete, .. } = ends[0] else { unreachable!() };
        assert_eq!(name, "get_weather");
        assert_eq!(arguments.get("city"), Some(&Value::String("Seoul".into())));
        assert!(!*incomplete);
        assert!(!text_of(&events).contains(input));
        assert!(seen.lock().expect("errors").is_empty());
    }

    #[test]
    fn handles_a_truncation_fixture_with_an_empty_yaml_body_that_validates_empty_arguments() {
        let (seen, handler) = error_collector();
        let mut parser = create_yaml_xml_stream_parser(fixture_tools(), Some(options_with(true, handler)));
        let input = "<get_location>\n";
        let events = feed_all(&mut parser, input);
        let ends: Vec<_> = events.iter().filter(|e| matches!(e, StreamParserEvent::ToolcallEnd { .. })).collect();
        assert_eq!(ends.len(), 1);
        let StreamParserEvent::ToolcallEnd { name, arguments, incomplete, .. } = ends[0] else { unreachable!() };
        assert_eq!(name, "get_location");
        assert!(arguments.is_empty());
        assert!(!*incomplete);
        assert!(!text_of(&events).contains(input));
        assert!(seen.lock().expect("errors").is_empty());
    }

    #[test]
    fn handles_a_truncation_fixture_with_an_invalid_yaml_mapping() {
        let (seen, handler) = error_collector();
        let mut parser = create_yaml_xml_stream_parser(fixture_tools(), Some(options_with(true, handler)));
        let input = "<get_weather>\n[invalid: yaml:";
        let events = feed_all(&mut parser, input);
        let ends: Vec<_> = events.iter().filter(|e| matches!(e, StreamParserEvent::ToolcallEnd { .. })).collect();
        assert_eq!(ends.len(), 1);
        let StreamParserEvent::ToolcallEnd { name, incomplete, .. } = ends[0] else { unreachable!() };
        assert_eq!(name, "get_weather");
        assert!(*incomplete);
        assert!(!text_of(&events).contains(input));
        assert!(!seen.lock().expect("errors").iter().any(|message| message.contains(input)));
    }

    #[test]
    fn handles_a_truncation_fixture_with_parseable_yaml_that_violates_todowrite_min_items() {
        let (seen, handler) = error_collector();
        let mut parser = create_yaml_xml_stream_parser(fixture_tools(), Some(options_with(true, handler)));
        let input = "<todowrite>\ntodos: []";
        let events = feed_all(&mut parser, input);
        let ends: Vec<_> = events.iter().filter(|e| matches!(e, StreamParserEvent::ToolcallEnd { .. })).collect();
        assert_eq!(ends.len(), 1);
        let StreamParserEvent::ToolcallEnd { name, incomplete, .. } = ends[0] else { unreachable!() };
        assert_eq!(name, "todowrite");
        assert!(*incomplete);
        assert!(!text_of(&events).contains(input));
        assert!(!seen.lock().expect("errors").iter().any(|message| message.contains(input)));
    }

    #[test]
    fn handles_a_truncation_fixture_with_an_unknown_yaml_xml_tag_that_remains_ordinary_text() {
        let (_seen, handler) = error_collector();
        let mut parser = create_yaml_xml_stream_parser(fixture_tools(), Some(options_with(true, handler)));
        let input = "<unknown_tool>\ncity: Seoul";
        let events = feed_all(&mut parser, input);
        assert!(!events.iter().any(is_toolcall_event));
        assert!(text_of(&events).contains(input));
    }

    #[test]
    fn flags_eof_immediately_after_an_opening_tag_when_required_arguments_are_missing() {
        let mut parser = create_yaml_xml_stream_parser(fixture_tools(), None);
        assert_eq!(
            feed_all(&mut parser, "<get_weather>"),
            vec![
                StreamParserEvent::ToolcallStart { index: 0, name: "get_weather".into(), id: "yaml-xml-tool-0".into() },
                StreamParserEvent::ToolcallEnd {
                    index: 0,
                    name: "get_weather".into(),
                    id: "yaml-xml-tool-0".into(),
                    arguments: Map::new(),
                    incomplete: true,
                    error_message: Some("Tool call was truncated mid-arguments".into()),
                },
            ]
        );
    }

    #[test]
    fn recovers_eof_immediately_after_an_opening_tag_when_empty_arguments_validate() {
        let mut parser = create_yaml_xml_stream_parser(fixture_tools(), None);
        assert_eq!(
            feed_all(&mut parser, "<get_location>"),
            vec![
                StreamParserEvent::ToolcallStart { index: 0, name: "get_location".into(), id: "yaml-xml-tool-0".into() },
                StreamParserEvent::ToolcallEnd {
                    index: 0,
                    name: "get_location".into(),
                    id: "yaml-xml-tool-0".into(),
                    arguments: Map::new(),
                    incomplete: false,
                    error_message: None,
                },
            ]
        );
    }

    #[test]
    fn ends_a_started_malformed_complete_call_as_incomplete_without_changing_its_raw_text_policy() {
        let mut parser = create_yaml_xml_stream_parser(vec![weather_tool()], Some(ParserOptions { emit_raw_tool_call_text_on_error: true, on_error: None }));
        let mut events = parser.feed("<get_weather>\ncity: Seoul\n");
        events.extend(parser.feed("[invalid: yaml:\n</get_weather>"));
        events.extend(parser.finish());
        assert!(events.contains(&StreamParserEvent::ToolcallEnd {
            index: 0,
            name: "get_weather".into(),
            id: "yaml-xml-tool-0".into(),
            arguments: Map::new(),
            incomplete: true,
            error_message: Some("Tool call arguments could not be parsed".into()),
        }));
        assert!(events.contains(&StreamParserEvent::Text { text: "<get_weather>\ncity: Seoul\n[invalid: yaml:\n</get_weather>".into() }));
    }

    #[test]
    fn keeps_yaml_xml_parsing_stable_across_random_chunk_splits() {
        for seed in [0u64, 1, 7, 13, 21] {
            let mut parser = create_yaml_xml_stream_parser(vec![weather_tool()], None);
            let input = "Checking... <get_weather>\nlocation: NYC\nunit: celsius\n</get_weather> found!";
            let mut events = Vec::new();
            for chunk in random_chunk_split(input, 1, 8, seed) {
                events.extend(parser.feed(&chunk));
            }
            events.extend(parser.finish());
            let StreamParserEvent::ToolcallEnd { name, arguments, .. } = end_of(&events).expect("toolcall end") else { unreachable!() };
            assert_eq!(name, "get_weather", "seed {seed}");
            assert_eq!(arguments.get("location"), Some(&Value::String("NYC".into())), "seed {seed}");
            assert_eq!(arguments.get("unit"), Some(&Value::String("celsius".into())), "seed {seed}");
            let text = text_of(&events);
            assert!(text.contains("Checking..."), "seed {seed}");
            assert!(text.contains("found!"), "seed {seed}");
            assert!(!text.contains("<get_weather>"), "seed {seed}");
        }
    }

    #[test]
    fn preserves_text_boundaries_around_tool_calls_in_the_parser_stream() {
        let mut parser = create_yaml_xml_stream_parser(vec![weather_tool()], None);
        assert_eq!(
            feed_all(&mut parser, "Before <get_weather>\nlocation: SF\n</get_weather> After"),
            vec![
                StreamParserEvent::Text { text: "Before ".into() },
                StreamParserEvent::ToolcallStart { index: 0, name: "get_weather".into(), id: "yaml-xml-tool-0".into() },
                StreamParserEvent::ToolcallDelta { index: 0, arguments_delta: r#"{"location":"SF"}"#.into() },
                StreamParserEvent::ToolcallEnd {
                    index: 0,
                    name: "get_weather".into(),
                    id: "yaml-xml-tool-0".into(),
                    arguments: json!({"location": "SF"}).as_object().expect("object").clone(),
                    incomplete: false,
                    error_message: None,
                },
                StreamParserEvent::Text { text: " After".into() },
            ]
        );
    }
}
