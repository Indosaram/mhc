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
    use serde_json::{json, Value};

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
    fn bash_tool() -> Tool {
        Tool { name: "Bash".into(), description: "Run a shell command".into(), parameters: json!({"type": "object", "required": ["command"], "properties": {"command": {"type": "string"}}}), freeform: None, constrained_sampling: None }
    }

    fn edit_tool() -> Tool {
        Tool {
            name: "Edit".into(),
            description: "Edit a file".into(),
            parameters: json!({"type": "object", "required": ["file_path", "edits"], "properties": {"file_path": {"type": "string"}, "edits": {"type": "array", "items": {"type": "object", "required": ["oldText", "newText"], "properties": {"oldText": {"type": "string"}, "newText": {"type": "string"}}}}}}),
            freeform: None,
            constrained_sampling: None,
        }
    }

    #[test]
    fn parses_a_function_calls_wrapped_invoke() {
        let text = r#"<function_calls><invoke name="Bash"><parameter name="command">echo hi</parameter></invoke></function_calls>"#;
        let calls = parse_antml_generated_text(text, &[bash_tool()], None);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Bash");
        assert_eq!(calls[0].arguments.get("command"), Some(&json!("echo hi")));
    }

    #[test]
    fn parses_multiple_invokes_inside_one_function_calls_block() {
        let text = concat!(
            "<function_calls>\n",
            "<invoke name=\"Bash\"><parameter name=\"command\">one</parameter></invoke>\n",
            "<invoke name=\"Bash\"><parameter name=\"command\">two</parameter></invoke>\n",
            "</function_calls>"
        );
        let calls = parse_antml_generated_text(text, &[bash_tool()], None);
        let commands: Vec<&str> = calls.iter().filter_map(|call| call.arguments.get("command").and_then(Value::as_str)).collect();
        assert_eq!(commands, vec!["one", "two"]);
    }

    #[test]
    fn parses_a_bare_invoke_without_the_wrapper() {
        let text = r#"<invoke name="Bash"><parameter name="command">ls</parameter></invoke>"#;
        let calls = parse_antml_generated_text(text, &[bash_tool()], None);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments.get("command"), Some(&json!("ls")));
    }

    #[test]
    fn repairs_the_pi_edits_regression_instead_of_rejecting_the_call() {
        let text = concat!(
            "<invoke name=\"Edit\">",
            "<parameter name=\"file_path\">some/file.py</parameter>",
            "<parameter name=\"edits\">[{\"oldText\":\"a\",\"newText\":\"b\",\"requireUnique\":true}]</parameter>",
            "<parameter name=\"notes\">extra</parameter>",
            "</invoke>"
        );
        let calls = parse_antml_generated_text(text, &[edit_tool()], None);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments.get("file_path"), Some(&json!("some/file.py")));
        assert_eq!(calls[0].arguments.get("edits"), Some(&json!([{"oldText": "a", "newText": "b"}])));
    }

    #[test]
    fn keeps_unknown_tool_invokes_out_of_the_parse_result() {
        let text = r#"<invoke name="Missing"><parameter name="command">x</parameter></invoke>"#;
        assert!(parse_antml_generated_text(text, &[bash_tool()], None).is_empty());
    }

    #[test]
    fn reports_unrecoverably_malformed_calls_through_on_error() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let options = ParserOptions {
            emit_raw_tool_call_text_on_error: false,
            on_error: Some(std::sync::Arc::new(move |message: &str, _metadata| {
                sink.lock().expect("error sink").push(message.to_string());
            })),
        };
        let text = r#"<invoke name="Bash"><parameter name="command"></parameter></invoke>"#;
        let strict_tool = Tool { name: "Bash".into(), description: "Run a shell command".into(), parameters: json!({"type": "object", "required": ["command"], "properties": {"command": {"type": "string", "minLength": 1}}}), freeform: None, constrained_sampling: None };
        assert!(parse_antml_generated_text(text, &[strict_tool], Some(&options)).is_empty());
        assert_eq!(seen.lock().expect("errors").as_slice(), ["Could not process antml tool call, keeping original text."]);
    }

}
