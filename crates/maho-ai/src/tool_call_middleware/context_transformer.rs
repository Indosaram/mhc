//! Port of senpi packages/ai/src/tool-call-middleware/context-transformer.ts.

use crate::types::{AssistantMessage, ContentBlock, Context, Message, ToolResultMessage, UserContent, UserMessage};

use super::protocols::anthropic_xml::{
    anthropic_xml_format_tool_call, anthropic_xml_format_tool_response, anthropic_xml_format_tools_system_prompt,
    create_anthropic_xml_stream_parser, parse_anthropic_xml_generated_text,
};
use super::protocols::antml::{
    antml_format_tool_call, antml_format_tool_response, antml_format_tools_system_prompt, create_antml_stream_parser,
    parse_antml_generated_text,
};
use super::protocols::gemma4::{
    gemma4_create_stream_parser, gemma4_format_tool_call, gemma4_format_tool_response, gemma4_format_tools_system_prompt,
    gemma4_parse_generated_text,
};
use super::protocols::hermes::{
    hermes_create_stream_parser, hermes_format_tool_call, hermes_format_tool_response, hermes_format_tools_system_prompt,
    hermes_parse_generated_text,
};
use super::protocols::kimi_xtml::{
    create_kimi_xtml_stream_parser, kimi_xtml_format_tool_call, kimi_xtml_format_tool_response,
    kimi_xtml_format_tools_system_prompt, parse_kimi_xtml_generated_text,
};
use super::protocols::morph_xml::{
    create_morph_xml_stream_parser, morph_xml_format_tool_call, morph_xml_format_tool_response,
    morph_xml_format_tools_system_prompt, parse_morph_xml_generated_text,
};
use super::protocols::yaml_xml::{
    create_yaml_xml_stream_parser, parse_yaml_xml_generated_text, yaml_xml_format_tool_call, yaml_xml_format_tool_response,
    yaml_xml_format_tools_system_prompt,
};
use super::types::{ParserOptions, ParsedToolCall, StreamParser, ToolCallFormat, ToolCallProtocol, ToolResultContent};
use crate::types::Tool;

macro_rules! protocol_impl {
    ($name:ident, $format_tools:path, $format_response:path, $format_call:path, $parse:path, $stream:path) => {
        struct $name;
        impl ToolCallProtocol for $name {
            fn format_tools_system_prompt(&self, tools: &[Tool]) -> String {
                $format_tools(tools)
            }
            fn format_tool_response(&self, tool_name: &str, tool_call_id: &str, content: &[ToolResultContent]) -> String {
                $format_response(tool_name, tool_call_id, content)
            }
            fn format_tool_call(&self, name: &str, args: &serde_json::Map<String, serde_json::Value>) -> String {
                $format_call(name, args)
            }
            fn parse_generated_text(
                &self,
                text: &str,
                tools: &[Tool],
                options: Option<&ParserOptions>,
            ) -> Vec<ParsedToolCall> {
                $parse(text, tools, options)
            }
            fn create_stream_parser(&self, tools: Vec<Tool>, options: Option<ParserOptions>) -> Box<dyn StreamParser + Send> {
                $stream(tools, options)
            }
        }
    };
}

protocol_impl!(
    HermesProtocol,
    hermes_format_tools_system_prompt,
    hermes_format_tool_response,
    hermes_format_tool_call,
    hermes_parse_generated_text,
    hermes_create_stream_parser
);
protocol_impl!(
    MorphXmlProtocol,
    morph_xml_format_tools_system_prompt,
    morph_xml_format_tool_response,
    morph_xml_format_tool_call,
    parse_morph_xml_generated_text,
    create_morph_xml_stream_parser
);
protocol_impl!(
    YamlXmlProtocol,
    yaml_xml_format_tools_system_prompt,
    yaml_xml_format_tool_response,
    yaml_xml_format_tool_call,
    parse_yaml_xml_generated_text,
    create_yaml_xml_stream_parser
);
protocol_impl!(
    Gemma4Protocol,
    gemma4_format_tools_system_prompt,
    gemma4_format_tool_response,
    gemma4_format_tool_call,
    gemma4_parse_generated_text,
    gemma4_create_stream_parser
);
protocol_impl!(
    AntmlProtocol,
    antml_format_tools_system_prompt,
    antml_format_tool_response,
    antml_format_tool_call,
    parse_antml_generated_text,
    create_antml_stream_parser
);
protocol_impl!(
    KimiXtmlProtocol,
    kimi_xtml_format_tools_system_prompt,
    kimi_xtml_format_tool_response,
    kimi_xtml_format_tool_call,
    parse_kimi_xtml_generated_text,
    create_kimi_xtml_stream_parser
);
protocol_impl!(
    AnthropicXmlProtocol,
    anthropic_xml_format_tools_system_prompt,
    anthropic_xml_format_tool_response,
    anthropic_xml_format_tool_call,
    parse_anthropic_xml_generated_text,
    create_anthropic_xml_stream_parser
);

/// Gets the protocol implementation for a given tool call format.
pub fn get_protocol(format: ToolCallFormat) -> &'static dyn ToolCallProtocol {
    match format {
        ToolCallFormat::AnthropicXml => &AnthropicXmlProtocol,
        ToolCallFormat::Antml => &AntmlProtocol,
        ToolCallFormat::Hermes => &HermesProtocol,
        ToolCallFormat::Xml | ToolCallFormat::MorphXml => &MorphXmlProtocol,
        ToolCallFormat::YamlXml => &YamlXmlProtocol,
        ToolCallFormat::Gemma4Delimiter => &Gemma4Protocol,
        ToolCallFormat::KimiXtml => &KimiXtmlProtocol,
    }
}

/// Transforms a context for text-based tool calling.
/// - Strips tools from context (provider sees tool-free request)
/// - Injects tool definitions into system prompt
/// - Converts tool call messages in history to text format
/// - Converts tool result messages to user messages with text content
pub fn transform_context(context: &Context, protocol: &dyn ToolCallProtocol) -> Context {
    let mut transformed = Context {
        system_prompt: context.system_prompt.clone(),
        messages: context.messages.iter().map(|message| transform_message(message, protocol)).collect(),
        tools: None,
    };

    if let Some(tools) = &context.tools
        && !tools.is_empty()
    {
        let tool_prompt = protocol.format_tools_system_prompt(tools);
        if !tool_prompt.is_empty() {
            transformed.system_prompt = Some(match &context.system_prompt {
                Some(existing) => format!("{tool_prompt}\n\n{existing}"),
                None => tool_prompt,
            });
        }
    }

    transformed
}

fn transform_message(message: &Message, protocol: &dyn ToolCallProtocol) -> Message {
    match message {
        Message::Assistant(assistant) => Message::Assistant(Box::new(transform_assistant_message(assistant, protocol))),
        Message::ToolResult(tool_result) => Message::User(transform_tool_result_message(tool_result, protocol)),
        other => other.clone(),
    }
}

fn transform_assistant_message(message: &AssistantMessage, protocol: &dyn ToolCallProtocol) -> AssistantMessage {
    let has_tool_calls = message.content.iter().any(|block| matches!(block, ContentBlock::ToolCall(_)));
    if !has_tool_calls {
        return message.clone();
    }

    let new_content: Vec<ContentBlock> = message
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text(_) | ContentBlock::Thinking(_) => block.clone(),
            ContentBlock::ToolCall(tool_call) => {
                let text = protocol.format_tool_call(&tool_call.name, &tool_call.arguments);
                ContentBlock::text(text)
            }
            ContentBlock::Image(_) | ContentBlock::ProviderNative(_) => block.clone(),
        })
        .collect();

    AssistantMessage { content: new_content, ..message.clone() }
}

fn tool_result_contents(message: &ToolResultMessage) -> Vec<ToolResultContent> {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(ToolResultContent::Text(text.clone())),
            ContentBlock::Image(image) => Some(ToolResultContent::Image(image.clone())),
            _ => None,
        })
        .collect()
}

fn transform_tool_result_message(message: &ToolResultMessage, protocol: &dyn ToolCallProtocol) -> UserMessage {
    let content = tool_result_contents(message);
    let formatted_response = protocol.format_tool_response(&message.tool_name, &message.tool_call_id, &content);
    UserMessage { content: UserContent::Text(formatted_response), timestamp: message.timestamp }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{StopReason, ToolCall, Usage};
    use serde_json::json;
    use crate::tool_call_middleware::types::{ParsedToolCall, ParserOptions, StreamParser, ToolCallProtocol, ToolResultContent};

    fn assistant_with(content: Vec<ContentBlock>) -> AssistantMessage {
        AssistantMessage {
            content,
            api: "openai-completions".into(),
            provider: "openai".into(),
            model: "m".into(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::ToolUse,
            stop_details: None,
            deferred: None,
            error_message: None,
            abort_source: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 0,
        }
    }

    fn tool(name: &str) -> Tool {
        Tool { name: name.into(), description: "d".into(), parameters: json!({"type": "object"}), freeform: None, constrained_sampling: None }
    }

    #[test]
    fn get_protocol_resolves_every_format_and_xml_alias_shares_morph_xml() {
        for format in [
            ToolCallFormat::Hermes,
            ToolCallFormat::MorphXml,
            ToolCallFormat::YamlXml,
            ToolCallFormat::Gemma4Delimiter,
            ToolCallFormat::AnthropicXml,
            ToolCallFormat::Antml,
            ToolCallFormat::KimiXtml,
        ] {
            let _ = get_protocol(format);
        }
        let xml_prompt = get_protocol(ToolCallFormat::Xml).format_tools_system_prompt(&[tool("t")]);
        let morph_prompt = get_protocol(ToolCallFormat::MorphXml).format_tools_system_prompt(&[tool("t")]);
        assert_eq!(xml_prompt, morph_prompt);
    }

    #[test]
    fn transform_context_strips_tools_and_injects_system_prompt() {
        let context = Context {
            system_prompt: Some("base".into()),
            messages: Vec::new(),
            tools: Some(vec![tool("get_weather")]),
        };
        let transformed = transform_context(&context, get_protocol(ToolCallFormat::Antml));
        assert!(transformed.tools.is_none());
        let prompt = transformed.system_prompt.expect("system prompt");
        assert!(prompt.contains("get_weather"));
        assert!(prompt.ends_with("base"));
    }

    #[test]
    fn transform_context_without_tools_leaves_system_prompt_untouched() {
        let context = Context { system_prompt: Some("base".into()), messages: Vec::new(), tools: None };
        let transformed = transform_context(&context, get_protocol(ToolCallFormat::Antml));
        assert_eq!(transformed.system_prompt, Some("base".into()));
    }

    #[test]
    fn transform_assistant_message_converts_tool_calls_to_text_and_passes_through_text_and_thinking() {
        let message = assistant_with(vec![
            ContentBlock::text("hello"),
            ContentBlock::ToolCall(ToolCall {
                id: "id-1".into(),
                name: "get_weather".into(),
                arguments: json!({"city": "Seoul"}).as_object().unwrap().clone(),
                ..ToolCall::default()
            }),
        ]);
        let transformed = transform_assistant_message(&message, get_protocol(ToolCallFormat::Antml));
        assert_eq!(transformed.content.len(), 2);
        assert!(matches!(&transformed.content[0], ContentBlock::Text(t) if t.text == "hello"));
        match &transformed.content[1] {
            ContentBlock::Text(t) => assert!(t.text.contains("get_weather")),
            other => panic!("expected text block, got {other:?}"),
        }
    }

    #[test]
    fn transform_assistant_message_without_tool_calls_passes_through_unchanged() {
        let message = assistant_with(vec![ContentBlock::text("hello")]);
        let transformed = transform_assistant_message(&message, get_protocol(ToolCallFormat::Antml));
        assert_eq!(transformed, message);
    }

    #[test]
    fn transform_tool_result_message_becomes_user_text_message() {
        let tool_result = ToolResultMessage {
            tool_call_id: "id-1".into(),
            tool_name: "get_weather".into(),
            content: vec![ContentBlock::text("sunny")],
            details: None,
            usage: None,
            added_tool_names: None,
            is_error: false,
            timestamp: 42,
        };
        let user = transform_tool_result_message(&tool_result, get_protocol(ToolCallFormat::Antml));
        assert_eq!(user.timestamp, 42);
        match user.content {
            UserContent::Text(text) => {
                assert!(text.contains("sunny"));
                assert!(text.contains("get_weather"));
            }
            other => panic!("expected text content, got {other:?}"),
        }
    }

    #[test]
    fn transform_message_passes_through_user_and_configuration_update() {
        let user = Message::User(UserMessage { content: UserContent::Text("hi".into()), timestamp: 1 });
        assert_eq!(transform_message(&user, get_protocol(ToolCallFormat::Antml)), user);
    }
    struct MockProtocol;

    impl ToolCallProtocol for MockProtocol {
        fn format_tools_system_prompt(&self, tools: &[Tool]) -> String {
            if tools.is_empty() {
                String::new()
            } else {
                format!("<tools>{}</tools>", tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", "))
            }
        }

        fn format_tool_response(&self, tool_name: &str, _tool_call_id: &str, content: &[ToolResultContent]) -> String {
            let text: String = content
                .iter()
                .filter_map(|block| match block {
                    ToolResultContent::Text(text) => Some(text.text.as_str()),
                    ToolResultContent::Image(_) => Some(""),
                })
                .collect();
            format!("<tool_response>{tool_name}:{text}</tool_response>")
        }

        fn format_tool_call(&self, name: &str, args: &serde_json::Map<String, serde_json::Value>) -> String {
            format!("<tool_call>{name}:{}</tool_call>", serde_json::Value::Object(args.clone()))
        }

        fn parse_generated_text(&self, _text: &str, _tools: &[Tool], _options: Option<&ParserOptions>) -> Vec<ParsedToolCall> {
            Vec::new()
        }

        fn create_stream_parser(&self, _tools: Vec<Tool>, _options: Option<ParserOptions>) -> Box<dyn StreamParser + Send> {
            Box::new(EmptyStreamParser)
        }
    }

    struct EmptyStreamParser;

    impl StreamParser for EmptyStreamParser {
        fn feed(&mut self, _text_delta: &str) -> Vec<crate::tool_call_middleware::types::StreamParserEvent> {
            Vec::new()
        }

        fn finish(&mut self) -> Vec<crate::tool_call_middleware::types::StreamParserEvent> {
            Vec::new()
        }
    }

    fn weather_tool() -> Tool {
        Tool {
            name: "get_weather".into(),
            description: "Get weather for a location".into(),
            parameters: json!({"type": "object", "required": ["city"], "properties": {"city": {"type": "string", "description": "City name"}}}),
            freeform: None,
            constrained_sampling: None,
        }
    }

    fn user_message(content: &str) -> Message {
        Message::User(UserMessage { content: UserContent::Text(content.into()), timestamp: 0 })
    }

    fn plain_assistant(content: Vec<ContentBlock>) -> AssistantMessage {
        AssistantMessage { api: "faux".into(), provider: "faux".into(), model: "faux-1".into(), stop_reason: StopReason::Stop, ..assistant_with(content) }
    }

    fn protocol_exposes_a_function_for_every_formatter(format: ToolCallFormat) {
        let protocol = get_protocol(format);
        let tools = vec![weather_tool()];
        assert!(!protocol.format_tools_system_prompt(&tools).is_empty());
        assert!(protocol.format_tool_call("get_weather", &json!({"city": "Seoul"}).as_object().expect("object").clone()).contains("get_weather"));
        let content = vec![ToolResultContent::Text(crate::types::TextContent { text: "sunny".into(), audience: None, text_signature: None })];
        assert!(protocol.format_tool_response("get_weather", "call-1", &content).contains("sunny"));
    }

    #[test]
    fn should_return_hermes_protocol_for_the_hermes_format() {
        protocol_exposes_a_function_for_every_formatter(ToolCallFormat::Hermes);
    }

    #[test]
    fn should_return_the_same_morph_xml_protocol_for_morph_xml_and_xml_formats() {
        let canonical = get_protocol(ToolCallFormat::MorphXml);
        let alias = get_protocol(ToolCallFormat::Xml);
        assert!(std::ptr::eq(canonical, alias));
        assert!(!canonical.format_tools_system_prompt(&[weather_tool()]).is_empty());
    }

    #[test]
    fn should_return_gemma4_protocol_for_the_gemma4_delimiter_format() {
        protocol_exposes_a_function_for_every_formatter(ToolCallFormat::Gemma4Delimiter);
    }

    #[test]
    fn should_return_yaml_xml_protocol_for_the_yaml_xml_format() {
        protocol_exposes_a_function_for_every_formatter(ToolCallFormat::YamlXml);
    }

    #[test]
    fn should_strip_tools_from_the_context() {
        let context = Context { system_prompt: Some("You are helpful".into()), messages: vec![user_message("Hello")], tools: Some(vec![weather_tool()]) };
        assert!(transform_context(&context, &MockProtocol).tools.is_none());
    }

    #[test]
    fn should_inject_tool_definitions_into_the_system_prompt() {
        let context = Context { system_prompt: Some("You are helpful".into()), messages: vec![user_message("Hello")], tools: Some(vec![weather_tool()]) };
        let transformed = transform_context(&context, &MockProtocol);
        let prompt = transformed.system_prompt.expect("system prompt");
        assert!(prompt.contains("<tools>"));
        assert!(prompt.contains("get_weather"));
        assert!(prompt.contains("You are helpful"));
    }

    #[test]
    fn should_not_modify_the_system_prompt_when_there_are_no_tools() {
        let context = Context { system_prompt: Some("You are helpful".into()), messages: vec![user_message("Hello")], tools: Some(Vec::new()) };
        assert_eq!(transform_context(&context, &MockProtocol).system_prompt.as_deref(), Some("You are helpful"));
    }

    #[test]
    fn should_not_mutate_the_original_context() {
        let context = Context { system_prompt: Some("You are helpful".into()), messages: vec![user_message("Hello")], tools: Some(vec![weather_tool()]) };
        let before = context.clone();
        let transformed = transform_context(&context, &MockProtocol);
        assert_eq!(context, before);
        assert_ne!(transformed.system_prompt, context.system_prompt);
    }

    #[test]
    fn should_convert_assistant_message_tool_calls_to_text() {
        let assistant_turn = Message::Assistant(Box::new(plain_assistant(vec![
            ContentBlock::text("Let me check the weather"),
            ContentBlock::ToolCall(ToolCall { id: "call_123".into(), name: "get_weather".into(), arguments: json!({"city": "Seoul"}).as_object().expect("object").clone(), incomplete: None, error_message: None, thought_signature: None, namespace: None }),
        ])));
        let context = Context { system_prompt: Some("You are helpful".into()), messages: vec![assistant_turn], tools: Some(vec![weather_tool()]) };
        let transformed = transform_context(&context, &MockProtocol);
        let Message::Assistant(transformed_assistant) = &transformed.messages[0] else { panic!("expected assistant") };
        assert_eq!(transformed_assistant.content.len(), 2);
        assert_eq!(transformed_assistant.content[0], ContentBlock::text("Let me check the weather"));
        assert_eq!(transformed_assistant.content[1], ContentBlock::text(r#"<tool_call>get_weather:{"city":"Seoul"}</tool_call>"#));
    }

    #[test]
    fn should_pass_through_an_assistant_message_without_tool_calls_unchanged() {
        let assistant_turn = Message::Assistant(Box::new(plain_assistant(vec![ContentBlock::text("Hello there")])));
        let context = Context { system_prompt: Some("You are helpful".into()), messages: vec![assistant_turn.clone()], tools: Some(vec![weather_tool()]) };
        assert_eq!(transform_context(&context, &MockProtocol).messages[0], assistant_turn);
    }

    #[test]
    fn should_convert_a_tool_result_message_to_a_user_message() {
        let tool_result = Message::ToolResult(ToolResultMessage {
            tool_call_id: "call_123".into(),
            tool_name: "get_weather".into(),
            content: vec![ContentBlock::text("Sunny, 23C")],
            details: None,
            usage: None,
            added_tool_names: None,
            is_error: false,
            timestamp: 0,
        });
        let context = Context { system_prompt: Some("You are helpful".into()), messages: vec![tool_result], tools: Some(vec![weather_tool()]) };
        let transformed = transform_context(&context, &MockProtocol);
        let Message::User(user) = &transformed.messages[0] else { panic!("expected user") };
        assert_eq!(user.content, UserContent::Text("<tool_response>get_weather:Sunny, 23C</tool_response>".into()));
    }

    #[test]
    fn should_pass_through_user_messages_unchanged() {
        let context = Context {
            system_prompt: Some("You are helpful".into()),
            messages: vec![user_message("Hello"), user_message("How are you?")],
            tools: Some(vec![weather_tool()]),
        };
        let transformed = transform_context(&context, &MockProtocol);
        assert_eq!(transformed.messages[0], context.messages[0]);
        assert_eq!(transformed.messages[1], context.messages[1]);
    }

    #[test]
    fn should_handle_a_complex_conversation_history() {
        let messages = vec![
            user_message("What's the weather?"),
            Message::Assistant(Box::new(plain_assistant(vec![
                ContentBlock::text("I'll check"),
                ContentBlock::ToolCall(ToolCall { id: "call_1".into(), name: "get_weather".into(), arguments: json!({"city": "Tokyo"}).as_object().expect("object").clone(), incomplete: None, error_message: None, thought_signature: None, namespace: None }),
            ]))),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "call_1".into(),
                tool_name: "get_weather".into(),
                content: vec![ContentBlock::text("Rainy")],
                details: None,
                usage: None,
                added_tool_names: None,
                is_error: false,
                timestamp: 2,
            }),
            Message::Assistant(Box::new(plain_assistant(vec![ContentBlock::text("It's rainy in Tokyo")]))),
        ];
        let context = Context { system_prompt: Some("You are helpful".into()), messages: messages.clone(), tools: Some(vec![weather_tool()]) };
        let transformed = transform_context(&context, &MockProtocol);
        assert_eq!(transformed.messages[0], messages[0]);
        let Message::Assistant(first_assistant) = &transformed.messages[1] else { panic!("expected assistant") };
        assert_eq!(first_assistant.content.len(), 2);
        assert_eq!(first_assistant.content[1], ContentBlock::text(r#"<tool_call>get_weather:{"city":"Tokyo"}</tool_call>"#));
        assert!(matches!(transformed.messages[2], Message::User(_)));
        assert_eq!(transformed.messages[3], messages[3]);
    }

    fn flagged_tool_call_message() -> Message {
        Message::Assistant(Box::new(plain_assistant(vec![ContentBlock::ToolCall(ToolCall {
            id: "call_flagged".into(),
            name: "get_weather".into(),
            arguments: json!({"city": "Seo"}).as_object().expect("object").clone(),
            incomplete: Some(true),
            error_message: Some("Tool call was truncated mid-arguments".into()),
            thought_signature: None,
            namespace: None,
        })])))
    }

    fn retry_diagnostic_message() -> Message {
        Message::ToolResult(ToolResultMessage {
            tool_call_id: "call_flagged".into(),
            tool_name: "get_weather".into(),
            content: vec![ContentBlock::text(
                "Tool call \"get_weather\" was not executed: the response ended before the tool call was complete. Re-issue the tool call with complete arguments.",
            )],
            details: None,
            usage: None,
            added_tool_names: None,
            is_error: true,
            timestamp: 2,
        })
    }

    fn replayed_text(transformed: &Context) -> String {
        let Message::Assistant(assistant) = &transformed.messages[0] else { panic!("expected assistant") };
        assistant
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn replays_flagged_anthropic_xml_calls_canonically_without_leaking_the_truncated_fragment() {
        let raw_fragment = "<invoke name=\"get_weather\"><parameter name=\"city\">Seo";
        let context = Context {
            system_prompt: Some("You are helpful".into()),
            messages: vec![flagged_tool_call_message(), retry_diagnostic_message()],
            tools: Some(vec![weather_tool()]),
        };
        let transformed = transform_context(&context, get_protocol(ToolCallFormat::AnthropicXml));
        assert_eq!(replayed_text(&transformed), "<invoke name=\"get_weather\">\n<parameter name=\"city\">Seo</parameter>\n</invoke>");
        assert!(matches!(transformed.messages[1], Message::User(_)));
        let serialized = serde_json::to_string(&transformed).expect("serializable");
        assert!(serialized.contains("was not executed"));
        assert!(!serialized.contains(raw_fragment));
    }

    #[test]
    fn replays_flagged_hermes_calls_canonically_without_leaking_the_truncated_fragment() {
        let raw_fragment = "<tool_call>{\"name\":\"get_weather\",\"arguments\":{\"city\":\"Seo";
        let context = Context {
            system_prompt: Some("You are helpful".into()),
            messages: vec![flagged_tool_call_message(), retry_diagnostic_message()],
            tools: Some(vec![weather_tool()]),
        };
        let transformed = transform_context(&context, get_protocol(ToolCallFormat::Hermes));
        assert_eq!(replayed_text(&transformed), "<tool_call>\n{\"name\":\"get_weather\",\"arguments\":{\"city\":\"Seo\"}}\n</tool_call>");
        assert!(matches!(transformed.messages[1], Message::User(_)));
        let serialized = serde_json::to_string(&transformed).expect("serializable");
        assert!(serialized.contains("was not executed"));
        assert!(!serialized.contains(raw_fragment));
    }

    #[test]
    fn replays_flagged_calls_byte_identically_to_unflagged_calls() {
        let unflagged = Message::Assistant(Box::new(plain_assistant(vec![ContentBlock::ToolCall(ToolCall {
            id: "call_123".into(),
            name: "get_weather".into(),
            arguments: json!({"city": "Seo"}).as_object().expect("object").clone(),
            incomplete: None,
            error_message: None,
            thought_signature: None,
            namespace: None,
        })])));
        let base_context = Context { system_prompt: Some("You are helpful".into()), messages: vec![unflagged], tools: Some(vec![weather_tool()]) };
        let flagged_context = Context { system_prompt: Some("You are helpful".into()), messages: vec![flagged_tool_call_message()], tools: Some(vec![weather_tool()]) };
        assert_eq!(
            serde_json::to_string(&transform_context(&flagged_context, get_protocol(ToolCallFormat::AnthropicXml))).expect("serializable"),
            serde_json::to_string(&transform_context(&base_context, get_protocol(ToolCallFormat::AnthropicXml))).expect("serializable")
        );
    }

    #[test]
    fn should_use_the_hermes_protocol_correctly() {
        let context = Context { system_prompt: Some("You are helpful".into()), messages: vec![user_message("Hello")], tools: Some(vec![weather_tool()]) };
        let prompt = transform_context(&context, get_protocol(ToolCallFormat::Hermes)).system_prompt.expect("system prompt");
        assert!(prompt.contains("<tools>"));
        assert!(prompt.contains("get_weather"));
        assert!(prompt.contains("<tool_call>"));
    }

    #[test]
    fn should_use_the_morph_xml_protocol_correctly() {
        let context = Context { system_prompt: Some("You are helpful".into()), messages: vec![user_message("Hello")], tools: Some(vec![weather_tool()]) };
        let prompt = transform_context(&context, get_protocol(ToolCallFormat::Xml)).system_prompt.expect("system prompt");
        assert!(prompt.contains("<tools>"));
        assert!(prompt.contains("get_weather"));
        assert!(prompt.contains("wrap each element in an <item> tag"));
        assert!(prompt.contains("Array<object> example"));
    }

    #[test]
    fn should_use_the_gemma4_protocol_correctly() {
        let context = Context { system_prompt: Some("You are helpful".into()), messages: vec![user_message("Hello")], tools: Some(vec![weather_tool()]) };
        let prompt = transform_context(&context, get_protocol(ToolCallFormat::Gemma4Delimiter)).system_prompt.expect("system prompt");
        assert!(prompt.contains("<|tool_call>"));
        assert!(prompt.contains("get_weather"));
    }

    #[test]
    fn should_use_the_yaml_xml_protocol_correctly() {
        let context = Context { system_prompt: Some("You are helpful".into()), messages: vec![user_message("Hello")], tools: Some(vec![weather_tool()]) };
        let prompt = transform_context(&context, get_protocol(ToolCallFormat::YamlXml)).system_prompt.expect("system prompt");
        assert!(prompt.contains("<tools>"));
        assert!(prompt.contains("get_weather"));
        assert!(prompt.contains("Inside the XML element, specify parameters using YAML syntax"));
    }

}
