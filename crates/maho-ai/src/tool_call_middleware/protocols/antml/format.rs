//! Port of senpi packages/ai/src/tool-call-middleware/protocols/antml/format.ts.

use crate::tool_call_middleware::protocols::anthropic_xml::format::{
    anthropic_xml_format_tool_call, anthropic_xml_format_tool_response, render_tool_definitions,
};
use crate::tool_call_middleware::types::ToolResultContent;
use crate::types::Tool;

pub fn antml_format_tools_system_prompt(tools: &[Tool]) -> String {
    if tools.is_empty() {
        return String::new();
    }

    format!(
        "# Tools\n\nYou may call one or more functions to assist with the user query.\n\nYou are provided with function signatures as JSON within <tools></tools> XML tags:\n<tools>{}</tools>\n\n# Format\n\nEmit every tool call inside one <function_calls> block.\nFor each function call, emit one <invoke> element with the tool name in its name attribute.\nPut each argument in one <parameter> element with its name in the name attribute.\nString and scalar parameters are written as-is; arrays and objects are written as JSON.\n\n# Example\n<function_calls>\n<invoke name=\"get_weather\">\n<parameter name=\"city\">Seoul</parameter>\n</invoke>\n</function_calls>",
        render_tool_definitions(tools)
    )
}

pub fn antml_format_tool_call(name: &str, args: &serde_json::Map<String, serde_json::Value>) -> String {
    ["<function_calls>".to_string(), anthropic_xml_format_tool_call(name, args), "</function_calls>".to_string()].join("\n")
}

pub fn antml_format_tool_response(tool_name: &str, tool_call_id: &str, content: &[ToolResultContent]) -> String {
    anthropic_xml_format_tool_response(tool_name, tool_call_id, content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Map, Value};

    fn tool(name: &str) -> Tool {
        Tool { name: name.into(), description: "d".into(), parameters: json!({"type": "object"}), freeform: None, constrained_sampling: None }
    }

    #[test]
    fn empty_for_no_tools() {
        assert_eq!(antml_format_tools_system_prompt(&[]), "");
    }

    #[test]
    fn includes_tool_json_and_function_calls_example() {
        let prompt = antml_format_tools_system_prompt(&[tool("get_weather")]);
        assert!(prompt.contains("get_weather"));
        assert!(prompt.contains("<function_calls>"));
    }

    #[test]
    fn format_tool_call_wraps_invoke_in_function_calls() {
        let mut args = Map::new();
        args.insert("city".into(), json!("Seoul"));
        let formatted = antml_format_tool_call("get_weather", &args);
        assert!(formatted.starts_with("<function_calls>\n<invoke"));
        assert!(formatted.ends_with("</invoke>\n</function_calls>"));
    }

    #[test]
    fn format_tool_response_delegates_to_anthropic_xml() {
        use crate::types::TextContent;
        let content = vec![ToolResultContent::Text(TextContent { text: "sunny".into(), audience: None, text_signature: None })];
        let formatted = antml_format_tool_response("get_weather", "id-1", &content);
        assert!(formatted.contains("sunny"));
    }
    #[test]
    fn renders_json_tool_definitions_and_a_function_calls_wrapped_example() {
        let tool = Tool {
            name: "get_weather".into(),
            description: "Get weather for a city".into(),
            parameters: json!({"type": "object", "required": ["city"], "properties": {"city": {"type": "string"}, "unit": {"type": "string"}}}),
            freeform: None,
            constrained_sampling: None,
        };
        let prompt = antml_format_tools_system_prompt(std::slice::from_ref(&tool));
        let start = prompt.rfind("<tools>").expect("tools open") + "<tools>".len();
        let end = prompt.rfind("</tools>").expect("tools close");
        let parsed: Value = serde_json::from_str(&prompt[start..end]).expect("tool json");
        assert_eq!(parsed, json!([{"name": tool.name, "description": tool.description, "parameters": tool.parameters}]));
        assert!(prompt.contains("<function_calls>"));
        assert!(prompt.contains(r#"<invoke name="get_weather">"#));
        assert!(prompt.contains("</function_calls>"));
    }

    #[test]
    fn returns_an_empty_prompt_without_tools() {
        assert_eq!(antml_format_tools_system_prompt(&[]), "");
    }

    #[test]
    fn wraps_the_canonical_invoke_serialization_in_a_function_calls_block() {
        let mut args = Map::new();
        args.insert("city".into(), json!("Seoul"));
        args.insert("tags".into(), json!(["today", "local"]));
        assert_eq!(
            antml_format_tool_call("get_weather", &args),
            "<function_calls>\n\
             <invoke name=\"get_weather\">\n\
             <parameter name=\"city\">Seoul</parameter>\n\
             <parameter name=\"tags\">[\"today\",\"local\"]</parameter>\n\
             </invoke>\n\
             </function_calls>"
        );
    }

    #[test]
    fn formats_tool_output_as_anthropic_style_function_results() {
        let content = vec![ToolResultContent::Text(crate::types::TextContent { text: "sunny".into(), audience: None, text_signature: None })];
        assert_eq!(
            antml_format_tool_response("get_weather", "call-1", &content),
            "<function_results>\n\
             <result>\n\
             <tool_name>get_weather</tool_name>\n\
             <stdout>sunny</stdout>\n\
             </result>\n\
             </function_results>"
        );
    }

}
