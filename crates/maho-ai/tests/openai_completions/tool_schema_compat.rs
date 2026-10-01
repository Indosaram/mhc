use std::sync::{Arc, Mutex};

use maho_ai::api::openai_completions::{stream, OpenAiCompletionsOptions};
use serde_json::{json, Value};

use super::harness::*;

fn kimi_model(base_url: &str) -> maho_ai::types::Model {
    model(&[
        ("id", json!("kimi-test")),
        ("name", json!("Kimi Test")),
        ("provider", json!("moonshotai")),
        ("baseUrl", json!(base_url)),
        ("reasoning", json!(false)),
    ])
}

fn sse_frames() -> Vec<Value> {
    vec![
        json!({
            "id": "chatcmpl-schema",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": "kimi-test",
            "choices": [{ "index": 0, "delta": { "content": "ok" }, "finish_reason": null }],
        }),
        json!({
            "id": "chatcmpl-schema",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": "kimi-test",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        }),
    ]
}

#[tokio::test]
async fn normalizes_tools_injected_by_the_final_payload_hook() {
    let bodies: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = bodies.clone();
    let server = RawServer::start(raw_handler(move |mut stream, request| {
        let sink = sink.clone();
        async move {
            sink.lock().expect("request lock").push(request_body(&request));
            let _ = write_sse(&mut stream, &sse_frames(), None).await;
        }
    }))
    .await;

    let mut options = options("test-key");
    options.stream.request.on_payload = Some(Arc::new(|payload: &Value, _model, _meta| {
        let mut next = payload.as_object().cloned().unwrap_or_default();
        next.insert(
            "tools".to_owned(),
            json!([{
                "type": "function",
                "function": {
                    "name": "injected_tool",
                    "description": "Injected after the initial conversion",
                    "parameters": {
                        "type": "object",
                        "anyOf": [
                            { "properties": { "path": { "type": "string" } } },
                            { "properties": { "query": { "type": "string" } } },
                        ],
                    },
                },
            }]),
        );
        Some(Value::Object(next))
    }));
    let result = finish(&stream(
        &kimi_model(&server.base_url),
        &context(vec![user_message("hello")], None),
        Some(options),
    ))
    .await;

    assert_eq!(result.stop_reason, maho_ai::types::StopReason::Stop);
    let bodies = bodies.lock().expect("request lock").clone();
    assert_eq!(
        bodies[0].get("tools"),
        Some(&json!([{
            "type": "function",
            "function": {
                "name": "injected_tool",
                "description": "Injected after the initial conversion",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "query": { "type": "string" },
                    },
                },
            },
        }]))
    );
    server.shutdown();
}

#[tokio::test]
async fn never_sends_a_tool_whose_root_parameters_lack_type_object() {
    let bodies: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = bodies.clone();
    let server = RawServer::start(raw_handler(move |mut stream, request| {
        let sink = sink.clone();
        async move {
            sink.lock().expect("request lock").push(request_body(&request));
            let frames = vec![
                json!({
                    "id": "chatcmpl-root",
                    "object": "chat.completion.chunk",
                    "created": 0,
                    "model": "kimi-test",
                    "choices": [{ "index": 0, "delta": { "content": "ok" }, "finish_reason": null }],
                }),
                json!({
                    "id": "chatcmpl-root",
                    "object": "chat.completion.chunk",
                    "created": 0,
                    "model": "kimi-test",
                    "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
                }),
            ];
            let _ = write_sse(&mut stream, &frames, None).await;
        }
    }))
    .await;

    let monitor = tool(
        "monitor",
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string" },
                "command": { "type": "string" },
                "bash_id": { "type": "string" },
            },
            "anyOf": [{ "required": ["command"] }, { "required": ["bash_id"] }],
        }),
    );
    let options: OpenAiCompletionsOptions = options("test-key");
    let result = finish(&stream(
        &kimi_model(&server.base_url),
        &context(vec![user_message("hello")], Some(vec![monitor])),
        Some(options),
    ))
    .await;

    assert_eq!(result.stop_reason, maho_ai::types::StopReason::Stop);
    let bodies = bodies.lock().expect("request lock").clone();
    let tools = bodies[0].get("tools").and_then(Value::as_array).expect("tools array");
    assert_eq!(tools.len(), 1);
    for tool in tools {
        let parameters = tool.get("function").and_then(|function| function.get("parameters")).expect("parameters");
        assert_eq!(parameters.get("type").and_then(Value::as_str), Some("object"));
        let mut keys: Vec<&str> = parameters
            .get("properties")
            .and_then(Value::as_object)
            .map(|properties| properties.keys().map(String::as_str).collect())
            .unwrap_or_default();
        keys.sort_unstable();
        assert_eq!(keys, vec!["action", "bash_id", "command"]);
    }
    server.shutdown();
}
