use maho_ai::types::{AssistantMessageEvent, ContentBlock, SimpleStreamOptions, StopReason};
use serde_json::json;

use super::harness::*;

fn lifecycle_model(base_url: &str) -> maho_ai::types::Model {
    model(&[
        ("id", json!("gpt-4o-mini")),
        ("name", json!("GPT-4o mini")),
        ("provider", json!("test")),
        ("baseUrl", json!(base_url)),
    ])
}

fn lifecycle_context() -> maho_ai::types::Context {
    context(vec![user_message("Think for a while")], None)
}

fn event_type(event: &AssistantMessageEvent) -> &'static str {
    match event {
        AssistantMessageEvent::Start { .. } => "start",
        AssistantMessageEvent::TextStart { .. } => "text_start",
        AssistantMessageEvent::TextDelta { .. } => "text_delta",
        AssistantMessageEvent::TextEnd { .. } => "text_end",
        AssistantMessageEvent::ThinkingStart { .. } => "thinking_start",
        AssistantMessageEvent::ThinkingDelta { .. } => "thinking_delta",
        AssistantMessageEvent::ThinkingEnd { .. } => "thinking_end",
        AssistantMessageEvent::ToolcallStart { .. } => "toolcall_start",
        AssistantMessageEvent::ToolcallDelta { .. } => "toolcall_delta",
        AssistantMessageEvent::ToolcallEnd { .. } => "toolcall_end",
        AssistantMessageEvent::Done { .. } => "done",
        AssistantMessageEvent::Error { .. } => "error",
    }
}

#[tokio::test]
async fn keeps_an_actively_streaming_response_alive_beyond_the_request_establishment_timeout() {
    let server = RawServer::start(raw_handler(|mut stream, _request| async move {
        use tokio::io::AsyncWriteExt;
        let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: keep-alive\r\ntransfer-encoding: chunked\r\n\r\n";
        let _ = stream.write_all(head.as_bytes()).await;
        let _ = stream.flush().await;
        for part in 1..=12u32 {
            let frame = json!({
                "id": "chatcmpl-active-stream",
                "choices": [{ "index": 0, "delta": { "reasoning_content": part.to_string() }, "finish_reason": null }],
            });
            let body = format!("data: {frame}\n\n");
            let _ = stream.write_all(format!("{:x}\r\n{body}\r\n", body.len()).as_bytes()).await;
            let _ = stream.flush().await;
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        }
        let finish = json!({
            "id": "chatcmpl-active-stream",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        });
        let body = format!("data: {finish}\n\ndata: [DONE]\n\n");
        let _ = stream.write_all(format!("{:x}\r\n{body}\r\n0\r\n\r\n", body.len()).as_bytes()).await;
        let _ = stream.flush().await;
    }))
    .await;

    let mut options: SimpleStreamOptions = simple_options("test");
    options.stream.request.max_retries = Some(0);
    options.stream.request.timeout_ms = Some(250);
    let response = finish(&run_simple(&lifecycle_model(&server.base_url), &lifecycle_context(), options, None)).await;

    assert_eq!(response.stop_reason, StopReason::Stop);
    assert_eq!(response.error_message, None);
    assert_eq!(response.content.len(), 1);
    match &response.content[0] {
        ContentBlock::Thinking(thinking) => {
            assert_eq!(thinking.thinking, "123456789101112");
            assert_eq!(thinking.thinking_signature.as_deref(), Some("reasoning_content"));
        }
        other => panic!("expected a thinking block, got {other:?}"),
    }
    server.shutdown();
}

#[tokio::test]
async fn still_times_out_when_response_headers_never_arrive() {
    let server = RawServer::start(raw_handler(|mut stream, _request| async move {
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        let _ = write_sse(&mut stream, &[], None).await;
    }))
    .await;

    let mut options: SimpleStreamOptions = simple_options("test");
    options.stream.request.timeout_ms = Some(40);
    let response = finish(&run_simple(
        &lifecycle_model(&server.base_url),
        &lifecycle_context(),
        options,
        None,
    ))
    .await;

    assert_eq!(response.stop_reason, StopReason::Error);
    let message = response.error_message.unwrap_or_default().to_lowercase();
    assert!(message.contains("timed out") || message.contains("aborted"), "{message}");
    server.shutdown();
}

#[tokio::test]
async fn rejects_a_real_sse_transport_eof_without_finish_reason() {
    let server = RawServer::start(raw_handler(|mut stream, _request| async move {
        use tokio::io::AsyncWriteExt;
        let frame = json!({
            "id": "chatcmpl-truncated",
            "choices": [{ "index": 0, "delta": { "reasoning_content": "partial" }, "finish_reason": null }],
        });
        let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
        let _ = stream.write_all(head.as_bytes()).await;
        let _ = stream.write_all(format!("data: {frame}\n\n").as_bytes()).await;
        let _ = stream.flush().await;
        let _ = stream.shutdown().await;
    }))
    .await;

    let mut options: SimpleStreamOptions = simple_options("test");
    options.stream.request.max_retries = Some(0);
    let response = finish(&run_simple(
        &lifecycle_model(&server.base_url),
        &lifecycle_context(),
        options,
        None,
    ))
    .await;

    assert_eq!(response.stop_reason, StopReason::Error);
    assert_eq!(response.error_message.as_deref(), Some("Stream ended without finish_reason"));
    assert_eq!(response.content.len(), 1);
    match &response.content[0] {
        ContentBlock::Thinking(thinking) => {
            assert_eq!(thinking.thinking, "partial");
            assert_eq!(thinking.thinking_signature.as_deref(), Some("reasoning_content"));
        }
        other => panic!("expected a thinking block, got {other:?}"),
    }
    server.shutdown();
}

#[tokio::test]
async fn ends_each_content_block_before_starting_the_next_block_type() {
    let server = RawServer::start(raw_handler(|mut stream, _request| async move {
        let frames = vec![
            json!({ "id": "chatcmpl-content-order", "choices": [{ "index": 0, "delta": { "reasoning_content": "thought" }, "finish_reason": null }] }),
            json!({ "id": "chatcmpl-content-order", "choices": [{ "index": 0, "delta": { "content": "answer" }, "finish_reason": null }] }),
            json!({
                "id": "chatcmpl-content-order",
                "choices": [{
                    "index": 0,
                    "delta": { "tool_calls": [{ "index": 0, "id": "call-1", "type": "function", "function": { "name": "echo", "arguments": "{\"text\":\"ok\"}" } }] },
                    "finish_reason": null,
                }],
            }),
            json!({ "id": "chatcmpl-content-order", "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }] }),
        ];
        let _ = write_sse(&mut stream, &frames, None).await;
    }))
    .await;

    let echo = tool("echo", json!({ "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] }));
    let mut options: SimpleStreamOptions = simple_options("test");
    options.stream.request.max_retries = Some(0);
    let response = run_simple(
        &lifecycle_model(&server.base_url),
        &context(vec![user_message("Think, answer, then call echo")], Some(vec![echo])),
        options,
        None,
    );
    let event_types: Vec<&str> = events(&response).await.iter().map(event_type).collect();

    assert_eq!(
        event_types,
        vec![
            "start",
            "thinking_start",
            "thinking_delta",
            "thinking_end",
            "text_start",
            "text_delta",
            "text_end",
            "toolcall_start",
            "toolcall_delta",
            "toolcall_end",
            "done",
        ]
    );
    server.shutdown();
}
