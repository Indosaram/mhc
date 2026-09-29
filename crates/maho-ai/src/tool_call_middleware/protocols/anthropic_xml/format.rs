//! Port of senpi packages/ai/src/tool-call-middleware/protocols/anthropic-xml/format.ts.

use serde_json::{Map, Value};

use super::xml_entities::{encode_xml_attribute, encode_xml_parameter_text, encode_xml_text};
use crate::tool_call_middleware::types::ToolResultContent;
use crate::types::Tool;

fn serialize_tool_value(value: &Value) -> String {
    match value {
        Value::Object(_) | Value::Array(_) => serde_json::to_string(value).unwrap_or_else(|_| "null".into()),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Null => "null".into(),
    }
}

pub fn render_tool_definitions(tools: &[Tool]) -> String {
    let entries: Vec<Value> = tools
        .iter()
        .map(|tool| {
            Value::Object(Map::from_iter([
                ("name".into(), Value::String(tool.name.clone())),
                ("description".into(), Value::String(tool.description.clone())),
                ("parameters".into(), tool.parameters.clone()),
            ]))
        })
        .collect();
    let json = serde_json::to_string(&Value::Array(entries)).unwrap_or_else(|_| "[]".into());
    json.replace('<', "\\u003c").replace('>', "\\u003e").replace('&', "\\u0026")
}

pub fn anthropic_xml_format_tools_system_prompt(tools: &[Tool]) -> String {
    if tools.is_empty() {
        return String::new();
    }

    format!(
        "# Tools\n\nYou may call one or more functions to assist with the user query.\n\nYou are provided with function signatures as JSON within <tools></tools> XML tags:\n<tools>{}</tools>\n\n# Format\n\nFor each function call, emit exactly one bare <invoke> element with the tool name in its name attribute.\nPut each argument in one <parameter> element with its name in the name attribute.\nDo not wrap the invoke element in another XML element.\n\n# Example\n<invoke name=\"get_weather\"><parameter name=\"city\">Seoul</parameter></invoke>",
        render_tool_definitions(tools)
    )
}

pub fn anthropic_xml_format_tool_call(name: &str, args: &Map<String, Value>) -> String {
    let mut lines = vec![format!("<invoke name=\"{}\">", encode_xml_attribute(name))];
    for (parameter_name, value) in args {
        lines.push(format!(
            "<parameter name=\"{}\">{}</parameter>",
            encode_xml_attribute(parameter_name),
            encode_xml_parameter_text(&serialize_tool_value(value))
        ));
    }
    lines.push("</invoke>".to_string());
    lines.join("\n")
}

pub fn anthropic_xml_format_tool_response(tool_name: &str, _tool_call_id: &str, content: &[ToolResultContent]) -> String {
    let text_content = content
        .iter()
        .filter_map(|entry| match entry {
            ToolResultContent::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "<function_results>\n<result>\n<tool_name>{}</tool_name>\n<stdout>{}</stdout>\n</result>\n</function_results>",
        encode_xml_text(tool_name),
        encode_xml_text(&text_content)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TextContent;
    use serde_json::json;

    fn tool(name: &str) -> Tool {
        Tool { name: name.into(), description: "d".into(), parameters: json!({"type": "object"}), freeform: None, constrained_sampling: None }
    }

    #[test]
    fn render_tool_definitions_escapes_angle_brackets_and_ampersand() {
        let mut t = tool("t");
        t.description = "a<b>c&d".into();
        let rendered = render_tool_definitions(&[t]);
        assert!(rendered.contains("\\u003c"));
        assert!(rendered.contains("\\u003e"));
        assert!(rendered.contains("\\u0026"));
    }

    #[test]
    fn anthropic_xml_format_tools_system_prompt_empty_for_no_tools() {
        assert_eq!(anthropic_xml_format_tools_system_prompt(&[]), "");
    }

    #[test]
    fn anthropic_xml_format_tools_system_prompt_includes_tool_json() {
        let prompt = anthropic_xml_format_tools_system_prompt(&[tool("get_weather")]);
        assert!(prompt.contains("get_weather"));
        assert!(prompt.contains("<tools>"));
    }

    #[test]
    fn anthropic_xml_format_tool_call_serializes_object_and_scalar_arguments() {
        let mut args = Map::new();
        args.insert("city".into(), json!("Seoul"));
        args.insert("meta".into(), json!({"k": 1}));
        let formatted = anthropic_xml_format_tool_call("get_weather", &args);
        assert!(formatted.starts_with("<invoke name=\"get_weather\">"));
        assert!(formatted.contains("<parameter name=\"city\">Seoul</parameter>"));
        assert!(formatted.contains("<parameter name=\"meta\">{\"k\":1}</parameter>"));
        assert!(formatted.ends_with("</invoke>"));
    }

    #[test]
    fn anthropic_xml_format_tool_response_wraps_text_content_only() {
        let content = vec![ToolResultContent::Text(TextContent { text: "sunny".into(), audience: None, text_signature: None })];
        let formatted = anthropic_xml_format_tool_response("get_weather", "id-1", &content);
        assert!(formatted.contains("<tool_name>get_weather</tool_name>"));
        assert!(formatted.contains("<stdout>sunny</stdout>"));
    }
}
