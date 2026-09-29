//! Port of senpi packages/ai/src/tool-call-middleware/protocols/gemma4.ts.

use serde_json::{Map, Value};

use crate::tool_call_middleware::types::{ParsedToolCall, ParserOptions, StreamParser, StreamParserEvent, ToolResultContent};
use crate::types::Tool;
use crate::utils::validation::validate_tool_arguments;

const STRING_DELIM: &str = "<|\"|>";
const TOOL_CALL_START: &str = "<|tool_call>";
const TOOL_CALL_END: &str = "<tool_call|>";
const TURN_END: &str = "<turn|>";
const TOOL_RESPONSE_START: &str = "<|tool_response>";

fn format_gemma4_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::String(text) => format!("{STRING_DELIM}{text}{STRING_DELIM}"),
        Value::Number(_) | Value::Bool(_) => value.to_string(),
        Value::Array(items) => format!("[{}]", items.iter().map(format_gemma4_value).collect::<Vec<_>>().join(",")),
        Value::Object(map) => format!("{{{}}}", map.iter().map(|(key, val)| format!("{key}:{}", format_gemma4_value(val))).collect::<Vec<_>>().join(",")),
    }
}

fn format_gemma4_args(args: &Map<String, Value>) -> String {
    args.iter().map(|(key, value)| format!("{key}:{}", format_gemma4_value(value))).collect::<Vec<_>>().join(",")
}

pub fn gemma4_format_tool_call(name: &str, args: &Map<String, Value>) -> String {
    format!("{TOOL_CALL_START}call:{name}{{{}}}{TOOL_CALL_END}", format_gemma4_args(args))
}

pub fn gemma4_format_tool_response(_tool_name: &str, _tool_call_id: &str, content: &[ToolResultContent]) -> String {
    let text_parts: Vec<&str> = content.iter().filter_map(|item| item.as_text().map(|text| text.text.as_str())).collect();
    format!("{TOOL_RESPONSE_START}{}", text_parts.join("\n"))
}

pub fn gemma4_format_tools_system_prompt(tools: &[Tool]) -> String {
    if tools.is_empty() {
        return String::new();
    }

    let tool_descriptions: Vec<String> = tools
        .iter()
        .map(|tool| {
            let schema = json_stringify_indent_3(&tool.parameters);
            format!("Name: {}\nDescription: {}\nParameters Schema:\n{schema}", tool.name, tool.description)
        })
        .collect();

    format!(
        "You have access to the following tools. Use them when appropriate.\n\n{}\n\nWhen you need to use a tool, format your response exactly like this:\n\n{TOOL_CALL_START}call:name{{key:{STRING_DELIM}value{STRING_DELIM}}}{TOOL_CALL_END}\n\nImportant formatting rules:\n- String values must be wrapped in {STRING_DELIM} delimiters\n- Numbers and booleans should be bare values (no delimiters)\n- Multiple arguments are separated by commas\n- Nested objects use {{key:value}} syntax\n- Arrays use [value1,value2] syntax\n\nExample:\n{TOOL_CALL_START}call:get_weather{{location:{STRING_DELIM}London{STRING_DELIM},unit:{STRING_DELIM}celsius{STRING_DELIM}}}{TOOL_CALL_END}",
        tool_descriptions.join("\n\n")
    )
}

/// `JSON.stringify(value, null, 3)`: serde_json's pretty printer defaults to 2-space indent.
fn json_stringify_indent_3(value: &Value) -> String {
    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"   ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut buf, formatter);
    serde::Serialize::serialize(value, &mut serializer).expect("json_stringify_indent_3: Value serialization cannot fail");
    String::from_utf8(buf).expect("json_stringify_indent_3: serde_json emits valid UTF-8")
}

fn is_integer_literal(text: &str) -> bool {
    let text = text.strip_prefix('-').unwrap_or(text);
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// `/^-?(?:\d+\.\d*|\.\d+)$/`.
fn is_float_literal(text: &str) -> bool {
    let text = text.strip_prefix('-').unwrap_or(text);
    if let Some((int_part, frac_part)) = text.split_once('.') {
        if !int_part.is_empty() && int_part.bytes().all(|b| b.is_ascii_digit()) {
            return true;
        }
        if int_part.is_empty() && !frac_part.is_empty() && frac_part.bytes().all(|b| b.is_ascii_digit()) {
            return true;
        }
    }
    false
}

fn parse_gemma4_value(value_string: &str) -> Value {
    let trimmed_value = value_string.trim();
    if trimmed_value.is_empty() {
        return Value::String(trimmed_value.to_string());
    }
    if trimmed_value == "true" {
        return Value::Bool(true);
    }
    if trimmed_value == "false" {
        return Value::Bool(false);
    }
    if trimmed_value.contains('.') {
        if is_float_literal(trimmed_value)
            && let Ok(parsed) = trimmed_value.parse::<f64>()
        {
            return Value::from(parsed);
        }
        return Value::String(trimmed_value.to_string());
    }
    if is_integer_literal(trimmed_value)
        && let Ok(parsed) = trimmed_value.parse::<i64>()
    {
        return Value::from(parsed);
    }
    Value::String(trimmed_value.to_string())
}

fn parse_gemma4_array(array_string: &str, partial: bool) -> Vec<Value> {
    let mut items = Vec::new();
    let chars: Vec<char> = array_string.chars().collect();
    let mut index = 0usize;

    while index < chars.len() {
        while index < chars.len() && matches!(chars[index], ' ' | ',' | '\n' | '\t') {
            index += 1;
        }
        if index >= chars.len() {
            break;
        }

        let remaining: String = chars[index..].iter().collect();
        if remaining.starts_with(STRING_DELIM) {
            index += STRING_DELIM.chars().count();
            let after: String = chars[index..].iter().collect();
            match after.find(STRING_DELIM) {
                None => {
                    items.push(Value::String(after));
                    break;
                }
                Some(byte_offset) => {
                    let char_offset = after[..byte_offset].chars().count();
                    items.push(Value::String(chars[index..index + char_offset].iter().collect()));
                    index += char_offset + STRING_DELIM.chars().count();
                    continue;
                }
            }
        }

        if chars[index] == '{' {
            let mut depth = 1i32;
            let object_start = index + 1;
            index += 1;
            while index < chars.len() && depth > 0 {
                let remaining: String = chars[index..].iter().collect();
                if remaining.starts_with(STRING_DELIM) {
                    index += STRING_DELIM.chars().count();
                    let after: String = chars[index..].iter().collect();
                    index = match after.find(STRING_DELIM) {
                        None => chars.len(),
                        Some(byte_offset) => index + after[..byte_offset].chars().count() + STRING_DELIM.chars().count(),
                    };
                    continue;
                }
                if chars[index] == '{' {
                    depth += 1;
                } else if chars[index] == '}' {
                    depth -= 1;
                }
                index += 1;
            }
            let body: String = if depth > 0 { chars[object_start..index].iter().collect() } else { chars[object_start..index - 1].iter().collect() };
            items.push(Value::Object(parse_gemma4_args(&body, depth > 0)));
            continue;
        }

        if chars[index] == '[' {
            let mut depth = 1i32;
            let sub_array_start = index + 1;
            index += 1;
            while index < chars.len() && depth > 0 {
                if chars[index] == '[' {
                    depth += 1;
                } else if chars[index] == ']' {
                    depth -= 1;
                }
                index += 1;
            }
            let body: String = if depth > 0 { chars[sub_array_start..index].iter().collect() } else { chars[sub_array_start..index - 1].iter().collect() };
            items.push(Value::Array(parse_gemma4_array(&body, depth > 0)));
            continue;
        }

        let value_start = index;
        while index < chars.len() && !matches!(chars[index], ',' | ']') {
            index += 1;
        }
        if partial && index >= chars.len() {
            break;
        }
        items.push(parse_gemma4_value(&chars[value_start..index].iter().collect::<String>()));
    }

    items
}

pub fn parse_gemma4_args(args_string: &str, partial: bool) -> Map<String, Value> {
    if args_string.trim().is_empty() {
        return Map::new();
    }

    let mut result = Map::new();
    let chars: Vec<char> = args_string.chars().collect();
    let mut index = 0usize;

    while index < chars.len() {
        while index < chars.len() && matches!(chars[index], ' ' | ',' | '\n' | '\t') {
            index += 1;
        }
        if index >= chars.len() {
            break;
        }

        let key_start = index;
        while index < chars.len() && chars[index] != ':' {
            index += 1;
        }
        if index >= chars.len() {
            break;
        }
        let key: String = chars[key_start..index].iter().collect::<String>().trim().to_string();
        index += 1;

        if index >= chars.len() {
            if !partial {
                result.insert(key, Value::String(String::new()));
            }
            break;
        }

        while index < chars.len() && matches!(chars[index], ' ' | '\n' | '\t') {
            index += 1;
        }
        if index >= chars.len() {
            if !partial {
                result.insert(key, Value::String(String::new()));
            }
            break;
        }

        let remaining: String = chars[index..].iter().collect();
        if remaining.starts_with(STRING_DELIM) {
            index += STRING_DELIM.chars().count();
            let value_start = index;
            let after: String = chars[index..].iter().collect();
            match after.find(STRING_DELIM) {
                None => {
                    result.insert(key, Value::String(chars[value_start..].iter().collect()));
                    break;
                }
                Some(byte_offset) => {
                    let char_offset = after[..byte_offset].chars().count();
                    result.insert(key, Value::String(chars[value_start..value_start + char_offset].iter().collect()));
                    index = value_start + char_offset + STRING_DELIM.chars().count();
                    continue;
                }
            }
        }

        if chars[index] == '{' {
            let mut depth = 1i32;
            let object_start = index + 1;
            index += 1;
            while index < chars.len() && depth > 0 {
                let remaining: String = chars[index..].iter().collect();
                if remaining.starts_with(STRING_DELIM) {
                    index += STRING_DELIM.chars().count();
                    let after: String = chars[index..].iter().collect();
                    index = match after.find(STRING_DELIM) {
                        None => chars.len(),
                        Some(byte_offset) => index + after[..byte_offset].chars().count() + STRING_DELIM.chars().count(),
                    };
                    continue;
                }
                if chars[index] == '{' {
                    depth += 1;
                } else if chars[index] == '}' {
                    depth -= 1;
                }
                index += 1;
            }
            let body: String = if depth > 0 { chars[object_start..index].iter().collect() } else { chars[object_start..index - 1].iter().collect() };
            result.insert(key, Value::Object(parse_gemma4_args(&body, depth > 0)));
            continue;
        }

        if chars[index] == '[' {
            let mut depth = 1i32;
            let array_start = index + 1;
            index += 1;
            while index < chars.len() && depth > 0 {
                let remaining: String = chars[index..].iter().collect();
                if remaining.starts_with(STRING_DELIM) {
                    index += STRING_DELIM.chars().count();
                    let after: String = chars[index..].iter().collect();
                    index = match after.find(STRING_DELIM) {
                        None => chars.len(),
                        Some(byte_offset) => index + after[..byte_offset].chars().count() + STRING_DELIM.chars().count(),
                    };
                    continue;
                }
                if chars[index] == '[' {
                    depth += 1;
                } else if chars[index] == ']' {
                    depth -= 1;
                }
                index += 1;
            }
            let body: String = if depth > 0 { chars[array_start..index].iter().collect() } else { chars[array_start..index - 1].iter().collect() };
            result.insert(key, Value::Array(parse_gemma4_array(&body, depth > 0)));
            continue;
        }

        let value_start = index;
        while index < chars.len() && !matches!(chars[index], ',' | '}' | ']') {
            index += 1;
        }
        if partial && index >= chars.len() {
            break;
        }
        result.insert(key, parse_gemma4_value(&chars[value_start..index].iter().collect::<String>()));
    }

    result
}

fn find_common_prefix(left: &str, right: &str) -> String {
    left.chars().zip(right.chars()).take_while(|(a, b)| a == b).map(|(a, _)| a).collect()
}

struct ToolCallExtract {
    name: String,
    raw_args: String,
}

/// `/<\|tool_call>call:([\w\-.]+)\{(.*?)\}(?:<tool_call\|>|<turn\|>)/gs` (non-greedy body, `s` flag).
fn extract_tool_calls(text: String) -> Vec<ToolCallExtract> {
    let mut results = Vec::new();
    let mut cursor = 0usize;
    while let Some(start_offset) = text[cursor..].find(TOOL_CALL_START) {
        let call_start = cursor + start_offset;
        let after_start = call_start + TOOL_CALL_START.len();
        let Some(prefix) = text[after_start..].strip_prefix("call:") else {
            cursor = after_start;
            continue;
        };
        let name_end_offset = prefix.find('{');
        let Some(name_end_offset) = name_end_offset else { break };
        let name = &prefix[..name_end_offset];
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) {
            cursor = after_start;
            continue;
        }
        let body_start = after_start + 5 + name_end_offset + 1;
        let terminator = [(TOOL_CALL_END, body_start), (TURN_END, body_start)]
            .into_iter()
            .filter_map(|(token, from)| text[from..].find(token).map(|i| (from + i, token)))
            .min_by_key(|(i, _)| *i);
        let Some((term_index, _term_token)) = terminator else { break };
        let close_brace = text[..term_index].rfind('}');
        let Some(close_brace) = close_brace else {
            cursor = after_start;
            continue;
        };
        if close_brace < body_start {
            cursor = after_start;
            continue;
        }
        let raw_args = text[body_start..close_brace].to_string();
        results.push(ToolCallExtract { name: name.to_string(), raw_args });
        cursor = term_index;
    }
    results
}

pub fn gemma4_parse_generated_text(text: &str, _tools: &[Tool], _options: Option<&ParserOptions>) -> Vec<ParsedToolCall> {
    extract_tool_calls(text.to_string()).into_iter().map(|extract| ParsedToolCall { name: extract.name, arguments: parse_gemma4_args(&extract.raw_args, false) }).collect()
}

struct Gemma4ArgsComplete {
    raw_args: String,
}

fn get_partial_token_suffix(text: &str, tokens: &[&str]) -> String {
    for token in tokens {
        let chars: Vec<char> = token.chars().collect();
        for index in (1..chars.len()).rev() {
            let prefix: String = chars[..index].iter().collect();
            if text.ends_with(&prefix) {
                return prefix;
            }
        }
    }
    String::new()
}

pub fn scan_gemma4_args_complete(content: &str) -> Option<Gemma4ArgsComplete> {
    let outer_open_index = content.find('{')?;
    let chars: Vec<char> = content.chars().collect();
    let mut delimiters: Vec<char> = Vec::new();
    let mut inside_string = false;
    let outer_open_char_index = content[..outer_open_index].chars().count();

    let mut index = outer_open_char_index;
    while index < chars.len() {
        let remaining: String = chars[index..].iter().collect();
        if remaining.starts_with(STRING_DELIM) {
            inside_string = !inside_string;
            index += STRING_DELIM.chars().count();
            continue;
        }
        if STRING_DELIM.starts_with(remaining.as_str()) {
            return None;
        }
        if inside_string {
            index += 1;
            continue;
        }
        let character = chars[index];
        if character == '{' || character == '[' {
            delimiters.push(character);
            index += 1;
            continue;
        }
        if character != '}' && character != ']' {
            index += 1;
            continue;
        }
        let opening = delimiters.pop();
        if (character == '}' && opening != Some('{')) || (character == ']' && opening != Some('[')) {
            return None;
        }
        if delimiters.is_empty() {
            let residue: String = chars[index + 1..].iter().collect::<String>().trim().to_string();
            if !residue.is_empty() && residue != get_partial_token_suffix(&residue, &[TOOL_CALL_END, TURN_END]) {
                return None;
            }
            return Some(Gemma4ArgsComplete { raw_args: chars[outer_open_char_index + 1..index].iter().collect() });
        }
        index += 1;
    }

    None
}

fn extract_partial_tool_call(content: &str) -> (Option<String>, String) {
    let Some(function_part) = content.strip_prefix("call:") else { return (None, String::new()) };
    let Some(brace_index) = function_part.find('{') else { return (None, String::new()) };
    let name = function_part[..brace_index].trim().to_string();
    let mut raw_args = function_part[brace_index + 1..].to_string();
    if raw_args.ends_with('}') {
        raw_args.pop();
    }
    (Some(name), raw_args)
}

fn strip_trailing_unsafe_characters(arguments_json: &str) -> String {
    let mut safe_prefix = arguments_json.to_string();
    while let Some(last) = safe_prefix.chars().last() {
        if !matches!(last, '}' | '"' | ']' | '<' | '|' | '\\' | '>') {
            break;
        }
        safe_prefix.pop();
    }
    safe_prefix
}

fn find_earliest_token<'a>(text: &str, tokens: &[&'a str]) -> Option<(usize, &'a str)> {
    tokens.iter().filter_map(|token| text.find(token).map(|index| (index, *token))).min_by_key(|(index, _)| *index)
}

struct Gemma4StreamParser {
    tool_call_ids: Vec<String>,
    tools: Vec<Tool>,
    options: Option<ParserOptions>,
    plain_text_carry: String,
    tool_call_carry: String,
    inside_tool_call: bool,
    tool_call_content: String,
    current_tool_index: i64,
    current_tool_name: Option<String>,
    current_arguments_json: String,
}

impl Gemma4StreamParser {
    fn resolve_tool(&self, name: &str) -> Option<&Tool> {
        self.tools.iter().find(|tool| tool.name == name)
    }

    fn current_tool_call_id(&self) -> String {
        let index = self.current_tool_index.max(0) as usize;
        self.tool_call_ids.get(index).cloned().unwrap_or_else(|| format!("gemma4-tool-{}", self.current_tool_index))
    }

    fn emit_tool_call_start(&mut self, events: &mut Vec<StreamParserEvent>, name: &str) {
        if self.current_tool_name.is_some() {
            return;
        }
        self.current_tool_name = Some(name.to_string());
        events.push(StreamParserEvent::ToolcallStart { index: self.current_tool_index as usize, name: name.to_string(), id: self.current_tool_call_id() });
    }

    fn emit_final_tool_call(&mut self, events: &mut Vec<StreamParserEvent>, name: &str, arguments_object: Map<String, Value>) {
        self.emit_tool_call_start(events, name);
        let arguments_json = Value::Object(arguments_object.clone()).to_string();
        let final_delta = arguments_json[self.current_arguments_json.len().min(arguments_json.len())..].to_string();
        if !final_delta.is_empty() {
            events.push(StreamParserEvent::ToolcallDelta { index: self.current_tool_index as usize, arguments_delta: final_delta });
        }
        events.push(StreamParserEvent::ToolcallEnd {
            index: self.current_tool_index as usize,
            name: name.to_string(),
            id: self.current_tool_call_id(),
            arguments: arguments_object,
            incomplete: false,
            error_message: None,
        });
    }

    fn emit_incomplete_tool_call(&mut self, events: &mut Vec<StreamParserEvent>, name: &str, arguments_object: Map<String, Value>) {
        self.emit_tool_call_start(events, name);
        events.push(StreamParserEvent::ToolcallEnd {
            index: self.current_tool_index as usize,
            name: name.to_string(),
            id: self.current_tool_call_id(),
            arguments: arguments_object,
            incomplete: true,
            error_message: Some("Tool call was truncated mid-arguments".to_string()),
        });
    }

    fn report_incomplete_tool_call(&self, retained_length: usize) {
        if let Some(options) = &self.options {
            options.report_error(
                "Could not complete Gemma4 tool call at finish.",
                Some(std::collections::HashMap::from([
                    ("protocol".to_string(), Value::String("gemma4-delimiter".to_string())),
                    ("retainedLength".to_string(), Value::from(retained_length)),
                ])),
            );
        }
    }

    fn report_dropped_tool_call(&self, retained_length: usize) {
        if let Some(options) = &self.options {
            options.report_error(
                "Gemma4 tool call dropped",
                Some(std::collections::HashMap::from([
                    ("protocol".to_string(), Value::String("gemma4-delimiter".to_string())),
                    ("retainedLength".to_string(), Value::from(retained_length)),
                ])),
            );
        }
    }

    fn emit_partial_tool_call(&mut self, events: &mut Vec<StreamParserEvent>) {
        let (name, raw_args) = extract_partial_tool_call(&self.tool_call_content);
        let Some(name) = name else { return };
        if self.resolve_tool(&name).is_none() {
            return;
        }

        self.emit_tool_call_start(events, &name);
        if raw_args.is_empty() {
            return;
        }

        let parsed_arguments = parse_gemma4_args(&raw_args, true);
        if parsed_arguments.is_empty() {
            return;
        }

        let current_arguments_json = Value::Object(parsed_arguments).to_string();
        let safe_arguments_json = strip_trailing_unsafe_characters(&current_arguments_json);
        if safe_arguments_json.is_empty() || safe_arguments_json == self.current_arguments_json {
            return;
        }

        if !self.current_arguments_json.is_empty() {
            let common_prefix = find_common_prefix(&self.current_arguments_json, &safe_arguments_json);
            if common_prefix.len() < self.current_arguments_json.len() {
                self.current_arguments_json = common_prefix;
                return;
            }
        }

        let arguments_delta = safe_arguments_json[self.current_arguments_json.len()..].to_string();
        if arguments_delta.is_empty() {
            return;
        }

        self.current_arguments_json = safe_arguments_json;
        events.push(StreamParserEvent::ToolcallDelta { index: self.current_tool_index as usize, arguments_delta });
    }

    fn emit_completed_tool_call(&mut self, events: &mut Vec<StreamParserEvent>) {
        let (name, raw_args) = extract_partial_tool_call(&self.tool_call_content);
        let Some(name) = name.filter(|n| self.resolve_tool(n).is_some()) else {
            self.report_dropped_tool_call(self.tool_call_content.len());
            return;
        };
        let arguments = parse_gemma4_args(&raw_args, false);
        self.emit_final_tool_call(events, &name, arguments);
    }
}

impl StreamParser for Gemma4StreamParser {
    fn feed(&mut self, text_delta: &str) -> Vec<StreamParserEvent> {
        let mut events = Vec::new();
        let mut remaining = text_delta.to_string();

        while !remaining.is_empty() {
            if !self.inside_tool_call {
                let combined_text = format!("{}{}", self.plain_text_carry, remaining);
                let Some(start_index) = combined_text.find(TOOL_CALL_START) else {
                    let partial_suffix = get_partial_token_suffix(&combined_text, &[TOOL_CALL_START]);
                    let flushable_text = if partial_suffix.is_empty() { combined_text.clone() } else { combined_text[..combined_text.len() - partial_suffix.len()].to_string() };
                    if !flushable_text.is_empty() {
                        events.push(StreamParserEvent::Text { text: flushable_text });
                    }
                    self.plain_text_carry = partial_suffix;
                    break;
                };

                let text_before_tool_call = combined_text[..start_index].to_string();
                if !text_before_tool_call.is_empty() {
                    events.push(StreamParserEvent::Text { text: text_before_tool_call });
                }

                let after_start = combined_text[start_index + TOOL_CALL_START.len()..].to_string();
                self.plain_text_carry.clear();
                self.inside_tool_call = true;
                self.tool_call_content.clear();
                self.tool_call_carry.clear();
                self.current_tool_index += 1;
                self.current_tool_name = None;
                self.current_arguments_json.clear();
                let index = self.current_tool_index as usize;
                if self.tool_call_ids.len() <= index {
                    self.tool_call_ids.resize(index + 1, String::new());
                }
                self.tool_call_ids[index] = format!("gemma4-tool-{index}");
                remaining = after_start;
                continue;
            }

            let combined_tool_call = format!("{}{}", self.tool_call_carry, remaining);
            let end_match = find_earliest_token(&combined_tool_call, &[TOOL_CALL_END, TURN_END]);

            let Some((end_index, end_token)) = end_match else {
                let partial_suffix = get_partial_token_suffix(&combined_tool_call, &[TOOL_CALL_END, TURN_END]);
                let flushable_content = if partial_suffix.is_empty() { combined_tool_call.clone() } else { combined_tool_call[..combined_tool_call.len() - partial_suffix.len()].to_string() };
                self.tool_call_carry = partial_suffix;
                if !flushable_content.is_empty() {
                    self.tool_call_content.push_str(&flushable_content);
                    self.emit_partial_tool_call(&mut events);
                }
                break;
            };

            let content_before_end = combined_tool_call[..end_index].to_string();
            self.tool_call_content.push_str(&content_before_end);
            self.tool_call_carry.clear();
            self.emit_partial_tool_call(&mut events);
            self.emit_completed_tool_call(&mut events);

            self.inside_tool_call = false;
            self.tool_call_content.clear();
            self.current_tool_name = None;
            self.current_arguments_json.clear();
            remaining = combined_tool_call[end_index + end_token.len()..].to_string();
        }

        events
    }

    fn finish(&mut self) -> Vec<StreamParserEvent> {
        let mut events = Vec::new();

        if !self.inside_tool_call && !self.plain_text_carry.is_empty() {
            events.push(StreamParserEvent::Text { text: self.plain_text_carry.clone() });
        }

        if self.inside_tool_call {
            let content = format!("{}{}", self.tool_call_content, self.tool_call_carry);
            let (name, raw_args) = extract_partial_tool_call(&content);
            let tool = name.as_deref().and_then(|n| self.resolve_tool(n)).cloned();

            match (&tool, &name) {
                (None, _) | (_, None) => self.report_dropped_tool_call(content.len()),
                (Some(tool), Some(name)) => {
                    let complete = scan_gemma4_args_complete(&content);
                    if let Some(complete) = complete {
                        let arguments_object = parse_gemma4_args(&complete.raw_args, false);
                        let tool_call = crate::types::ToolCall {
                            id: self.current_tool_call_id(),
                            name: name.clone(),
                            arguments: arguments_object.clone(),
                            incomplete: None,
                            error_message: None,
                            thought_signature: None,
                            namespace: None,
                        };
                        match validate_tool_arguments(tool, &tool_call) {
                            Ok(Value::Object(validated_arguments)) => self.emit_final_tool_call(&mut events, name, validated_arguments),
                            Ok(_) | Err(_) => {
                                self.report_incomplete_tool_call(content.len());
                                self.emit_incomplete_tool_call(&mut events, name, parse_gemma4_args(&raw_args, true));
                            }
                        }
                    } else {
                        self.report_incomplete_tool_call(content.len());
                        self.emit_incomplete_tool_call(&mut events, name, parse_gemma4_args(&raw_args, true));
                    }
                }
            }
        }

        self.plain_text_carry.clear();
        self.tool_call_carry.clear();
        self.inside_tool_call = false;
        self.tool_call_content.clear();
        self.current_tool_name = None;
        self.current_arguments_json.clear();

        events
    }
}

pub fn gemma4_create_stream_parser(tools: Vec<Tool>, options: Option<ParserOptions>) -> Box<dyn StreamParser + Send> {
    Box::new(Gemma4StreamParser {
        tool_call_ids: Vec::new(),
        tools,
        options,
        plain_text_carry: String::new(),
        tool_call_carry: String::new(),
        inside_tool_call: false,
        tool_call_content: String::new(),
        current_tool_index: -1,
        current_tool_name: None,
        current_arguments_json: String::new(),
    })
}
