//! Port of senpi packages/ai/src/tool-call-middleware/protocols/morph-xml.ts.

use std::collections::HashMap;

use serde_json::{Map, Value};
use uuid::Uuid;

use super::xml_tool_tag_scanner::{find_earliest_xml_tool_tag, find_self_closing_tool_tag, get_safe_xml_text_length};
use crate::tool_call_middleware::types::{ParsedToolCall, ParserOptions, StreamParser, StreamParserEvent, ToolResultContent};
use crate::types::Tool;
use crate::utils::validation::validate_tool_arguments;

const INDENT: &str = "   ";

pub fn morph_xml_format_tools_system_prompt(tools: &[Tool]) -> String {
    let tools_text = render_tools_for_xml_prompt(tools);

    let header = "# Tools\nYou may call one or more functions to assist with the user query.";
    let definitions = format!("You have access to the following functions:\n<tools>\n{tools_text}\n</tools>");
    let rules = "<rules>\n- Use exactly one XML element whose tag name is the function name.\n- Put each parameter as a child element.\n- Values must follow the schema exactly (numbers, arrays, objects, enums -> copy as-is).\n- For array parameters, wrap each element in an <item> tag inside the parameter tag.\n- For array<object> parameters, every <item> must contain the object's fields as child tags.\n- Never repeat the array parameter tag to represent multiple entries.\n- Do not add or remove functions or parameters.\n- Each required parameter must appear once.\n- Output nothing before or after the function call.\n- It is also possible to call multiple types of functions in one turn or to call a single function multiple times.\n</rules>";
    let examples = "For each function call, output the function name and parameter in the following format:\n<example_function_name>\n   <example_parameter_1>value_1</example_parameter_1>\n   <example_parameter_2>This is the value for the second parameter\nthat can span\nmultiple lines</example_parameter_2>\n</example_function_name>";
    let array_example = "Array example:\n<example_array_tool>\n   <items>\n      <item>first</item>\n      <item>second</item>\n   </items>\n</example_array_tool>\n\nArray<object> example:\n<example_todo_tool>\n   <todos>\n      <item>\n         <content>Inspect parser edge cases</content>\n         <status>in_progress</status>\n         <priority>high</priority>\n      </item>\n      <item>\n         <content>Add regression tests</content>\n         <status>pending</status>\n         <priority>medium</priority>\n      </item>\n   </todos>\n</example_todo_tool>";

    [header, &definitions, rules, examples, array_example]
        .into_iter()
        .filter(|section| !section.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn render_tools_for_xml_prompt(tools: &[Tool]) -> String {
    if tools.is_empty() {
        return "none".to_string();
    }
    tools.iter().map(render_tool_for_xml_prompt).collect::<Vec<_>>().join("\n\n")
}

fn render_tool_for_xml_prompt(tool: &Tool) -> String {
    let mut lines = vec![format!("name: {}", tool.name)];
    if !tool.description.is_empty() {
        lines.push(format!("description: {}", tool.description));
    }
    lines.push("parameters:".to_string());
    let normalized_schema = normalize_schema(&tool.parameters);
    lines.extend(render_parameters_summary(normalized_schema.as_ref(), 1));
    lines.push(format!("schema: {}", stringify_schema(normalized_schema.as_ref())));
    lines.join("\n")
}

/// Either a JSON-schema object/boolean or `undefined` (senpi `JsonSchema = Record<string, unknown> | boolean | undefined`).
#[derive(Clone, Debug, PartialEq)]
enum JsonSchema {
    Object(Map<String, Value>),
    Bool(bool),
}

impl JsonSchema {
    fn as_object(&self) -> Option<&Map<String, Value>> {
        match self {
            JsonSchema::Object(map) => Some(map),
            JsonSchema::Bool(_) => None,
        }
    }
}

fn normalize_schema(schema: &Value) -> Option<JsonSchema> {
    match schema {
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(map)) => Some(JsonSchema::Object(map)),
            Ok(_) | Err(_) => {
                let mut fallback = Map::new();
                fallback.insert("type".to_string(), Value::String("string".to_string()));
                fallback.insert("const".to_string(), Value::String(text.clone()));
                Some(JsonSchema::Object(fallback))
            }
        },
        Value::Object(map) => Some(JsonSchema::Object(map.clone())),
        Value::Bool(flag) => Some(JsonSchema::Bool(*flag)),
        _ => None,
    }
}

fn render_parameters_summary(schema: Option<&JsonSchema>, indent_level: usize) -> Vec<String> {
    let indent = INDENT.repeat(indent_level);

    let Some(schema) = schema else {
        return vec![format!("{indent}(none)")];
    };
    let schema_obj = match schema {
        JsonSchema::Bool(true) => return vec![format!("{indent}(any)")],
        JsonSchema::Bool(false) => return vec![format!("{indent}(no valid parameters)")],
        JsonSchema::Object(map) => map,
    };

    let schema_types = get_schema_types_from_value(schema_obj);
    let is_object_like = schema_types.contains(&"object".to_string()) || schema_obj.contains_key("properties");

    if is_object_like {
        let properties = schema_obj.get("properties").and_then(Value::as_object);
        let required: std::collections::HashSet<&str> = schema_obj
            .get("required")
            .and_then(Value::as_array)
            .map(|values| values.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();

        let Some(properties) = properties else {
            return vec![format!("{indent}(no named parameters)")];
        };
        let mut names: Vec<&String> = properties.keys().collect();
        names.sort();
        if names.is_empty() {
            return vec![format!("{indent}(no named parameters)")];
        }

        return names
            .into_iter()
            .map(|name| {
                let prop_schema = properties.get(name).and_then(normalize_schema);
                render_property_summary_line(&indent, name, prop_schema.as_ref(), required.contains(name.as_str()))
            })
            .collect();
    }

    vec![format!("{indent}- value ({})", summarize_type(Some(schema)))]
}

fn render_property_summary_line(indent: &str, prop_name: &str, prop_schema: Option<&JsonSchema>, required: bool) -> String {
    let type_label = summarize_type(prop_schema);
    let required_label = if required { "required" } else { "optional" };
    let extras = collect_property_extras(prop_schema);
    let extra_text = if extras.is_empty() { String::new() } else { format!(" - {}", extras.join("; ")) };
    format!("{indent}- {prop_name} ({type_label}, {required_label}){extra_text}")
}

fn collect_property_extras(prop_schema: Option<&JsonSchema>) -> Vec<String> {
    let Some(schema) = prop_schema.and_then(JsonSchema::as_object) else { return Vec::new() };
    let mut extras = Vec::new();
    if let Some(Value::Array(values)) = schema.get("enum") {
        extras.push(format!("enum: {}", format_value(&Value::Array(values.clone()))));
    }
    if let Some(default) = schema.get("default") {
        extras.push(format!("default: {}", format_value(default)));
    }
    if let Some(Value::String(description)) = schema.get("description") {
        extras.push(description.clone());
    }
    extras
}

fn summarize_type(schema: Option<&JsonSchema>) -> String {
    let schema = match schema {
        None => return "unknown".to_string(),
        Some(JsonSchema::Bool(true)) => return "any".to_string(),
        Some(JsonSchema::Bool(false)) => return "never".to_string(),
        Some(JsonSchema::Object(map)) => map,
    };

    let schema_type = schema.get("type");
    let mut base_type = String::new();
    if let Some(Value::Array(types)) = schema_type {
        let names: Vec<&str> = types.iter().filter_map(Value::as_str).collect();
        if !names.is_empty() {
            base_type = names.join(" | ");
        }
    } else if let Some(Value::String(name)) = schema_type {
        base_type = name.clone();
    } else if let Some(Value::Array(values)) = schema.get("enum") {
        let inferred: std::collections::HashSet<&'static str> = values.iter().map(json_type_name).collect();
        if inferred.len() == 1 {
            base_type = inferred.into_iter().next().unwrap_or_default().to_string();
        }
    } else if schema.contains_key("const") {
        base_type = json_type_name(schema.get("const").unwrap_or(&Value::Null)).to_string();
    }

    if base_type.is_empty() {
        base_type = "any".to_string();
    }

    if base_type == "array"
        && let Some(items) = schema.get("items")
    {
        {
            let item_type = match items {
                Value::Array(list) => list.iter().map(|item| summarize_type(normalize_schema(item).as_ref())).collect::<Vec<_>>().join(" | "),
                other => summarize_type(normalize_schema(other).as_ref()),
            };
            return format!("array<{item_type}>");
        }
    }

    if base_type == "string"
        && let Some(Value::String(format)) = schema.get("format")
    {
        return format!("string ({format})");
    }

    base_type
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "string",
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        Value::Null => "null",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn format_value(value: &Value) -> String {
    match value {
        Value::String(text) => serde_json::to_string(text).unwrap_or_default(),
        Value::Number(_) | Value::Bool(_) => value.to_string(),
        Value::Null => "null".to_string(),
        Value::Array(items) => format!("[{}]", items.iter().map(format_value).collect::<Vec<_>>().join(", ")),
        Value::Object(_) => value.to_string(),
    }
}

fn stringify_schema(schema: Option<&JsonSchema>) -> String {
    match schema {
        None => "null".to_string(),
        Some(JsonSchema::Bool(flag)) => flag.to_string(),
        Some(JsonSchema::Object(map)) => strip_schema_keys(&Value::Object(map.clone())).to_string(),
    }
}

fn strip_schema_keys(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(strip_schema_keys).collect()),
        Value::Object(map) => {
            Value::Object(map.iter().filter(|(key, _)| key.as_str() != "$schema").map(|(key, value)| (key.clone(), strip_schema_keys(value))).collect())
        }
        other => other.clone(),
    }
}

pub fn morph_xml_format_tool_response(tool_name: &str, _tool_call_id: &str, content: &[ToolResultContent]) -> String {
    let combined_text = content.iter().filter_map(ToolResultContent::as_text).map(|c| c.text.as_str()).collect::<Vec<_>>().join("\n");
    let result_value = serde_json::from_str::<Value>(&combined_text).unwrap_or(Value::String(combined_text));
    let result_lines = format_xml_node("result", Some(&result_value), 1);

    let mut lines = vec!["<tool_response>".to_string(), format!("   <tool_name>{}</tool_name>", escape_xml(tool_name))];
    lines.extend(result_lines);
    lines.push("</tool_response>".to_string());
    lines.join("\n")
}

pub fn morph_xml_format_tool_call(name: &str, args: &Map<String, Value>) -> String {
    format_xml_node(name, Some(&Value::Object(args.clone())), 0).join("\n")
}

struct StreamToolState {
    id: String,
    index: usize,
    last_arguments_snapshot: Option<String>,
    name: String,
    schema: Option<JsonSchema>,
    started: bool,
    tool: Option<Tool>,
}

fn should_emit_raw_tool_call_text_on_error(options: Option<&ParserOptions>) -> bool {
    options.is_some_and(|o| o.emit_raw_tool_call_text_on_error)
}

fn get_schema_types_from_value(schema: &Map<String, Value>) -> Vec<String> {
    match schema.get("type") {
        Some(Value::String(name)) => vec![name.clone()],
        Some(Value::Array(names)) => names.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        _ => Vec::new(),
    }
}

fn is_structurally_compatible_with_schema(value: &Value, schema: Option<&JsonSchema>) -> bool {
    let schema_obj = match schema {
        None => return true,
        Some(JsonSchema::Bool(true)) => return true,
        Some(JsonSchema::Bool(false)) => return false,
        Some(JsonSchema::Object(map)) => map,
    };

    let schema_types = get_schema_types_from_value(schema_obj);
    match value {
        Value::Null => schema_types.is_empty() || schema_types.iter().any(|t| t == "null"),
        Value::Array(items) => {
            if !schema_types.is_empty() && !schema_types.iter().any(|t| t == "array") {
                return false;
            }
            let item_schema = get_array_item_schema(Some(&JsonSchema::Object(schema_obj.clone())));
            items.iter().all(|item| is_structurally_compatible_with_schema(item, item_schema.as_ref()))
        }
        Value::Object(map) => {
            if !schema_types.is_empty() && !schema_types.iter().any(|t| t == "object") {
                return false;
            }
            map.iter().all(|(key, entry)| {
                let property_schema = get_property_schema(Some(&JsonSchema::Object(schema_obj.clone())), key);
                is_structurally_compatible_with_schema(entry, property_schema.as_ref())
            })
        }
        Value::String(_) => schema_types.is_empty() || schema_types.iter().any(|t| t == "string"),
        Value::Number(_) => schema_types.is_empty() || schema_types.iter().any(|t| t == "number" || t == "integer"),
        Value::Bool(_) => schema_types.is_empty() || schema_types.iter().any(|t| t == "boolean"),
    }
}

fn validate_morph_xml_arguments(tool: Option<&Tool>, arguments_record: &Map<String, Value>) -> bool {
    let Some(tool) = tool else { return true };
    let tool_call = crate::types::ToolCall {
        id: "morph-xml-validation".to_string(),
        name: tool.name.clone(),
        arguments: arguments_record.clone(),
        incomplete: None,
        error_message: None,
        thought_signature: None,
        namespace: None,
    };
    validate_tool_arguments(tool, &tool_call).is_ok()
}

fn parse_and_validate_morph_xml_arguments(tool: Option<&Tool>, tool_body: &str, schema: Option<&JsonSchema>) -> Option<Map<String, Value>> {
    let parsed_arguments = parse_morph_xml_arguments(tool_body, schema);
    if !is_structurally_compatible_with_schema(&Value::Object(parsed_arguments.clone()), schema) {
        return None;
    }
    if !validate_morph_xml_arguments(tool, &parsed_arguments) {
        return None;
    }
    Some(parsed_arguments)
}

fn parse_morph_xml_partial_arguments_safely(tool_body: &str, schema: Option<&JsonSchema>) -> Option<Map<String, Value>> {
    parse_morph_xml_partial_arguments(tool_body, schema)
}

pub fn parse_morph_xml_generated_text(text: &str, tools: &[Tool], options: Option<&ParserOptions>) -> Vec<ParsedToolCall> {
    if tools.is_empty() || text.is_empty() {
        return Vec::new();
    }

    let tool_schema_map = create_tool_schema_map(tools);
    let tool_map: HashMap<&str, &Tool> = tools.iter().map(|t| (t.name.as_str(), t)).collect();
    let mut parsed_tool_calls = Vec::new();

    for tool in tools {
        let schema = tool_schema_map.get(&tool.name).and_then(Option::as_ref);
        let mut cursor = 0usize;
        while let Some((index, length, tag)) = find_self_closing_tool_tag(text, &tool.name, cursor) {
            let parsed_arguments = parse_and_validate_morph_xml_arguments(Some(tool), "", schema);
            match parsed_arguments {
                Some(arguments) => parsed_tool_calls.push(ParsedToolCall { name: tool.name.clone(), arguments }),
                None => {
                    if let Some(options) = options {
                        options.report_error("Could not process XML tool call.", Some(HashMap::from([("toolCall".to_string(), Value::String(tag))])));
                    }
                }
            }
            cursor = index + length;
        }

        for (tool_body, full_match) in find_paired_tag_matches(text, &tool.name) {
            let parsed_arguments = parse_and_validate_morph_xml_arguments(tool_map.get(tool.name.as_str()).copied(), &tool_body, schema);
            match parsed_arguments {
                Some(arguments) => parsed_tool_calls.push(ParsedToolCall { name: tool.name.clone(), arguments }),
                None => {
                    if let Some(options) = options {
                        options.report_error("Could not process XML tool call.", Some(HashMap::from([("toolCall".to_string(), Value::String(full_match))])));
                    }
                }
            }
        }
    }

    parsed_tool_calls
}

/// `<\s*name\s*>([\s\S]*?)<\/\s*name\s*>` with the `g` flag: returns each `(body, fullMatch)` pair.
fn find_paired_tag_matches(text: &str, tool_name: &str) -> Vec<(String, String)> {
    let mut results = Vec::new();
    let mut cursor = 0usize;
    while let Some(open) = match_open_tag(text, tool_name, cursor) {
        let Some(close) = match_close_tag(text, tool_name, open.end) else {
            break;
        };
        let body = text[open.end..close.start].to_string();
        let full_match = text[open.start..close.end].to_string();
        results.push((body, full_match));
        cursor = close.end;
    }
    results
}

struct TagSpan {
    start: usize,
    end: usize,
}

fn match_open_tag(text: &str, tool_name: &str, from_index: usize) -> Option<TagSpan> {
    let bytes = text.as_bytes();
    let mut i = from_index;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let mut cursor = i + 1;
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
            return Some(TagSpan { start: i, end: cursor + 1 });
        }
        i += 1;
    }
    None
}

fn match_close_tag(text: &str, tool_name: &str, from_index: usize) -> Option<TagSpan> {
    let bytes = text.as_bytes();
    let mut i = from_index;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let mut cursor = i + 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
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
            return Some(TagSpan { start: i, end: cursor + 1 });
        }
        i += 1;
    }
    None
}

/// The first (leftmost) `</\s*name\s*>` at or after `from_index`, or `None`.
fn find_close_tag(text: &str, tool_name: &str, from_index: usize) -> Option<TagSpan> {
    match_close_tag(text, tool_name, from_index)
}

pub fn create_morph_xml_stream_parser(tools: Vec<Tool>, options: Option<ParserOptions>) -> Box<dyn StreamParser + Send> {
    Box::new(MorphXmlStreamParser {
        tool_schema_map: create_tool_schema_map(&tools),
        tool_names: tools.iter().map(|t| t.name.clone()).collect(),
        tools,
        options,
        buffer: String::new(),
        next_tool_call_index: 0,
        current_tool_state: None,
    })
}

struct MorphXmlStreamParser {
    tool_schema_map: HashMap<String, Option<JsonSchema>>,
    tool_names: Vec<String>,
    tools: Vec<Tool>,
    options: Option<ParserOptions>,
    buffer: String,
    next_tool_call_index: usize,
    current_tool_state: Option<StreamToolState>,
}

impl MorphXmlStreamParser {
    fn tool_by_name(&self, name: &str) -> Option<&Tool> {
        self.tools.iter().find(|t| t.name == name)
    }

    fn emit_arguments_snapshot(&mut self, events: &mut Vec<StreamParserEvent>, arguments_record: &Map<String, Value>) {
        let Some(tool_state) = &mut self.current_tool_state else { return };
        if !is_structurally_compatible_with_schema(&Value::Object(arguments_record.clone()), tool_state.schema.as_ref()) {
            return;
        }

        let arguments_snapshot = Value::Object(arguments_record.clone()).to_string();
        if Some(&arguments_snapshot) == tool_state.last_arguments_snapshot.as_ref() || arguments_snapshot == "{}" {
            return;
        }

        if !tool_state.started {
            events.push(StreamParserEvent::ToolcallStart { index: tool_state.index, name: tool_state.name.clone(), id: tool_state.id.clone() });
            tool_state.started = true;
        }

        tool_state.last_arguments_snapshot = Some(arguments_snapshot.clone());
        events.push(StreamParserEvent::ToolcallDelta { index: tool_state.index, arguments_delta: arguments_snapshot });
    }

    fn process_buffer(&mut self) -> Vec<StreamParserEvent> {
        let mut events = Vec::new();

        while !self.buffer.is_empty() {
            if self.current_tool_state.is_some() {
                let tool_name = self.current_tool_state.as_ref().expect("checked above").name.clone();
                let closing_tag_match = find_close_tag(&self.buffer, &tool_name, 0);

                let Some(closing_tag_match) = closing_tag_match else {
                    let schema = self.current_tool_state.as_ref().expect("checked above").schema.clone();
                    if let Some(partial_arguments) = parse_morph_xml_partial_arguments_safely(&self.buffer, schema.as_ref()) {
                        self.emit_arguments_snapshot(&mut events, &partial_arguments);
                    }
                    break;
                };

                let tool_body = self.buffer[..closing_tag_match.start].to_string();
                let schema = self.current_tool_state.as_ref().expect("checked above").schema.clone();
                if let Some(partial_arguments) = parse_morph_xml_partial_arguments_safely(&tool_body, schema.as_ref()) {
                    self.emit_arguments_snapshot(&mut events, &partial_arguments);
                }

                let tool = self.current_tool_state.as_ref().expect("checked above").tool.clone();
                let parsed_arguments = parse_and_validate_morph_xml_arguments(tool.as_ref(), &tool_body, schema.as_ref());
                let original_call_text = format!("<{tool_name}>{tool_body}{}", &self.buffer[closing_tag_match.start..closing_tag_match.end]);
                self.buffer = self.buffer[closing_tag_match.end..].to_string();

                let Some(parsed_arguments) = parsed_arguments else {
                    if let Some(options) = &self.options {
                        options.report_error(
                            "Could not process streaming XML tool call.",
                            Some(HashMap::from([("toolCall".to_string(), Value::String(original_call_text.clone()))])),
                        );
                    }
                    let started = self.current_tool_state.as_ref().expect("checked above").started;
                    if !started && should_emit_raw_tool_call_text_on_error(self.options.as_ref()) {
                        events.push(StreamParserEvent::Text { text: original_call_text });
                    }
                    self.current_tool_state = None;
                    continue;
                };

                let (index, id, name, started) = {
                    let ts = self.current_tool_state.as_ref().expect("checked above");
                    (ts.index, ts.id.clone(), ts.name.clone(), ts.started)
                };
                if !started {
                    events.push(StreamParserEvent::ToolcallStart { index, name: name.clone(), id: id.clone() });
                    self.current_tool_state.as_mut().expect("checked above").started = true;
                    self.emit_arguments_snapshot(&mut events, &parsed_arguments);
                }
                events.push(StreamParserEvent::ToolcallEnd { index, name, id, arguments: parsed_arguments, incomplete: false, error_message: None });
                self.current_tool_state = None;
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
                let tool = self.tool_by_name(&opening_tag.name).cloned();
                let schema = self.tool_schema_map.get(&opening_tag.name).cloned().flatten();
                let parsed_arguments = parse_and_validate_morph_xml_arguments(tool.as_ref(), "", schema.as_ref());
                let Some(parsed_arguments) = parsed_arguments else {
                    if let Some(options) = &self.options {
                        options.report_error("Could not process XML tool call.", Some(HashMap::from([("toolCall".to_string(), Value::String(opening_tag.tag.clone()))])));
                    }
                    if should_emit_raw_tool_call_text_on_error(self.options.as_ref()) {
                        events.push(StreamParserEvent::Text { text: opening_tag.tag.clone() });
                    }
                    continue;
                };
                let id = Uuid::new_v4().to_string();
                let index = self.next_tool_call_index;
                self.next_tool_call_index += 1;
                events.push(StreamParserEvent::ToolcallStart { index, name: opening_tag.name.clone(), id: id.clone() });
                events.push(StreamParserEvent::ToolcallEnd { index, name: opening_tag.name.clone(), id, arguments: parsed_arguments, incomplete: false, error_message: None });
                continue;
            }

            let schema = self.tool_schema_map.get(&opening_tag.name).cloned().flatten();
            let tool = self.tool_by_name(&opening_tag.name).cloned();
            self.current_tool_state = Some(StreamToolState {
                id: Uuid::new_v4().to_string(),
                index: self.next_tool_call_index,
                last_arguments_snapshot: None,
                name: opening_tag.name.clone(),
                schema,
                started: false,
                tool,
            });
            self.next_tool_call_index += 1;
        }

        events
    }
}

impl StreamParser for MorphXmlStreamParser {
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
            let parsed_arguments = parse_and_validate_morph_xml_arguments(tool_state.tool.as_ref(), &self.buffer, tool_state.schema.as_ref());
            if let Some(parsed_arguments) = parsed_arguments {
                if !tool_state.started {
                    events.push(StreamParserEvent::ToolcallStart { index: tool_state.index, name: tool_state.name.clone(), id: tool_state.id.clone() });
                }
                let arguments_snapshot = Value::Object(parsed_arguments.clone()).to_string();
                if tool_state.last_arguments_snapshot.as_ref() != Some(&arguments_snapshot)
                    && is_structurally_compatible_with_schema(&Value::Object(parsed_arguments.clone()), tool_state.schema.as_ref())
                    && arguments_snapshot != "{}"
                {
                    events.push(StreamParserEvent::ToolcallDelta { index: tool_state.index, arguments_delta: arguments_snapshot });
                }
                events.push(StreamParserEvent::ToolcallEnd {
                    index: tool_state.index,
                    name: tool_state.name.clone(),
                    id: tool_state.id.clone(),
                    arguments: parsed_arguments,
                    incomplete: false,
                    error_message: None,
                });
            } else if let Some(snapshot) = &tool_state.last_arguments_snapshot {
                if let Some(options) = &self.options {
                    options.report_error(
                        "Could not complete streaming XML tool call at finish.",
                        Some(HashMap::from([
                            ("protocol".to_string(), Value::String("morph-xml".to_string())),
                            ("retainedLength".to_string(), Value::from(self.buffer.len())),
                        ])),
                    );
                }
                let snapshot_arguments = serde_json::from_str::<Value>(snapshot).ok().and_then(|v| v.as_object().cloned()).unwrap_or_default();
                if !tool_state.started {
                    events.push(StreamParserEvent::ToolcallStart { index: tool_state.index, name: tool_state.name.clone(), id: tool_state.id.clone() });
                }
                events.push(StreamParserEvent::ToolcallEnd {
                    index: tool_state.index,
                    name: tool_state.name.clone(),
                    id: tool_state.id.clone(),
                    arguments: snapshot_arguments,
                    incomplete: true,
                    error_message: Some("Tool call was truncated mid-arguments".to_string()),
                });
            } else {
                let partial_arguments = parse_morph_xml_partial_arguments_safely(&self.buffer, tool_state.schema.as_ref()).unwrap_or_default();
                if let Some(options) = &self.options {
                    options.report_error(
                        "Could not complete streaming XML tool call at finish.",
                        Some(HashMap::from([
                            ("protocol".to_string(), Value::String("morph-xml".to_string())),
                            ("retainedLength".to_string(), Value::from(self.buffer.len())),
                        ])),
                    );
                }
                if !tool_state.started {
                    events.push(StreamParserEvent::ToolcallStart { index: tool_state.index, name: tool_state.name.clone(), id: tool_state.id.clone() });
                }
                events.push(StreamParserEvent::ToolcallEnd {
                    index: tool_state.index,
                    name: tool_state.name.clone(),
                    id: tool_state.id.clone(),
                    arguments: partial_arguments,
                    incomplete: true,
                    error_message: Some("Tool call was truncated mid-arguments".to_string()),
                });
            }

            self.buffer.clear();
        }

        events
    }
}

fn format_xml_node(tag_name: &str, value: Option<&Value>, depth: usize) -> Vec<String> {
    let indent = INDENT.repeat(depth);

    match value {
        None | Some(Value::Null) => vec![format!("{indent}<{tag_name}></{tag_name}>")],
        Some(Value::String(text)) => vec![format!("{indent}<{tag_name}>{}</{tag_name}>", escape_xml(text))],
        Some(Value::Number(_)) | Some(Value::Bool(_)) => vec![format!("{indent}<{tag_name}>{}</{tag_name}>", value.expect("checked above"))],
        Some(Value::Array(items)) => {
            if items.is_empty() {
                return vec![format!("{indent}<{tag_name}></{tag_name}>")];
            }
            let mut lines = vec![format!("{indent}<{tag_name}>")];
            for item in items {
                lines.extend(format_xml_node("item", Some(item), depth + 1));
            }
            lines.push(format!("{indent}</{tag_name}>"));
            lines
        }
        Some(Value::Object(map)) => {
            if map.is_empty() {
                return vec![format!("{indent}<{tag_name}></{tag_name}>")];
            }
            let mut lines = vec![format!("{indent}<{tag_name}>")];
            for (key, entry_value) in map {
                lines.extend(format_xml_node(key, Some(entry_value), depth + 1));
            }
            lines.push(format!("{indent}</{tag_name}>"));
            lines
        }
    }
}

fn create_tool_schema_map(tools: &[Tool]) -> HashMap<String, Option<JsonSchema>> {
    tools.iter().map(|tool| (tool.name.clone(), normalize_schema(&tool.parameters))).collect()
}

fn parse_morph_xml_arguments(tool_body: &str, schema: Option<&JsonSchema>) -> Map<String, Value> {
    let wrapped = format!("<root>{tool_body}</root>");
    let Some(parsed_root) = parse_xml_root(&wrapped) else { return Map::new() };
    convert_children_to_object(&parsed_root.children, schema)
}

fn parse_morph_xml_partial_arguments(tool_body: &str, schema: Option<&JsonSchema>) -> Option<Map<String, Value>> {
    let mut partial_arguments = Map::new();
    let normalized_schema = schema.cloned();
    let mut position = 0usize;

    while position < tool_body.len() {
        let Some(next_tag_index) = tool_body[position..].find('<').map(|i| i + position) else { break };
        if !tool_body[position..next_tag_index].trim().is_empty() {
            break;
        }

        let Some(opening_tag) = parse_opening_tag(tool_body, next_tag_index) else { break };
        let property_schema = get_property_schema(normalized_schema.as_ref(), &opening_tag.name);
        let closing_tag = format!("</{}>", opening_tag.name);
        let closing_tag_index = tool_body[opening_tag.end_index..].find(&closing_tag).map(|i| i + opening_tag.end_index);

        let Some(closing_tag_index) = closing_tag_index else {
            let partial_value_text = &tool_body[opening_tag.end_index..];
            if partial_value_text.contains('<') {
                break;
            }
            partial_arguments.insert(opening_tag.name.clone(), coerce_partial_xml_value(partial_value_text, property_schema.as_ref()));
            break;
        };

        let complete_node = parse_xml_root(&tool_body[next_tag_index..closing_tag_index + closing_tag.len()]);
        let Some(complete_node) = complete_node else { break };
        partial_arguments.insert(opening_tag.name.clone(), convert_xml_node_value(&complete_node, property_schema.as_ref()));
        position = closing_tag_index + closing_tag.len();
    }

    Some(partial_arguments)
}

#[derive(Clone)]
enum XmlPart {
    Text(String),
    Child(XmlNode),
}

#[derive(Clone)]
struct XmlNode {
    name: String,
    children: Vec<XmlNode>,
    parts: Vec<XmlPart>,
    text_segments: Vec<String>,
}

/// Scans `<\s*(\/)?\s*([A-Za-z_][\w.-]*)\s*(\/)?\s*>` tags in document order and builds one root
/// `XmlNode`, mirroring senpi's hand-rolled stack-based XML reader (not a general XML parser).
fn parse_xml_root(xml: &str) -> Option<XmlNode> {
    let mut stack: Vec<XmlNode> = Vec::new();
    let mut root_node: Option<XmlNode> = None;
    let mut last_index = 0usize;
    let bytes = xml.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let tag_start = i;
        let mut cursor = i + 1;
        let is_closing_tag = cursor < bytes.len() && bytes[cursor] == b'/';
        if is_closing_tag {
            cursor += 1;
        }
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let name_start = cursor;
        if cursor >= bytes.len() || !(bytes[cursor].is_ascii_alphabetic() || bytes[cursor] == b'_') {
            i += 1;
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && (bytes[cursor].is_ascii_alphanumeric() || matches!(bytes[cursor], b'_' | b'.' | b'-')) {
            cursor += 1;
        }
        let tag_name = xml[name_start..cursor].to_string();
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let is_self_closing = cursor < bytes.len() && bytes[cursor] == b'/';
        if is_self_closing {
            cursor += 1;
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
        }
        if cursor >= bytes.len() || bytes[cursor] != b'>' {
            i += 1;
            continue;
        }
        let tag_end = cursor + 1;

        let text_content = &xml[last_index..tag_start];
        if !stack.is_empty() && !text_content.is_empty() {
            let top = stack.last_mut().expect("checked above");
            top.text_segments.push(text_content.to_string());
            top.parts.push(XmlPart::Text(text_content.to_string()));
        } else if stack.is_empty() && !text_content.trim().is_empty() {
            return None;
        }

        if is_closing_tag {
            let completed_node = stack.pop()?;
            if completed_node.name != tag_name {
                return None;
            }
            if let Some(top) = stack.last_mut() {
                top.children.push(completed_node.clone());
                top.parts.push(XmlPart::Child(completed_node));
            } else if root_node.is_none() {
                root_node = Some(completed_node);
            } else {
                return None;
            }
        } else {
            let node = XmlNode { name: tag_name, children: Vec::new(), parts: Vec::new(), text_segments: Vec::new() };
            if is_self_closing {
                if let Some(top) = stack.last_mut() {
                    top.children.push(node.clone());
                    top.parts.push(XmlPart::Child(node));
                } else if root_node.is_none() {
                    root_node = Some(node);
                } else {
                    return None;
                }
            } else {
                stack.push(node);
            }
        }

        last_index = tag_end;
        i = tag_end;
    }

    if !stack.is_empty() {
        return None;
    }
    if last_index < xml.len() && !xml[last_index..].trim().is_empty() {
        return None;
    }

    root_node
}

fn serialize_xml_node(node: &XmlNode) -> String {
    let inner_content: String = node
        .parts
        .iter()
        .map(|part| match part {
            XmlPart::Text(text) => text.clone(),
            XmlPart::Child(child) => serialize_xml_node(child),
        })
        .collect();
    format!("<{}>{}</{}>", node.name, inner_content, node.name)
}

fn convert_children_to_object(children: &[XmlNode], schema: Option<&JsonSchema>) -> Map<String, Value> {
    let mut object_value = Map::new();

    for child in children {
        let property_schema = get_property_schema(schema, &child.name);
        let property_value = convert_xml_node_value(child, property_schema.as_ref());

        match object_value.get(&child.name).cloned() {
            None => {
                object_value.insert(child.name.clone(), property_value);
            }
            Some(Value::Array(mut existing)) => {
                existing.push(property_value);
                object_value.insert(child.name.clone(), Value::Array(existing));
            }
            Some(existing) => {
                object_value.insert(child.name.clone(), Value::Array(vec![existing, property_value]));
            }
        }
    }

    object_value
}

fn convert_xml_node_value(node: &XmlNode, schema: Option<&JsonSchema>) -> Value {
    let normalized_schema = schema.cloned();
    let schema_types = get_schema_types(normalized_schema.as_ref());

    if node.children.is_empty() {
        return coerce_xml_value(&node.text_segments.join(""), normalized_schema.as_ref());
    }

    if schema_types.iter().any(|t| t == "string") {
        let text: String = node
            .parts
            .iter()
            .map(|part| match part {
                XmlPart::Text(text) => unescape_xml(text),
                XmlPart::Child(child) => serialize_xml_node(child),
            })
            .collect();
        return Value::String(text);
    }

    if should_treat_as_array(node, normalized_schema.as_ref()) {
        if node.children.iter().any(|child| child.name != "item") {
            return Value::String(String::new());
        }
        let item_schema = get_array_item_schema(normalized_schema.as_ref());
        let item_nodes: Vec<&XmlNode> = node.children.iter().filter(|child| child.name == "item").collect();
        return Value::Array(item_nodes.into_iter().map(|child| convert_xml_node_value(child, item_schema.as_ref())).collect());
    }

    Value::Object(convert_children_to_object(&node.children, normalized_schema.as_ref()))
}

fn should_treat_as_array(node: &XmlNode, schema: Option<&JsonSchema>) -> bool {
    let types = get_schema_types(schema);
    if types.iter().any(|t| t == "array") {
        return true;
    }
    node.children.iter().all(|child| child.name == "item")
}

fn get_array_item_schema(schema: Option<&JsonSchema>) -> Option<JsonSchema> {
    let schema = schema?.as_object()?;
    match schema.get("items") {
        Some(items @ Value::Object(_)) => normalize_schema(items),
        _ => None,
    }
}

fn get_property_schema(schema: Option<&JsonSchema>, property_name: &str) -> Option<JsonSchema> {
    let schema_obj = schema?.as_object()?;

    if let Some(Value::Object(properties)) = schema_obj.get("properties")
        && let Some(property_schema) = properties.get(property_name)
        && matches!(property_schema, Value::Bool(_) | Value::Object(_))
    {
        return normalize_schema(property_schema);
    }

    for union_key in ["anyOf", "oneOf", "allOf"] {
        let Some(Value::Array(union_schemas)) = schema_obj.get(union_key) else { continue };
        for union_schema in union_schemas {
            let Value::Object(union_map) = union_schema else { continue };
            if let Some(nested) = get_property_schema(Some(&JsonSchema::Object(union_map.clone())), property_name) {
                return Some(nested);
            }
        }
    }

    None
}

fn get_schema_types(schema: Option<&JsonSchema>) -> Vec<String> {
    let Some(schema) = schema.and_then(JsonSchema::as_object) else { return Vec::new() };
    get_schema_types_from_value(schema)
}

fn coerce_xml_value(raw_value: &str, schema: Option<&JsonSchema>) -> Value {
    let decoded_value = unescape_xml(raw_value);
    let trimmed_value = decoded_value.trim();
    let types = get_schema_types(schema);

    if types.iter().any(|t| t == "integer")
        && is_integer_literal(trimmed_value)
        && let Ok(n) = trimmed_value.parse::<i64>()
    {
        return Value::from(n);
    }
    if types.iter().any(|t| t == "number") && is_number_literal(trimmed_value)
        && let Ok(n) = trimmed_value.parse::<f64>()
    {
        return Value::from(n);
    }
    if types.iter().any(|t| t == "boolean") {
        if trimmed_value == "true" {
            return Value::Bool(true);
        }
        if trimmed_value == "false" {
            return Value::Bool(false);
        }
    }
    if types.iter().any(|t| t == "null") && trimmed_value == "null" {
        return Value::Null;
    }
    if (types.iter().any(|t| t == "array" || t == "object")) && !trimmed_value.is_empty()
        && let Ok(parsed) = serde_json::from_str::<Value>(trimmed_value)
    {
        return parsed;
    }

    Value::String(decoded_value)
}

fn coerce_partial_xml_value(raw_value: &str, schema: Option<&JsonSchema>) -> Value {
    let types = get_schema_types(schema);
    if types.iter().any(|t| t == "integer" || t == "number") {
        let trimmed_value = unescape_xml(raw_value).trim().to_string();
        if !trimmed_value.is_empty() && is_number_literal(&trimmed_value) {
            if types.iter().any(|t| t == "integer") {
                if let Ok(n) = trimmed_value.parse::<i64>() {
                    return Value::from(n);
                }
            } else if let Ok(n) = trimmed_value.parse::<f64>() {
                return Value::from(n);
            }
        }
    }
    Value::String(unescape_xml(raw_value))
}

fn is_integer_literal(text: &str) -> bool {
    let text = text.strip_prefix('-').unwrap_or(text);
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

fn is_number_literal(text: &str) -> bool {
    let text = text.strip_prefix('-').unwrap_or(text);
    if text.is_empty() {
        return false;
    }
    let (int_part, frac_part) = text.split_once('.').map_or((text, None), |(i, f)| (i, Some(f)));
    match frac_part {
        None => !int_part.is_empty() && int_part.bytes().all(|b| b.is_ascii_digit()),
        Some(frac) => {
            (!int_part.is_empty() || !frac.is_empty())
                && int_part.bytes().all(|b| b.is_ascii_digit())
                && !frac.is_empty()
                && frac.bytes().all(|b| b.is_ascii_digit())
        }
    }
}

struct OpeningTag {
    end_index: usize,
    name: String,
}

/// `<\s*([A-Za-z_][\w.-]*)\s*>` anchored (sticky) at `start_index`.
fn parse_opening_tag(text: &str, start_index: usize) -> Option<OpeningTag> {
    let bytes = text.as_bytes();
    let mut cursor = start_index;
    if cursor >= bytes.len() || bytes[cursor] != b'<' {
        return None;
    }
    cursor += 1;
    while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    let name_start = cursor;
    if cursor >= bytes.len() || !(bytes[cursor].is_ascii_alphabetic() || bytes[cursor] == b'_') {
        return None;
    }
    cursor += 1;
    while cursor < bytes.len() && (bytes[cursor].is_ascii_alphanumeric() || matches!(bytes[cursor], b'_' | b'.' | b'-')) {
        cursor += 1;
    }
    let name = text[name_start..cursor].to_string();
    while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    if cursor >= bytes.len() || bytes[cursor] != b'>' {
        return None;
    }
    Some(OpeningTag { end_index: cursor + 1, name })
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

fn unescape_xml(text: &str) -> String {
    text.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::types::TextContent;

    fn weather_tool() -> Tool {
        Tool {
            name: "get_weather".into(),
            description: "Get weather for a location".into(),
            parameters: json!({"type": "object", "properties": {"city": {"type": "string"}, "days": {"type": "integer"}}, "required": ["city", "days"]}),
            freeform: None,
            constrained_sampling: None,
        }
    }

    fn todo_write_tool() -> Tool {
        Tool {
            name: "todowrite".into(),
            description: "Write todos".into(),
            parameters: json!({"type": "object", "properties": {"todos": {"type": "array", "items": {"type": "object", "properties": {"content": {"type": "string"}, "status": {"type": "string"}, "priority": {"type": "string"}}, "required": ["content", "status", "priority"]}}}, "required": ["todos"]}),
            freeform: None,
            constrained_sampling: None,
        }
    }

    fn location_tool() -> Tool {
        Tool { name: "get_location".into(), description: "Get the location".into(), parameters: json!({"type": "object", "properties": {}}), freeform: None, constrained_sampling: None }
    }

    fn parsed_call(name: &str, arguments: Value) -> ParsedToolCall {
        ParsedToolCall { name: name.to_string(), arguments: arguments.as_object().expect("object arguments").clone() }
    }

    #[test]
    fn parses_multiple_xml_tool_calls_with_string_and_number_parameters() {
        let text = "Here you go\n<get_weather><city>Seoul</city><days>3</days></get_weather>\n<get_weather><city>Busan</city><days>1</days></get_weather>";
        let parsed = parse_morph_xml_generated_text(text, &[weather_tool()], None);
        assert_eq!(
            parsed,
            vec![
                parsed_call("get_weather", json!({"city": "Seoul", "days": 3})),
                parsed_call("get_weather", json!({"city": "Busan", "days": 1})),
            ]
        );
    }

    #[test]
    fn coerces_string_values_using_the_tool_schema() {
        let text = "<get_weather><city>Tokyo</city><days>42</days></get_weather>";
        let parsed = parse_morph_xml_generated_text(text, &[weather_tool()], None);
        assert_eq!(parsed, vec![parsed_call("get_weather", json!({"city": "Tokyo", "days": 42}))]);
        assert!(parsed[0].arguments.get("days").expect("days").is_number());
    }

    #[test]
    fn rejects_malformed_array_object_payloads_instead_of_coercing_empty_items_into_strings() {
        let text = "<todowrite><todos><item/></todos></todowrite>";
        let parsed = parse_morph_xml_generated_text(text, &[todo_write_tool()], None);
        assert_eq!(parsed, vec![]);
    }

    #[test]
    fn rejects_array_object_payloads_when_object_fields_are_provided_without_item_wrappers() {
        let text = "<todowrite><todos><content>Inspect code</content><status>pending</status><priority>high</priority></todos></todowrite>";
        let parsed = parse_morph_xml_generated_text(text, &[todo_write_tool()], None);
        assert_eq!(parsed, vec![]);
    }

    #[test]
    fn parses_self_closing_tool_calls_without_arguments() {
        let parsed = parse_morph_xml_generated_text("<get_location/>", &[location_tool()], None);
        assert_eq!(parsed, vec![parsed_call("get_location", json!({}))]);
    }

    #[test]
    fn parses_self_closing_tool_calls_with_surrounding_text() {
        let parsed = parse_morph_xml_generated_text("prefix <get_location /> suffix", &[location_tool()], None);
        assert_eq!(parsed, vec![parsed_call("get_location", json!({}))]);
    }

    #[test]
    fn reports_invalid_xml_tool_calls_through_on_error() {
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called_clone = called.clone();
        let options = ParserOptions {
            emit_raw_tool_call_text_on_error: false,
            on_error: Some(std::sync::Arc::new(move |_msg: &str, _meta| called_clone.store(true, std::sync::atomic::Ordering::SeqCst))),
        };
        let parsed = parse_morph_xml_generated_text("<todowrite><todos><item/></todos></todowrite>", &[todo_write_tool()], Some(&options));
        assert_eq!(parsed, vec![]);
        assert!(called.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn emits_streaming_events_while_parsing_incremental_xml_tool_call_content() {
        let mut parser = create_morph_xml_stream_parser(vec![weather_tool()], None);
        let mut all_events = parser.feed("Before <get_weather><city>Seo");
        all_events.extend(parser.feed("ul</city><days>4</days></get_weather> After"));
        all_events.extend(parser.finish());

        assert!(all_events.contains(&StreamParserEvent::Text { text: "Before ".to_string() }));
        assert!(all_events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallStart { index: 0, name, .. } if name == "get_weather")));
        assert!(all_events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallDelta { index: 0, arguments_delta } if arguments_delta == "{\"city\":\"Seo\"}")));
        assert!(all_events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallDelta { index: 0, arguments_delta } if arguments_delta == "{\"city\":\"Seoul\",\"days\":4}")));
        assert!(all_events.contains(&StreamParserEvent::Text { text: " After".to_string() }));
        assert!(all_events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallEnd { index: 0, name, arguments, .. } if name == "get_weather" && arguments == json!({"city": "Seoul", "days": 4}).as_object().expect("object"))));
    }

    #[test]
    fn suppresses_malformed_array_object_xml_by_default_when_it_cannot_satisfy_the_schema() {
        let mut parser = create_morph_xml_stream_parser(vec![todo_write_tool()], None);
        let mut all_events = parser.feed("<todowrite><todos><item/></todos></todowrite>");
        all_events.extend(parser.finish());
        assert_eq!(all_events, vec![]);
    }

    #[test]
    fn does_not_emit_partial_toolcall_progress_for_arrays_that_violate_min_items_before_the_call_is_complete() {
        let mut parser = create_morph_xml_stream_parser(vec![todo_write_tool()], None);
        let first_events = parser.feed("<todowrite><todos></todos>");
        let second_events = parser.feed("<content>x</content></todowrite>");
        assert_eq!(first_events, vec![]);
        assert_eq!(second_events, vec![]);
    }

    #[test]
    fn parses_self_closing_tool_calls_in_the_stream() {
        let mut parser = create_morph_xml_stream_parser(vec![location_tool()], None);
        let mut all_events = parser.feed("<get_location/>");
        all_events.extend(parser.finish());
        assert_eq!(all_events.len(), 2);
        assert!(matches!(&all_events[0], StreamParserEvent::ToolcallStart { index: 0, name, .. } if name == "get_location"));
        assert!(matches!(&all_events[1], StreamParserEvent::ToolcallEnd { index: 0, name, arguments, .. } if name == "get_location" && arguments.is_empty()));
    }

    #[test]
    fn parses_self_closing_tool_calls_with_surrounding_text_in_the_stream() {
        let mut parser = create_morph_xml_stream_parser(vec![location_tool()], None);
        let mut all_events = parser.feed("prefix <get_location /> suffix");
        all_events.extend(parser.finish());
        assert_eq!(all_events.len(), 4);
        assert_eq!(all_events[0], StreamParserEvent::Text { text: "prefix ".to_string() });
        assert!(matches!(&all_events[1], StreamParserEvent::ToolcallStart { index: 0, name, .. } if name == "get_location"));
        assert!(matches!(&all_events[2], StreamParserEvent::ToolcallEnd { index: 0, name, .. } if name == "get_location"));
        assert_eq!(all_events[3], StreamParserEvent::Text { text: " suffix".to_string() });
    }

    #[test]
    fn parses_self_closing_tool_calls_when_whitespace_appears_after_the_opening_bracket_across_chunks() {
        let mut parser = create_morph_xml_stream_parser(vec![location_tool()], None);
        let mut all_events = parser.feed("prefix < get_loc");
        all_events.extend(parser.feed("ation/> suffix"));
        all_events.extend(parser.finish());
        assert_eq!(all_events.len(), 4);
        assert_eq!(all_events[0], StreamParserEvent::Text { text: "prefix ".to_string() });
        assert!(matches!(&all_events[1], StreamParserEvent::ToolcallStart { index: 0, name, .. } if name == "get_location"));
        assert!(matches!(&all_events[2], StreamParserEvent::ToolcallEnd { index: 0, name, .. } if name == "get_location"));
        assert_eq!(all_events[3], StreamParserEvent::Text { text: " suffix".to_string() });
    }

    #[test]
    fn handles_mismatched_inner_xml_without_throwing() {
        let mut parser = create_morph_xml_stream_parser(vec![weather_tool()], None);
        let mut all_events = parser.feed("<get_weather><location>NY</get_weather>");
        all_events.extend(parser.finish());
        let has_tool_call = all_events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallEnd { .. }));
        let text_output: String = all_events.iter().filter_map(|e| if let StreamParserEvent::Text { text } = e { Some(text.as_str()) } else { None }).collect();
        assert!(has_tool_call || text_output.is_empty());
    }

    #[test]
    fn suppresses_malformed_xml_tool_markup_by_default_and_reports_on_error() {
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called_clone = called.clone();
        let options = ParserOptions {
            emit_raw_tool_call_text_on_error: false,
            on_error: Some(std::sync::Arc::new(move |_msg: &str, _meta| called_clone.store(true, std::sync::atomic::Ordering::SeqCst))),
        };
        let mut parser = create_morph_xml_stream_parser(vec![todo_write_tool()], Some(options));
        let mut all_events = parser.feed("prefix <todowrite><todos><item/></todos></todowrite> suffix");
        all_events.extend(parser.finish());
        assert_eq!(all_events, vec![StreamParserEvent::Text { text: "prefix ".to_string() }, StreamParserEvent::Text { text: " suffix".to_string() }]);
        assert!(called.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn emits_malformed_xml_tool_markup_when_raw_fallback_is_explicitly_enabled() {
        let options = ParserOptions { emit_raw_tool_call_text_on_error: true, on_error: None };
        let mut parser = create_morph_xml_stream_parser(vec![todo_write_tool()], Some(options));
        let mut all_events = parser.feed("prefix <todowrite><todos><item/></todos></todowrite> suffix");
        all_events.extend(parser.finish());
        assert_eq!(
            all_events,
            vec![
                StreamParserEvent::Text { text: "prefix ".to_string() },
                StreamParserEvent::Text { text: "<todowrite><todos><item/></todos></todowrite>".to_string() },
                StreamParserEvent::Text { text: " suffix".to_string() },
            ]
        );
    }

    #[test]
    fn flags_unfinished_invalid_xml_tool_calls_at_finish_without_raw_markup() {
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called_clone = called.clone();
        let options = ParserOptions {
            emit_raw_tool_call_text_on_error: false,
            on_error: Some(std::sync::Arc::new(move |_msg: &str, _meta| called_clone.store(true, std::sync::atomic::Ordering::SeqCst))),
        };
        let mut parser = create_morph_xml_stream_parser(vec![todo_write_tool()], Some(options));
        let mut all_events = parser.feed("<todowrite><todos><item/></todos>");
        all_events.extend(parser.finish());
        assert!(all_events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallEnd { name, incomplete: true, error_message: Some(m), .. } if name == "todowrite" && m == "Tool call was truncated mid-arguments")));
        assert!(!all_events.iter().any(|e| matches!(e, StreamParserEvent::Text { .. })));
        assert!(called.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn flags_unfinished_invalid_xml_tool_calls_even_when_raw_fallback_is_enabled() {
        let options = ParserOptions { emit_raw_tool_call_text_on_error: true, on_error: None };
        let mut parser = create_morph_xml_stream_parser(vec![todo_write_tool()], Some(options));
        let mut all_events = parser.feed("<todowrite><todos><item/></todos>");
        all_events.extend(parser.finish());
        assert!(all_events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallEnd { name, incomplete: true, .. } if name == "todowrite")));
        assert!(!all_events.iter().any(|e| matches!(e, StreamParserEvent::Text { .. })));
    }

    fn fixture_tools() -> Vec<Tool> {
        vec![
            Tool {
                name: "get_weather".into(),
                description: "Get weather".into(),
                parameters: json!({"type": "object", "properties": {"city": {"type": "string"}, "days": {"type": "integer"}}, "required": ["city"]}),
                freeform: None,
                constrained_sampling: None,
            },
            Tool {
                name: "todowrite".into(),
                description: "Write todos".into(),
                parameters: json!({"type": "object", "properties": {"todos": {"type": "array", "minItems": 1, "items": {"type": "object", "properties": {"content": {"type": "string"}, "status": {"type": "string"}, "priority": {"type": "string"}}, "required": ["content", "status", "priority"]}}}, "required": ["todos"]}),
                freeform: None,
                constrained_sampling: None,
            },
            Tool { name: "get_location".into(), description: "Get location".into(), parameters: json!({"type": "object", "properties": {}}), freeform: None, constrained_sampling: None },
        ]
    }

    #[test]
    fn handles_eof_truncation_fixture_r3_r4_pass_closed_child_with_only_tool_close_missing() {
        let mut parser = create_morph_xml_stream_parser(fixture_tools(), None);
        let mut all_events = parser.feed("<get_weather><city>NY</city>");
        all_events.extend(parser.finish());
        let toolcall_ends: Vec<_> = all_events.iter().filter(|e| matches!(e, StreamParserEvent::ToolcallEnd { .. })).collect();
        assert_eq!(toolcall_ends.len(), 1);
        assert!(matches!(toolcall_ends[0], StreamParserEvent::ToolcallEnd { name, arguments, incomplete: false, .. } if name == "get_weather" && arguments == json!({"city": "NY"}).as_object().expect("object")));
    }

    #[test]
    fn handles_eof_truncation_fixture_r3_fail_unclosed_child_makes_xml_argument_body_unparseable() {
        let onerror_calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let calls_clone = onerror_calls.clone();
        let options = ParserOptions {
            emit_raw_tool_call_text_on_error: false,
            on_error: Some(std::sync::Arc::new(move |msg: &str, _meta| calls_clone.lock().expect("lock").push(msg.to_string()))),
        };
        let input = "<get_weather><city>NY</city><days>";
        let mut parser = create_morph_xml_stream_parser(fixture_tools(), Some(options));
        let mut all_events = parser.feed(input);
        all_events.extend(parser.finish());
        let toolcall_ends: Vec<_> = all_events.iter().filter(|e| matches!(e, StreamParserEvent::ToolcallEnd { .. })).collect();
        assert_eq!(toolcall_ends.len(), 1);
        assert!(matches!(toolcall_ends[0], StreamParserEvent::ToolcallEnd { name, incomplete: true, error_message: Some(m), .. } if name == "get_weather" && m == "Tool call was truncated mid-arguments"));
        let text_output: String = all_events.iter().filter_map(|e| if let StreamParserEvent::Text { text } = e { Some(text.as_str()) } else { None }).collect();
        assert!(!text_output.contains(input));
        assert!(!onerror_calls.lock().expect("lock").iter().any(|m| m.contains(input)));
    }

    #[test]
    fn handles_eof_truncation_fixture_r4_fail_parseable_empty_todos_array_violates_min_items() {
        let onerror_calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let calls_clone = onerror_calls.clone();
        let options = ParserOptions {
            emit_raw_tool_call_text_on_error: false,
            on_error: Some(std::sync::Arc::new(move |msg: &str, _meta| calls_clone.lock().expect("lock").push(msg.to_string()))),
        };
        let input = "<todowrite><todos></todos>";
        let mut parser = create_morph_xml_stream_parser(fixture_tools(), Some(options));
        let mut all_events = parser.feed(input);
        all_events.extend(parser.finish());
        let toolcall_ends: Vec<_> = all_events.iter().filter(|e| matches!(e, StreamParserEvent::ToolcallEnd { .. })).collect();
        assert_eq!(toolcall_ends.len(), 1);
        assert!(matches!(toolcall_ends[0], StreamParserEvent::ToolcallEnd { name, incomplete: true, error_message: Some(m), .. } if name == "todowrite" && m == "Tool call was truncated mid-arguments"));
        let text_output: String = all_events.iter().filter_map(|e| if let StreamParserEvent::Text { text } = e { Some(text.as_str()) } else { None }).collect();
        assert!(!text_output.contains(input));
        assert!(!onerror_calls.lock().expect("lock").iter().any(|m| m.contains(input)));
    }

    #[test]
    fn handles_eof_truncation_fixture_r2_fail_unknown_xml_tag_remains_ordinary_text() {
        let mut parser = create_morph_xml_stream_parser(fixture_tools(), None);
        let input = "<unknown_tool><city>Seoul</city>";
        let mut all_events = parser.feed(input);
        all_events.extend(parser.finish());
        assert!(!all_events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallStart { .. } | StreamParserEvent::ToolcallDelta { .. } | StreamParserEvent::ToolcallEnd { .. })));
        let text_output: String = all_events.iter().filter_map(|e| if let StreamParserEvent::Text { text } = e { Some(text.as_str()) } else { None }).collect();
        assert!(text_output.contains(input));
    }

    #[test]
    fn flags_a_stale_snapshot_instead_of_executing_it() {
        let raw_fragment = "<city>Seoul</city><days>4";
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, HashMap<String, Value>)>::new()));
        let calls_clone = calls.clone();
        let options = ParserOptions {
            emit_raw_tool_call_text_on_error: false,
            on_error: Some(std::sync::Arc::new(move |msg: &str, meta: Option<&HashMap<String, Value>>| {
                calls_clone.lock().expect("lock").push((msg.to_string(), meta.cloned().unwrap_or_default()));
            })),
        };
        let mut parser = create_morph_xml_stream_parser(vec![weather_tool()], Some(options));
        let mut all_events = parser.feed(&format!("<get_weather>{raw_fragment}"));
        all_events.extend(parser.finish());

        assert!(all_events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallEnd { name, arguments, incomplete: true, error_message: Some(m), .. } if name == "get_weather" && arguments.get("city") == Some(&json!("Seoul")) && m == "Tool call was truncated mid-arguments")));
        assert!(!all_events.iter().any(|e| matches!(e, StreamParserEvent::Text { .. })));

        let recorded = calls.lock().expect("lock");
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "Could not complete streaming XML tool call at finish.");
        assert_eq!(recorded[0].1.get("protocol"), Some(&json!("morph-xml")));
        assert_eq!(recorded[0].1.get("retainedLength"), Some(&json!(raw_fragment.len())));
        assert!(!format!("{:?}", recorded[0].1).contains(raw_fragment));
    }

    #[test]
    fn force_completes_unfinished_calls_at_finish_when_the_partial_xml_is_parseable() {
        let mut parser = create_morph_xml_stream_parser(vec![weather_tool()], None);
        let mut all_events = parser.feed("<get_weather><location>NY");
        all_events.extend(parser.finish());
        let toolcall_end = all_events.iter().find(|e| matches!(e, StreamParserEvent::ToolcallEnd { .. }));
        if let Some(StreamParserEvent::ToolcallEnd { arguments, .. }) = toolcall_end {
            assert_eq!(arguments, json!({"location": "NY"}).as_object().expect("object"));
        } else {
            let text_output: String = all_events.iter().filter_map(|e| if let StreamParserEvent::Text { text } = e { Some(text.as_str()) } else { None }).collect();
            assert!(!text_output.contains("<get_weather>"));
        }
    }

    #[test]
    fn handles_consecutive_tool_calls_without_leaking_xml_tags_into_text_output() {
        let tool_a = Tool { name: "tool_a".into(), description: "A".into(), parameters: json!({"type": "object", "properties": {}}), freeform: None, constrained_sampling: None };
        let tool_b = Tool { name: "tool_b".into(), description: "B".into(), parameters: json!({"type": "object", "properties": {}}), freeform: None, constrained_sampling: None };
        let mut parser = create_morph_xml_stream_parser(vec![tool_a, tool_b], None);
        let mut all_events = parser.feed("<tool_a></tool_a><tool_b></tool_b>");
        all_events.extend(parser.finish());
        let tool_calls: Vec<_> = all_events.iter().filter(|e| matches!(e, StreamParserEvent::ToolcallEnd { .. })).collect();
        let text_output: String = all_events.iter().filter_map(|e| if let StreamParserEvent::Text { text } = e { Some(text.as_str()) } else { None }).collect();
        assert_eq!(tool_calls.len(), 2);
        assert!(!text_output.contains("<tool_a>"));
        assert!(!text_output.contains("<tool_b>"));
    }

    #[test]
    fn handles_tool_calls_separated_only_by_whitespace_without_leaking_xml_tags() {
        let tool_a = Tool { name: "tool_a".into(), description: "A".into(), parameters: json!({"type": "object", "properties": {}}), freeform: None, constrained_sampling: None };
        let tool_b = Tool { name: "tool_b".into(), description: "B".into(), parameters: json!({"type": "object", "properties": {}}), freeform: None, constrained_sampling: None };
        let mut parser = create_morph_xml_stream_parser(vec![tool_a, tool_b], None);
        let mut all_events = parser.feed("<tool_a></tool_a>\n  \n<tool_b></tool_b>");
        all_events.extend(parser.finish());
        let tool_calls: Vec<_> = all_events.iter().filter(|e| matches!(e, StreamParserEvent::ToolcallEnd { .. })).collect();
        let text_output: String = all_events.iter().filter_map(|e| if let StreamParserEvent::Text { text } = e { Some(text.as_str()) } else { None }).collect();
        assert_eq!(tool_calls.len(), 2);
        assert!(!text_output.contains("<tool_a>"));
        assert!(!text_output.contains("<tool_b>"));
    }

    #[test]
    fn accepts_whitespace_in_the_closing_tag_name_while_streaming() {
        let tool = Tool {
            name: "get_weather".into(),
            description: "Get weather".into(),
            parameters: json!({"type": "object", "properties": {"city": {"type": "string"}}}),
            freeform: None,
            constrained_sampling: None,
        };
        let mut parser = create_morph_xml_stream_parser(vec![tool], None);
        let mut all_events = parser.feed("<get_weather><city>SF</city></ get_weather>");
        all_events.extend(parser.finish());
        let toolcall_end = all_events.iter().find(|e| matches!(e, StreamParserEvent::ToolcallEnd { .. })).expect("toolcall_end");
        assert!(matches!(toolcall_end, StreamParserEvent::ToolcallEnd { name, arguments, .. } if name == "get_weather" && arguments == json!({"city": "SF"}).as_object().expect("object")));
    }

    #[test]
    fn parses_xml_tool_calls_correctly_when_streamed_character_by_character() {
        let mut parser = create_morph_xml_stream_parser(vec![weather_tool()], None);
        let input = "<get_weather><city>Seoul</city><days>3</days></get_weather>";
        let mut events = Vec::new();
        for character in input.chars() {
            events.extend(parser.feed(&character.to_string()));
        }
        events.extend(parser.finish());
        let toolcall_end = events.iter().find(|e| matches!(e, StreamParserEvent::ToolcallEnd { .. })).expect("toolcall_end");
        assert!(matches!(toolcall_end, StreamParserEvent::ToolcallEnd { name, arguments, .. } if name == "get_weather" && arguments == json!({"city": "Seoul", "days": 3}).as_object().expect("object")));
    }

    fn seeded_random(seed: u64) -> impl FnMut() -> f64 {
        let mut current = seed;
        move || {
            current = (current * 9301 + 49_297) % 233_280;
            current as f64 / 233_280.0
        }
    }

    fn random_chunk_split(text: &str, min_size: usize, max_size: usize, seed: u64) -> Vec<String> {
        let mut random = seeded_random(seed);
        let chars: Vec<char> = text.chars().collect();
        let mut chunks = Vec::new();
        let mut index = 0usize;
        while index < chars.len() {
            let size = (random() * (max_size - min_size + 1) as f64).floor() as usize + min_size;
            let end = (index + size).min(chars.len());
            chunks.push(chars[index..end].iter().collect::<String>());
            index += size;
        }
        chunks
    }

    #[test]
    fn keeps_tool_call_parsing_stable_across_random_chunk_splits() {
        for seed in [0u64, 1, 7, 13, 21] {
            let mut parser = create_morph_xml_stream_parser(vec![weather_tool()], None);
            let input = "Checking... <get_weather><city>NYC</city><days>2</days></get_weather> found!";
            let chunks = random_chunk_split(input, 1, 8, seed);
            let mut events = Vec::new();
            for chunk in &chunks {
                events.extend(parser.feed(chunk));
            }
            events.extend(parser.finish());

            let toolcall_end = events.iter().find(|e| matches!(e, StreamParserEvent::ToolcallEnd { .. })).unwrap_or_else(|| panic!("seed {seed}: expected toolcall_end"));
            assert!(matches!(toolcall_end, StreamParserEvent::ToolcallEnd { name, arguments, .. } if name == "get_weather" && arguments == json!({"city": "NYC", "days": 2}).as_object().expect("object")), "seed {seed}");
            let text_output: String = events.iter().filter_map(|e| if let StreamParserEvent::Text { text } = e { Some(text.as_str()) } else { None }).collect();
            assert!(text_output.contains("Checking..."), "seed {seed}");
            assert!(text_output.contains("found!"), "seed {seed}");
            assert!(!text_output.contains("<get_weather>"), "seed {seed}");
        }
    }

    #[test]
    fn suppresses_malformed_xml_tool_markup_from_text_output_by_default_when_parsing_fails() {
        let strict_tool = Tool {
            name: "bad_tool".into(),
            description: "Strict tool".into(),
            parameters: json!({"type": "object", "properties": {"name": {"type": "string"}}}),
            freeform: None,
            constrained_sampling: None,
        };
        let mut parser = create_morph_xml_stream_parser(vec![strict_tool], None);
        let mut all_events = parser.feed("Calling tool:\n");
        all_events.extend(parser.feed("<bad_tool><name>first</name><name>second</name></bad_tool>"));
        all_events.extend(parser.feed("\nDone!"));
        all_events.extend(parser.finish());

        let tool_calls: Vec<_> = all_events.iter().filter(|e| matches!(e, StreamParserEvent::ToolcallEnd { .. })).collect();
        let text_output: String = all_events.iter().filter_map(|e| if let StreamParserEvent::Text { text } = e { Some(text.as_str()) } else { None }).collect();
        assert_eq!(tool_calls.len(), 0);
        assert!(text_output.contains("Calling tool:"));
        assert!(text_output.contains("Done!"));
        assert!(!text_output.contains("<bad_tool>"));
        assert!(!text_output.contains("</bad_tool>"));
        assert!(!text_output.contains("<name>"));
    }

    #[test]
    fn preserves_raw_inner_xml_for_string_typed_fields() {
        let write_file_tool = Tool {
            name: "write_file".into(),
            description: "Write a file".into(),
            parameters: json!({"type": "object", "properties": {"file_path": {"type": "string"}, "content": {"type": "string"}, "encoding": {"type": "string"}}, "required": ["file_path", "content"]}),
            freeform: None,
            constrained_sampling: None,
        };
        let mut parser = create_morph_xml_stream_parser(vec![write_file_tool], None);
        let html = "<html><body><h1>Hi</h1><p>World</p></body></html>";
        let parts = ["<write_file>".to_string(), "<file_path>/home/username/myfile.html</file_path>".to_string(), "<content>".to_string(), html.to_string(), "</content>".to_string(), "<encoding>utf-8</encoding>".to_string(), "</write_file>".to_string()];
        let mut events = Vec::new();
        for part in &parts {
            let chars: Vec<char> = part.chars().collect();
            let mut index = 0usize;
            while index < chars.len() {
                let end = (index + 7).min(chars.len());
                events.extend(parser.feed(&chars[index..end].iter().collect::<String>()));
                index += 7;
            }
        }
        events.extend(parser.finish());

        let toolcall_end = events.iter().find(|e| matches!(e, StreamParserEvent::ToolcallEnd { .. })).expect("toolcall_end");
        assert!(matches!(toolcall_end, StreamParserEvent::ToolcallEnd { name, arguments, .. } if name == "write_file" && arguments == json!({"file_path": "/home/username/myfile.html", "content": html, "encoding": "utf-8"}).as_object().expect("object")));
    }

    #[test]
    fn morph_xml_format_tool_call_serializes_arguments_as_xml() {
        let mut args = Map::new();
        args.insert("city".into(), json!("Seoul"));
        let formatted = morph_xml_format_tool_call("get_weather", &args);
        assert_eq!(formatted, "<get_weather>\n   <city>Seoul</city>\n</get_weather>");
    }

    #[test]
    fn morph_xml_format_tool_response_wraps_result_in_tool_response() {
        let content = vec![ToolResultContent::Text(TextContent { text: "sunny".into(), audience: None, text_signature: None })];
        let formatted = morph_xml_format_tool_response("get_weather", "id-1", &content);
        assert!(formatted.starts_with("<tool_response>"));
        assert!(formatted.contains("<tool_name>get_weather</tool_name>"));
        assert!(formatted.contains("sunny"));
    }

    #[test]
    fn morph_xml_format_tools_system_prompt_includes_tool_name_and_schema() {
        let prompt = morph_xml_format_tools_system_prompt(&[weather_tool()]);
        assert!(prompt.contains("name: get_weather"));
        assert!(prompt.contains("<example_function_name>"));
    }
}
