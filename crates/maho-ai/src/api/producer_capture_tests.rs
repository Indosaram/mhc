//! Ports of senpi packages/ai/test/producer-capture.test.ts and
//! producer-responses-capture-fixture.ts. The TS suites mock the Anthropic and OpenAI SDK clients;
//! here the Anthropic stream reads fixed SSE from a local server, Completions reads the chunks
//! through its `ChatTransport` seam (the analogue of `vi.mock("openai")`), and the Responses
//! processor reads frames served locally.

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use serde_json::{Value, json};

use crate::api::openai_completions::{
    ChatRequest, ChatStreamResponse, ChatTransport, OpenAiCompletionsError,
    OpenAiCompletionsOptions, stream_with_transport,
};
use crate::api::openai_responses_shared::{ResponsesStreamOptions, process_responses_stream};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, Context, DoneReason, Model, StreamOptions, Tool,
};
use crate::utils::assistant_capture::{AssistantRecord, CapturedEvent};
use crate::utils::capture_test_records::{
    DEADLINE, assert_matches_object, capture, capture_json, collect_records, drain_records,
    event_type, progress_records, public_bytes, public_events, subscribe_capture_for_test,
};
use crate::utils::capture_test_server::{FixtureServer, fixture_context, sse_data, sse_events};
use crate::utils::event_stream::AssistantMessageEventStream;

/// `raw` from producer-responses-capture-fixture.ts.
pub(super) const RAW: &str = r#"{ "value": "\uD83D\uDE00", "quote": "\\\"𝄞" }"#;

/// `raw.slice(a, b)` in UTF-16 code units.
pub(super) fn raw_slice(start: usize, end: Option<usize>) -> String {
    let units: Vec<u16> = RAW.encode_utf16().collect();
    let end = end.unwrap_or(units.len()).min(units.len());
    String::from_utf16(&units[start.min(end)..end]).expect("raw slice on a char boundary")
}

pub(super) fn raw_length() -> usize {
    RAW.encode_utf16().count()
}

/// `getModel("anthropic", "claude-haiku-4-5")`.
fn haiku() -> Model {
    crate::models_generated::MODELS
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
        .clone()
}

/// `anthropic(events)`: the frames between `message_start` and `message_delta`/`message_stop`.
pub(super) async fn anthropic(events: Vec<Value>) -> (AssistantMessageEventStream, FixtureServer) {
    let mut frames = vec![
        json!({ "type": "message_start", "message": { "id": "msg_fixture", "usage": { "input_tokens": 1, "output_tokens": 0 } } }),
    ];
    frames.extend(events);
    frames.push(json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 1 } }));
    frames.push(json!({ "type": "message_stop" }));
    let server = FixtureServer::start("text/event-stream", sse_events(&frames)).await;
    let mut model = haiku();
    model.base_url = server.origin.clone();
    let mut options = StreamOptions::default();
    options.request.api_key = Some("fixture".into());
    options.request.max_retries = Some(0);
    let stream = crate::api::anthropic_messages::stream(&model, &fixture_context(), Some(options));
    (stream, server)
}

struct ScriptedChunks(Mutex<Option<Vec<Value>>>);

impl ChatTransport for ScriptedChunks {
    fn create_chat_completion(
        &self,
        _request: ChatRequest,
    ) -> crate::types::BoxFuture<'static, Result<ChatStreamResponse, OpenAiCompletionsError>> {
        let chunks = self.0.lock().expect("chunks").take().unwrap_or_default();
        Box::pin(async move {
            Ok(ChatStreamResponse {
                stream: futures::stream::iter(chunks.into_iter().map(Ok)).boxed(),
                status: 200,
                headers: Default::default(),
            })
        })
    }
}

/// `completions(deferred, custom)`.
fn completions(deferred: bool, custom: bool) -> AssistantMessageEventStream {
    let mut chunks = Vec::new();
    if deferred {
        chunks.push(json!({ "choices": [{ "delta": { "content": "hello", "reasoning_content": "thought" }, "finish_reason": null }] }));
    }
    for (index, chunk) in [raw_slice(0, Some(20)), String::new(), raw_slice(20, None)]
        .into_iter()
        .enumerate()
    {
        let mut call = json!({ "index": 0 });
        if index == 0 {
            call["id"] = json!("call_fixture");
            call["type"] = json!(if custom { "custom" } else { "function" });
        }
        if custom {
            call["custom"] = json!({ "name": "echo", "input": chunk });
        } else {
            call["function"] = json!({ "name": "echo", "arguments": chunk });
        }
        chunks.push(
            json!({ "choices": [{ "delta": { "tool_calls": [call] }, "finish_reason": null }] }),
        );
    }
    chunks.push(json!({ "choices": [{ "delta": {}, "finish_reason": "tool_calls" }] }));
    let mut model = crate::models_generated::get_builtin_model("openai", "gpt-4o")
        .expect("gpt-4o")
        .clone();
    model.api = "openai-completions".into();
    model.compat = Some(crate::model::ModelCompat(
        json!({ "supportsOpenAIGrammarTools": true })
            .as_object()
            .cloned()
            .expect("compat"),
    ));
    let mut tool: Tool = serde_json::from_value(json!({
        "name": "echo", "description": "fixture",
        "parameters": { "type": "object", "properties": { "input": { "type": "string" } }, "required": ["input"] }
    }))
    .expect("tool");
    if custom {
        tool.constrained_sampling = Some(
            serde_json::from_value(
                json!({ "type": "grammar", "variants": { "openai_lark": "start: \"ok\"" } }),
            )
            .expect("grammar"),
        );
    }
    let context = Context {
        tools: Some(vec![tool]),
        ..fixture_context()
    };
    let mut options = OpenAiCompletionsOptions::default();
    options.stream.request.api_key = Some("fixture".into());
    options.stream.request.max_retries = Some(0);
    stream_with_transport(
        &model,
        &context,
        Some(options),
        Arc::new(ScriptedChunks(Mutex::new(Some(chunks)))),
    )
}

fn responses_output() -> AssistantMessage {
    serde_json::from_value(json!({
        "role": "assistant", "api": "openai-responses", "provider": "openai", "model": "gpt-5",
        "content": [], "stopReason": "pending", "timestamp": 1,
        "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } }
    }))
    .expect("responses output")
}

/// Runs `process_responses_stream` over `frames` served as SSE.
pub(super) async fn process_frames(
    frames: &[Value],
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
) {
    let server = FixtureServer::start("text/event-stream", sse_data(frames, false)).await;
    let response = reqwest::get(&server.origin)
        .await
        .expect("fixture response");
    let model = crate::models_generated::get_builtin_model("openai", "gpt-5")
        .expect("gpt-5")
        .clone();
    tokio::time::timeout(
        DEADLINE,
        process_responses_stream(
            Box::pin(response.bytes_stream()),
            output,
            stream,
            &model,
            &ResponsesStreamOptions::default(),
        ),
    )
    .await
    .expect("responses deadline")
    .expect("responses stream");
}

/// `responses(seed, reset)` from producer-responses-capture-fixture.ts.
pub(super) async fn responses(seed: &str, reset: bool) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::assistant();
    let mut message = responses_output();
    let seed_length = seed.encode_utf16().count();
    let item = json!({ "type": "function_call", "id": "fc_fixture", "call_id": "call_fixture", "name": "echo", "arguments": seed });
    let mut frames =
        vec![json!({ "type": "response.output_item.added", "output_index": 0, "item": item })];
    for delta in [
        raw_slice(seed_length, Some(20)),
        String::new(),
        raw_slice(20, None),
    ] {
        frames.push(json!({ "type": "response.function_call_arguments.delta", "output_index": 0, "delta": delta }));
    }
    if reset {
        frames.push(json!({ "type": "response.function_call_arguments.done", "output_index": 0, "arguments": "{ \"reset\": true }" }));
    }
    let mut done_item = item.clone();
    done_item["arguments"] = json!(if reset { "{ \"reset\": true }" } else { RAW });
    frames
        .push(json!({ "type": "response.output_item.done", "output_index": 0, "item": done_item }));
    frames.push(json!({ "type": "response.completed", "response": { "id": "resp_fixture", "status": "completed", "output": [] } }));
    process_frames(&frames, &mut message, &stream).await;
    stream.push(AssistantMessageEvent::Done {
        reason: DoneReason::ToolUse,
        message: message.clone(),
    });
    stream.end(Some(message));
    stream
}

fn tool_start(events: &[CapturedEvent]) -> &CapturedEvent {
    events
        .iter()
        .find(|event| event_type(&event.value) == "toolcall_start")
        .expect("no toolcall_start event")
}

fn tool_deltas(events: &[CapturedEvent]) -> Vec<(&CapturedEvent, &str)> {
    events
        .iter()
        .filter_map(|event| match &event.value {
            AssistantMessageEvent::ToolcallDelta { delta, .. } => Some((event, delta.as_str())),
            _ => None,
        })
        .collect()
}

/// `assertDeltas`: each delta's capture offset is the UTF-16 length before it, ending at `raw`.
/// (TS also checks the public event carries no capture key or symbol: Rust events have no field
/// for one, so the public type proves it.)
fn assert_deltas(events: &[CapturedEvent], start: usize) {
    let mut offset = start;
    for (event, delta) in tool_deltas(events) {
        assert_eq!(
            capture(event).and_then(|capture| capture.offset),
            Some(offset)
        );
        offset += delta.encode_utf16().count();
    }
    assert_eq!(offset, raw_length());
}

/// TS events alias the producer's one `output`; Rust partials are push-time copies, so each is
/// pointed at the last partial (the object's final state) before comparing bytes.
fn aliased(mut events: Vec<CapturedEvent>, to: &AssistantMessage) -> Vec<CapturedEvent> {
    for event in &mut events {
        crate::utils::capture_test_records::alias_partial(&mut event.value, to);
    }
    events
}

fn fixture(name: &str) -> Value {
    let text = match name {
        "anthropic" => include_str!("capture_fixtures/producer_0.json"),
        "completions grammar deferred=false" => include_str!("capture_fixtures/producer_1.json"),
        "completions grammar deferred=true" => include_str!("capture_fixtures/producer_2.json"),
        "responses" => include_str!("capture_fixtures/producer_3.json"),
        other => panic!("no fixture {other}"),
    };
    serde_json::from_str(text).expect("snapshot")
}

/// The snapshot pins `Date.now()` to 1; the Rust producers stamp the clock, so both sides zero it.
fn snapshot(name: &str) -> Value {
    let mut value = fixture(name);
    crate::utils::capture_test_records::zero_timestamps(&mut value);
    value
}

fn final_partial(events: &[CapturedEvent]) -> AssistantMessage {
    crate::utils::capture_test_records::terminal_message(&events.last().expect("terminal").value)
        .clone()
}

// producer capture #4272

#[tokio::test]
async fn keeps_baseline_public_bytes_for_the_three_provider_fixture_paths() {
    let _subscription = subscribe_capture_for_test();
    let (a, _server) = anthropic(vec![
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "tool_use", "id": "call_fixture", "name": "echo", "input": { "initial": true } } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "input_json_delta", "partial_json": RAW } }),
        json!({ "type": "content_block_stop", "index": 0 }),
    ])
    .await;
    let events = public_events(&drain_records(&a).await);
    let terminal = final_partial(&events);
    assert_eq!(
        public_bytes(&aliased(events, &terminal)),
        snapshot("anthropic")
    );
    let events = public_events(&collect_records(&responses("", true).await).await);
    let terminal = final_partial(&events);
    assert_eq!(
        public_bytes(&aliased(events, &terminal)),
        snapshot("responses")
    );
    for deferred in [false, true] {
        let stream = completions(deferred, true);
        let events = public_events(&drain_records(&stream).await);
        let terminal = final_partial(&events);
        assert_eq!(
            public_bytes(&aliased(events, &terminal)),
            snapshot(&format!("completions grammar deferred={deferred}")),
            "deferred={deferred}"
        );
    }
}

#[tokio::test]
async fn anthropic_freezes_an_object_only_seed_before_the_consumer_resumes() {
    let _subscription = subscribe_capture_for_test();
    let mut frames = vec![
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "tool_use", "id": "call_fixture", "name": "echo", "input": { "initial": true } } }),
    ];
    for partial_json in [raw_slice(0, Some(20)), String::new(), raw_slice(20, None)] {
        frames.push(json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "input_json_delta", "partial_json": partial_json } }));
    }
    frames.push(json!({ "type": "content_block_stop", "index": 0 }));
    let (stream, _server) = anthropic(frames).await;
    let events = public_events(&drain_records(&stream).await);
    let start = tool_start(&events);
    assert_matches_object(
        &capture_json(capture(start)),
        &json!({
            "block": { "arguments": { "initial": true } },
            "rawArgumentPrefix": "",
            "parsedThrough": 0,
            "coveredCodeUnits": 0,
        }),
    );
    assert_deltas(&events, 0);
    // `Object.isFrozen(block)`: the seed is an immutable shared `Arc`, so a later mutation of the
    // producer's block cannot reach it.
    assert!(
        capture(start)
            .and_then(|capture| capture.block.as_ref())
            .is_some()
    );
}

#[tokio::test]
async fn responses_preserves_nonempty_raw_seed_independently_of_its_parsed_seed() {
    let _subscription = subscribe_capture_for_test();
    let seed = raw_slice(0, Some(5));
    let events = public_events(&collect_records(&responses(&seed, false).await).await);
    assert_matches_object(
        &capture_json(capture(tool_start(&events))),
        &json!({
            "block": { "arguments": {} },
            "rawArgumentPrefix": seed,
            "parsedThrough": 0,
            "coveredCodeUnits": 5,
            "nextOffset": 5,
        }),
    );
    assert_deltas(&events, 5);
}

/// `it.each([false, true])("Completions records exact coverage with deferred=%s")`.
#[tokio::test]
async fn completions_records_exact_coverage() {
    for deferred in [false, true] {
        let _subscription = subscribe_capture_for_test();
        let stream = completions(deferred, false);
        let events = public_events(&drain_records(&stream).await);
        let raw_arguments: Value = serde_json::from_str(RAW).expect("raw json");
        assert_matches_object(
            &capture_json(capture(tool_start(&events))),
            &json!({
                "rawArgumentPrefix": if deferred { RAW } else { "" },
                "coveredCodeUnits": if deferred { raw_length() } else { 0 },
                "nextOffset": 0,
                "parsedThrough": if deferred { raw_length() } else { 0 },
                "block": { "arguments": if deferred { raw_arguments } else { json!({}) } },
            }),
        );
        assert_deltas(&events, 0);
    }
}

#[tokio::test]
async fn completions_grammar_captures_the_normalized_emitted_spelling() {
    let _subscription = subscribe_capture_for_test();
    let stream = completions(true, true);
    let events = public_events(&drain_records(&stream).await);
    let emitted: String = tool_deltas(&events)
        .into_iter()
        .map(|(_, delta)| delta)
        .collect();
    assert_eq!(
        serde_json::from_str::<Value>(&emitted).expect("emitted json"),
        json!({ "input": RAW })
    );
    let units: Vec<u16> = emitted.encode_utf16().collect();
    let prefix = String::from_utf16(&units[..units.len() - 2]).expect("prefix");
    assert_matches_object(
        &capture_json(capture(tool_start(&events))),
        &json!({ "rawArgumentPrefix": prefix, "coveredCodeUnits": units.len() - 2 }),
    );
}

#[tokio::test]
async fn responses_queues_authoritative_raw_resets_between_deltas_and_ends() {
    let _subscription = subscribe_capture_for_test();
    let records: Vec<AssistantRecord> = collect_records(&responses("", true).await).await;
    assert!(progress_records(&records).contains(&json!({
        "type": "message_tool_arguments_reset",
        "contentIndex": 0,
        "rawArgumentPrefix": "{ \"reset\": true }",
        "parsedThrough": 17,
    })));
}
