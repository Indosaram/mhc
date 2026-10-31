//! Port of senpi packages/ai/test/devin-agent-producer-capture.test.ts. The stub Cascade edge
//! answers GetUserJwt with a unary proto and the chat with fixed Connect frames, as
//! devin-agent-stream-harness.ts's `serveEdge` does.

use std::net::SocketAddr;

use axum::Router;
use axum::body::Bytes;
use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use axum::routing::any;
use prost::Message as _;
use serde_json::{Value, json};

use crate::api::devin_agent::r#gen::cascade_pb::{
    ChatToolCall, GetChatMessageResponse, GetUserJwtResponse, StopReason,
};
use crate::api::devin_agent::wire::DEVIN_USER_JWT_PATH;
use crate::types::{
    Context, InputModality, Message, Model, ModelCost, StreamOptions, UserContent, UserMessage,
};
use crate::utils::assistant_capture::AssistantRecord;
use crate::utils::capture_test_records::{
    alias_partial, assert_matches_object, block_json, capture, capture_json, delta_captures,
    drain_records, first_event, progress_records, public_bytes, public_events, record_kinds,
    subscribe_capture_for_test, terminal_message, zero_timestamps,
};
use crate::utils::capture_test_server::minted_identity_keys;

/// `MODEL` from devin-agent-stream-harness.ts.
fn model(base_url: &str) -> Model {
    Model {
        deprecated: None,
        id: "swe-1-6".into(),
        name: "SWE-1.6".into(),
        api: "devin-agent".into(),
        provider: "devin".into(),
        base_url: base_url.into(),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        supports_assistant_prefill: None,
        input: vec![InputModality::Text],
        cost: ModelCost::default(),
        context_window: 200_000,
        max_tokens: 128_000,
        sampling_params: None,
        headers: None,
        cache_retention: None,
        upstream_model_id: None,
        service_tier: None,
        recover_text_tool_calls: None,
        compat: None,
        default_thinking_level: None,
        input_limits: None,
    }
}

/// `CONTEXT` from devin-agent-stream-harness.ts.
fn context() -> Context {
    Context {
        active_tool_names: None,
        system_prompt: Some("sys".into()),
        messages: vec![Message::User(UserMessage {
            content: UserContent::Text("hi".into()),
            timestamp: 0,
        })],
        tools: None,
    }
}

fn framed(flags: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(flags);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

fn frame(message: GetChatMessageResponse) -> Vec<u8> {
    framed(0x00, &message.encode_to_vec())
}

fn trailer() -> Vec<u8> {
    framed(0x02, b"{\"metadata\":{}}")
}

fn tool_call(id: &str, name: &str, arguments_json: &str) -> ChatToolCall {
    ChatToolCall {
        id: id.into(),
        name: name.into(),
        arguments_json: arguments_json.into(),
        ..Default::default()
    }
}

fn thinking(delta: &str, signature: &str) -> GetChatMessageResponse {
    GetChatMessageResponse {
        message_id: "m1".into(),
        delta_thinking: delta.into(),
        delta_signature: signature.into(),
        ..Default::default()
    }
}

fn chat_body() -> Vec<u8> {
    let mut body = Vec::new();
    for message in [
        thinking("plan", "s1"),
        thinking(" more", "s1"),
        thinking(" end", "s2"),
        GetChatMessageResponse {
            message_id: "m1".into(),
            delta_tool_calls: vec![tool_call("call-1", "", "{\"pa")],
            ..Default::default()
        },
        GetChatMessageResponse {
            message_id: "m1".into(),
            delta_tool_calls: vec![tool_call("", "read", "{\"path\":\"a.ts\"}")],
            ..Default::default()
        },
        GetChatMessageResponse {
            message_id: "m1".into(),
            stop_reason: StopReason::FunctionCall as i32,
            ..Default::default()
        },
    ] {
        body.extend(frame(message));
    }
    body.extend(trailer());
    body
}

async fn capture_records() -> Vec<AssistantRecord> {
    let router = Router::new().fallback(any(|uri: Uri| async move {
        if uri.path() == DEVIN_USER_JWT_PATH {
            let response = GetUserJwtResponse {
                user_jwt: "jwt-1".into(),
                custom_api_server_url: String::new(),
            };
            return (
                StatusCode::OK,
                [("content-type", "application/proto")],
                Bytes::from(response.encode_to_vec()),
            )
                .into_response();
        }
        (
            StatusCode::OK,
            [("content-type", "application/connect+proto")],
            Bytes::from(chat_body()),
        )
            .into_response()
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub edge");
    let addr: SocketAddr = listener.local_addr().expect("stub edge addr");
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router.into_make_service()).await;
    });
    let mut options = StreamOptions::default();
    options.request.api_key = Some("session-abc".into());
    let records = drain_records(&crate::api::devin_agent::stream(
        &model(&format!("http://{addr}")),
        &context(),
        Some(options),
    ))
    .await;
    server.abort();
    records
}

// Devin producer capture #4272

#[tokio::test]
async fn keeps_the_public_bytes_recorded_before_capture() {
    let _subscription = subscribe_capture_for_test();
    let mut events = public_events(&capture_records().await);
    // TS partials alias the producer's one output; Rust partials are push-time copies.
    let terminal = terminal_message(&events.last().expect("terminal").value).clone();
    for event in &mut events {
        alias_partial(&mut event.value, &terminal);
    }
    let mut snapshot: Value =
        serde_json::from_str(include_str!("capture_fixtures/devin_agent.json")).expect("snapshot");
    zero_timestamps(&mut snapshot);
    assert_eq!(public_bytes(&events), snapshot);
}

#[tokio::test]
async fn records_signature_replacement_and_a_late_tool_name_in_order() {
    let _subscription = subscribe_capture_for_test();
    let records = capture_records().await;
    assert_eq!(
        record_kinds(&records),
        [
            "start",
            "thinking_start:0",
            "message_block_metadata:0",
            "thinking_delta:0",
            "thinking_delta:0",
            "message_block_metadata:0",
            "thinking_delta:0",
            "thinking_end:0",
            "toolcall_start:1",
            "toolcall_delta:1",
            "message_block_metadata:1",
            "toolcall_delta:1",
            "toolcall_end:1",
            "done",
        ]
    );
    assert_eq!(
        progress_records(&records),
        [
            json!({ "type": "message_block_metadata", "contentIndex": 0, "thinkingSignature": "s1" }),
            json!({ "type": "message_block_metadata", "contentIndex": 0, "thinkingSignature": "s2" }),
            json!({ "type": "message_block_metadata", "contentIndex": 1, "name": "read" }),
        ]
    );
    let events = public_events(&records);
    assert_eq!(
        block_json(capture(first_event(&events, "thinking_start", Some(0)))),
        json!({ "type": "thinking", "thinking": "" })
    );
    assert_eq!(
        block_json(capture(first_event(&events, "thinking_end", Some(0)))),
        json!({ "type": "thinking", "thinking": "plan more end", "thinkingSignature": "s2" })
    );
}

#[tokio::test]
async fn captures_the_closure_held_argument_text_as_suffix_deltas_with_the_stores_parse_frontier() {
    let _subscription = subscribe_capture_for_test();
    let events = public_events(&capture_records().await);
    assert_matches_object(
        &capture_json(capture(first_event(&events, "toolcall_start", Some(1)))),
        &json!({
            "block": { "type": "toolCall", "id": "call-1", "name": "", "arguments": {} },
            "rawArgumentPrefix": "",
            "parsedThrough": 0,
            "coveredCodeUnits": 0,
        }),
    );
    assert_eq!(
        delta_captures(&events, "toolcall_delta"),
        vec![
            ("{\"pa".to_owned(), Some(0), Some(4)),
            ("th\":\"a.ts\"}".to_owned(), Some(4), Some(15)),
        ]
    );
    assert_matches_object(
        &block_json(capture(first_event(&events, "toolcall_end", Some(1)))),
        &json!({ "name": "read", "arguments": { "path": "a.ts" } }),
    );
    let done = capture(first_event(&events, "done", None))
        .and_then(|capture| capture.message.as_deref())
        .expect("done capture message");
    assert_matches_object(
        &serde_json::to_value(done).expect("done json"),
        &json!({ "stopReason": "toolUse", "responseId": "m1" }),
    );
    assert!(minted_identity_keys(Some(done)).is_empty());
}
