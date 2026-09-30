mod support;

use std::io::{Read, Write};
use std::net::TcpListener;

use maho_agent::proxy::{ProxySerializableStreamOptions, ProxyStreamOptions, stream_proxy};
use maho_ai::types::{ContentBlock, Context, StopReason};
use support::test_model;

fn serve_sse(body: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept");
        let mut buffer = [0_u8; 4096];
        let _ = socket.read(&mut buffer);
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

fn proxy_options(base_url: String) -> ProxyStreamOptions {
    ProxyStreamOptions {
        serializable: ProxySerializableStreamOptions::default(),
        signal: None,
        auth_token: "test-token".to_owned(),
        proxy_url: base_url,
    }
}

fn context() -> Context {
    Context { system_prompt: Some(String::new()), messages: Vec::new(), tools: None }
}

async fn collect_events(
    stream: &maho_ai::utils::event_stream::AssistantMessageEventStream,
) -> Vec<maho_ai::types::AssistantMessageEvent> {
    tokio::time::timeout(std::time::Duration::from_secs(10), stream.collect())
        .await
        .expect("proxy stream terminated")
        .expect("events")
}

#[tokio::test]
async fn preserves_tool_call_metadata_received_only_on_toolcall_end() {
    let base_url = serve_sse(sse(&[
        serde_json::json!({ "type": "start" }),
        serde_json::json!({ "type": "toolcall_start", "contentIndex": 0, "id": "call_test|fc_test", "toolName": "lookup" }),
        serde_json::json!({ "type": "toolcall_delta", "contentIndex": 0, "delta": "{\"value\":\"hello\"}" }),
        serde_json::json!({
            "type": "toolcall_end",
            "contentIndex": 0,
            "toolCall": {
                "type": "toolCall",
                "id": "call_test|fc_test",
                "name": "lookup",
                "arguments": { "value": "hello" },
                "namespace": "dynamic_tools",
            },
        }),
        serde_json::json!({ "type": "done", "reason": "toolUse", "usage": usage_json() }),
    ]));

    let stream = stream_proxy(&test_model(), &context(), proxy_options(base_url));
    let events = collect_events(&stream).await;
    let result = stream.result().await.expect("result");

    let end_event = events
        .iter()
        .find(|event| matches!(event, maho_ai::types::AssistantMessageEvent::ToolcallEnd { .. }))
        .expect("toolcall_end event");
    match end_event {
        maho_ai::types::AssistantMessageEvent::ToolcallEnd { tool_call, .. } => {
            assert_eq!(tool_call.namespace.as_deref(), Some("dynamic_tools"));
        }
        other => panic!("expected toolcall_end, got {:?}", other),
    }
    match &result.content[0] {
        ContentBlock::ToolCall(call) => {
            assert_eq!(call.arguments.get("value"), Some(&serde_json::Value::String("hello".to_owned())));
            assert_eq!(call.namespace.as_deref(), Some("dynamic_tools"));
        }
        other => panic!("expected tool call, got {}", other.type_name()),
    }
}

#[tokio::test]
async fn processes_terminal_metadata_when_the_event_is_not_newline_terminated() {
    let start = format!("data: {}\n\n", serde_json::json!({ "type": "start" }));
    let done = format!(
        "data: {}",
        serde_json::json!({ "type": "done", "reason": "stop", "usage": usage_json(), "providerThinkingLevel": "high" })
    );
    let base_url = serve_sse(format!("{start}{done}"));

    let stream = stream_proxy(&test_model(), &context(), proxy_options(base_url));
    let events = collect_events(&stream).await;
    let result = stream.result().await.expect("result");

    let names: Vec<&'static str> = events.iter().map(event_name).collect();
    assert_eq!(names, vec!["start", "done"]);
    assert_eq!(result.stop_reason, StopReason::Stop);
    assert_eq!(result.provider_thinking_level.as_deref(), Some("high"));
}

#[tokio::test]
async fn emits_an_error_instead_of_hanging_when_the_stream_ends_without_a_terminal_event() {
    let base_url = serve_sse(format!("data: {}\n\n", serde_json::json!({ "type": "start" })));

    let stream = stream_proxy(&test_model(), &context(), proxy_options(base_url));
    let events = collect_events(&stream).await;
    let result = stream.result().await.expect("result");

    let names: Vec<&'static str> = events.iter().map(event_name).collect();
    assert_eq!(names, vec!["start", "error"]);
    assert_eq!(result.stop_reason, StopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("Connection closed by proxy server")
    );
}

fn event_name(event: &maho_ai::types::AssistantMessageEvent) -> &'static str {
    match event {
        maho_ai::types::AssistantMessageEvent::Start { .. } => "start",
        maho_ai::types::AssistantMessageEvent::TextStart { .. } => "text_start",
        maho_ai::types::AssistantMessageEvent::TextDelta { .. } => "text_delta",
        maho_ai::types::AssistantMessageEvent::TextEnd { .. } => "text_end",
        maho_ai::types::AssistantMessageEvent::ThinkingStart { .. } => "thinking_start",
        maho_ai::types::AssistantMessageEvent::ThinkingDelta { .. } => "thinking_delta",
        maho_ai::types::AssistantMessageEvent::ThinkingEnd { .. } => "thinking_end",
        maho_ai::types::AssistantMessageEvent::ToolcallStart { .. } => "toolcall_start",
        maho_ai::types::AssistantMessageEvent::ToolcallDelta { .. } => "toolcall_delta",
        maho_ai::types::AssistantMessageEvent::ToolcallEnd { .. } => "toolcall_end",
        maho_ai::types::AssistantMessageEvent::Done { .. } => "done",
        maho_ai::types::AssistantMessageEvent::Error { .. } => "error",
    }
}
