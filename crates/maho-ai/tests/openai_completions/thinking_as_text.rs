use std::sync::{Arc, Mutex};

use maho_ai::api::openai_completions::{
    convert_messages, ConvertCompletionsMessagesOptions, ResolvedOpenAICompletionsCompat,
};
use maho_ai::types::{ContentBlock, MaxTokensField, SessionAffinityFormat, StopReason, ThinkingFormat};
use serde_json::{json, Value};

use super::harness::*;

fn thinking_as_text_compat() -> ResolvedOpenAICompletionsCompat {
    ResolvedOpenAICompletionsCompat {
        supports_store: true,
        supports_developer_role: true,
        supports_reasoning_effort: true,
        supports_usage_in_streaming: true,
        supports_finish_reason: true,
        max_tokens_field: MaxTokensField::MaxCompletionTokens,
        requires_tool_result_name: false,
        requires_assistant_after_tool_result: false,
        requires_thinking_as_text: true,
        requires_reasoning_content_on_assistant_messages: false,
        thinking_format: ThinkingFormat::Openai,
        supports_disabled_thinking: true,
        open_router_routing: Default::default(),
        vercel_gateway_routing: Default::default(),
        chat_template_kwargs: Default::default(),
        chat_template_args: Some(Default::default()),
        zai_tool_stream: false,
        supports_thinking_token_budget: Some(false),
        thinking_token_budget_field: None,
        supports_strict_mode: true,
        tool_schema_flavor: None,
        tool_call_format: None,
        supports_openai_grammar_tools: false,
        cache_control_format: None,
        send_session_affinity_headers: false,
        deferred_tools_mode: None,
        session_affinity_format: SessionAffinityFormat::Openai,
        supports_prompt_cache_key: Some(false),
        supports_max_output_tokens: true,
        vllm_priority: None,
        venice_parameters: None,
        supports_long_cache_retention: true,
    }
}

fn build_model(base_url: &str) -> maho_ai::types::Model {
    model(&[
        ("id", json!("repro-model")),
        ("name", json!("Repro Model")),
        ("provider", json!("repro-provider")),
        ("baseUrl", json!(base_url)),
        ("reasoning", json!(true)),
        ("maxTokens", json!(4096)),
        (
            "compat",
            json!({
                "supportsStore": true,
                "supportsDeveloperRole": true,
                "supportsReasoningEffort": true,
                "supportsUsageInStreaming": true,
                "supportsFinishReason": true,
                "maxTokensField": "max_completion_tokens",
                "requiresToolResultName": false,
                "requiresAssistantAfterToolResult": false,
                "requiresThinkingAsText": true,
                "requiresReasoningContentOnAssistantMessages": false,
                "thinkingFormat": "openai",
                "supportsDisabledThinking": true,
                "supportsThinkingTokenBudget": false,
                "supportsStrictMode": true,
                "supportsOpenaiGrammarTools": false,
                "sendSessionAffinityHeaders": false,
                "sessionAffinityFormat": "openai",
                "supportsPromptCacheKey": false,
                "supportsMaxOutputTokens": true,
                "supportsLongCacheRetention": true,
            }),
        ),
    ])
}

fn build_context(assistant: maho_ai::types::Message) -> maho_ai::types::Context {
    let mut first = user_message("hello");
    let mut last = user_message("continue");
    if let maho_ai::types::Message::User(user) = &mut first {
        user.timestamp = 1;
    }
    if let maho_ai::types::Message::User(user) = &mut last {
        user.timestamp = 3;
    }
    context(vec![first, assistant, last], None)
}

fn build_assistant(content: Vec<ContentBlock>) -> maho_ai::types::Message {
    let mut message = assistant(content, StopReason::Stop);
    if let maho_ai::types::Message::Assistant(assistant) = &mut message {
        assistant.provider = "repro-provider".to_owned();
        assistant.model = "repro-model".to_owned();
        assistant.timestamp = 2;
    }
    message
}

#[test]
fn serializes_same_model_thinking_plus_text_replay_as_assistant_text_parts() {
    let messages = convert_messages(
        &build_model("http://127.0.0.1:1"),
        &build_context(build_assistant(vec![
            thinking_block("internal reasoning", None),
            text_block("visible answer"),
        ])),
        &thinking_as_text_compat(),
        &ConvertCompletionsMessagesOptions::default(),
    )
    .expect("convert messages");

    assert_eq!(
        messages[1],
        json!({
            "role": "assistant",
            "content": [
                { "type": "text", "text": "internal reasoning" },
                { "type": "text", "text": "visible answer" },
            ],
        })
    );
}

#[test]
fn serializes_same_model_thinking_only_replay_as_assistant_text_parts() {
    let messages = convert_messages(
        &build_model("http://127.0.0.1:1"),
        &build_context(build_assistant(vec![thinking_block("internal reasoning", None)])),
        &thinking_as_text_compat(),
        &ConvertCompletionsMessagesOptions::default(),
    )
    .expect("convert messages");

    assert_eq!(
        messages[1],
        json!({ "role": "assistant", "content": [{ "type": "text", "text": "internal reasoning" }] })
    );
}

#[test]
fn omits_standalone_same_model_thinking_replay_when_thinking_is_off() {
    let messages = convert_messages(
        &build_model("http://127.0.0.1:1"),
        &build_context(build_assistant(vec![
            thinking_block("internal reasoning", None),
            text_block("visible answer"),
        ])),
        &thinking_as_text_compat(),
        &ConvertCompletionsMessagesOptions { preserve_thinking: Some(false), ..Default::default() },
    )
    .expect("convert messages");

    assert_eq!(messages[1], json!({ "role": "assistant", "content": "visible answer" }));
}

#[tokio::test]
async fn reaches_the_endpoint_with_thinking_replay_when_reasoning_is_requested() {
    let request_bodies: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = request_bodies.clone();
    let server = RawServer::start(raw_handler(move |mut stream, request| {
        let sink = sink.clone();
        async move {
            if let Some(body) = request_body(&request).as_object() {
                sink.lock().expect("request lock").push(Value::Object(body.clone()));
            }
            let frames = vec![
                json!({
                    "id": "chatcmpl-repro",
                    "object": "chat.completion.chunk",
                    "created": 0,
                    "model": "repro-model",
                    "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "ok" }, "finish_reason": null }],
                }),
                json!({
                    "id": "chatcmpl-repro",
                    "object": "chat.completion.chunk",
                    "created": 0,
                    "model": "repro-model",
                    "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
                    "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
                }),
            ];
            let _ = write_sse(&mut stream, &frames, None).await;
        }
    }))
    .await;

    let mut options = options("test-key");
    options.reasoning_effort = Some(maho_ai::types::ModelThinkingLevel::Low);
    let stream = maho_ai::api::openai_completions::stream(
        &build_model(&server.base_url),
        &build_context(build_assistant(vec![
            thinking_block("internal reasoning", None),
            text_block("visible answer"),
        ])),
        Some(options),
    );
    let events = events(&stream).await;

    let bodies = request_bodies.lock().expect("request lock").clone();
    assert_eq!(bodies.len(), 1);
    assert_eq!(
        bodies[0].get("messages").and_then(Value::as_array).map(|messages| &messages[1]),
        Some(&json!({
            "role": "assistant",
            "content": [
                { "type": "text", "text": "internal reasoning" },
                { "type": "text", "text": "visible answer" },
            ],
        }))
    );

    let terminal = events.last().expect("terminal event");
    assert!(matches!(terminal, maho_ai::types::AssistantMessageEvent::Done { .. }));
    server.shutdown();
}
