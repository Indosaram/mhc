//! Port of senpi packages/ai/src/tool-call-middleware/protocols/json-mix.ts.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::tool_call_middleware::types::{ParsedToolCall, ParserOptions, StreamParser, StreamParserEvent};
use crate::types::Tool;
use crate::utils::validation::validate_tool_arguments;

pub struct JsonMixOptions {
    pub tool_call_start: &'static str,
    pub tool_call_end: &'static str,
    pub create_tool_call_id: fn(usize) -> String,
}

fn should_emit_raw_tool_call_text_on_error(options: Option<&ParserOptions>) -> bool {
    options.is_some_and(|o| o.emit_raw_tool_call_text_on_error)
}

fn normalize_tool_names(tools: &[Tool]) -> std::collections::HashSet<&str> {
    tools.iter().map(|tool| tool.name.as_str()).collect()
}

fn remove_trailing_commas(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut result = String::new();
    let mut in_string = false;
    let mut is_escaping = false;

    let mut index = 0usize;
    while index < chars.len() {
        let character = chars[index];

        if in_string {
            result.push(character);
            if is_escaping {
                is_escaping = false;
            } else if character == '\\' {
                is_escaping = true;
            } else if character == '"' {
                in_string = false;
            }
            index += 1;
            continue;
        }

        if character == '"' {
            in_string = true;
            result.push(character);
            index += 1;
            continue;
        }

        if character == ',' {
            let mut look_ahead_index = index + 1;
            while look_ahead_index < chars.len() && chars[look_ahead_index].is_whitespace() {
                look_ahead_index += 1;
            }
            if look_ahead_index < chars.len() && matches!(chars[look_ahead_index], '}' | ']') {
                index += 1;
                continue;
            }
        }

        result.push(character);
        index += 1;
    }

    result
}

/// `/"([A-Za-z0-9_.$-]+)'(?=\s*:)/g` -> `"$1"`: repairs a stray closing single-quote after an
/// object key that should have been a double-quote.
fn normalize_malformed_object_keys(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut result = String::new();
    let mut index = 0usize;

    while index < chars.len() {
        if chars[index] != '"' {
            result.push(chars[index]);
            index += 1;
            continue;
        }
        let key_start = index + 1;
        let mut key_end = key_start;
        while key_end < chars.len() && (chars[key_end].is_ascii_alphanumeric() || matches!(chars[key_end], '_' | '.' | '$' | '-')) {
            key_end += 1;
        }
        if key_end == key_start || key_end >= chars.len() || chars[key_end] != '\'' {
            result.push(chars[index]);
            index += 1;
            continue;
        }
        let mut lookahead = key_end + 1;
        while lookahead < chars.len() && chars[lookahead].is_whitespace() {
            lookahead += 1;
        }
        if lookahead >= chars.len() || chars[lookahead] != ':' {
            result.push(chars[index]);
            index += 1;
            continue;
        }
        result.push('"');
        result.extend(&chars[key_start..key_end]);
        result.push('"');
        index = key_end + 1;
    }

    result
}

fn ensure_object_delimiters(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return trimmed.to_string();
    }
    if trimmed.contains(':') {
        return format!("{{{trimmed}}}");
    }
    trimmed.to_string()
}

fn trim_excess_trailing_closers(text: &str) -> String {
    let open_braces = text.chars().filter(|c| *c == '{').count();
    let close_braces = text.chars().filter(|c| *c == '}').count();
    let mut excess_closers = close_braces as i64 - open_braces as i64;
    if excess_closers <= 0 {
        return text.to_string();
    }

    let mut result = text.to_string();
    while excess_closers > 0 && result.ends_with('}') {
        result.pop();
        excess_closers -= 1;
    }
    result
}

fn parse_relaxed_json(text: &str) -> Option<Value> {
    let without_trailing_commas = remove_trailing_commas(text);
    let normalized_keys = normalize_malformed_object_keys(&without_trailing_commas);
    let with_delimiters = ensure_object_delimiters(&normalized_keys);
    let trimmed_closers = trim_excess_trailing_closers(&with_delimiters);

    for attempt in [text, without_trailing_commas.as_str(), normalized_keys.as_str(), with_delimiters.as_str(), trimmed_closers.as_str()] {
        if let Ok(value) = serde_json::from_str::<Value>(attempt) {
            return Some(value);
        }
    }
    None
}

fn is_record(value: &Value) -> bool {
    value.is_object()
}

fn parse_tool_call_json(text: &str, tools: &[Tool]) -> Option<ParsedToolCall> {
    let parsed_value = parse_relaxed_json(text)?;
    let parsed_object = parsed_value.as_object()?;
    let name = parsed_object.get("name")?.as_str()?;
    if !normalize_tool_names(tools).contains(name) {
        return None;
    }
    let arguments = parsed_object.get("arguments").filter(|v| is_record(v))?.as_object()?.clone();
    Some(ParsedToolCall { name: name.to_string(), arguments })
}

fn skip_json_whitespace(chars: &[char], from_index: usize) -> usize {
    let mut index = from_index;
    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    index
}

/// Scans a top-level (depth-1) object key to find where `property`'s value begins, tracking
/// string escapes and brace depth by hand since this is a streaming partial-JSON scan, not a
/// full parse.
fn find_top_level_property_value_start(chars: &[char], property: &str) -> Option<usize> {
    let object_start = skip_json_whitespace(chars, 0);
    if object_start >= chars.len() || chars[object_start] != '{' {
        return None;
    }

    let mut depth = 0i32;
    let mut in_string = false;
    let mut is_escaping = false;
    let mut index = object_start;

    while index < chars.len() {
        let character = chars[index];

        if in_string {
            if is_escaping {
                is_escaping = false;
            } else if character == '\\' {
                is_escaping = true;
            } else if character == '"' {
                in_string = false;
            }
            index += 1;
            continue;
        }

        if character == '{' {
            depth += 1;
            index += 1;
            continue;
        }
        if character == '}' {
            depth = (depth - 1).max(0);
            index += 1;
            continue;
        }
        if character != '"' {
            index += 1;
            continue;
        }

        if depth != 1 {
            in_string = true;
            index += 1;
            continue;
        }

        let key_start = index + 1;
        let mut key_end = key_start;
        let mut key_escaping = false;
        while key_end < chars.len() {
            let key_character = chars[key_end];
            if key_escaping {
                key_escaping = false;
            } else if key_character == '\\' {
                key_escaping = true;
            } else if key_character == '"' {
                break;
            }
            key_end += 1;
        }

        if key_end >= chars.len() || chars[key_end] != '"' {
            return None;
        }

        let key: String = chars[key_start..key_end].iter().collect();
        let mut value_cursor = skip_json_whitespace(chars, key_end + 1);
        if value_cursor >= chars.len() || chars[value_cursor] != ':' {
            index = key_end + 1;
            continue;
        }

        value_cursor = skip_json_whitespace(chars, value_cursor + 1);
        if key == property {
            return if value_cursor < chars.len() { Some(value_cursor) } else { None };
        }

        index = value_cursor;
    }

    None
}

fn extract_top_level_string_property(text: &str, property: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let value_start = find_top_level_property_value_start(&chars, property)?;
    if value_start >= chars.len() || chars[value_start] != '"' {
        return None;
    }

    let mut value_end = value_start + 1;
    let mut is_escaping = false;
    while value_end < chars.len() {
        let character = chars[value_end];
        if is_escaping {
            is_escaping = false;
        } else if character == '\\' {
            is_escaping = true;
        } else if character == '"' {
            return Some(chars[value_start + 1..value_end].iter().collect());
        }
        value_end += 1;
    }

    None
}

struct JsonValueSlice {
    text: String,
    complete: bool,
}

fn extract_json_value_slice(chars: &[char], value_start: usize) -> Option<JsonValueSlice> {
    if value_start >= chars.len() {
        return None;
    }

    let first_character = chars[value_start];
    if first_character == '{' || first_character == '[' {
        let mut stack = vec![first_character];
        let mut in_string = false;
        let mut is_escaping = false;

        let mut index = value_start + 1;
        while index < chars.len() {
            let character = chars[index];
            if in_string {
                if is_escaping {
                    is_escaping = false;
                } else if character == '\\' {
                    is_escaping = true;
                } else if character == '"' {
                    in_string = false;
                }
                index += 1;
                continue;
            }

            if character == '"' {
                in_string = true;
                index += 1;
                continue;
            }

            if character == '{' || character == '[' {
                stack.push(character);
                index += 1;
                continue;
            }

            if character == '}' || character == ']' {
                if let Some(open_character) = stack.last() {
                    if (*open_character == '{' && character == '}') || (*open_character == '[' && character == ']') {
                        stack.pop();
                        if stack.is_empty() {
                            return Some(JsonValueSlice { text: chars[value_start..index + 1].iter().collect(), complete: true });
                        }
                    }
                }
            }
            index += 1;
        }

        return Some(JsonValueSlice { text: chars[value_start..].iter().collect(), complete: false });
    }

    if first_character == '"' {
        let mut is_escaping = false;
        let mut index = value_start + 1;
        while index < chars.len() {
            let character = chars[index];
            if is_escaping {
                is_escaping = false;
            } else if character == '\\' {
                is_escaping = true;
            } else if character == '"' {
                return Some(JsonValueSlice { text: chars[value_start..index + 1].iter().collect(), complete: true });
            }
            index += 1;
        }
        return Some(JsonValueSlice { text: chars[value_start..].iter().collect(), complete: false });
    }

    let mut index = value_start;
    while index < chars.len() {
        let character = chars[index];
        if character == ',' || character == '}' || character.is_whitespace() {
            break;
        }
        index += 1;
    }

    Some(JsonValueSlice { text: chars[value_start..index].iter().collect(), complete: index < chars.len() })
}

/// `String.prototype.indexOf`-with-partial-suffix-fallback: returns the earliest index of
/// `searched_text`, or (when it appears only as a truncated suffix at the very end of `text`)
/// the index where that partial match begins.
fn get_potential_start_index(text: &str, searched_text: &str) -> Option<usize> {
    if searched_text.is_empty() {
        return None;
    }

    let chars: Vec<char> = text.chars().collect();
    let searched_chars: Vec<char> = searched_text.chars().collect();

    if let Some(pos) = chars.windows(searched_chars.len().max(1)).position(|window| window == searched_chars.as_slice()) {
        return Some(pos);
    }

    let start_at = chars.len().saturating_sub(searched_chars.len().saturating_sub(1));
    for index in start_at..chars.len() {
        let suffix_length = chars.len() - index;
        let is_match = (0..suffix_length).all(|suffix_index| chars[index + suffix_index] == searched_chars[suffix_index]);
        if is_match {
            return Some(index);
        }
    }

    None
}

struct ArgumentsProgress {
    tool_name: Option<String>,
    arguments_text: Option<String>,
    arguments_complete: bool,
}

fn extract_arguments_progress(tool_call_json: &str) -> ArgumentsProgress {
    let tool_name = extract_top_level_string_property(tool_call_json, "name");
    let chars: Vec<char> = tool_call_json.chars().collect();
    let Some(arguments_start) = find_top_level_property_value_start(&chars, "arguments") else {
        return ArgumentsProgress { tool_name, arguments_text: None, arguments_complete: false };
    };

    let arguments_slice = extract_json_value_slice(&chars, arguments_start);
    ArgumentsProgress {
        tool_name,
        arguments_text: arguments_slice.as_ref().map(|s| s.text.clone()),
        arguments_complete: arguments_slice.as_ref().is_some_and(|s| s.complete),
    }
}

fn extract_best_effort_arguments(tool_call_json: &str) -> Map<String, Value> {
    let progress = extract_arguments_progress(tool_call_json);
    let Some(arguments_text) = progress.arguments_text else { return Map::new() };
    parse_relaxed_json(&arguments_text).and_then(|v| v.as_object().cloned()).unwrap_or_default()
}

fn strip_trailing_tool_call_end_prefix(raw: &str, tool_call_end: &str) -> String {
    let without_whitespace = raw.trim_end();
    let end_chars: Vec<char> = tool_call_end.chars().collect();
    for length in (1..end_chars.len()).rev() {
        let suffix: String = end_chars[..length].iter().collect();
        if without_whitespace.ends_with(&suffix) {
            return without_whitespace[..without_whitespace.len() - suffix.len()].trim_end().to_string();
        }
    }
    without_whitespace.to_string()
}

struct ActiveToolCall {
    index: usize,
    id: String,
    name: String,
    emitted_arguments: String,
}

struct JsonMixStreamState {
    active_tool_call: Option<ActiveToolCall>,
    buffer: String,
    current_tool_call_json: String,
    is_inside_tool_call: bool,
    tool_call_count: usize,
}

fn emit_text(events: &mut Vec<StreamParserEvent>, text: String) {
    if !text.is_empty() {
        events.push(StreamParserEvent::Text { text });
    }
}

fn emit_tool_call_progress(state: &mut JsonMixStreamState, events: &mut Vec<StreamParserEvent>, tools: &[Tool], options: &JsonMixOptions) {
    if !state.is_inside_tool_call || state.current_tool_call_json.is_empty() {
        return;
    }

    let progress = extract_arguments_progress(&state.current_tool_call_json);
    let tool_names = normalize_tool_names(tools);
    let (Some(tool_name), Some(arguments_text), true) = (progress.tool_name, progress.arguments_text, progress.arguments_complete) else { return };
    if !tool_names.contains(tool_name.as_str()) {
        return;
    }

    let Some(parsed_arguments) = parse_relaxed_json(&arguments_text).and_then(|v| v.as_object().cloned()) else { return };

    if state.active_tool_call.is_none() {
        let index = state.tool_call_count;
        let id = (options.create_tool_call_id)(index);
        state.active_tool_call = Some(ActiveToolCall { index, id: id.clone(), name: tool_name.clone(), emitted_arguments: String::new() });
        state.tool_call_count += 1;
        events.push(StreamParserEvent::ToolcallStart { index, name: tool_name.clone(), id });
    }

    let active = state.active_tool_call.as_mut().expect("just set above");
    let canonical_arguments = Value::Object(parsed_arguments).to_string();
    if !canonical_arguments.starts_with(&active.emitted_arguments) {
        active.emitted_arguments.clear();
    }

    let arguments_delta = canonical_arguments[active.emitted_arguments.len()..].to_string();
    if arguments_delta.is_empty() {
        return;
    }

    active.emitted_arguments = canonical_arguments;
    events.push(StreamParserEvent::ToolcallDelta { index: active.index, arguments_delta });
}

fn finalize_tool_call(state: &mut JsonMixStreamState, events: &mut Vec<StreamParserEvent>, tools: &[Tool], options: &JsonMixOptions, parser_options: Option<&ParserOptions>) {
    let full_segment = format!("{}{}{}", options.tool_call_start, state.current_tool_call_json, options.tool_call_end);
    let parsed_tool_call = parse_tool_call_json(&state.current_tool_call_json, tools);

    let Some(parsed_tool_call) = parsed_tool_call else {
        if let Some(parser_options) = parser_options {
            parser_options.report_error("Could not process JSON tool call, keeping original text.", Some(HashMap::from([("toolCall".to_string(), Value::String(full_segment.clone()))])));
        }
        if let Some(active) = &state.active_tool_call {
            events.push(StreamParserEvent::ToolcallEnd {
                index: active.index,
                name: active.name.clone(),
                id: active.id.clone(),
                arguments: extract_best_effort_arguments(&state.current_tool_call_json),
                incomplete: true,
                error_message: Some("Tool call arguments could not be parsed".to_string()),
            });
        }
        if should_emit_raw_tool_call_text_on_error(parser_options) {
            emit_text(events, full_segment);
        }
        state.active_tool_call = None;
        state.current_tool_call_json.clear();
        state.is_inside_tool_call = false;
        return;
    };

    if state.active_tool_call.is_none() {
        let index = state.tool_call_count;
        let id = (options.create_tool_call_id)(index);
        state.active_tool_call = Some(ActiveToolCall { index, id: id.clone(), name: parsed_tool_call.name.clone(), emitted_arguments: String::new() });
        state.tool_call_count += 1;
        events.push(StreamParserEvent::ToolcallStart { index, name: parsed_tool_call.name.clone(), id });
    }

    let active = state.active_tool_call.as_mut().expect("just set above");
    let canonical_arguments = Value::Object(parsed_tool_call.arguments.clone()).to_string();
    if canonical_arguments != active.emitted_arguments {
        let arguments_delta = canonical_arguments[active.emitted_arguments.len().min(canonical_arguments.len())..].to_string();
        if !arguments_delta.is_empty() {
            events.push(StreamParserEvent::ToolcallDelta { index: active.index, arguments_delta });
        }
    }

    events.push(StreamParserEvent::ToolcallEnd {
        index: active.index,
        name: parsed_tool_call.name.clone(),
        id: active.id.clone(),
        arguments: parsed_tool_call.arguments,
        incomplete: false,
        error_message: None,
    });

    state.active_tool_call = None;
    state.current_tool_call_json.clear();
    state.is_inside_tool_call = false;
}

fn flush_inside_tool_call_buffer(state: &mut JsonMixStreamState, events: &mut Vec<StreamParserEvent>, tools: &[Tool], options: &JsonMixOptions) {
    let potential_end_index = get_potential_start_index(&state.buffer, options.tool_call_end);
    if let Some(potential_end_index) = potential_end_index
        && potential_end_index + options.tool_call_end.chars().count() > state.buffer.chars().count()
    {
        let chars: Vec<char> = state.buffer.chars().collect();
        state.current_tool_call_json.push_str(&chars[..potential_end_index].iter().collect::<String>());
        state.buffer = chars[potential_end_index..].iter().collect();
        emit_tool_call_progress(state, events, tools, options);
        return;
    }

    state.current_tool_call_json.push_str(&state.buffer);
    state.buffer.clear();
    emit_tool_call_progress(state, events, tools, options);
}

fn flush_outside_tool_call_buffer(state: &mut JsonMixStreamState, events: &mut Vec<StreamParserEvent>, options: &JsonMixOptions) {
    let potential_start_index = get_potential_start_index(&state.buffer, options.tool_call_start);
    if let Some(potential_start_index) = potential_start_index
        && potential_start_index + options.tool_call_start.chars().count() > state.buffer.chars().count()
    {
        let chars: Vec<char> = state.buffer.chars().collect();
        emit_text(events, chars[..potential_start_index].iter().collect());
        state.buffer = chars[potential_start_index..].iter().collect();
        return;
    }

    emit_text(events, std::mem::take(&mut state.buffer));
}

pub fn format_json_mix_tool_call(name: &str, args: &Map<String, Value>, tool_call_start: &str, tool_call_end: &str) -> String {
    let mut wrapper = Map::new();
    wrapper.insert("name".to_string(), Value::String(name.to_string()));
    wrapper.insert("arguments".to_string(), Value::Object(args.clone()));
    format!("{tool_call_start}\n{}\n{tool_call_end}", Value::Object(wrapper))
}

pub fn parse_json_mix_generated_text(text: &str, tools: &[Tool], tool_call_start: &str, tool_call_end: &str, parser_options: Option<&ParserOptions>) -> Vec<ParsedToolCall> {
    let mut parsed_tool_calls = Vec::new();
    let mut cursor = 0usize;

    while let Some(start_offset) = text[cursor..].find(tool_call_start) {
        let body_start = cursor + start_offset + tool_call_start.len();
        let Some(end_offset) = text[body_start..].find(tool_call_end) else { break };
        let body_end = body_start + end_offset;
        let tool_call_json = &text[body_start..body_end];
        let full_match = &text[cursor + start_offset..body_end + tool_call_end.len()];

        match parse_tool_call_json(tool_call_json, tools) {
            Some(parsed) => parsed_tool_calls.push(parsed),
            None => {
                if let Some(parser_options) = parser_options {
                    parser_options.report_error("Could not process JSON tool call, keeping original text.", Some(HashMap::from([("toolCall".to_string(), Value::String(full_match.to_string()))])));
                }
            }
        }
        cursor = body_end + tool_call_end.len();
    }

    parsed_tool_calls
}

struct JsonMixStreamParser {
    tools: Vec<Tool>,
    options: JsonMixOptions,
    parser_options: Option<ParserOptions>,
    state: JsonMixStreamState,
}

impl StreamParser for JsonMixStreamParser {
    fn feed(&mut self, text_delta: &str) -> Vec<StreamParserEvent> {
        let mut events = Vec::new();
        self.state.buffer.push_str(text_delta);

        let mut next_tag_index = get_potential_start_index(&self.state.buffer, if self.state.is_inside_tool_call { self.options.tool_call_end } else { self.options.tool_call_start });

        while let Some(index) = next_tag_index {
            let current_tag = if self.state.is_inside_tool_call { self.options.tool_call_end } else { self.options.tool_call_start };
            let buffer_char_len = self.state.buffer.chars().count();
            if index + current_tag.chars().count() > buffer_char_len {
                break;
            }

            let chars: Vec<char> = self.state.buffer.chars().collect();
            if self.state.is_inside_tool_call {
                self.state.current_tool_call_json.push_str(&chars[..index].iter().collect::<String>());
                self.state.buffer = chars[index + current_tag.chars().count()..].iter().collect();
                emit_tool_call_progress(&mut self.state, &mut events, &self.tools, &self.options);
                finalize_tool_call(&mut self.state, &mut events, &self.tools, &self.options, self.parser_options.as_ref());
            } else {
                emit_text(&mut events, chars[..index].iter().collect());
                self.state.buffer = chars[index + current_tag.chars().count()..].iter().collect();
                self.state.is_inside_tool_call = true;
                self.state.current_tool_call_json.clear();
                self.state.active_tool_call = None;
            }

            next_tag_index = get_potential_start_index(&self.state.buffer, if self.state.is_inside_tool_call { self.options.tool_call_end } else { self.options.tool_call_start });
        }

        if self.state.is_inside_tool_call {
            flush_inside_tool_call_buffer(&mut self.state, &mut events, &self.tools, &self.options);
        } else {
            flush_outside_tool_call_buffer(&mut self.state, &mut events, &self.options);
        }

        events
    }

    fn finish(&mut self) -> Vec<StreamParserEvent> {
        let mut events = Vec::new();

        if !self.state.is_inside_tool_call {
            emit_text(&mut events, std::mem::take(&mut self.state.buffer));
            return events;
        }

        let raw = format!("{}{}", self.state.current_tool_call_json, self.state.buffer);
        let remainder = strip_trailing_tool_call_end_prefix(&raw, self.options.tool_call_end);
        let parsed_tool_call = parse_tool_call_json(&remainder, &self.tools);
        let resolved_tool = parsed_tool_call.as_ref().and_then(|call| self.tools.iter().find(|tool| tool.name == call.name));

        if let (Some(parsed_tool_call), Some(resolved_tool)) = (&parsed_tool_call, resolved_tool) {
            let tool_call_id = self.state.active_tool_call.as_ref().map(|a| a.id.clone()).unwrap_or_else(|| (self.options.create_tool_call_id)(self.state.tool_call_count));
            let tool_call = crate::types::ToolCall {
                id: tool_call_id,
                name: parsed_tool_call.name.clone(),
                arguments: parsed_tool_call.arguments.clone(),
                incomplete: None,
                error_message: None,
                thought_signature: None,
                namespace: None,
            };

            if validate_tool_arguments(resolved_tool, &tool_call).is_ok() {
                if self.state.active_tool_call.is_none() {
                    let index = self.state.tool_call_count;
                    let id = (self.options.create_tool_call_id)(index);
                    self.state.active_tool_call = Some(ActiveToolCall { index, id: id.clone(), name: parsed_tool_call.name.clone(), emitted_arguments: String::new() });
                    self.state.tool_call_count += 1;
                    events.push(StreamParserEvent::ToolcallStart { index, name: parsed_tool_call.name.clone(), id });
                }

                let active = self.state.active_tool_call.as_mut().expect("just set above");
                let canonical_arguments = Value::Object(parsed_tool_call.arguments.clone()).to_string();
                if canonical_arguments != active.emitted_arguments {
                    let arguments_delta = canonical_arguments[active.emitted_arguments.len().min(canonical_arguments.len())..].to_string();
                    if !arguments_delta.is_empty() {
                        events.push(StreamParserEvent::ToolcallDelta { index: active.index, arguments_delta });
                    }
                }

                events.push(StreamParserEvent::ToolcallEnd {
                    index: active.index,
                    name: parsed_tool_call.name.clone(),
                    id: active.id.clone(),
                    arguments: parsed_tool_call.arguments.clone(),
                    incomplete: false,
                    error_message: None,
                });

                self.state.active_tool_call = None;
                self.state.current_tool_call_json.clear();
                self.state.is_inside_tool_call = false;
                self.state.buffer.clear();
                return events;
            }
        }

        let name = extract_top_level_string_property(&remainder, "name");
        let known_tool = name.as_deref().and_then(|n| self.tools.iter().find(|tool| tool.name == n));
        let active_tool_call_name = self.state.active_tool_call.as_ref().map(|a| a.name.clone());

        if known_tool.is_some() || active_tool_call_name.is_some() {
            let tool_call_name = active_tool_call_name.or_else(|| known_tool.map(|t| t.name.clone()));
            if let Some(tool_call_name) = tool_call_name {
                let (index, id) = if let Some(active) = &self.state.active_tool_call {
                    (active.index, active.id.clone())
                } else {
                    let index = self.state.tool_call_count;
                    let id = (self.options.create_tool_call_id)(index);
                    self.state.tool_call_count += 1;
                    events.push(StreamParserEvent::ToolcallStart { index, name: tool_call_name.clone(), id: id.clone() });
                    (index, id)
                };
                events.push(StreamParserEvent::ToolcallEnd {
                    index,
                    name: tool_call_name,
                    id,
                    arguments: extract_best_effort_arguments(&remainder),
                    incomplete: true,
                    error_message: Some("Tool call was truncated mid-arguments".to_string()),
                });
            }
            if let Some(parser_options) = &self.parser_options {
                parser_options.report_error("Could not complete streaming JSON tool call at finish.", Some(HashMap::from([("protocol".to_string(), Value::String("hermes".to_string())), ("retainedLength".to_string(), Value::from(raw.len()))])));
            }
        } else if let Some(_name) = &name {
            if let Some(parser_options) = &self.parser_options {
                parser_options.report_error("Could not complete streaming JSON tool call at finish.", Some(HashMap::from([("protocol".to_string(), Value::String("hermes".to_string())), ("retainedLength".to_string(), Value::from(raw.len()))])));
            }
            if should_emit_raw_tool_call_text_on_error(self.parser_options.as_ref()) {
                emit_text(&mut events, format!("{}{}", self.options.tool_call_start, raw));
            }
        } else if let Some(parser_options) = &self.parser_options {
            parser_options.report_error("Could not complete streaming JSON tool call at finish.", Some(HashMap::from([("protocol".to_string(), Value::String("hermes".to_string())), ("retainedLength".to_string(), Value::from(raw.len()))])));
        }

        self.state.active_tool_call = None;
        self.state.current_tool_call_json.clear();
        self.state.is_inside_tool_call = false;
        self.state.buffer.clear();
        events
    }
}

pub fn create_json_mix_stream_parser(tools: Vec<Tool>, options: JsonMixOptions, parser_options: Option<ParserOptions>) -> Box<dyn StreamParser + Send> {
    Box::new(JsonMixStreamParser {
        tools,
        options,
        parser_options,
        state: JsonMixStreamState { active_tool_call: None, buffer: String::new(), current_tool_call_json: String::new(), is_inside_tool_call: false, tool_call_count: 0 },
    })
}
