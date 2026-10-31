//! Port of senpi packages/ai/test/producer-capture-native.test.ts.

use std::net::SocketAddr;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::Response;
use axum::routing::any;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use crate::types::{AssistantMessage, AssistantMessageEvent, StreamOptions};
use crate::utils::assistant_capture::{
    AssistantProgress, AssistantRecord, BlockPhase, CaptureHint, assistant_records,
};
use crate::utils::capture_test_records::{
    DEADLINE, assert_matches_object, block_json, capture, collect_records, event_type,
    progress_records, subscribe_capture_for_test,
};
use crate::utils::capture_test_server::fixture_context;
use crate::utils::event_stream::AssistantMessageEventStream;

fn message() -> AssistantMessage {
    serde_json::from_value(json!({
        "role": "assistant", "api": "openai-responses", "provider": "openai", "model": "gpt-5",
        "timestamp": 1, "content": [], "stopReason": "pending",
        "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } }
    }))
    .expect("message")
}

/// A server whose one SSE response stays open while the test writes frames (TS `TransformStream`).
async fn held_open_wire() -> (
    String,
    mpsc::UnboundedSender<Value>,
    tokio::task::JoinHandle<()>,
) {
    let (sender, receiver) = mpsc::unbounded_channel::<Value>();
    let receiver = std::sync::Arc::new(tokio::sync::Mutex::new(Some(receiver)));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind wire");
    let addr: SocketAddr = listener.local_addr().expect("wire addr");
    let router = Router::new().fallback(any(move || {
        let receiver = receiver.clone();
        async move {
            let receiver = receiver.lock().await.take().expect("one request");
            let frames = futures::stream::unfold(receiver, |mut receiver| async move {
                let frame = receiver.recv().await?;
                let text = format!(
                    "event: {}\ndata: {frame}\n\n",
                    frame["type"].as_str().unwrap_or_default()
                );
                Some((
                    Ok::<_, std::convert::Infallible>(Bytes::from(text)),
                    receiver,
                ))
            });
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(Body::from_stream(frames))
                .expect("wire response")
        }
    }));
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router.into_make_service()).await;
    });
    (format!("http://{addr}"), sender, handle)
}

// native producer progress #4272

#[tokio::test]
async fn observes_anthropic_native_insert_end_and_signature_only_changes_while_the_wire_is_held_open()
 {
    let _subscription = subscribe_capture_for_test();
    let (origin, send, server) = held_open_wire().await;
    let mut model = crate::models_generated::MODELS
        .get("anthropic")
        .and_then(|models| {
            models.values().find(|model| {
                let cost = model.cost.rates();
                model.id.split('-').count() == 4
                    && cost.input == 1.0
                    && cost.output == 5.0
                    && cost.cache_read == 0.1
            })
        })
        .expect("claude-haiku-4-5")
        .clone();
    model.base_url = origin;
    let mut options = StreamOptions::default();
    options.request.api_key = Some("fixture".into());
    options.request.max_retries = Some(0);
    let stream = crate::api::anthropic_messages::stream(&model, &fixture_context(), Some(options));
    let reader = assistant_records(&stream);
    for frame in [
        json!({ "type": "message_start", "message": { "id": "msg_fixture", "usage": { "input_tokens": 1, "output_tokens": 0 } } }),
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "redacted_thinking", "data": "opaque" } }),
        json!({ "type": "content_block_stop", "index": 0 }),
        json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "thinking", "thinking": "", "signature": "" } }),
        json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "signature_delta", "signature": "signed" } }),
        json!({ "type": "content_block_stop", "index": 1 }),
        json!({ "type": "content_block_start", "index": 2, "content_block": { "type": "server_tool_use", "id": "native", "name": "search", "input": {} } }),
        json!({ "type": "content_block_delta", "index": 2, "delta": { "type": "input_json_delta", "partial_json": "{ \"q\": \"x\" }" } }),
        json!({ "type": "content_block_stop", "index": 2 }),
    ] {
        send.send(frame).expect("send frame");
    }
    // Read until the native end record arrives, with the wire still open.
    let mut records: Vec<AssistantRecord> = Vec::new();
    loop {
        let record = tokio::time::timeout(DEADLINE, reader.next())
            .await
            .expect("native end deadline")
            .expect("record")
            .expect("record before end");
        let end = matches!(&record, AssistantRecord::Progress(progress)
            if matches!(progress.as_ref(), AssistantProgress::MessageBlockState { phase: BlockPhase::End, .. }));
        records.push(record);
        if end {
            break;
        }
    }
    let controls = progress_records(&records);
    assert_eq!(controls.len(), 3, "{controls:#?}");
    assert_matches_object(
        &Value::Array(controls),
        &json!([
            { "type": "message_block_metadata", "contentIndex": 1, "thinkingSignatureDelta": "signed" },
            { "type": "message_block_state", "phase": "insert", "contentIndex": 2, "block": { "raw": { "input": {} } } },
            { "type": "message_block_state", "phase": "end", "contentIndex": 2, "block": { "raw": { "input": { "q": "x" } } } },
        ]),
    );
    let thinking_start = records
        .iter()
        .find_map(|record| match record {
            AssistantRecord::Event(event) if event_type(&event.value) == "thinking_start" => {
                Some(event)
            }
            _ => None,
        })
        .expect("no thinking_start record");
    assert_eq!(
        block_json(capture(thinking_start)),
        json!({
            "type": "thinking", "thinking": "[Reasoning redacted]",
            "thinkingSignature": "opaque", "redacted": true
        })
    );
    assert!(!records.iter().any(|record| matches!(record,
        AssistantRecord::Event(event) if event_type(&event.value) == "done")));
    send.send(json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 1 } }))
        .expect("send");
    send.send(json!({ "type": "message_stop" })).expect("send");
    drop(send);
    tokio::time::timeout(DEADLINE, reader.collect())
        .await
        .expect("consume deadline")
        .expect("consume");
    server.abort();
}

#[tokio::test]
async fn keeps_responses_native_replacement_and_terminal_image_backfill_in_original_index_order() {
    let _subscription = subscribe_capture_for_test();
    let stream = AssistantMessageEventStream::assistant();
    let mut output = message();
    let frames = [
        json!({ "type": "response.output_item.added", "output_index": 0, "item": { "type": "web_search_call", "id": "search", "status": "in_progress" } }),
        json!({ "type": "response.output_item.done", "output_index": 0, "item": { "type": "web_search_call", "id": "search", "status": "completed" } }),
        json!({ "type": "response.completed", "response": { "id": "resp", "status": "completed", "output": [
            { "type": "web_search_call", "id": "search", "status": "completed" },
            { "type": "image_generation_call", "id": "image", "status": "completed", "result": "YWJj" },
        ] } }),
    ];
    super::producer_capture_tests::process_frames(&frames, &mut output, &stream).await;
    stream.end(Some(output));
    let records = collect_records(&stream).await;
    assert_eq!(records.len(), 4, "{:#?}", progress_records(&records));
    assert_matches_object(
        &Value::Array(progress_records(&records)),
        &json!([
            { "type": "message_block_state", "contentIndex": 0, "phase": "insert", "block": { "raw": { "status": "in_progress" } } },
            { "type": "message_block_state", "contentIndex": 0, "phase": "end", "block": { "raw": { "status": "completed" } } },
            { "type": "message_block_state", "contentIndex": 1, "phase": "insert", "block": { "raw": { "result": "YWJj" } } },
            { "type": "message_block_state", "contentIndex": 1, "phase": "end", "block": { "raw": { "result": "YWJj" } } },
        ]),
    );
}

#[tokio::test]
async fn does_no_parsing_or_content_reads_on_no_growth_deltas_and_retains_a_stale_parse_frontier() {
    let _subscription = subscribe_capture_for_test();
    let stream = AssistantMessageEventStream::with_capture_blocks(true);
    let mut partial = message();
    partial.content.push(
        serde_json::from_value(json!({ "type": "toolCall", "id": "call", "name": "echo", "arguments": { "seed": { "value": 1 } } }))
            .expect("call"),
    );
    stream.push(AssistantMessageEvent::ToolcallStart {
        content_index: 0,
        partial: partial.clone(),
    });
    // TS makes `partial.content` throw on read; here the deltas carry no content at all, so a
    // delta capture that read a block would find none and record nothing.
    let mut hollow = partial.clone();
    hollow.content.clear();
    for _ in 0..100 {
        stream.push(AssistantMessageEvent::ToolcallDelta {
            content_index: 0,
            delta: String::new(),
            partial: hollow.clone(),
        });
    }
    stream.push_with_hint(
        AssistantMessageEvent::ToolcallDelta {
            content_index: 0,
            delta: "{\"changed\":true}".into(),
            partial: hollow,
        },
        CaptureHint::parsed_through(0),
    );
    stream.end(Some(partial));
    let records = collect_records(&stream).await;
    let parsed_through: Vec<Option<usize>> = records[1..]
        .iter()
        .map(|record| match record {
            AssistantRecord::Event(event) => {
                capture(event).and_then(|capture| capture.parsed_through)
            }
            AssistantRecord::Progress(_) => None,
        })
        .collect();
    assert_eq!(parsed_through, vec![Some(0); 101]);
    let AssistantRecord::Event(start) = &records[0] else {
        panic!("start record");
    };
    assert_matches_object(
        &block_json(capture(start)),
        &json!({ "arguments": { "seed": { "value": 1 } } }),
    );
}
