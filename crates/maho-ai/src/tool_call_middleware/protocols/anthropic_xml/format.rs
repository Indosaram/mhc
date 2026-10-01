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
    use crate::types::{ImageContent, TextContent};
    use serde_json::{json, Value};

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
    #[test]
    fn renders_json_tool_definitions_and_a_bare_invoke_example() {
        let tool = Tool {
            name: "get_weather".into(),
            description: "Get weather for a city".into(),
            parameters: json!({"type": "object", "required": ["city"], "properties": {"city": {"type": "string"}, "unit": {"type": "string"}}}),
            freeform: None,
            constrained_sampling: None,
        };
        let prompt = anthropic_xml_format_tools_system_prompt(std::slice::from_ref(&tool));
        let start = prompt.rfind("<tools>").expect("tools open") + "<tools>".len();
        let end = prompt.rfind("</tools>").expect("tools close");
        let parsed: Value = serde_json::from_str(&prompt[start..end]).expect("tool json");
        assert_eq!(
            parsed,
            json!([{"name": tool.name, "description": tool.description, "parameters": tool.parameters}])
        );
        assert!(prompt.contains(r#"<invoke name="get_weather"><parameter name="city">Seoul</parameter></invoke>"#));
        assert!(prompt.contains("exactly one"));
        assert!(!prompt.contains("<function_calls>"));
    }

    #[test]
    fn formats_scalar_values_verbatim_and_nested_values_as_compact_json() {
        let mut args = Map::new();
        args.insert("city".into(), json!("Seoul"));
        args.insert("includeForecast".into(), json!(true));
        args.insert("days".into(), json!(2));
        args.insert("filters".into(), json!({"temperature": "mild"}));
        args.insert("tags".into(), json!(["today", "local"]));
        let formatted = anthropic_xml_format_tool_call("get_weather", &args);
        assert_eq!(
            formatted,
            "<invoke name=\"get_weather\">\n\
             <parameter name=\"city\">Seoul</parameter>\n\
             <parameter name=\"includeForecast\">true</parameter>\n\
             <parameter name=\"days\">2</parameter>\n\
             <parameter name=\"filters\">{\"temperature\":\"mild\"}</parameter>\n\
             <parameter name=\"tags\">[\"today\",\"local\"]</parameter>\n\
             </invoke>"
        );
        assert!(!formatted.contains("<function_calls>"));
    }

    #[test]
    fn escapes_xml_sensitive_tool_parameter_and_scalar_values() {
        let mut args = Map::new();
        args.insert("query<&\"".into(), json!("<unsafe> & \"value\""));
        let formatted = anthropic_xml_format_tool_call("search<&\"", &args);
        assert_eq!(
            formatted,
            "<invoke name=\"search&lt;&amp;&quot;\">\n\
             <parameter name=\"query&lt;&amp;&quot;\">&lt;unsafe&gt; &amp; \"value\"</parameter>\n\
             </invoke>"
        );
    }

    #[test]
    fn extracts_text_content_into_anthropic_style_function_results() {
        let content = vec![
            ToolResultContent::Text(TextContent { text: "first line".into(), audience: None, text_signature: None }),
            ToolResultContent::Image(ImageContent { data: "ignored".into(), mime_type: "image/png".into() }),
            ToolResultContent::Text(TextContent { text: "second line".into(), audience: None, text_signature: None }),
        ];
        let formatted = anthropic_xml_format_tool_response("run_command", "call-1", &content);
        assert_eq!(
            formatted,
            "<function_results>\n\
             <result>\n\
             <tool_name>run_command</tool_name>\n\
             <stdout>first line\nsecond line</stdout>\n\
             </result>\n\
             </function_results>"
        );
    }

    #[test]
    fn escapes_xml_sensitive_tool_names_and_stdout_content() {
        let content = vec![ToolResultContent::Text(TextContent {
            text: "line </stdout> & <result> \"output\"".into(),
            audience: None,
            text_signature: None,
        })];
        let formatted = anthropic_xml_format_tool_response("tool</tool_name>&\"", "call-2", &content);
        assert_eq!(
            formatted,
            "<function_results>\n\
             <result>\n\
             <tool_name>tool&lt;/tool_name&gt;&amp;\"</tool_name>\n\
             <stdout>line &lt;/stdout&gt; &amp; &lt;result&gt; \"output\"</stdout>\n\
             </result>\n\
             </function_results>"
        );
    }

}
