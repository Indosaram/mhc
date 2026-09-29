//! Port of senpi packages/ai/src/tool-call-middleware/protocols/hermes.ts.

use serde_json::{Map, Value};

use super::json_mix::{create_json_mix_stream_parser, format_json_mix_tool_call, parse_json_mix_generated_text, JsonMixOptions};
use crate::tool_call_middleware::types::{ParsedToolCall, ParserOptions, StreamParser, ToolResultContent};
use crate::types::Tool;

const TOOL_CALL_START: &str = "<tool_call>";
const TOOL_CALL_END: &str = "</tool_call>";

fn render_tool_definition(tool: &Tool) -> String {
    format!(
        "{{\"type\": \"function\", \"function\": {{\"name\": {}, \"description\": {}, \"parameters\": {}}}}}",
        Value::String(tool.name.clone()),
        Value::String(tool.description.clone()),
        tool.parameters
    )
}

pub fn hermes_format_tools_system_prompt(tools: &[Tool]) -> String {
    if tools.is_empty() {
        return String::new();
    }

    let tools_rendered = tools.iter().map(render_tool_definition).collect::<Vec<_>>().join("\n");

    format!(
        "You are a function calling AI model. You are provided with function signatures within <tools></tools> XML tags. You may call one or more functions to assist with the user query. Don't make assumptions about what values to plug into functions. Here are the available tools: <tools> {tools_rendered} </tools>\nUse the following pydantic model json schema for each tool call you will make: {{\"properties\": {{\"name\": {{\"title\": \"Name\", \"type\": \"string\"}}, \"arguments\": {{\"title\": \"Arguments\", \"type\": \"object\"}}}}, \"required\": [\"name\", \"arguments\"], \"title\": \"FunctionCall\", \"type\": \"object\"}}\nFor each function call return a json object with function name and arguments within <tool_call></tool_call> XML tags as follows:\n<tool_call>\n{{\"name\": \"<function-name>\", \"arguments\": <args-dict>}}\n</tool_call>"
    )
}

pub fn hermes_format_tool_response(tool_name: &str, _tool_call_id: &str, content: &[ToolResultContent]) -> String {
    let text_content = content.iter().filter_map(ToolResultContent::as_text).map(|c| c.text.as_str()).collect::<Vec<_>>().join("\n");
    let mut wrapper = Map::new();
    wrapper.insert("name".to_string(), Value::String(tool_name.to_string()));
    wrapper.insert("content".to_string(), Value::String(text_content));
    format!("<tool_response>{}</tool_response>", Value::Object(wrapper))
}

pub fn hermes_format_tool_call(name: &str, args: &Map<String, Value>) -> String {
    format_json_mix_tool_call(name, args, TOOL_CALL_START, TOOL_CALL_END)
}

pub fn hermes_parse_generated_text(text: &str, tools: &[Tool], options: Option<&ParserOptions>) -> Vec<ParsedToolCall> {
    parse_json_mix_generated_text(text, tools, TOOL_CALL_START, TOOL_CALL_END, options)
}

fn hermes_tool_call_id(index: usize) -> String {
    format!("hermes-tool-{index}")
}

pub fn hermes_create_stream_parser(tools: Vec<Tool>, options: Option<ParserOptions>) -> Box<dyn StreamParser + Send> {
    create_json_mix_stream_parser(tools, JsonMixOptions { tool_call_start: TOOL_CALL_START, tool_call_end: TOOL_CALL_END, create_tool_call_id: hermes_tool_call_id }, options)
}
