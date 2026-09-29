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
