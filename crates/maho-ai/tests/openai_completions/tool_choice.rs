use std::sync::{Arc, Mutex};

use maho_ai::api::openai_completions::{convert_messages, ConvertCompletionsMessagesOptions};
use maho_ai::types::{AssistantMessageEvent, SimpleStreamOptions, StopReason, ThinkingLevel};
use serde_json::{json, Value};

use super::harness::*;

fn base_model(overrides: &[(&str, Value)]) -> maho_ai::types::Model {
    let mut model = catalog_model("openai", "gpt-4o-mini");
    model.compat = None;
    model.reasoning = true;
    let mut value = serde_json::to_value(&model).expect("model json");
    for (key, override_value) in overrides {
        value[key] = override_value.clone();
    }
    serde_json::from_value(value).expect("model")
}

fn payload_capture(_model: &maho_ai::types::Model) -> (Arc<Mutex<Option<Value>>>, maho_ai::types::OnPayload) {
    let sink: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let writer = sink.clone();
    let hook: maho_ai::types::OnPayload = Arc::new(move |payload: &Value, _model, _meta| {
        *writer.lock().expect("payload lock") = Some(payload.clone());
        None
    });
    (sink, hook)
}

async fn capture_simple(
    model: &maho_ai::types::Model,
    reasoning: Option<ThinkingLevel>,
    tools: Option<Vec<maho_ai::types::Tool>>,
    system_prompt: Option<&str>,
) -> Value {
    let (sink, hook) = payload_capture(model);
    let transport = ScriptedTransport::success([json!({
        "choices": [{ "delta": {}, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
    })]);
    let mut context = context(vec![user_message("Hi")], tools);
    context.system_prompt = system_prompt.map(str::to_owned);
    let mut options: SimpleStreamOptions = simple_options("test");
    options.reasoning = reasoning;
    options.stream.request.on_payload = Some(hook);
    let message = finish(&run_simple(model, &context, options, Some(transport.clone()))).await;
    assert_eq!(message.stop_reason, StopReason::Stop, "request should reach the scripted transport");
    sink.lock().expect("payload lock").clone().unwrap_or_else(|| transport.last_request().body)
}

fn ping_tool() -> maho_ai::types::Tool {
    tool("ping", json!({ "type": "object", "properties": { "ok": { "type": "boolean" } }, "required": ["ok"] }))
}

fn read_tool() -> maho_ai::types::Tool {
    tool("read", json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }))
}

#[tokio::test]
async fn forwards_tool_choice_from_simple_options_to_payload() {
    let model = base_model(&[]);
    let (sink, hook) = payload_capture(&model);
    let transport = ScriptedTransport::success([json!({
        "choices": [{ "delta": {}, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
    })]);
    let mut options = simple_options("test");
    options.stream.request.on_payload = Some(hook);
    options.stream.extra.insert("toolChoice".to_owned(), json!("required"));
    let _ = finish(&run_simple(&model, &context(vec![user_message("Call ping with ok=true")], Some(vec![ping_tool()])), options, Some(transport.clone()))).await;

    let payload = sink.lock().expect("payload lock").clone().unwrap_or_else(|| transport.last_request().body);
    assert_eq!(payload.get("tool_choice"), Some(&json!("required")));
    assert!(payload.get("tools").and_then(Value::as_array).is_some_and(|tools| !tools.is_empty()));
}

#[tokio::test]
async fn retries_forced_tool_choice_400_once_without_tool_choice() {
    let model = base_model(&[]);
    let transport = ScriptedTransport::new([
        Err(maho_ai::api::openai_completions::OpenAiCompletionsError::http(
            "tool_choice is not supported by this model",
            400,
            reqwest::header::HeaderMap::new(),
        )),
        Ok(ScriptedResponse::chunks([json!({
            "choices": [{ "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
        })])),
    ]);
    let mut options = options("test");
    options.tool_choice = Some(json!("required"));
    let response = finish(&run(&model, &context(vec![user_message("Call ping with ok=true")], Some(vec![ping_tool()])), options, transport.clone())).await;

    let calls = transport.requests();
    assert_eq!(response.stop_reason, StopReason::Stop);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].body.get("tool_choice"), Some(&json!("required")));
    assert_eq!(calls[1].body.get("tool_choice"), None);
}

#[tokio::test]
async fn omits_strict_when_compat_disables_strict_mode() {
    let model = base_model(&[("compat", json!({ "supportsStrictMode": false }))]);
    let payload = capture_simple(&model, None, Some(vec![ping_tool()]), None).await;

    let function = payload
        .get("tools")
        .and_then(Value::as_array)
        .and_then(|tools| tools.first())
        .and_then(|tool| tool.get("function"))
        .expect("tool function");
    assert!(!function.as_object().expect("function object").contains_key("strict"));
}

#[tokio::test]
async fn maps_groq_qwen_reasoning_levels_to_default_reasoning_effort() {
    let model = catalog_model("groq", "qwen/qwen3.6-27b");
    let payload = capture_simple(&model, Some(ThinkingLevel::Medium), None, None).await;

    assert_eq!(payload.get("reasoning_effort").and_then(Value::as_str), Some("default"));
}

#[tokio::test]
async fn keeps_normal_reasoning_effort_for_groq_models_without_compat_mapping() {
    let model = catalog_model("groq", "openai/gpt-oss-20b");
    let payload = capture_simple(&model, Some(ThinkingLevel::Medium), None, None).await;

    assert_eq!(payload.get("reasoning_effort").and_then(Value::as_str), Some("medium"));
}

#[tokio::test]
async fn enables_tool_stream_for_supported_z_ai_models_with_tools() {
    let model = catalog_model("zai", "glm-5.2");
    let payload = capture_simple(&model, None, Some(vec![ping_tool()]), None).await;

    assert_eq!(payload.get("tool_stream"), Some(&json!(true)));
}

#[tokio::test]
async fn maps_z_ai_glm_5_3_thinking_levels_to_reasoning_effort() {
    let model = catalog_model("zai", "glm-5.3");
    let cases = [
        (ThinkingLevel::Low, "low"),
        (ThinkingLevel::Medium, "high"),
        (ThinkingLevel::High, "high"),
        (ThinkingLevel::Xhigh, "max"),
        (ThinkingLevel::Max, "max"),
    ];

    for (reasoning, effort) in cases {
        let payload = capture_simple(&model, Some(reasoning), None, None).await;
        assert_eq!(payload.get("thinking"), Some(&json!({ "type": "enabled", "clear_thinking": false })));
        assert_eq!(payload.get("reasoning_effort").and_then(Value::as_str), Some(effort));
    }
}

#[tokio::test]
async fn does_not_send_disabled_thinking_for_glm_5_3_when_reasoning_is_off_through_stream_simple() {
    let model = catalog_model("zai", "glm-5.3");
    let payload = capture_simple(&model, None, None, None).await;

    assert_eq!(payload.get("thinking"), Some(&json!({ "type": "enabled", "clear_thinking": false })));
    assert_eq!(payload.get("reasoning_effort"), None);
}

#[tokio::test]
async fn maps_z_ai_glm_5_2_thinking_levels_to_reasoning_effort() {
    let model = catalog_model("zai", "glm-5.2");
    let cases = [
        (ThinkingLevel::Low, "high"),
        (ThinkingLevel::Medium, "high"),
        (ThinkingLevel::High, "high"),
        (ThinkingLevel::Max, "max"),
    ];

    for (reasoning, effort) in cases {
        let payload = capture_simple(&model, Some(reasoning), None, None).await;
        assert_eq!(payload.get("thinking"), Some(&json!({ "type": "enabled", "clear_thinking": false })));
        assert_eq!(payload.get("reasoning_effort").and_then(Value::as_str), Some(effort));
    }
}

#[tokio::test]
async fn preserves_z_ai_thinking_when_replaying_reasoning_content() {
    let model = catalog_model("zai", "glm-5.2");
    let mut replayed = assistant(
        vec![thinking_block("prior reasoning", Some("reasoning_content")), tool_call("call_1", "read", json!({ "path": "README.md" }))],
        StopReason::ToolUse,
    );
    if let maho_ai::types::Message::Assistant(assistant) = &mut replayed {
        assistant.provider = "zai".to_owned();
        assistant.model = "glm-5.2".to_owned();
    }
    let messages = context(
        vec![
            user_message("Read README.md"),
            replayed,
            tool_result("call_1", vec![text_block("contents")]),
            user_message("Continue"),
        ],
        None,
    );
    let (sink, hook) = payload_capture(&model);
    let transport = ScriptedTransport::success([json!({
        "choices": [{ "delta": {}, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
    })]);
    let mut options = simple_options("test");
    options.reasoning = Some(ThinkingLevel::High);
    options.stream.request.on_payload = Some(hook);
    let _ = finish(&run_simple(&model, &messages, options, Some(transport.clone()))).await;

    let payload = sink.lock().expect("payload lock").clone().unwrap_or_else(|| transport.last_request().body);
    let replayed_assistant = payload
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| messages.iter().find(|message| message.get("role").and_then(Value::as_str) == Some("assistant")))
        .expect("replayed assistant");
    assert_eq!(replayed_assistant.get("reasoning_content").and_then(Value::as_str), Some("prior reasoning"));
    assert_eq!(payload.get("thinking"), Some(&json!({ "type": "enabled", "clear_thinking": false })));
}

#[tokio::test]
async fn omits_z_ai_glm_5_2_reasoning_effort_when_thinking_is_off() {
    let model = catalog_model("zai", "glm-5.2");
    let payload = capture_simple(&model, None, None, None).await;

    assert_eq!(payload.get("thinking"), Some(&json!({ "type": "disabled" })));
    assert_eq!(payload.get("reasoning_effort"), None);
}

#[tokio::test]
async fn honors_z_ai_compat_override_that_disables_tool_stream() {
    let mut model = catalog_model("zai", "glm-5.2");
    let mut compat = model.compat.clone().unwrap_or_default().0;
    compat.remove("zaiToolStream");
    model.compat = Some(maho_ai::model::ModelCompat(compat));
    let payload = capture_simple(&model, None, Some(vec![ping_tool()]), None).await;

    assert_eq!(payload.get("tool_stream"), None);
}

#[tokio::test]
async fn respects_explicit_z_ai_tool_stream_compat_override() {
    let mut model = catalog_model("zai", "glm-5.2");
    let mut compat = model.compat.clone().unwrap_or_default().0;
    compat.insert("zaiToolStream".to_owned(), json!(true));
    model.compat = Some(maho_ai::model::ModelCompat(compat));
    let payload = capture_simple(&model, None, Some(vec![ping_tool()]), None).await;

    assert_eq!(payload.get("tool_stream"), Some(&json!(true)));
}

#[tokio::test]
async fn omits_tool_stream_when_no_tools_are_provided() {
    let model = catalog_model("zai", "glm-5.2");
    let payload = capture_simple(&model, None, None, None).await;

    assert_eq!(payload.get("tool_stream"), None);
}

#[tokio::test]
async fn maps_non_standard_provider_finish_reason_values_to_stop_reason_error() {
    let model = catalog_model("zai", "glm-5.2");
    let transport = ScriptedTransport::success([
        json!({ "choices": [{ "delta": { "content": "partial" }, "finish_reason": null }] }),
        json!({
            "choices": [{ "delta": {}, "finish_reason": "network_error" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
        }),
    ]);
    let response = finish(&run_simple(&model, &context(vec![user_message("Hi")], None), simple_options("test"), Some(transport))).await;

    assert_eq!(response.stop_reason, StopReason::Error);
    assert_eq!(response.error_message.as_deref(), Some("Provider finish_reason: network_error"));
}

#[tokio::test]
async fn ignores_null_stream_chunks_from_openai_compatible_providers() {
    let model = base_model(&[]);
    let transport = ScriptedTransport::success([
        Value::Null,
        json!({ "id": "chatcmpl-test", "choices": [{ "delta": { "content": "OK" }, "finish_reason": null }] }),
        json!({
            "id": "chatcmpl-test",
            "choices": [{ "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 3, "completion_tokens": 1 },
        }),
    ]);
    let response = finish(&run_simple(&model, &context(vec![user_message("Reply with exactly OK")], None), simple_options("test"), Some(transport))).await;

    assert_eq!(response.stop_reason, StopReason::Stop);
    assert_eq!(response.error_message, None);
    assert_eq!(response.response_id.as_deref(), Some("chatcmpl-test"));
    assert_eq!(response.usage.total_tokens, 4);
    assert_eq!(response.content.len(), 1);
}

#[tokio::test]
async fn errors_when_a_stream_ends_after_only_null_finish_reason_chunks() {
    let model = base_model(&[]);
    let transport = ScriptedTransport::success([
        json!({ "id": "chatcmpl-truncated", "choices": [{ "delta": { "content": "partial answer" }, "finish_reason": null }] }),
        json!({ "id": "chatcmpl-truncated", "choices": [{ "delta": { "content": "partial answer" }, "finish_reason": null }] }),
    ]);
    let response = finish(&run_simple(&model, &context(vec![user_message("Reply with a longer sentence")], None), simple_options("test"), Some(transport))).await;

    assert_eq!(response.stop_reason, StopReason::Error);
    assert_eq!(response.error_message.as_deref(), Some("Stream ended without finish_reason"));
}

#[tokio::test]
async fn accepts_streams_without_finish_reason_when_compat_disables_it() {
    let model = base_model(&[("compat", json!({ "supportsFinishReason": false }))]);
    let transport = ScriptedTransport::success([json!({
        "id": "chatcmpl-no-finish-reason",
        "choices": [{ "delta": { "content": "complete answer" }, "finish_reason": null }],
    })]);
    let response = finish(&run_simple(&model, &context(vec![user_message("Reply with a complete answer")], None), simple_options("test"), Some(transport))).await;

    assert_eq!(response.stop_reason, StopReason::Stop);
    assert_eq!(response.error_message, None);
    assert_eq!(response.content.len(), 1);
}

#[tokio::test]
async fn ignores_empty_custom_objects_on_function_tool_call_deltas() {
    let model = base_model(&[]);
    let transport = ScriptedTransport::success([json!({
        "id": "chatcmpl-empty-custom",
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": "read", "arguments": "{\"path\":\"README.md\"}" },
                    "custom": {},
                }],
            },
            "finish_reason": "tool_calls",
        }],
    })]);
    let response = finish(&run_simple(&model, &context(vec![user_message("Read README.md")], Some(vec![read_tool()])), simple_options("test"), Some(transport))).await;

    assert_eq!(response.content.len(), 1);
    match &response.content[0] {
        maho_ai::types::ContentBlock::ToolCall(call) => {
            assert_eq!(call.id, "call_1");
            assert_eq!(call.name, "read");
            assert_eq!(call.arguments.get("path").and_then(Value::as_str), Some("README.md"));
        }
        other => panic!("expected a tool call, got {other:?}"),
    }
}

#[tokio::test]
async fn coalesces_tool_call_deltas_by_stable_index_when_provider_mutates_ids_mid_stream() {
    let model = base_model(&[]);
    let transport = ScriptedTransport::success([
        json!({
            "id": "chatcmpl-kimi-bad-stream",
            "choices": [{ "delta": { "tool_calls": [{ "index": 0, "id": "functions.read:0", "type": "function", "function": { "name": "read", "arguments": "" } }] }, "finish_reason": null }],
        }),
        json!({
            "id": "chatcmpl-kimi-bad-stream",
            "choices": [{ "delta": { "tool_calls": [{ "index": 0, "id": "chatcmpl-tool-a", "type": "function", "function": { "name": null, "arguments": "{\"path\":\"README" } }] }, "finish_reason": null }],
        }),
        json!({
            "id": "chatcmpl-kimi-bad-stream",
            "choices": [{ "delta": { "tool_calls": [{ "index": 0, "id": "chatcmpl-tool-b", "type": "function", "function": { "name": null, "arguments": ".md\"}" } }] }, "finish_reason": "tool_calls" }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5 },
        }),
    ]);
    let stream = run_simple(&model, &context(vec![user_message("Read README.md")], Some(vec![read_tool()])), simple_options("test"), Some(transport));
    let mut tool_call_indexes = Vec::new();
    let mut collected = Vec::new();
    while let Some(event) = stream.next().await.expect("stream event") {
        match &event {
            AssistantMessageEvent::ToolcallStart { content_index, .. }
            | AssistantMessageEvent::ToolcallDelta { content_index, .. }
            | AssistantMessageEvent::ToolcallEnd { content_index, .. } => tool_call_indexes.push(*content_index),
            _ => {}
        }
        collected.push(event);
    }
    let response = stream.result().await.expect("result");

    assert_eq!(response.stop_reason, StopReason::ToolUse);
    assert_eq!(tool_call_indexes, vec![0, 0, 0, 0, 0]);
    assert_eq!(response.content.len(), 1);
    match &response.content[0] {
        maho_ai::types::ContentBlock::ToolCall(call) => {
            assert_eq!(call.id, "functions.read:0");
            assert_eq!(call.name, "read");
            assert_eq!(call.arguments.get("path").and_then(Value::as_str), Some("README.md"));
        }
        other => panic!("expected a tool call, got {other:?}"),
    }
    let _ = collected;
}

#[tokio::test]
async fn uses_system_messages_for_non_openai_anthropic_openrouter_reasoning_model_instructions() {
    let model = catalog_model("openrouter", "deepseek/deepseek-v4-pro");
    let payload = capture_simple(&model, None, None, Some("Follow instructions.")).await;

    let role = payload
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| messages.first())
        .and_then(|message| message.get("role"))
        .and_then(Value::as_str);
    assert_eq!(role, Some("system"));
}

#[tokio::test]
async fn keeps_developer_messages_for_openai_and_anthropic_openrouter_batch_instructions() {
    let batch_id = maho_ai::compat::get_models("openrouter")
        .expect("openrouter catalog")
        .into_iter()
        .map(|model| model.id.clone())
        .find(|id| id.starts_with("anthropic/") && id.ends_with("batch"))
        .expect("an OpenRouter anthropic batch model in the catalog");
    for id in ["openai/gpt-5.2-codex".to_owned(), batch_id] {
        let model = catalog_model("openrouter", &id);
        let payload = capture_simple(&model, None, None, Some("Follow instructions.")).await;

        let role = payload
            .get("messages")
            .and_then(Value::as_array)
            .and_then(|messages| messages.first())
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str);
        assert_eq!(role, Some("developer"), "model {id}");
    }
}

#[tokio::test]
async fn keeps_developer_messages_for_openai_reasoning_model_instructions() {
    let model = base_model(&[("id", json!("gpt-5.5")), ("name", json!("GPT-5.5")), ("reasoning", json!(true))]);
    let payload = capture_simple(&model, None, None, Some("Follow instructions.")).await;

    let role = payload
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| messages.first())
        .and_then(|message| message.get("role"))
        .and_then(Value::as_str);
    assert_eq!(role, Some("developer"));
}

#[tokio::test]
async fn replays_xiaomi_mi_mo_assistant_tool_calls_with_empty_reasoning_content_when_thinking_is_missing() {
    let model = catalog_model("xiaomi", "mimo-v2.5-pro");
    let mut replayed = assistant(vec![tool_call("call_1", "read", json!({ "path": "README.md" }))], StopReason::ToolUse);
    if let maho_ai::types::Message::Assistant(assistant) = &mut replayed {
        assistant.provider = "xiaomi".to_owned();
        assistant.model = "mimo-v2.5-pro".to_owned();
    }
    let messages = context(
        vec![user_message("Read README.md"), replayed, tool_result("call_1", vec![text_block("contents")])],
        None,
    );
    let (sink, hook) = payload_capture(&model);
    let transport = ScriptedTransport::success([json!({
        "choices": [{ "delta": {}, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
    })]);
    let mut options = simple_options("test");
    options.reasoning = Some(ThinkingLevel::High);
    options.stream.request.on_payload = Some(hook);
    let _ = finish(&run_simple(&model, &messages, options, Some(transport.clone()))).await;

    let payload = sink.lock().expect("payload lock").clone().unwrap_or_else(|| transport.last_request().body);
    let replayed_assistant = payload
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| messages.iter().find(|message| message.get("role").and_then(Value::as_str) == Some("assistant")))
        .expect("replayed assistant");
    assert_eq!(replayed_assistant.get("reasoning_content").and_then(Value::as_str), Some(""));
    assert_eq!(payload.get("thinking"), Some(&json!({ "type": "enabled" })));
    assert_eq!(payload.get("reasoning_effort").and_then(Value::as_str), Some("high"));
}

#[tokio::test]
async fn normalizes_open_code_go_reasoning_deltas_to_reasoning_content_for_replay() {
    let model = catalog_model("opencode-go", "kimi-k2.6");
    let transport = ScriptedTransport::success([json!({
        "id": "chatcmpl-opencode-go-reasoning",
        "choices": [{ "delta": { "reasoning": "think" }, "finish_reason": "stop" }],
    })]);
    let response = finish(&run_simple(&model, &context(vec![user_message("Use reasoning.")], None), simple_options("test"), Some(transport))).await;

    match &response.content[0] {
        maho_ai::types::ContentBlock::Thinking(thinking) => {
            assert_eq!(thinking.thinking, "think");
            assert_eq!(thinking.thinking_signature.as_deref(), Some("reasoning_content"));
        }
        other => panic!("expected a thinking block, got {other:?}"),
    }
}

#[tokio::test]
async fn keeps_non_open_code_go_reasoning_deltas_on_the_original_reasoning_field() {
    let model = base_model(&[]);
    let transport = ScriptedTransport::success([json!({
        "id": "chatcmpl-reasoning",
        "choices": [{ "delta": { "reasoning": "think" }, "finish_reason": "stop" }],
    })]);
    let response = finish(&run_simple(&model, &context(vec![user_message("Use reasoning.")], None), simple_options("test"), Some(transport))).await;

    match &response.content[0] {
        maho_ai::types::ContentBlock::Thinking(thinking) => {
            assert_eq!(thinking.thinking, "think");
            assert_eq!(thinking.thinking_signature.as_deref(), Some("reasoning"));
        }
        other => panic!("expected a thinking block, got {other:?}"),
    }
}

#[tokio::test]
async fn replays_open_code_go_reasoning_thinking_blocks_as_reasoning_content() {
    let mut model = catalog_model("opencode-go", "kimi-k2.6");
    model.api = "openai-completions".to_owned();
    let mut replayed = assistant(
        vec![thinking_block("think", Some("reasoning")), tool_call("call_1", "read", json!({ "path": "README.md" }))],
        StopReason::Stop,
    );
    if let maho_ai::types::Message::Assistant(assistant) = &mut replayed {
        assistant.provider = "opencode-go".to_owned();
        assistant.model = "kimi-k2.6".to_owned();
    }
    let resolved = maho_ai::api::openai_completions::get_compat(&model);
    let messages = convert_messages(
        &model,
        &context(vec![replayed], None),
        &resolved,
        &ConvertCompletionsMessagesOptions::default(),
    )
    .expect("convert messages");

    let replayed_assistant = messages
        .iter()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
        .expect("replayed assistant");
    assert_eq!(replayed_assistant.get("reasoning_content").and_then(Value::as_str), Some("think"));
    assert_eq!(replayed_assistant.get("reasoning"), None);
}

#[tokio::test]
async fn sends_thinking_disabled_for_open_code_go_kimi_k2_6_when_thinking_is_off() {
    let model = catalog_model("opencode-go", "kimi-k2.6");
    let payload = capture_simple(&model, None, None, None).await;

    assert_eq!(payload.get("thinking"), Some(&json!({ "type": "disabled" })));
    assert_eq!(payload.get("reasoning_effort"), None);
}

#[tokio::test]
async fn sends_thinking_enabled_for_open_code_go_kimi_k2_6_when_thinking_is_enabled() {
    let model = catalog_model("opencode-go", "kimi-k2.6");
    let payload = capture_simple(&model, Some(ThinkingLevel::High), None, None).await;

    assert_eq!(payload.get("thinking"), Some(&json!({ "type": "enabled" })));
    assert_eq!(payload.get("reasoning_effort"), None);
}

#[tokio::test]
async fn omits_disabled_thinking_for_moonshot_kimi_k2_7_code_models() {
    for (provider, id) in [("moonshotai", "kimi-k2.7-code"), ("moonshotai-cn", "kimi-k2.7-code")] {
        let model = catalog_model(provider, id);
        let payload = capture_simple(&model, None, None, None).await;

        assert_eq!(payload.get("thinking"), None, "{provider}/{id}");
        assert_eq!(payload.get("reasoning_effort"), None, "{provider}/{id}");
    }
}

#[tokio::test]
async fn keeps_disabled_thinking_for_moonshot_kimi_k2_6_when_thinking_is_off() {
    let model = catalog_model("moonshotai-cn", "kimi-k2.6");
    let payload = capture_simple(&model, None, None, None).await;

    assert_eq!(payload.get("thinking"), Some(&json!({ "type": "disabled" })));
    assert_eq!(payload.get("reasoning_effort"), None);
}

#[tokio::test]
async fn does_not_double_count_reasoning_tokens_in_completion_usage() {
    let model = base_model(&[]);
    let transport = ScriptedTransport::success([json!({
        "id": "chatcmpl-usage",
        "choices": [{ "delta": { "content": "hi" }, "finish_reason": null }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "prompt_tokens_details": { "cached_tokens": 20, "cache_write_tokens": 5 },
            "completion_tokens_details": { "reasoning_tokens": 30 },
        },
    })]);
    let response = finish(&run_simple(&model, &context(vec![user_message("hi")], None), simple_options("test"), Some(transport))).await;

    assert_eq!(response.usage.input, 75);
    assert_eq!(response.usage.output, 50);
    assert_eq!(response.usage.cache_read, 20);
    assert_eq!(response.usage.cache_write, 5);
    assert_eq!(response.usage.reasoning, Some(30));
    assert_eq!(response.usage.total_tokens, 150);
}

#[tokio::test]
async fn preserves_prompt_tokens_details_cache_read_write_fields_from_chunk_usage() {
    let model = base_model(&[]);
    let transport = ScriptedTransport::success([json!({
        "id": "chatcmpl-cache",
        "choices": [{ "delta": {}, "finish_reason": "stop" }],
        "usage": {
            "prompt_tokens": 200,
            "completion_tokens": 10,
            "prompt_tokens_details": { "cached_tokens": 50, "cache_write_tokens": 25 },
        },
    })]);
    let response = finish(&run_simple(&model, &context(vec![user_message("hi")], None), simple_options("test"), Some(transport))).await;

    assert_eq!(response.usage.cache_read, 50);
    assert_eq!(response.usage.cache_write, 25);
    assert_eq!(response.usage.input, 125);
}

#[tokio::test]
async fn preserves_prompt_tokens_details_cache_read_write_fields_from_choice_usage_fallback() {
    let model = base_model(&[]);
    let transport = ScriptedTransport::success([json!({
        "id": "chatcmpl-cache-fallback",
        "choices": [{
            "delta": {},
            "finish_reason": "stop",
            "usage": {
                "prompt_tokens": 300,
                "completion_tokens": 20,
                "prompt_tokens_details": { "cached_tokens": 100, "cache_write_tokens": 40 },
            },
        }],
    })]);
    let response = finish(&run_simple(&model, &context(vec![user_message("hi")], None), simple_options("test"), Some(transport))).await;

    assert_eq!(response.usage.cache_read, 100);
    assert_eq!(response.usage.cache_write, 40);
    assert_eq!(response.usage.input, 160);
}

#[tokio::test]
async fn uses_openrouter_reasoning_object_instead_of_reasoning_effort() {
    let mut model = catalog_model("openrouter", "deepseek/deepseek-r1");
    model.reasoning = true;
    let payload = capture_simple(&model, Some(ThinkingLevel::High), None, None).await;

    assert!(payload.get("reasoning").is_some());
    assert_eq!(payload.get("reasoning_effort"), None);
}

#[tokio::test]
async fn uses_configurable_chat_template_boolean_thinking_kwargs() {
    let model = base_model(&[(
        "compat",
        json!({
            "thinkingFormat": "chat-template",
            "chatTemplateKwargs": { "enable_thinking": { "$var": "thinking.enabled" } },
        }),
    )]);
    let payload = capture_simple(&model, Some(ThinkingLevel::Medium), None, None).await;

    assert_eq!(payload.get("chat_template_kwargs"), Some(&json!({ "enable_thinking": true })));
}

#[tokio::test]
async fn uses_qwen_chat_template_thinking_kwargs() {
    let model = base_model(&[("compat", json!({ "thinkingFormat": "qwen-chat-template" }))]);
    let payload = capture_simple(&model, Some(ThinkingLevel::Medium), None, None).await;

    assert_eq!(payload.get("chat_template_kwargs"), Some(&json!({ "enable_thinking": true, "preserve_thinking": true })));
}

#[tokio::test]
async fn uses_configurable_chat_template_effort_kwargs_with_static_kwargs() {
    let model = base_model(&[(
        "compat",
        json!({
            "thinkingFormat": "chat-template",
            "supportsReasoningEffort": true,
            "chatTemplateKwargs": {
                "enable_thinking": { "$var": "thinking.enabled" },
                "reasoning_effort": { "$var": "thinking.effort" },
                "static_flag": "always",
            },
        }),
    )]);
    let payload = capture_simple(&model, Some(ThinkingLevel::High), None, None).await;

    let kwargs = payload.get("chat_template_kwargs").and_then(Value::as_object).expect("chat template kwargs");
    assert_eq!(kwargs.get("enable_thinking"), Some(&json!(true)));
    assert_eq!(kwargs.get("static_flag"), Some(&json!("always")));
}

#[tokio::test]
async fn uses_ant_ling_compatibility_metadata() {
    let model = catalog_model("ant-ling", "Ring-2.6-1T");
    let compat = model.compat.clone().expect("catalog compat").openai_completions();
    assert_eq!(compat.supports_store, Some(false));
    assert_eq!(compat.supports_developer_role, Some(false));
    assert_eq!(compat.supports_reasoning_effort, Some(false));
    assert_eq!(compat.max_tokens_field, Some(maho_ai::types::MaxTokensField::MaxTokens));
    assert_eq!(compat.thinking_format, Some(maho_ai::types::ThinkingFormat::AntLing));
    assert_eq!(compat.supports_long_cache_retention, Some(false));
    assert_eq!(compat.supports_strict_mode, None);
    assert_eq!(compat.requires_reasoning_content_on_assistant_messages, None);

    let (sink, hook) = payload_capture(&model);
    let transport = ScriptedTransport::success([json!({
        "choices": [{ "delta": {}, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
    })]);
    let mut context = context(vec![user_message("Hi")], None);
    context.system_prompt = Some("Follow instructions.".to_owned());
    let mut options = simple_options("test");
    options.reasoning = Some(ThinkingLevel::High);
    options.stream.max_tokens = Some(123);
    options.stream.cache_retention = Some(maho_ai::types::CacheRetention::Long);
    options.stream.session_id = Some("ant-ling-session".to_owned());
    options.stream.request.on_payload = Some(hook);
    let _ = finish(&run_simple(&model, &context, options, Some(transport.clone()))).await;

    let payload = sink.lock().expect("payload lock").clone().unwrap_or_else(|| transport.last_request().body);
    assert_eq!(payload.get("max_tokens").and_then(Value::as_u64), Some(123));
    assert_eq!(payload.get("max_completion_tokens"), None);
    assert_eq!(
        payload
            .get("messages")
            .and_then(Value::as_array)
            .and_then(|messages| messages.first())
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str),
        Some("system")
    );
    assert_eq!(payload.get("reasoning"), Some(&json!({ "effort": "high" })));
    assert_eq!(payload.get("reasoning_effort"), None);
    assert_eq!(payload.get("store"), None);
    assert_eq!(payload.get("prompt_cache_key"), None);
    assert_eq!(payload.get("prompt_cache_retention"), None);
}

#[tokio::test]
async fn omits_ant_ling_reasoning_for_unmapped_direct_reasoning_efforts_and_non_reasoning_models() {
    let ring = catalog_model("ant-ling", "Ring-2.6-1T");
    let (sink, hook) = payload_capture(&ring);
    let transport = ScriptedTransport::success([json!({
        "choices": [{ "delta": {}, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
    })]);
    let mut options = options("test");
    options.reasoning_effort = Some(maho_ai::types::ModelThinkingLevel::Medium);
    options.stream.request.on_payload = Some(hook);
    let _ = finish(&run(&ring, &context(vec![user_message("Hi")], None), options, transport.clone())).await;
    let payload = sink.lock().expect("payload lock").clone().unwrap_or_else(|| transport.last_request().body);
    assert_eq!(payload.get("reasoning"), None);

    let ling = catalog_model("ant-ling", "Ling-2.6-flash");
    let payload = capture_simple(&ling, Some(ThinkingLevel::High), None, None).await;
    assert_eq!(payload.get("reasoning"), None);
}

#[tokio::test]
async fn sends_max_tokens_for_open_code_completions_models() {
    for (provider, id) in [("opencode-go", "kimi-k2.6"), ("opencode", "kimi-k2.6")] {
        let model = catalog_model(provider, id);
        assert_eq!(
            model.compat.clone().expect("compat").openai_completions().max_tokens_field,
            Some(maho_ai::types::MaxTokensField::MaxTokens),
            "{provider}/{id}"
        );

        let (sink, hook) = payload_capture(&model);
        let transport = ScriptedTransport::success([json!({
            "choices": [{ "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
        })]);
        let mut options = simple_options("test");
        options.stream.max_tokens = Some(123);
        options.stream.request.on_payload = Some(hook);
        let _ = finish(&run_simple(&model, &context(vec![user_message("Hi")], None), options, Some(transport.clone()))).await;

        let payload = sink.lock().expect("payload lock").clone().unwrap_or_else(|| transport.last_request().body);
        assert_eq!(payload.get("max_tokens").and_then(Value::as_u64), Some(123), "{provider}/{id}");
        assert_eq!(payload.get("max_completion_tokens"), None, "{provider}/{id}");
    }
}

#[tokio::test]
async fn sends_max_tokens_for_built_in_and_custom_deep_seek_api_models() {
    let mut local = catalog_model("openai", "gpt-4o-mini");
    local.compat = None;
    local.reasoning = true;

    let mut custom = local.clone();
    custom.id = "custom-deepseek-model".to_owned();
    custom.name = "Custom DeepSeek Model".to_owned();
    custom.provider = "custom-deepseek".to_owned();
    custom.base_url = "https://api.deepseek.com".to_owned();

    let mut custom_uppercase = custom.clone();
    custom_uppercase.id = "custom-uppercase-deepseek-model".to_owned();
    custom_uppercase.name = "Custom Uppercase DeepSeek Model".to_owned();
    custom_uppercase.base_url = "https://API.DeepSeek.COM".to_owned();

    let native = [catalog_model("deepseek", "deepseek-flash"), catalog_model("deepseek", "deepseek-v4-pro")];
    for model in &native {
        assert_eq!(
            model.compat.clone().expect("compat").openai_completions().max_tokens_field,
            Some(maho_ai::types::MaxTokensField::MaxTokens),
            "{}",
            model.id
        );
    }

    for model in native.iter().cloned().chain([custom, custom_uppercase]) {
        let (sink, hook) = payload_capture(&model);
        let transport = ScriptedTransport::success([json!({
            "choices": [{ "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
        })]);
        let mut options = simple_options("test");
        options.stream.max_tokens = Some(123);
        options.stream.request.on_payload = Some(hook);
        let _ = finish(&run_simple(&model, &context(vec![user_message("Hi")], None), options, Some(transport.clone()))).await;

        let payload = sink.lock().expect("payload lock").clone().unwrap_or_else(|| transport.last_request().body);
        assert_eq!(payload.get("max_tokens").and_then(Value::as_u64), Some(123), "{}", model.id);
        assert_eq!(payload.get("max_completion_tokens"), None, "{}", model.id);
    }
}

#[tokio::test]
async fn sends_max_tokens_for_z_ai_completions_models() {
    for id in ["glm-5-turbo", "glm-5.2", "glm-5.3"] {
        let model = catalog_model("zai", id);
        assert_eq!(
            model.compat.clone().expect("compat").openai_completions().max_tokens_field,
            Some(maho_ai::types::MaxTokensField::MaxTokens),
            "{id}"
        );

        let (sink, hook) = payload_capture(&model);
        let transport = ScriptedTransport::success([json!({
            "choices": [{ "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
        })]);
        let mut options = simple_options("test");
        options.stream.max_tokens = Some(123);
        options.stream.request.on_payload = Some(hook);
        let _ = finish(&run_simple(&model, &context(vec![user_message("Hi")], None), options, Some(transport.clone()))).await;

        let payload = sink.lock().expect("payload lock").clone().unwrap_or_else(|| transport.last_request().body);
        assert_eq!(payload.get("max_tokens").and_then(Value::as_u64), Some(123), "{id}");
        assert_eq!(payload.get("max_completion_tokens"), None, "{id}");
    }
}

#[tokio::test]
async fn omits_reasoning_effort_for_open_code_grok_build() {
    let model = catalog_model("opencode", "grok-build-0.1");
    let payload = capture_simple(&model, Some(ThinkingLevel::High), None, None).await;

    assert_eq!(payload.get("reasoning_effort"), None);
}

#[tokio::test]
async fn accumulates_mixed_content_reasoning_and_parallel_tool_call_deltas_independently() {
    let model = base_model(&[]);
    let transport = ScriptedTransport::success([
        json!({
            "id": "chatcmpl-mixed-deltas",
            "choices": [{
                "delta": {
                    "content": "answer 1",
                    "reasoning_content": "think 1",
                    "tool_calls": [
                        { "index": 0, "id": "tc_read_initial", "type": "function", "function": { "name": "read", "arguments": "{\"path\":\"README" } },
                        { "index": 1, "id": "tc_grep_initial", "type": "function", "function": { "name": "grep", "arguments": "{\"pattern\":\"TODO" } },
                        { "id": "tc_list_no_index", "type": "function", "function": { "name": "list", "arguments": "{\"path\":\"packages" } },
                        { "id": "tc_write_no_index", "type": "function", "function": { "name": "write", "arguments": "{\"path\":\"out" } },
                    ],
                },
                "finish_reason": null,
            }],
        }),
        json!({
            "id": "chatcmpl-mixed-deltas",
            "choices": [{
                "delta": {
                    "content": " answer 2",
                    "tool_calls": [
                        { "index": 1, "id": "tc_grep_changed", "type": "function", "function": { "arguments": "\",\"path\":\"src" } },
                        { "id": "tc_write_no_index", "type": "function", "function": { "arguments": ".txt\",\"content\":\"ok\"}" } },
                        { "id": "tc_list_no_index", "type": "function", "function": { "arguments": "/ai\"}" } },
                    ],
                },
                "finish_reason": null,
            }],
        }),
        json!({
            "id": "chatcmpl-mixed-deltas",
            "choices": [{
                "delta": {
                    "content": "\n",
                    "reasoning_content": " think 2",
                    "tool_calls": [
                        { "index": 0, "id": "tc_read_changed", "type": "function", "function": { "arguments": ".md\"}" } },
                        { "index": 1, "type": "function", "function": { "arguments": "\"}" } },
                    ],
                },
                "finish_reason": "tool_calls",
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 8, "completion_tokens_details": { "reasoning_tokens": 2 } },
        }),
    ]);
    let tools = vec![
        tool("read", json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] })),
        tool("grep", json!({ "type": "object", "properties": { "pattern": { "type": "string" }, "path": { "type": "string" } }, "required": ["pattern", "path"] })),
        tool("list", json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] })),
        tool("write", json!({ "type": "object", "properties": { "path": { "type": "string" }, "content": { "type": "string" } }, "required": ["path", "content"] })),
    ];
    let stream = run_simple(&model, &context(vec![user_message("Think, answer, and use tools.")], Some(tools)), simple_options("test"), Some(transport));

    let mut event_types: Vec<&str> = Vec::new();
    let mut tool_events_by_index: std::collections::BTreeMap<usize, Vec<&str>> = std::collections::BTreeMap::new();
    while let Some(event) = stream.next().await.expect("stream event") {
        let (name, index) = match &event {
            AssistantMessageEvent::Start { .. } => ("start", None),
            AssistantMessageEvent::TextStart { content_index, .. } => ("text_start", Some(*content_index)),
            AssistantMessageEvent::TextDelta { content_index, .. } => ("text_delta", Some(*content_index)),
            AssistantMessageEvent::TextEnd { content_index, .. } => ("text_end", Some(*content_index)),
            AssistantMessageEvent::ThinkingStart { content_index, .. } => ("thinking_start", Some(*content_index)),
            AssistantMessageEvent::ThinkingDelta { content_index, .. } => ("thinking_delta", Some(*content_index)),
            AssistantMessageEvent::ThinkingEnd { content_index, .. } => ("thinking_end", Some(*content_index)),
            AssistantMessageEvent::ToolcallStart { content_index, .. } => ("toolcall_start", Some(*content_index)),
            AssistantMessageEvent::ToolcallDelta { content_index, .. } => ("toolcall_delta", Some(*content_index)),
            AssistantMessageEvent::ToolcallEnd { content_index, .. } => ("toolcall_end", Some(*content_index)),
            AssistantMessageEvent::Done { .. } => ("done", None),
            AssistantMessageEvent::Error { .. } => ("error", None),
        };
        event_types.push(name);
        if name.starts_with("toolcall")
            && let Some(index) = index
        {
            tool_events_by_index.entry(index).or_default().push(name);
        }
    }
    let response = stream.result().await.expect("result");

    assert_eq!(response.stop_reason, StopReason::ToolUse);
    let count = |name: &str| event_types.iter().filter(|event| **event == name).count();
    assert_eq!(count("text_start"), 1);
    assert_eq!(count("text_delta"), 3);
    assert_eq!(count("text_end"), 1);
    assert_eq!(count("thinking_start"), 1);
    assert_eq!(count("thinking_delta"), 2);
    assert_eq!(count("thinking_end"), 1);
    assert_eq!(count("toolcall_start"), 4);
    assert_eq!(count("toolcall_delta"), 9);
    assert_eq!(count("toolcall_end"), 4);

    let position = |name: &str| event_types.iter().position(|event| *event == name).expect("event present");
    assert!(position("text_end") < position("thinking_start"));
    assert!(position("thinking_end") < position("toolcall_start"));

    assert_eq!(tool_events_by_index.get(&2).map(Vec::as_slice), Some(&["toolcall_start", "toolcall_delta", "toolcall_delta", "toolcall_end"][..]));
    assert_eq!(tool_events_by_index.get(&3).map(Vec::as_slice), Some(&["toolcall_start", "toolcall_delta", "toolcall_delta", "toolcall_delta", "toolcall_end"][..]));
    assert_eq!(tool_events_by_index.get(&4).map(Vec::as_slice), Some(&["toolcall_start", "toolcall_delta", "toolcall_delta", "toolcall_end"][..]));
    assert_eq!(tool_events_by_index.get(&5).map(Vec::as_slice), Some(&["toolcall_start", "toolcall_delta", "toolcall_delta", "toolcall_end"][..]));

    assert_eq!(response.content.len(), 6);
    match &response.content[0] {
        maho_ai::types::ContentBlock::Text(text) => assert_eq!(text.text, "answer 1 answer 2\n"),
        other => panic!("expected text, got {other:?}"),
    }
    match &response.content[1] {
        maho_ai::types::ContentBlock::Thinking(thinking) => {
            assert_eq!(thinking.thinking, "think 1 think 2");
            assert_eq!(thinking.thinking_signature.as_deref(), Some("reasoning_content"));
        }
        other => panic!("expected thinking, got {other:?}"),
    }
    let calls: Vec<(&str, &str, &serde_json::Map<String, Value>)> = response.content[2..]
        .iter()
        .map(|block| match block {
            maho_ai::types::ContentBlock::ToolCall(call) => (call.id.as_str(), call.name.as_str(), &call.arguments),
            other => panic!("expected tool call, got {other:?}"),
        })
        .collect();
    assert_eq!(calls[0].0, "tc_read_initial");
    assert_eq!(calls[0].1, "read");
    assert_eq!(calls[0].2.get("path").and_then(Value::as_str), Some("README.md"));
    assert_eq!(calls[1].0, "tc_grep_initial");
    assert_eq!(calls[1].1, "grep");
    assert_eq!(calls[1].2.get("pattern").and_then(Value::as_str), Some("TODO"));
    assert_eq!(calls[1].2.get("path").and_then(Value::as_str), Some("src"));
    assert_eq!(calls[2].0, "tc_list_no_index");
    assert_eq!(calls[2].1, "list");
    assert_eq!(calls[2].2.get("path").and_then(Value::as_str), Some("packages/ai"));
    assert_eq!(calls[3].0, "tc_write_no_index");
    assert_eq!(calls[3].1, "write");
    assert_eq!(calls[3].2.get("path").and_then(Value::as_str), Some("out.txt"));
    assert_eq!(calls[3].2.get("content").and_then(Value::as_str), Some("ok"));
}

