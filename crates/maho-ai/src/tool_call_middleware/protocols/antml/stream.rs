//! Port of senpi packages/ai/src/tool-call-middleware/protocols/antml/stream.ts.

use super::config::ANTML_INVOKE_CONFIG;
use crate::tool_call_middleware::protocols::anthropic_xml::stream::create_invoke_stream_parser;
use crate::tool_call_middleware::types::{ParserOptions, StreamParser};
use crate::types::Tool;

pub fn create_antml_stream_parser(tools: Vec<Tool>, options: Option<ParserOptions>) -> Box<dyn StreamParser + Send> {
    create_invoke_stream_parser(tools, &ANTML_INVOKE_CONFIG, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_call_middleware::types::StreamParserEvent;
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
    fn streams_an_antml_invoke_wrapped_in_function_calls() {
        let tools = vec![tool("get_weather")];
        let mut parser = create_antml_stream_parser(tools, None);
        let events =
            parser.feed(r#"<function_calls><invoke name="get_weather"><parameter name="city">Seoul</parameter></invoke></function_calls>"#);
        assert!(events.iter().any(|e| matches!(e, StreamParserEvent::ToolcallStart { name, .. } if name == "get_weather")));
        assert!(parser.finish().is_empty());
    }
}
