//! Port of senpi packages/ai/test/cursor-agent-producer-capture.test.ts, plus a direct unit test of
//! `apply_streamed_args_delta`'s capture hint. The TS suite stands up an `http2` server answering
//! with fixed Connect frames; here an `h2` server does the same for the real `stream()`.

use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::Bytes;
use prost::Message as _;
use serde_json::{Value, json};
use tokio::net::TcpListener;

use super::r#gen::agent_pb::{
    AgentServerMessage, InteractionUpdate, McpArgs, McpToolCall, PartialToolCallUpdate,
    ThinkingDeltaUpdate, ToolCall as PbToolCall, ToolCallCompletedUpdate, ToolCallStartedUpdate,
    TurnEndedUpdate, agent_server_message, interaction_update, tool_call,
};
use super::types::CursorAgentOptions;
use super::{frame_connect_message, stream_with_options};
use crate::types::{
    Context, InputModality, Message, Model, ModelCost, StreamOptions, UserContent, UserMessage,
};
use crate::utils::assistant_capture::AssistantRecord;
use crate::utils::capture_test_records::{
    alias_partial, assert_matches_object, block_json, capture, capture_json, delta_captures,
    drain_records, first_event, public_bytes, public_events, record_kinds,
    subscribe_capture_for_test, terminal_message, zero_timestamps,
};
use crate::utils::capture_test_server::minted_identity_keys;

fn mcp_call() -> PbToolCall {
    PbToolCall {
        tool: Some(tool_call::Tool::McpToolCall(McpToolCall {
            args: Some(McpArgs {
                name: "echo".into(),
                tool_name: "echo".into(),
                tool_call_id: "call_1".into(),
                ..McpArgs::default()
            }),
            ..McpToolCall::default()
        })),
        tool_call_id: None,
    }
}

/// Cursor sends cumulative argument snapshots and reparses only after 1024 new code units.
fn snapshots() -> [String; 3] {
    let first = format!("{{\"text\":\"{}", "x".repeat(600));
    let second = format!("{first}{}", "y".repeat(600));
    let third = format!("{second}\"}}");
    [first, second, third]
}

fn interaction(message: interaction_update::Message) -> Vec<u8> {
    let value = AgentServerMessage {
        message: Some(agent_server_message::Message::InteractionUpdate(
            InteractionUpdate {
                message: Some(message),
            },
        )),
    };
    frame_connect_message(&value.encode_to_vec(), 0)
}

/// One HTTP/2 exchange answering `application/connect+proto` with `frames`, then ending.
async fn serve(frames: Vec<Vec<u8>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let Ok((socket, _)) = listener.accept().await else {
            return;
        };
        let Ok(mut connection) = h2::server::handshake(socket).await else {
            return;
        };
        let Some(Ok((_request, mut respond))) = connection.accept().await else {
            return;
        };
        let response = http::Response::builder()
            .status(200)
            .header("content-type", "application/connect+proto")
            .body(())
            .expect("response");
        let mut send = respond.send_response(response, false).expect("respond");
        for frame in frames {
            let _ = send.send_data(Bytes::from(frame), false);
        }
        let _ = send.send_data(Bytes::new(), true);
        // The connection future drives the socket IO; keep polling until the client closes.
        while connection.accept().await.is_some() {}
    });
    format!("http://{address}")
}

static TURN: AtomicUsize = AtomicUsize::new(0);

async fn capture_records() -> Vec<AssistantRecord> {
    let mut frames = vec![
        interaction(interaction_update::Message::ThinkingDelta(
            ThinkingDeltaUpdate { text: "hm".into() },
        )),
        interaction(interaction_update::Message::ToolCallStarted(
            ToolCallStartedUpdate {
                call_id: "env-1".into(),
                tool_call: Some(mcp_call()),
                ..ToolCallStartedUpdate::default()
            },
        )),
    ];
    for args_text_delta in snapshots() {
        frames.push(interaction(interaction_update::Message::PartialToolCall(
            PartialToolCallUpdate {
                call_id: "env-1".into(),
                args_text_delta,
                ..PartialToolCallUpdate::default()
            },
        )));
    }
    frames.push(interaction(interaction_update::Message::ToolCallCompleted(
        ToolCallCompletedUpdate {
            call_id: "env-1".into(),
            tool_call: Some(mcp_call()),
            ..ToolCallCompletedUpdate::default()
        },
    )));
    frames.push(interaction(interaction_update::Message::TurnEnded(
        TurnEndedUpdate::default(),
    )));
    let base_url = serve(frames).await;
    let model = Model {
        deprecated: None,
        id: "composer-2".into(),
        name: "fixture".into(),
        api: "cursor-agent".into(),
        provider: "cursor".into(),
        base_url,
        reasoning: true,
        default_thinking_level: None,
        thinking_level_map: None,
        prompt_cache: None,
        supports_assistant_prefill: None,
        input: vec![InputModality::Text],
        cost: ModelCost::default(),
        context_window: 200_000,
        max_tokens: 64_000,
        sampling_params: None,
        headers: None,
        cache_retention: None,
        upstream_model_id: None,
        service_tier: None,
        recover_text_tool_calls: None,
        compat: None,
        input_limits: None,
    };
    let context = Context {
        active_tool_names: None,
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserContent::Text("fixture".into()),
            timestamp: 1,
        })],
        tools: None,
    };
    let turn = TURN.fetch_add(1, Ordering::Relaxed) + 1;
    let mut base = StreamOptions::default();
    base.request.api_key = Some("test-token".into());
    let options = CursorAgentOptions {
        base,
        conversation_id: Some(format!("producer-capture-{turn}")),
        ..CursorAgentOptions::default()
    };
    drain_records(&stream_with_options(&model, &context, Some(options))).await
}

// Cursor producer capture #4272

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
        serde_json::from_str(include_str!("../capture_fixtures/cursor_agent.json"))
            .expect("snapshot");
    zero_timestamps(&mut snapshot);
    assert_eq!(public_bytes(&events), snapshot);
}

#[tokio::test]
async fn keeps_the_parse_frontier_at_cursors_throttled_reparse_so_the_stale_preview_survives() {
    let _subscription = subscribe_capture_for_test();
    let records = capture_records().await;
    assert_eq!(
        record_kinds(&records),
        [
            "start",
            "thinking_start:0",
            "thinking_delta:0",
            "thinking_end:0",
            "toolcall_start:1",
            "toolcall_delta:1",
            "toolcall_delta:1",
            "toolcall_delta:1",
            "toolcall_end:1",
            "done",
        ]
    );
    let events = public_events(&records);
    assert_matches_object(
        &capture_json(capture(first_event(&events, "toolcall_start", Some(1)))),
        &json!({
            "block": { "type": "toolCall", "id": "call_1", "name": "echo", "arguments": {} },
            "rawArgumentPrefix": "",
            "parsedThrough": 0,
            "coveredCodeUnits": 0,
        }),
    );
    let deltas = delta_captures(&events, "toolcall_delta");
    assert_eq!(
        deltas
            .iter()
            .map(|(_, offset, parsed_through)| (*offset, *parsed_through))
            .collect::<Vec<_>>(),
        [
            (Some(0), Some(0)),
            (Some(609), Some(1209)),
            (Some(1209), Some(1209)),
        ]
    );
    let [_, second, third] = snapshots();
    assert_eq!(
        deltas
            .iter()
            .map(|(delta, _, _)| delta.as_str())
            .collect::<String>(),
        third
    );
    assert_matches_object(
        &block_json(capture(first_event(&events, "toolcall_end", Some(1)))),
        &json!({ "arguments": { "text": &second[9..] } }),
    );
    let done = capture(first_event(&events, "done", None))
        .and_then(|capture| capture.message.as_deref())
        .expect("done capture message");
    assert_matches_object(
        &serde_json::to_value(done).expect("done json"),
        &json!({ "stopReason": "stop" }),
    );
    assert!(minted_identity_keys(Some(done)).is_empty());
}

/// `apply_streamed_args_delta` alone: each cumulative snapshot is pushed as its new suffix, and the
/// capture's `parsedThrough` is the UTF-16 length last parsed into `arguments` (0 until the
/// throttled reparse fires, then frozen until the next one).
#[tokio::test]
async fn apply_streamed_args_delta_hints_the_throttled_parse_frontier() {
    let _subscription = subscribe_capture_for_test();
    let stream = crate::utils::event_stream::AssistantMessageEventStream::with_capture_blocks(true);
    let mut output: crate::types::AssistantMessage = serde_json::from_value(json!({
        "role": "assistant", "api": "cursor-agent", "provider": "cursor", "model": "composer-2",
        "content": [{ "type": "toolCall", "id": "call_1", "name": "echo", "arguments": {} }],
        "stopReason": "stop", "timestamp": 1,
        "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } }
    }))
    .expect("output");
    let mut state = super::BlockState {
        current_text_block: None,
        current_text_index: None,
        current_thinking_block: None,
        current_thinking_index: None,
        current_tool_call: Some(0),
        open_tool_calls: Default::default(),
        resolved_mcp_tool_call_ids: Default::default(),
        streaming: Default::default(),
        on_tool_result: None,
    };
    state.streaming.insert(
        0,
        crate::utils::block_symbols::StreamingBlockState {
            block_index: Some(0),
            partial_json: Some(String::new()),
            block_kind: Some("mcp".into()),
            envelope_id: Some("env-1".into()),
            ..Default::default()
        },
    );
    stream.push(crate::types::AssistantMessageEvent::ToolcallStart {
        content_index: 0,
        partial: output.clone(),
    });
    let [first, second, third] = snapshots();
    for snapshot in [&first, &first, &second, &third] {
        super::apply_streamed_args_delta(&mut output, &stream, &mut state, "env-1", snapshot);
    }
    stream.end(Some(output.clone()));
    let events = public_events(&crate::utils::capture_test_records::collect_records(&stream).await);
    assert_eq!(
        delta_captures(&events, "toolcall_delta"),
        vec![
            (first.clone(), Some(0), Some(0)),
            (second[first.len()..].to_owned(), Some(609), Some(1209)),
            (third[second.len()..].to_owned(), Some(1209), Some(1209)),
        ],
        "a repeated snapshot pushes nothing; the reparse fires once 1024 units accrue"
    );
    let crate::types::ContentBlock::ToolCall(call) = &output.content[0] else {
        panic!("tool call");
    };
    assert_eq!(
        Value::Object(call.arguments.clone()),
        json!({ "text": &second[9..] }),
        "arguments keep the stale reparse, not the closed final snapshot"
    );
}
