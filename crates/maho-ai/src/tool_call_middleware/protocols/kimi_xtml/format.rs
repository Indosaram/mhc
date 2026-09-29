//! Port of senpi packages/ai/src/tool-call-middleware/protocols/kimi-xtml/format.ts.

use serde_json::{Map, Value};

use super::markers::{XTML_ARGUMENT_CLOSE, XTML_ARGUMENT_OPEN, XTML_CALL_CLOSE, XTML_CALL_OPEN, XTML_SEP, XTML_TOOLS_CLOSE, XTML_TOOLS_OPEN};
use crate::tool_call_middleware::types::ToolResultContent;
use crate::types::Tool;

fn infer_xtml_type(value: &Value) -> &'static str {
    match value {
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        Value::String(_) | Value::Null => "string",
    }
}

fn serialize_xtml_value(value: &Value, value_type: &str) -> String {
    if value_type == "object" || value_type == "array" {
        return value.to_string();
    }
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

pub fn kimi_xtml_format_tool_call(name: &str, args: &Map<String, Value>) -> String {
    let rendered_args: String = args
        .iter()
        .map(|(key, value)| {
            let value_type = infer_xtml_type(value);
            format!("{XTML_ARGUMENT_OPEN}key=\"{key}\" type=\"{value_type}\"{XTML_SEP}{}{XTML_ARGUMENT_CLOSE}", serialize_xtml_value(value, value_type))
        })
        .collect();
    format!("{XTML_TOOLS_OPEN}{XTML_CALL_OPEN}tool=\"{name}\" index=\"1\"{XTML_SEP}{rendered_args}{XTML_CALL_CLOSE}{XTML_TOOLS_CLOSE}")
}

pub fn kimi_xtml_format_tool_response(tool_name: &str, _tool_call_id: &str, content: &[ToolResultContent]) -> String {
    let text = content.iter().filter_map(ToolResultContent::as_text).map(|item| item.text.as_str()).collect::<Vec<_>>().join("\n");
    format!("## Return of {tool_name}\n{text}")
}

pub fn kimi_xtml_format_tools_system_prompt(tools: &[Tool]) -> String {
    if tools.is_empty() {
        return String::new();
    }

    let tool_descriptions = tools
        .iter()
        .map(|tool| format!("Name: {}\nDescription: {}\nParameters Schema:\n{}", tool.name, tool.description, json_stringify_indent_3(&tool.parameters)))
        .collect::<Vec<_>>()
        .join("\n\n");

    format!(
        "You have access to the following tools. Use them when appropriate.\n\n{tool_descriptions}\n\nWhen you need to use a tool, format your response exactly like this, with no other text inside the tools block:\n\n{XTML_TOOLS_OPEN}{XTML_CALL_OPEN}tool=\"tool_name\" index=\"1\"{XTML_SEP}{XTML_ARGUMENT_OPEN}key=\"param\" type=\"string\"{XTML_SEP}value{XTML_ARGUMENT_CLOSE}{XTML_CALL_CLOSE}{XTML_TOOLS_CLOSE}\n\nFormatting rules:\n- Emit one {XTML_CALL_OPEN}...{XTML_CALL_CLOSE} section per tool call inside a single tools block.\n- Argument types: string (default when type is omitted), number, boolean, object, array.\n- object and array values must be strict JSON; string values are raw text and need no quoting.\n- Example: {XTML_TOOLS_OPEN}{XTML_CALL_OPEN}tool=\"get_weather\" index=\"1\"{XTML_SEP}{XTML_ARGUMENT_OPEN}key=\"city\" type=\"string\"{XTML_SEP}Seoul{XTML_ARGUMENT_CLOSE}{XTML_CALL_CLOSE}{XTML_TOOLS_CLOSE}"
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::types::TextContent;

    fn tool(name: &str) -> Tool {
        Tool { name: name.into(), description: "d".into(), parameters: json!({"type": "object"}), freeform: None, constrained_sampling: None }
    }

    #[test]
    fn empty_for_no_tools() {
        assert_eq!(kimi_xtml_format_tools_system_prompt(&[]), "");
    }

    #[test]
    fn includes_tool_name_and_example_block() {
        let prompt = kimi_xtml_format_tools_system_prompt(&[tool("get_weather")]);
        assert!(prompt.contains("Name: get_weather"));
        assert!(prompt.contains(XTML_TOOLS_OPEN));
    }

    #[test]
    fn format_tool_call_wraps_arguments_with_inferred_types() {
        let mut args = Map::new();
        args.insert("city".into(), json!("Seoul"));
        args.insert("days".into(), json!(3));
        let formatted = kimi_xtml_format_tool_call("get_weather", &args);
        assert!(formatted.starts_with(XTML_TOOLS_OPEN));
        assert!(formatted.contains("key=\"city\" type=\"string\""));
        assert!(formatted.contains("key=\"days\" type=\"number\""));
        assert!(formatted.ends_with(XTML_TOOLS_CLOSE));
    }

    #[test]
    fn format_tool_response_prefixes_return_header() {
        let content = vec![ToolResultContent::Text(TextContent { text: "sunny".into(), audience: None, text_signature: None })];
        let formatted = kimi_xtml_format_tool_response("get_weather", "id-1", &content);
        assert_eq!(formatted, "## Return of get_weather\nsunny");
    }
}
