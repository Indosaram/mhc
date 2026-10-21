mod support;

use std::io::{Read, Write};
use std::net::TcpListener;

use maho_agent::proxy::{ProxySerializableStreamOptions, ProxyStreamOptions, stream_proxy};
use maho_ai::types::{AssistantMessage, ContentBlock, Context};
use support::test_model;

fn serve_sse(body: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept");
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).expect("request headers");
            request.push(byte[0]);
        }
        let request = String::from_utf8(request).expect("request headers UTF-8");
        let length = request.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().expect("content length"))
        }).expect("request body length");
        let mut request_body = vec![0; length];
        socket.read_exact(&mut request_body).expect("complete request body");
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = socket.write_all(response.as_bytes());
        let _ = socket.flush();
    });
    format!("http://{address}")
}

fn sse(events: &[serde_json::Value]) -> String {
    events
        .iter()
        .map(|event| format!("data: {}\n\n", serde_json::to_string(event).expect("json")))
        .collect()
}

fn usage_json() -> serde_json::Value {
    serde_json::json!({
        "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 },
    })
}

#[tokio::test]
async fn structured_usage_limit_is_retained_by_proxy() {
    let message = stream_events(vec![serde_json::json!({
        "type": "error", "reason": "error", "errorMessage": "ordinary failure",
        "errorCause": { "type": "usage_limit", "scope": "account" }, "usage": usage_json()
    })]).await;
    assert_eq!(message.error_cause, Some(maho_ai::types::AssistantErrorCause::UsageLimit { scope: maho_ai::types::UsageLimitScope::Account }));
}

async fn stream_events(events: Vec<serde_json::Value>) -> AssistantMessage {
    let base_url = serve_sse(sse(&events));
    let stream = stream_proxy(
        &test_model(),
        &Context {
            active_tool_names: None,
            system_prompt: None,
            messages: Vec::new(),
            tools: None,
        },
        ProxyStreamOptions {
            serializable: ProxySerializableStreamOptions::default(),
            signal: None,
            auth_token: "test-token".to_owned(),
            proxy_url: base_url,
        },
    );
    tokio::time::timeout(std::time::Duration::from_secs(10), stream.result())
        .await
        .expect("proxy stream terminated")
        .expect("result")
}

#[tokio::test]
async fn preserves_a_flagged_final_tool_call_payload() {
    let message = stream_events(vec![
        serde_json::json!({ "type": "start" }),
        serde_json::json!({ "type": "toolcall_start", "contentIndex": 0, "id": "started-id", "toolName": "get_weather" }),
        serde_json::json!({
            "type": "toolcall_end",
            "contentIndex": 0,
            "toolCall": {
                "type": "toolCall",
                "id": "final-id",
                "name": "get_weather",
                "arguments": {},
                "incomplete": true,
                "errorMessage": "Tool call was truncated mid-arguments",
            },
        }),
        serde_json::json!({ "type": "done", "reason": "toolUse", "usage": usage_json() }),
    ])
    .await;

    assert_eq!(message.content.len(), 1);
    match &message.content[0] {
        ContentBlock::ToolCall(call) => {
            assert_eq!(call.id, "final-id");
            assert_eq!(call.name, "get_weather");
            assert!(call.arguments.is_empty());
            assert_eq!(call.incomplete, Some(true));
            assert_eq!(
                call.error_message.as_deref(),
                Some("Tool call was truncated mid-arguments")
            );
        }
        other => panic!("expected tool call, got {}", other.type_name()),
    }
}

#[tokio::test]
async fn legacy_payloads_reconstruct_arguments_from_deltas() {
    let message = stream_events(vec![
        serde_json::json!({ "type": "start" }),
        serde_json::json!({ "type": "toolcall_start", "contentIndex": 0, "id": "legacy-id", "toolName": "get_weather" }),
        serde_json::json!({ "type": "toolcall_delta", "contentIndex": 0, "delta": "{\"city\":\"Seoul\"}" }),
        serde_json::json!({ "type": "toolcall_end", "contentIndex": 0 }),
        serde_json::json!({ "type": "done", "reason": "toolUse", "usage": usage_json() }),
    ])
    .await;

    assert_eq!(message.content.len(), 1);
    match &message.content[0] {
        ContentBlock::ToolCall(call) => {
            assert_eq!(call.id, "legacy-id");
            assert_eq!(call.name, "get_weather");
            assert_eq!(
                call.arguments.get("city"),
                Some(&serde_json::Value::String("Seoul".to_owned()))
            );
        }
        other => panic!("expected tool call, got {}", other.type_name()),
    }
}

#[tokio::test]
async fn legacy_payloads_without_deltas_degrade_to_empty_arguments_without_an_incomplete_flag() {
    let message = stream_events(vec![
        serde_json::json!({ "type": "start" }),
        serde_json::json!({ "type": "toolcall_start", "contentIndex": 0, "id": "legacy-id", "toolName": "get_weather" }),
        serde_json::json!({ "type": "toolcall_end", "contentIndex": 0 }),
        serde_json::json!({ "type": "done", "reason": "toolUse", "usage": usage_json() }),
    ])
    .await;

    assert_eq!(message.content.len(), 1);
    match &message.content[0] {
        ContentBlock::ToolCall(call) => {
            assert_eq!(call.id, "legacy-id");
            assert_eq!(call.name, "get_weather");
            assert!(call.arguments.is_empty());
            assert_eq!(call.incomplete, None);
        }
        other => panic!("expected tool call, got {}", other.type_name()),
    }
}
