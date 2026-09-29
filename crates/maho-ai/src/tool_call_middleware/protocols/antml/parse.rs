//! Port of senpi packages/ai/src/tool-call-middleware/protocols/antml/parse.ts.

use super::config::ANTML_INVOKE_CONFIG;
use crate::tool_call_middleware::protocols::anthropic_xml::parse::parse_invoke_generated_text;
use crate::tool_call_middleware::types::{ParsedToolCall, ParserOptions};
use crate::types::Tool;

pub fn parse_antml_generated_text(text: &str, tools: &[Tool], options: Option<&ParserOptions>) -> Vec<ParsedToolCall> {
    parse_invoke_generated_text(text, tools, &ANTML_INVOKE_CONFIG, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str) -> Tool {
        Tool {
            name: name.into(),
            description: "d".into(),
            parameters: json!({"type": "object", "properties": {"city": {"type": "string"}}}),
            freeform: None,
            constrained_sampling: None,
        }
    }

    #[test]
    fn parses_an_invoke_wrapped_in_function_calls() {
        let text = r#"<function_calls><invoke name="get_weather"><parameter name="city">Seoul</parameter></invoke></function_calls>"#;
        let calls = parse_antml_generated_text(text, &[tool("get_weather")], None);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_weather");
    }
}
