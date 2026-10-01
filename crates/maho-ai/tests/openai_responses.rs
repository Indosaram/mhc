//! Port of senpi packages/ai/test/{openai-responses*,azure-openai-*}.test.ts cases that cover
//! api/openai-responses.ts, api/openai-responses-shared.ts and api/azure-openai-responses.ts.
//!
//! The replay tests drive the Rust `stream()` against the same recorded fixture bytes
//! `tools/golden/ai-replay.mjs` served to the pinned senpi `stream()`, and compare the event
//! sequence with `crates/maho-ai/tests/golden/replay/<case>.json`; `assert_matches_golden` reproduces
//! senpi's own harness normalization (see its doc comment).

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use maho_ai::api::azure_openai_responses;
use maho_ai::api::openai_responses;
use maho_ai::api::openai_responses_shared::{
    convert_responses_messages, convert_responses_tools, process_responses_events,
    ConvertResponsesMessagesOptions, ConvertResponsesToolsOptions, ResponsesStreamOptions,
};
use maho_ai::types::{
    AssistantMessage, AssistantMessageEvent, CacheRetention, ConstrainedSampling, ConstrainedSamplingConfig,
    ContentBlock, Context, FreeformToolFormat, InputModality, JsonSchemaStrictness, Message, Model, ModelCost,
    ProviderEnv, ProviderHeaders, SimpleStreamOptions, StopReason, StreamOptions, ThinkingContent, ThinkingLevel, Tool,
    ToolCall, ToolChoice, ToolResultMessage, Usage, UserContent, UserMessage,
};
use serde_json::{json, Value};
use support::mock_server::{with_timeout, Fixture, MockFixtureServer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const ALLOWED_TOOL_CALL_PROVIDERS: [&str; 3] = ["openai", "chatgpt-subscription", "opencode"];
const VALID_IMAGE_BASE64: &str = "iVBORw0KGgo=";
const MAX_NATIVE_IMAGE_BASE64_CHARS: usize = 24 * 1024 * 1024;

fn allowed(providers: &[&str]) -> BTreeSet<String> {
    providers.iter().map(|provider| (*provider).to_owned()).collect()
}

fn model(api: &str, provider: &str, id: &str) -> Model {
    Model {
        id: id.to_owned(),
        name: id.to_owned(),
        api: api.to_owned(),
        provider: provider.to_owned(),
        base_url: String::new(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![InputModality::Text, InputModality::Image],
        cost: ModelCost::default(),
        context_window: 400_000,
        max_tokens: 128_000,
        sampling_params: None,
        headers: None,
        cache_retention: None,
        upstream_model_id: None,
        service_tier: None,
        recover_text_tool_calls: None,
        compat: None,
    }
}

fn azure_model() -> Model {
    model("azure-openai-responses", "azure-openai-responses", "gpt-5-mini")
}

fn output(model: &Model) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Pending,
        stop_details: None,
        deferred: None,
        error_message: None,
        abort_source: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 0,
    }
}

fn user_message(text: &str) -> Message {
    Message::User(UserMessage { content: UserContent::Text(text.into()), timestamp: 0 })
}

fn context_with(messages: Vec<Message>) -> Context {
    Context { system_prompt: Some("You are concise.".into()), messages, tools: None }
}

fn convert(model: &Model, context: &Context) -> Vec<Value> {
    convert_responses_messages(
        model,
        context,
        &allowed(&ALLOWED_TOOL_CALL_PROVIDERS),
        &ConvertResponsesMessagesOptions::default(),
    )
}

fn tool_call(id: &str, name: &str, arguments: Value) -> ContentBlock {
    ContentBlock::ToolCall(ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: arguments.as_object().cloned().unwrap_or_default(),
        incomplete: None,
        error_message: None,
        thought_signature: None,
        namespace: None,
    })
}

fn assistant(model: &Model, content: Vec<ContentBlock>) -> Message {
    let mut message = output(model);
    message.content = content;
    message.stop_reason = StopReason::Stop;
    Message::Assistant(Box::new(message))
}

fn items_of<'a>(items: &'a [Value], kind: &str) -> Vec<&'a Value> {
    items.iter().filter(|item| item.get("type").and_then(Value::as_str) == Some(kind)).collect()
}

fn assistant_texts(items: &[Value]) -> Vec<String> {
    let mut texts = Vec::new();
    for item in items_of(items, "message") {
        let Some(content) = item.get("content").and_then(Value::as_array) else { continue };
        for block in content {
            if let Some(text) = block.get("text").and_then(Value::as_str) {
                texts.push(text.to_owned());
            }
        }
    }
    texts
}

// =============================================================================
// Golden replay
// =============================================================================

fn golden_path(case: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/replay").join(format!("{case}.json"))
}

fn case_path(case: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/golden/cases")
        .join(format!("{case}.json"))
}

fn load_case(case: &str) -> Value {
    let path = case_path(case);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}")))
        .expect("replay case JSON")
}

fn sse_frames(case: &str) -> Vec<(Option<String>, String)> {
    load_case(case)["fixture"]["events"]
        .as_array()
        .expect("fixture.events")
        .iter()
        .map(|event| {
            (
                event.get("event").and_then(Value::as_str).map(str::to_owned),
                event["data"].as_str().expect("fixture event data").to_owned(),
            )
        })
        .collect()
}

fn sse_fixture_bytes(case: &str) -> Vec<u8> {
    let mut body = String::new();
    for (event, data) in sse_frames(case) {
        if let Some(event) = event {
            body.push_str(&format!("event: {event}\n"));
        }
        body.push_str(&format!("data: {data}\n\n"));
    }
    body.into_bytes()
}

/// The final `AssistantMessage` senpi's harness aliases into every event, with its timestamp pinned.
fn final_message(events: &[Value]) -> Value {
    let mut message = events
        .iter()
        .rev()
        .find_map(|event| {
            event.get("message").or_else(|| event.get("error")).filter(|value| value.get("role").is_some()).cloned()
        })
        .expect("a terminal message");
    if let Some(object) = message.as_object_mut() {
        object.insert("timestamp".into(), json!(0));
    }
    message
}

fn normalize_event(mut event: Value, final_message: &Value, shared_containers: &[&str]) -> Value {
    for key in ["partial", "message", "error"] {
        let is_message =
            matches!(event.get(key), Some(Value::Object(message)) if message.contains_key("role"));
        if !is_message {
            continue;
        }
        let Some(object) = event.get_mut(key).and_then(Value::as_object_mut) else { continue };
        if let Some(content) = final_message.get("content") {
            object.insert("content".into(), content.clone());
        }
        for container in shared_containers {
            if let Some(value) = final_message.get(*container) {
                object.insert((*container).into(), value.clone());
            }
        }
        object.insert("timestamp".into(), json!(0));
    }
    // senpi's `ToolCall` repeats its `type` literal; the Rust port carries it as the enum tag, which
    // is absent on the bare `toolCall` a `toolcall_end` event embeds.
    if let Some(tool_call) = event.get_mut("toolCall").and_then(Value::as_object_mut)
        && tool_call.contains_key("id")
        && tool_call.contains_key("arguments")
    {
        tool_call.insert("type".into(), json!("toolCall"));
    }
    event
}

/// senpi's harness hands each event the live `AssistantMessage`, so an array the implementation
/// mutates in place shows its final state in every earlier event; this reproduces that aliasing
/// before comparing, so event type, order, deltas, ids and usage are still compared as emitted.
fn assert_matches_golden(case: &str, events: &[AssistantMessageEvent], shared_containers: &[&str]) {
    let raw: Vec<Value> = events.iter().map(|event| serde_json::to_value(event).expect("event JSON")).collect();
    let terminal = final_message(&raw);
    let actual: Vec<Value> =
        raw.into_iter().map(|event| normalize_event(event, &terminal, shared_containers)).collect();
    let path = golden_path(case);
    let golden: Value = serde_json::from_str(
        &std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}")),
    )
    .expect("golden JSON");
    assert!(
        values_equal(&Value::Array(actual.clone()), &golden),
        "replay mismatch for {case}: regenerate with `bun tools/golden/ai-replay.mjs --case {case}`\n{}",
        first_difference(&golden, &Value::Array(actual.clone()), "events").unwrap_or_default(),
    );
}

fn first_difference(golden: &Value, actual: &Value, path: &str) -> Option<String> {
    match (golden, actual) {
        (Value::Object(golden), Value::Object(actual)) => {
            for (key, expected) in golden {
                match actual.get(key) {
                    Some(produced) => {
                        if let Some(difference) = first_difference(expected, produced, &format!("{path}.{key}")) {
                            return Some(difference);
                        }
                    }
                    None => return Some(format!("{path}.{key}: missing on the maho side: {expected}")),
                }
            }
            actual
                .keys()
                .find(|key| !golden.contains_key(*key))
                .map(|key| format!("{path}.{key}: unexpected on the maho side: {}", actual[key]))
        }
        (Value::Array(golden), Value::Array(actual)) => {
            for (index, (expected, produced)) in golden.iter().zip(actual).enumerate() {
                if let Some(difference) = first_difference(expected, produced, &format!("{path}[{index}]")) {
                    return Some(difference);
                }
            }
            (golden.len() != actual.len())
                .then(|| format!("{path}: senpi recorded {} element(s), maho produced {}", golden.len(), actual.len()))
        }
        (Value::String(golden), Value::String(actual)) if golden != actual => Some(format!(
            "{path}: senpi len {} bytes {:?} vs maho len {} bytes {:?}",
            golden.len(),
            golden.as_bytes(),
            actual.len(),
            actual.as_bytes()
        )),
        _ if values_equal(golden, actual) => None,
        _ => Some(format!("{path}: senpi {golden:?} vs maho {actual:?}")),
    }
}

fn values_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => left.as_f64() == right.as_f64(),
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len() && left.iter().zip(right).all(|(left, right)| values_equal(left, right))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, value)| right.get(key).is_some_and(|other| values_equal(value, other)))
        }
        _ => left == right,
    }
}

async fn collect(stream: maho_ai::types::AssistantMessageEventStream) -> Vec<AssistantMessageEvent> {
    with_timeout(30, stream.collect()).await.expect("stream collected")
}

fn responses_options() -> StreamOptions {
    let mut options = StreamOptions::default();
    options.request.api_key = Some("test".into());
    options.request.max_retries = Some(0);
    options.request.timeout_ms = Some(10_000);
    options
}

fn replay_model(case: &str, base_url: String) -> Model {
    let spec = load_case(case);
    let mut model = model("openai-responses", "test-replay", "gpt-5");
    model.base_url = base_url;
    model.reasoning = spec["model"]["reasoning"].as_bool().unwrap_or(false);
    model.input = spec["model"]["input"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .map(|value| match value.as_str() {
                    Some("image") => InputModality::Image,
                    Some("video") => InputModality::Video,
                    _ => InputModality::Text,
                })
                .collect()
        })
        .unwrap_or_else(|| vec![InputModality::Text]);
    model
}

fn hello_context() -> Context {
    Context { system_prompt: None, messages: vec![user_message("Hello")], tools: None }
}

#[tokio::test]
async fn openai_responses_replays_the_recorded_stream() {
    let case = "ai-replay-openai-responses-basic";
    let server = MockFixtureServer::start(Fixture::sse(
        sse_frames(case).iter().map(|(event, data)| (event.as_deref(), data.as_str())).collect::<Vec<_>>(),
    ))
    .await;
    let model = replay_model(case, server.base_url().to_owned());
    let events = collect(openai_responses::stream(&model, &hello_context(), Some(responses_options()))).await;
    assert_matches_golden(case, &events, &[]);
}

#[tokio::test]
async fn openai_responses_replays_a_two_tool_call_stream() {
    let case = "ai-replay-openai-responses-tool-calls";
    let server = MockFixtureServer::start(Fixture::sse(
        sse_frames(case).iter().map(|(event, data)| (event.as_deref(), data.as_str())).collect::<Vec<_>>(),
    ))
    .await;
    let model = replay_model(case, server.base_url().to_owned());
    let events = collect(openai_responses::stream(&model, &hello_context(), Some(responses_options()))).await;
    let tool_ends = events
        .iter()
        .filter(|event| matches!(event, AssistantMessageEvent::ToolcallEnd { .. }))
        .count();
    assert_eq!(tool_ends, 2, "the recorded stream carries two tool calls");
    assert_matches_golden(case, &events, &[]);
}

#[tokio::test]
async fn openai_responses_mid_stream_close_ends_with_a_typed_error() {
    let case = "ai-replay-openai-responses-eof-truncated";
    let bytes = sse_fixture_bytes(case);
    let cut = load_case(case)["fixture"]["closeAfterBytes"].as_u64().expect("closeAfterBytes") as usize;
    assert!(cut < bytes.len(), "the case cuts the fixture before its terminator frame");
    let server = MockFixtureServer::start(
        Fixture::sse(
            sse_frames(case).iter().map(|(event, data)| (event.as_deref(), data.as_str())).collect::<Vec<_>>(),
        )
        .closed_after(cut),
    )
    .await;
    let model = replay_model(case, server.base_url().to_owned());
    let events = collect(openai_responses::stream(&model, &hello_context(), Some(responses_options()))).await;
    let last = events.last().expect("a terminal event");
    let AssistantMessageEvent::Error { error, .. } = last else {
        panic!("a mid-stream close must end with a typed error event, got {last:?}")
    };
    assert_eq!(error.stop_reason, StopReason::Error);
    assert_eq!(error.error_message.as_deref(), Some("OpenAI Responses stream ended without a stop reason"));
    assert_matches_golden(case, &events, &[]);
}

// =============================================================================
// convertResponsesMessages / processResponsesStream cases
// =============================================================================

#[test]
fn empty_tool_result_uses_the_no_tool_output_placeholder() {
    let model = model("openai-responses", "openai", "gpt-4o-mini");
    let context = context_with(vec![
        user_message("Run the command"),
        assistant(&model, vec![tool_call("tool-1", "bash", json!({ "command": "true" }))]),
        Message::ToolResult(ToolResultMessage {
            tool_call_id: "tool-1".into(),
            tool_name: "bash".into(),
            content: vec![ContentBlock::text("")],
            details: None,
            usage: None,
            added_tool_names: None,
            is_error: false,
            timestamp: 1,
        }),
    ]);

    let input = convert(&model, &context);
    let outputs = items_of(&input, "function_call_output");
    assert_eq!(outputs.len(), 1);
    let output = outputs[0]["output"].as_str().expect("string output");
    assert_eq!(output, "(no tool output)");
    assert!(!output.contains("see attached image"));
}

#[test]
fn foreign_thinking_signatures_are_demoted_or_dropped() {
    let model = model("openai-responses", "chatgpt-subscription", "gpt-5.5");

    let kimi = convert(
        &model,
        &context_with(vec![
            user_message("Hi"),
            assistant(
                &model,
                vec![
                    ContentBlock::Thinking(maho_ai::types::ThinkingContent {
                        thinking: "deeply considered result".into(),
                        thinking_signature: Some("reasoning_content".into()),
                        ..Default::default()
                    }),
                    ContentBlock::text("answer"),
                ],
            ),
        ]),
    );
    assert!(assistant_texts(&kimi).contains(&String::from("deeply considered result")));
    assert!(items_of(&kimi, "reasoning").is_empty());

    let anthropic = convert(
        &model,
        &context_with(vec![
            user_message("Hi"),
            assistant(
                &model,
                vec![ContentBlock::Thinking(maho_ai::types::ThinkingContent {
                    thinking: "claude was here".into(),
                    thinking_signature: Some("EqQBCkYICxgCKkFudGVzdA==".into()),
                    ..Default::default()
                })],
            ),
        ]),
    );
    assert!(items_of(&anthropic, "reasoning").is_empty());
    assert!(assistant_texts(&anthropic).contains(&String::from("claude was here")));

    let non_reasoning_json = convert(
        &model,
        &context_with(vec![
            user_message("Hi"),
            assistant(
                &model,
                vec![ContentBlock::Thinking(maho_ai::types::ThinkingContent {
                    thinking: String::new(),
                    thinking_signature: Some(json!({ "type": "message", "id": "msg_x" }).to_string()),
                    ..Default::default()
                })],
            ),
        ]),
    );
    assert!(items_of(&non_reasoning_json, "reasoning").is_empty());

    let genuine = convert(
        &model,
        &context_with(vec![
            user_message("Hi"),
            assistant(
                &model,
                vec![ContentBlock::Thinking(maho_ai::types::ThinkingContent {
                    thinking: String::new(),
                    thinking_signature: Some(json!({ "type": "reasoning", "id": "rs_abc123", "summary": [] }).to_string()),
                    ..Default::default()
                })],
            ),
        ]),
    );
    let replayed = items_of(&genuine, "reasoning");
    assert_eq!(replayed.len(), 1);
    assert_eq!(replayed[0]["id"].as_str(), Some("rs_abc123"));
}

#[test]
fn foreign_copilot_tool_item_ids_are_hashed_into_a_bounded_fc_id() {
    const COPILOT_RAW_TOOL_CALL_ID: &str = "call_4VnzVawQXPB9MgYib7CiQFEY|I9b95oN1wD/cHXKTw3PpRkL6KkCtzTJhUxMouMWYwHeTo2j3htzfSk7YPx2vifiIM4g3A8XXyOj8q4Bt6SLUG7gqY1E3ELkrkVQNHglRfUmWj84lqxJY+Puieb3VKyX0FB+83TUzn91cDMF/4gzt990IzqVrc+nIb9RRscRD070Du16q1glydVjWR0SBJsE6TbY/esOjFpqplogQqrajm1eI++f3eLi73R6q7hVusY0QbeFySVxABCjhN0lXB04caBe1rzHjYzul6MAXj7uq+0r17VLq+yrtyYhN12wkmFqHeqTyEei6EFPbMy24Nc+IbJlkP0OCg02W+gOnyBFcbi2ctvJFSOhSjt1CqBdqCnnhwUqXjbWiT0wh3DmLScRgTHmGkaI+oAcQQjfic65nxj+TnEkReA==";
    let model = model("openai-responses", "chatgpt-subscription", "gpt-5.5");
    let mut foreign = output(&model);
    foreign.api = "openai-responses".into();
    foreign.provider = "github-copilot".into();
    foreign.model = "gpt-5.5".into();
    foreign.stop_reason = StopReason::ToolUse;
    foreign.content = vec![tool_call(COPILOT_RAW_TOOL_CALL_ID, "edit", json!({ "path": "src/styles/app.css" }))];
    let context = context_with(vec![
        user_message("Use the tool."),
        Message::Assistant(Box::new(foreign)),
        Message::ToolResult(ToolResultMessage {
            tool_call_id: COPILOT_RAW_TOOL_CALL_ID.into(),
            tool_name: "edit".into(),
            content: vec![ContentBlock::text("ok")],
            details: None,
            usage: None,
            added_tool_names: None,
            is_error: false,
            timestamp: 2,
        }),
    ]);

    let input = convert(&model, &context);
    let function_calls = items_of(&input, "function_call");
    assert_eq!(function_calls.len(), 1);
    let item_id = function_calls[0]["id"].as_str().expect("fc item id");
    let raw_item_id = COPILOT_RAW_TOOL_CALL_ID.split('|').nth(1).expect("item id");
    let expected = format!("fc_{}", maho_ai::utils::hash::short_hash(raw_item_id));
    assert_eq!(item_id, expected);
    assert!(item_id.len() <= 64);
    assert!(item_id.starts_with("fc_") && item_id[3..].chars().all(|c| c.is_ascii_alphanumeric()));
}

#[test]
fn fallback_message_ids_are_unique_per_text_block() {
    let model = model("openai-responses", "chatgpt-subscription", "gpt-5.5");
    let mut foreign = output(&model);
    foreign.api = "anthropic-messages".into();
    foreign.provider = "anthropic".into();
    foreign.model = String::new();
    foreign.content = vec![
        ContentBlock::Thinking(maho_ai::types::ThinkingContent {
            thinking: "private reasoning".into(),
            ..Default::default()
        }),
        ContentBlock::text("visible answer"),
    ];
    let context = context_with(vec![user_message("hello"), Message::Assistant(Box::new(foreign))]);

    let input = convert(&model, &context);
    let ids: Vec<String> = items_of(&input, "message")
        .iter()
        .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect();
    assert_eq!(ids, vec![String::from("msg_pi_1"), String::from("msg_pi_1_1")]);
    assert_eq!(ids.iter().collect::<BTreeSet<_>>().len(), ids.len());
}

#[test]
fn namespaces_round_trip_only_when_the_target_can_replay_them() {
    let target_model = model("openai-responses", "openai", "gpt-5.4");
    let mut produced = output(&target_model);
    produced.content = vec![
        ContentBlock::ToolCall(ToolCall {
            id: "call_function|fc_test".into(),
            name: "lookup".into(),
            arguments: json!({ "value": "hello" }).as_object().cloned().unwrap_or_default(),
            incomplete: None,
            error_message: None,
            thought_signature: None,
            namespace: Some("dynamic_tools".into()),
        }),
        ContentBlock::ToolCall(ToolCall {
            id: "call_custom|ctc_test".into(),
            name: "query".into(),
            arguments: json!({ "input": "hello" }).as_object().cloned().unwrap_or_default(),
            incomplete: None,
            error_message: None,
            thought_signature: None,
            namespace: Some("dynamic_tools".into()),
        }),
    ];
    let options = ConvertResponsesMessagesOptions {
        grammar_tool_input_properties: [("query".to_owned(), "input".to_owned())].into_iter().collect(),
        ..ConvertResponsesMessagesOptions::default()
    };

    let same_model = convert_responses_messages(
        &target_model,
        &context_with(vec![Message::Assistant(Box::new(produced.clone()))]),
        &allowed(&ALLOWED_TOOL_CALL_PROVIDERS),
        &options,
    );
    assert_eq!(items_of(&same_model, "function_call")[0]["namespace"].as_str(), Some("dynamic_tools"));
    assert_eq!(items_of(&same_model, "custom_tool_call")[0]["namespace"].as_str(), Some("dynamic_tools"));

    let other_model = model("openai-responses", "openai", "gpt-5.2");
    let other_provider = model("azure-openai-responses", "azure-openai-responses", "gpt-5.4");
    let other_api = model("openai-codex-responses", "chatgpt-subscription", "gpt-5.3-codex-spark");
    for target in [other_model, other_provider, other_api] {
        let replayed = convert_responses_messages(
            &target,
            &context_with(vec![Message::Assistant(Box::new(produced.clone()))]),
            &allowed(&ALLOWED_TOOL_CALL_PROVIDERS),
            &options,
        );
        let function_call = items_of(&replayed, "function_call");
        let custom_tool_call = items_of(&replayed, "custom_tool_call");
        assert_eq!(function_call.len(), 1);
        assert!(function_call[0].get("namespace").is_none());
        assert_eq!(custom_tool_call.len(), 1);
        assert!(custom_tool_call[0].get("namespace").is_none());
    }
}

#[test]
fn ordinary_function_calls_gain_no_namespace() {
    let model = model("openai-responses", "openai", "gpt-5.4");
    let mut produced = output(&model);
    produced.content = vec![tool_call("call_test|fc_test", "lookup", json!({ "value": "hello" }))];

    let input = convert(&model, &context_with(vec![Message::Assistant(Box::new(produced))]));
    let function_calls = items_of(&input, "function_call");
    assert_eq!(function_calls.len(), 1);
    assert!(function_calls[0].get("namespace").is_none());
}

fn run_events(model: &Model, events: Vec<Value>) -> (AssistantMessage, Vec<AssistantMessageEvent>) {
    run_events_with(model, events, ResponsesStreamOptions::default())
}

fn run_events_with(
    model: &Model,
    events: Vec<Value>,
    options: ResponsesStreamOptions<'_>,
) -> (AssistantMessage, Vec<AssistantMessageEvent>) {
    let mut produced = output(model);
    let stream = maho_ai::types::AssistantMessageEventStream::assistant();
    process_responses_events(events, &mut produced, &stream, model, &options).expect("stream processed");
    let events = stream.queue();
    (produced, events)
}

fn terminal_response(output_items: Vec<Value>) -> Value {
    json!({
        "type": "response.completed",
        "response": {
            "id": "resp_native",
            "status": "completed",
            "output": output_items,
            "usage": {
                "input_tokens": 0,
                "output_tokens": 0,
                "total_tokens": 0,
                "input_tokens_details": { "cached_tokens": 0 },
            },
        },
    })
}

fn provider_native_raw(message: &AssistantMessage) -> Vec<Value> {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ProviderNative(native) => Some(native.raw.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn image_generation_call_frames_reconcile_into_one_final_block() {
    let model = model("openai-responses", "openai", "gpt-5.4");
    let added = json!({ "type": "image_generation_call", "id": "ig_1", "status": "in_progress" });
    let done = json!({ "type": "image_generation_call", "id": "ig_1", "status": "completed", "result": VALID_IMAGE_BASE64 });

    let (message, _) = run_events(
        &model,
        vec![
            json!({ "type": "response.output_item.added", "item": added, "output_index": 0 }),
            json!({ "type": "response.output_item.done", "item": done, "output_index": 0 }),
            terminal_response(vec![done.clone()]),
        ],
    );

    assert_eq!(provider_native_raw(&message), vec![done]);
}

#[test]
fn completed_image_keeps_a_nonempty_revised_prompt_and_drops_blank_ones() {
    let model = model("openai-responses", "openai", "gpt-5.4");
    let without_prompt =
        json!({ "type": "image_generation_call", "id": "ig_no_prompt", "status": "completed", "result": VALID_IMAGE_BASE64 });
    let empty_prompt = json!({
        "type": "image_generation_call",
        "id": "ig_empty_prompt",
        "status": "completed",
        "result": VALID_IMAGE_BASE64,
        "revised_prompt": "   ",
    });
    let (message, _) = run_events(
        &model,
        vec![
            json!({ "type": "response.output_item.done", "item": without_prompt.clone(), "output_index": 0 }),
            json!({ "type": "response.output_item.done", "item": empty_prompt.clone(), "output_index": 1 }),
            terminal_response(vec![without_prompt.clone(), empty_prompt]),
        ],
    );
    assert_eq!(
        provider_native_raw(&message),
        vec![
            without_prompt,
            json!({ "type": "image_generation_call", "id": "ig_empty_prompt", "status": "completed", "result": VALID_IMAGE_BASE64 }),
        ]
    );

    let with_prompt = json!({
        "type": "image_generation_call",
        "id": "ig_prompt",
        "status": "completed",
        "result": VALID_IMAGE_BASE64,
        "revised_prompt": "A small blue square",
    });
    let (message, _) = run_events(
        &model,
        vec![
            json!({ "type": "response.output_item.done", "item": with_prompt.clone(), "output_index": 0 }),
            terminal_response(vec![with_prompt.clone()]),
        ],
    );
    assert_eq!(provider_native_raw(&message), vec![with_prompt]);
}

#[test]
fn unusable_completed_image_results_become_malformed_without_the_base64() {
    let model = model("openai-responses", "openai", "gpt-5.4");
    let items = vec![
        json!({ "type": "image_generation_call", "id": "ig_null", "status": "completed", "result": null }),
        json!({ "type": "image_generation_call", "id": "ig_missing", "status": "completed" }),
        json!({ "type": "image_generation_call", "id": "ig_empty", "status": "completed", "result": "" }),
        json!({ "type": "image_generation_call", "id": "ig_invalid", "status": "completed", "result": "not base64" }),
    ];
    let mut events: Vec<Value> = items
        .iter()
        .enumerate()
        .map(|(index, item)| json!({ "type": "response.output_item.done", "item": item, "output_index": index }))
        .collect();
    events.push(terminal_response(items.clone()));

    let (message, _) = run_events(&model, events);
    let expected: Vec<Value> = items
        .iter()
        .map(|item| json!({ "type": "image_generation_call", "id": item["id"], "status": "malformed" }))
        .collect();
    assert_eq!(provider_native_raw(&message), expected);
}

#[test]
fn failed_and_unknown_image_statuses_survive_verbatim() {
    let model = model("openai-responses", "openai", "gpt-5.4");
    let failed = json!({
        "type": "image_generation_call",
        "id": "ig_failed",
        "status": "failed",
        "result": VALID_IMAGE_BASE64,
        "revised_prompt": "must not survive",
    });
    let unknown =
        json!({ "type": "image_generation_call", "id": "ig_unknown", "status": "provider_specific_state", "result": VALID_IMAGE_BASE64 });
    let (message, _) = run_events(
        &model,
        vec![
            json!({ "type": "response.output_item.done", "item": failed.clone(), "output_index": 0 }),
            json!({ "type": "response.output_item.done", "item": unknown.clone(), "output_index": 1 }),
            terminal_response(vec![failed.clone(), unknown.clone()]),
        ],
    );
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(
        provider_native_raw(&message),
        vec![
            json!({ "type": "image_generation_call", "id": "ig_failed", "status": "failed" }),
            json!({ "type": "image_generation_call", "id": "ig_unknown", "status": "provider_specific_state" }),
        ]
    );
}

#[test]
fn partial_image_events_are_ignored_entirely() {
    let model = model("openai-responses", "openai", "gpt-5.4");
    let (message, _) = run_events(
        &model,
        vec![
            json!({
                "type": "response.image_generation_call.partial_image",
                "item_id": "ig_partial",
                "output_index": 0,
                "partial_image_index": 0,
                "partial_image_b64": VALID_IMAGE_BASE64,
            }),
            terminal_response(Vec::new()),
        ],
    );
    assert!(provider_native_raw(&message).is_empty());
    assert!(!serde_json::to_string(&message.content).expect("content JSON").contains(VALID_IMAGE_BASE64));
}

#[test]
fn terminal_output_backfills_an_image_whose_done_frame_is_absent() {
    let model = model("openai-responses", "openai", "gpt-5.4");
    let added = json!({ "type": "image_generation_call", "id": "ig_backfill", "status": "in_progress" });
    let completed = json!({
        "type": "image_generation_call",
        "id": "ig_backfill",
        "status": "completed",
        "result": VALID_IMAGE_BASE64,
        "revised_prompt": "Backfilled prompt",
    });
    let (message, _) = run_events(
        &model,
        vec![
            json!({ "type": "response.output_item.added", "item": added, "output_index": 0 }),
            terminal_response(vec![completed.clone()]),
        ],
    );
    assert_eq!(provider_native_raw(&message), vec![completed]);
}

#[test]
fn oversized_aggregate_image_results_fail_without_retaining_base64() {
    let model = model("openai-responses", "openai", "gpt-5.4");
    let first = json!({
        "type": "image_generation_call",
        "id": "ig_large_1",
        "status": "completed",
        "result": "A".repeat(MAX_NATIVE_IMAGE_BASE64_CHARS / 2),
    });
    let second = json!({
        "type": "image_generation_call",
        "id": "ig_large_2",
        "status": "completed",
        "result": "A".repeat(MAX_NATIVE_IMAGE_BASE64_CHARS / 2 + 4),
    });
    let mut produced = output(&model);
    let stream = maho_ai::types::AssistantMessageEventStream::assistant();
    let error = process_responses_events(
        vec![
            json!({ "type": "response.output_item.done", "item": first.clone(), "output_index": 0 }),
            json!({ "type": "response.output_item.done", "item": second.clone(), "output_index": 1 }),
            terminal_response(vec![first.clone(), second]),
        ],
        &mut produced,
        &stream,
        &model,
        &ResponsesStreamOptions::default(),
    )
    .expect_err("the aggregate must be rejected");
    assert!(error.message.contains("Native image generation results exceed the 24 MiB base64 limit"));
    assert_eq!(
        provider_native_raw(&produced),
        vec![json!({ "type": "image_generation_call", "id": "ig_large_1", "status": "malformed" })]
    );
}

#[test]
fn unknown_output_items_surface_as_provider_native_content() {
    let model = model("openai-responses", "openai", "gpt-5.4");
    let web_search = json!({ "type": "web_search_call", "id": "ws_1", "status": "completed", "query": "hello" });
    let file_search = json!({ "type": "file_search_call", "id": "fs_1", "status": "completed", "query": "world" });
    let (message, _) = run_events(
        &model,
        vec![
            json!({ "type": "response.output_item.added", "item": web_search.clone(), "output_index": 0 }),
            json!({ "type": "response.output_item.done", "item": web_search.clone(), "output_index": 0 }),
            json!({ "type": "response.output_item.added", "item": file_search.clone(), "output_index": 1 }),
            json!({ "type": "response.output_item.done", "item": file_search.clone(), "output_index": 1 }),
            terminal_response(vec![web_search.clone(), file_search.clone()]),
        ],
    );
    assert_eq!(provider_native_raw(&message), vec![web_search, file_search]);
}

#[test]
fn image_provider_native_blocks_are_dropped_from_assistant_replay() {
    let model = model("openai-responses", "openai", "gpt-5.4");
    let mut produced = output(&model);
    produced.content = vec![
        ContentBlock::text("kept"),
        ContentBlock::ProviderNative(maho_ai::types::ProviderNativeContent {
            subtype: "web_search_call".into(),
            raw: json!({ "type": "web_search_call", "id": "ws_1" }),
        }),
        ContentBlock::ProviderNative(maho_ai::types::ProviderNativeContent {
            subtype: "image_generation_call".into(),
            raw: json!({ "type": "image_generation_call", "id": "ig_replay", "status": "completed", "result": VALID_IMAGE_BASE64 }),
        }),
    ];

    let input = convert(&model, &context_with(vec![Message::Assistant(Box::new(produced))]));
    let messages = items_of(&input, "message");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"].as_str(), Some("assistant"));
    assert_eq!(messages[0]["content"][0]["text"].as_str(), Some("kept"));
}

#[test]
fn an_absent_error_message_stays_absent() {
    let model = model("openai-responses", "openai", "gpt-5.4");
    let function_call = json!({
        "type": "function_call",
        "id": "fc_test",
        "call_id": "call_test",
        "name": "lookup",
        "arguments": "",
    });
    let (message, _) = run_events(
        &model,
        vec![
            json!({ "type": "response.output_item.added", "item": function_call.clone(), "output_index": 0 }),
            json!({ "type": "response.output_item.done", "item": function_call, "output_index": 0 }),
            json!({ "type": "response.completed", "response": { "id": "resp_test", "status": "completed" } }),
        ],
    );
    assert!(message.error_message.is_none());
}

#[test]
fn function_namespaces_round_trip_from_output_item_done() {
    let model = model("openai-responses", "openai", "gpt-5.4");
    let added =
        json!({ "type": "function_call", "id": "fc_test", "call_id": "call_test", "name": "lookup", "arguments": "" });
    let done = json!({
        "type": "function_call",
        "id": "fc_test",
        "call_id": "call_test",
        "name": "lookup",
        "arguments": "{\"value\":\"hello\"}",
        "namespace": "dynamic_tools",
    });
    let (message, events) = run_events(
        &model,
        vec![
            json!({ "type": "response.output_item.added", "item": added, "output_index": 0 }),
            json!({ "type": "response.output_item.done", "item": done, "output_index": 0 }),
            json!({ "type": "response.completed", "response": { "id": "resp_test", "status": "completed" } }),
        ],
    );

    let ContentBlock::ToolCall(tool_call) = &message.content[0] else { panic!("expected a toolCall block") };
    assert_eq!(tool_call.id, "call_test|fc_test");
    assert_eq!(tool_call.name, "lookup");
    assert_eq!(Value::Object(tool_call.arguments.clone()), json!({ "value": "hello" }));
    assert_eq!(tool_call.namespace.as_deref(), Some("dynamic_tools"));
    assert!(events.iter().any(|event| matches!(event, AssistantMessageEvent::ToolcallEnd { .. })));

    let input = convert(&model, &context_with(vec![Message::Assistant(Box::new(message))]));
    let replayed = items_of(&input, "function_call");
    assert_eq!(replayed.len(), 1);
    assert_eq!(replayed[0]["id"].as_str(), Some("fc_test"));
    assert_eq!(replayed[0]["call_id"].as_str(), Some("call_test"));
    assert_eq!(replayed[0]["arguments"].as_str(), Some("{\"value\":\"hello\"}"));
    assert_eq!(replayed[0]["namespace"].as_str(), Some("dynamic_tools"));
}

#[test]
fn custom_tool_namespaces_round_trip_from_output_item_done() {
    let model = model("openai-responses", "openai", "gpt-5.4");
    let added =
        json!({ "type": "custom_tool_call", "id": "ctc_test", "call_id": "call_test", "name": "query", "input": "" });
    let done = json!({
        "type": "custom_tool_call",
        "id": "ctc_test",
        "call_id": "call_test",
        "name": "query",
        "input": "hello",
        "namespace": "dynamic_tools",
    });
    let options = ResponsesStreamOptions {
        grammar_tool_input_properties: &BTreeMap::from([("query".to_owned(), "input".to_owned())]),
        ..ResponsesStreamOptions::default()
    };
    let (message, _) = run_events_with(
        &model,
        vec![
            json!({ "type": "response.output_item.added", "item": added, "output_index": 0 }),
            json!({ "type": "response.output_item.done", "item": done, "output_index": 0 }),
            json!({ "type": "response.completed", "response": { "id": "resp_test", "status": "completed" } }),
        ],
        options,
    );

    let ContentBlock::ToolCall(tool_call) = &message.content[0] else { panic!("expected a toolCall block") };
    assert_eq!(tool_call.id, "call_test|ctc_test");
    assert_eq!(tool_call.name, "query");
    assert_eq!(Value::Object(tool_call.arguments.clone()), json!({ "input": "hello" }));
    assert_eq!(tool_call.namespace.as_deref(), Some("dynamic_tools"));

    let convert_options = ConvertResponsesMessagesOptions {
        grammar_tool_input_properties: BTreeMap::from([("query".to_owned(), "input".to_owned())]),
        ..ConvertResponsesMessagesOptions::default()
    };
    let input = convert_responses_messages(
        &model,
        &context_with(vec![Message::Assistant(Box::new(message))]),
        &allowed(&ALLOWED_TOOL_CALL_PROVIDERS),
        &convert_options,
    );
    let replayed = items_of(&input, "custom_tool_call");
    assert_eq!(replayed.len(), 1);
    assert_eq!(replayed[0]["id"].as_str(), Some("ctc_test"));
    assert_eq!(replayed[0]["call_id"].as_str(), Some("call_test"));
    assert_eq!(replayed[0]["input"].as_str(), Some("hello"));
    assert_eq!(replayed[0]["namespace"].as_str(), Some("dynamic_tools"));
}

#[test]
fn persisted_tool_call_blocks_carry_parsed_arguments_and_no_scratch_buffer() {
    let model = model("openai-responses", "openai", "gpt-5-mini");
    let added =
        json!({ "type": "function_call", "id": "fc_test", "call_id": "call_test", "name": "edit", "arguments": "" });
    let arguments_json = "{\"path\":\"README.md\",\"content\":\"updated\"}";
    let done = json!({
        "type": "function_call",
        "id": "fc_test",
        "call_id": "call_test",
        "name": "edit",
        "arguments": arguments_json,
    });
    let (message, events) = run_events(
        &model,
        vec![
            json!({ "type": "response.output_item.added", "item": added, "output_index": 0 }),
            json!({ "type": "response.function_call_arguments.delta", "output_index": 0, "delta": "{\"path\":\"README.md\"" }),
            json!({ "type": "response.function_call_arguments.delta", "output_index": 0, "delta": ",\"content\":\"updated\"}" }),
            json!({ "type": "response.function_call_arguments.done", "output_index": 0, "arguments": arguments_json }),
            json!({ "type": "response.output_item.done", "item": done, "output_index": 0 }),
            json!({ "type": "response.completed", "sequence_number": 5, "response": { "id": "resp_test", "status": "completed" } }),
        ],
    );

    assert_eq!(message.content.len(), 1);
    let ContentBlock::ToolCall(persisted) = &message.content[0] else { panic!("expected a toolCall block") };
    assert_eq!(Value::Object(persisted.arguments.clone()), json!({ "path": "README.md", "content": "updated" }));
    assert!(!serde_json::to_string(persisted).expect("tool call JSON").contains("partialJson"));

    let toolcall_end = events
        .iter()
        .find_map(|event| match event {
            AssistantMessageEvent::ToolcallEnd { tool_call, .. } => Some(tool_call),
            _ => None,
        })
        .expect("a toolcall_end event");
    assert_eq!(toolcall_end.arguments, persisted.arguments);
    assert!(!serde_json::to_string(toolcall_end).expect("tool call JSON").contains("partialJson"));
}

#[test]
fn final_custom_tool_input_is_persisted_from_output_item_done() {
    let model = model("openai-responses", "openai", "gpt-5-mini");
    let added = json!({ "type": "custom_tool_call", "call_id": "call_patch", "name": "apply_patch", "input": "" });
    let input = "*** Begin Patch\n*** Add File: sample.txt\n+hello\n*** End Patch";
    let done = json!({ "type": "custom_tool_call", "call_id": "call_patch", "name": "apply_patch", "input": input });
    let (message, events) = run_events(
        &model,
        vec![
            json!({ "type": "response.output_item.added", "item": added, "output_index": 0 }),
            json!({ "type": "response.output_item.done", "item": done, "output_index": 0 }),
        ],
    );

    assert_eq!(message.content.len(), 1);
    let ContentBlock::ToolCall(persisted) = &message.content[0] else { panic!("expected a toolCall block") };
    assert_eq!(Value::Object(persisted.arguments.clone()), json!({ "input": input }));
    assert!(events.iter().any(|event| matches!(event, AssistantMessageEvent::ToolcallEnd { .. })));
}

// =============================================================================
// Azure OpenAI Responses cases
// =============================================================================

fn azure_options(env: &[(&str, &str)]) -> StreamOptions {
    let mut options = StreamOptions::default();
    options.request.api_key = Some("test-api-key".into());
    options.request.env = Some(env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect::<ProviderEnv>());
    options
}

#[test]
fn azure_base_urls_normalize_to_the_openai_v1_root() {
    let cases = [
        ("https://marc-quicktests-resource.cognitiveservices.azure.com", "https://marc-quicktests-resource.cognitiveservices.azure.com/openai/v1"),
        ("https://marc-quicktests-resource.ai.azure.com", "https://marc-quicktests-resource.ai.azure.com/openai/v1"),
        ("https://my-resource.openai.azure.com", "https://my-resource.openai.azure.com/openai/v1"),
        ("https://my-resource.cognitiveservices.azure.com/openai", "https://my-resource.cognitiveservices.azure.com/openai/v1"),
        ("https://my-resource.cognitiveservices.azure.com/openai/v1", "https://my-resource.cognitiveservices.azure.com/openai/v1"),
        ("https://my-resource.services.ai.azure.com/openai/v1/responses", "https://my-resource.services.ai.azure.com/openai/v1"),
        ("https://my-proxy.example.com/v1", "https://my-proxy.example.com/v1"),
        ("https://my-resource.openai.azure.com/openai?api-version=2024-12-01", "https://my-resource.openai.azure.com/openai/v1"),
        ("https://my-proxy.example.com/v1?custom=true", "https://my-proxy.example.com/v1?custom=true"),
    ];
    for (input, expected) in cases {
        assert_eq!(
            azure_openai_responses::normalize_azure_base_url(input).expect("normalizes"),
            expected,
            "for {input}"
        );
    }
    assert!(
        azure_openai_responses::normalize_azure_base_url("not-a-url")
            .expect_err("rejects")
            .contains("Invalid Azure OpenAI base URL")
    );
}

#[test]
fn azure_config_prefers_the_option_then_the_env_and_the_resource_name() {
    let model = azure_model();
    let (base_url, api_version) =
        azure_openai_responses::resolve_azure_config(&model, &azure_options(&[("AZURE_OPENAI_BASE_URL", "https://my-resource.openai.azure.com")]))
            .expect("resolves");
    assert_eq!(base_url, "https://my-resource.openai.azure.com/openai/v1");
    assert_eq!(api_version, "v1");

    let (base_url, _) = azure_openai_responses::resolve_azure_config(
        &model,
        &azure_options(&[("AZURE_OPENAI_RESOURCE_NAME", "my-resource")]),
    )
    .expect("resolves");
    assert_eq!(base_url, "https://my-resource.openai.azure.com/openai/v1");

    let mut options = azure_options(&[]);
    options.extra.insert("azureBaseUrl".into(), json!("https://my-resource.openai.azure.com"));
    options.extra.insert("azureApiVersion".into(), json!("2025-01-01"));
    let (base_url, api_version) = azure_openai_responses::resolve_azure_config(&model, &options).expect("resolves");
    assert_eq!(base_url, "https://my-resource.openai.azure.com/openai/v1");
    assert_eq!(api_version, "2025-01-01");
}

#[test]
fn azure_params_clamp_the_cache_key_and_disable_server_side_storage() {
    let model = azure_model();
    let context = context_with(vec![user_message("hello")]);

    let mut options = azure_options(&[]);
    options.session_id = Some("x".repeat(67));
    let params = azure_openai_responses::build_params(&model, &context, &options, "gpt-5-mini", &BTreeMap::new())
        .expect("params");
    assert_eq!(params["prompt_cache_key"].as_str(), Some("x".repeat(64).as_str()));
    assert_eq!(params["store"], json!(false));

    let mut disabled = azure_options(&[]);
    disabled.cache_retention = Some(CacheRetention::None);
    disabled.session_id = Some("cache-disabled-session".into());
    let params = azure_openai_responses::build_params(&model, &context, &disabled, "gpt-5-mini", &BTreeMap::new())
        .expect("params");
    assert!(params.get("prompt_cache_key").is_none());
}

#[test]
fn azure_tools_honor_supports_strict_mode_false() {
    let mut model = azure_model();
    model.compat = Some(maho_ai::model::ModelCompat(
        json!({ "supportsStrictMode": false }).as_object().cloned().unwrap_or_default(),
    ));
    let mut context = context_with(vec![user_message("hello")]);
    context.tools = Some(vec![Tool {
        name: "preferred".into(),
        description: "Preferred constrained tool".into(),
        parameters: json!({ "type": "object", "properties": { "value": { "type": "string" } } }),
        freeform: None,
        constrained_sampling: Some(maho_ai::types::ConstrainedSampling::Config(
            maho_ai::types::ConstrainedSamplingConfig::JsonSchema {
                strict: maho_ai::types::JsonSchemaStrictness::Prefer,
            },
        )),
    }]);

    let params = azure_openai_responses::build_params(
        &model,
        &context,
        &azure_options(&[]),
        "gpt-5-mini",
        &BTreeMap::new(),
    )
    .expect("params");
    let tools = params["tools"].as_array().expect("tools");
    assert_eq!(tools.len(), 1);
    assert!(tools[0].get("strict").is_none());
}

#[test]
fn azure_default_headers_use_pi_user_agent_and_let_explicit_headers_win() {
    let model = azure_model();
    let headers = azure_openai_responses::build_azure_headers(&model, "test-api-key", None);
    assert_eq!(
        headers["user-agent"].to_str().expect("ascii"),
        maho_ai::utils::pi_user_agent::get_pi_user_agent()
    );
    assert_eq!(headers["api-key"].to_str().expect("ascii"), "test-api-key");

    let overrides: ProviderHeaders = [("User-Agent".to_owned(), Some("custom-agent".to_owned()))].into_iter().collect();
    let headers = azure_openai_responses::build_azure_headers(&model, "test-api-key", Some(&overrides));
    assert_eq!(headers["user-agent"].to_str().expect("ascii"), "custom-agent");
}

#[test]
fn azure_deployment_name_map_overrides_the_model_id() {
    let model = azure_model();
    let options = azure_options(&[("AZURE_OPENAI_DEPLOYMENT_NAME_MAP", "gpt-5-mini=deployed-mini, other=ignored")]);
    assert_eq!(azure_openai_responses::resolve_deployment_name(&model, &options), "deployed-mini");

    let mut explicit = azure_options(&[]);
    explicit.extra.insert("azureDeploymentName".into(), json!("option-deployment"));
    assert_eq!(azure_openai_responses::resolve_deployment_name(&model, &explicit), "option-deployment");
    assert_eq!(
        azure_openai_responses::resolve_deployment_name(&model, &azure_options(&[])),
        "gpt-5-mini"
    );
}

#[test]
fn azure_reasoning_encrypted_content_replay_prefers_the_done_frame() {
    let model = azure_model();
    let done_item = json!({
        "type": "reasoning",
        "id": "rs_done",
        "summary": [],
        "encrypted_content": "from-output-item-done",
    });
    let completed_item = json!({
        "type": "reasoning",
        "id": "rs_done",
        "summary": [],
        "encrypted_content": "from-response-completed",
    });
    let (message, _) = run_events(
        &model,
        vec![
            json!({ "type": "response.output_item.added", "output_index": 0, "sequence_number": 0, "item": { "type": "reasoning", "id": "rs_done", "summary": [] } }),
            json!({ "type": "response.output_item.done", "output_index": 0, "sequence_number": 1, "item": done_item }),
            json!({ "type": "response.completed", "sequence_number": 2, "response": { "id": "resp_test", "status": "completed", "output": [completed_item] } }),
        ],
    );

    let replayed = replay_reasoning(&model, message);
    assert_eq!(replayed["type"].as_str(), Some("reasoning"));
    assert_eq!(replayed["id"].as_str(), Some("rs_done"));
    assert_eq!(replayed["encrypted_content"].as_str(), Some("from-output-item-done"));
}

#[test]
fn azure_reasoning_encrypted_content_is_filled_from_the_terminal_response() {
    let model = azure_model();
    let done_item = json!({ "type": "reasoning", "id": "rs_missing", "summary": [] });
    let completed_item = json!({
        "type": "reasoning",
        "id": "rs_missing",
        "summary": [],
        "encrypted_content": "from-response-completed",
    });
    let (message, _) = run_events(
        &model,
        vec![
            json!({ "type": "response.output_item.added", "output_index": 0, "sequence_number": 0, "item": { "type": "reasoning", "id": "rs_missing", "summary": [] } }),
            json!({ "type": "response.output_item.done", "output_index": 0, "sequence_number": 1, "item": done_item }),
            json!({ "type": "response.completed", "sequence_number": 2, "response": { "id": "resp_test", "status": "completed", "output": [completed_item] } }),
        ],
    );

    let replayed = replay_reasoning(&model, message);
    assert_eq!(replayed["id"].as_str(), Some("rs_missing"));
    assert_eq!(replayed["encrypted_content"].as_str(), Some("from-response-completed"));
}

fn replay_reasoning(model: &Model, message: AssistantMessage) -> Value {
    let context = Context {
        system_prompt: None,
        messages: vec![
            user_message("first"),
            Message::Assistant(Box::new(message)),
            user_message("follow-up"),
        ],
        tools: None,
    };
    let input = convert_responses_messages(
        model,
        &context,
        &allowed(&["azure-openai-responses"]),
        &ConvertResponsesMessagesOptions::default(),
    );
    items_of(&input, "reasoning").first().map(|item| (*item).clone()).expect("a replayed reasoning item")
}

// =============================================================================
// Payload capture cases (a recording fixture server)
// =============================================================================

struct RecordingServer {
    addr: SocketAddr,
    captured: Arc<Mutex<Option<Value>>>,
    captured_headers: Arc<Mutex<Option<String>>>,
    captured_request_line: Arc<Mutex<Option<String>>>,
    handle: tokio::task::JoinHandle<()>,
}

impl RecordingServer {
    async fn start(body: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind recording server");
        let addr = listener.local_addr().expect("recording server addr");
        let captured: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
        let captured_headers: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let captured_request_line: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let sink = captured.clone();
        let header_sink = captured_headers.clone();
        let request_line_sink = captured_request_line.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else { return };
                let body = body.clone();
                let sink = sink.clone();
                let header_sink = header_sink.clone();
                let request_line_sink = request_line_sink.clone();
                tokio::spawn(async move {
                    let mut buffer = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let header_end = loop {
                        let Ok(read) = socket.read(&mut chunk).await else { return };
                        if read == 0 {
                            return;
                        }
                        buffer.extend_from_slice(&chunk[..read]);
                        if let Some(index) = find_subslice(&buffer, b"\r\n\r\n") {
                            break index + 4;
                        }
                    };
                    let raw_headers = String::from_utf8_lossy(&buffer[..header_end]).to_string();
                    *header_sink.lock().expect("headers lock") = Some(raw_headers.clone());
                    *request_line_sink.lock().expect("request line lock") =
                        raw_headers.lines().next().map(str::to_owned);
                    let headers = raw_headers.to_lowercase();
                    let content_length = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    while buffer.len() - header_end < content_length {
                        let Ok(read) = socket.read(&mut chunk).await else { return };
                        if read == 0 {
                            break;
                        }
                        buffer.extend_from_slice(&chunk[..read]);
                    }
                    let body_text = String::from_utf8_lossy(&buffer[header_end..]).to_string();
                    *sink.lock().expect("capture lock") = serde_json::from_str(&body_text).ok();
                    let header = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(header.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                    let _ = socket.flush().await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Self { addr, captured, captured_headers, captured_request_line, handle }
    }

    fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    fn captured(&self) -> Value {
        self.captured.lock().expect("capture lock").clone().expect("a captured payload")
    }

    fn captured_headers(&self) -> String {
        self.captured_headers.lock().expect("headers lock").clone().expect("captured headers")
    }

    /// The request-target of the first request line (`POST /v1/responses HTTP/1.1`).
    fn captured_path(&self) -> String {
        let line = self
            .captured_request_line
            .lock()
            .expect("request line lock")
            .clone()
            .expect("a captured request line");
        line.split_whitespace().nth(1).unwrap_or_default().to_owned()
    }
}

impl Drop for RecordingServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

fn completed_sse(id: &str) -> Vec<u8> {
    let frame = json!({
        "type": "response.completed",
        "response": {
            "id": id,
            "status": "completed",
            "output": [],
            "usage": { "input_tokens": 0, "output_tokens": 0, "total_tokens": 0, "input_tokens_details": { "cached_tokens": 0 } },
        },
    });
    format!("data: {frame}\n\ndata: [DONE]\n\n").into_bytes()
}

fn proxy_model(compat: Option<Value>) -> Model {
    let mut model = model("openai-responses", "openai", "gpt-5.5");
    model.base_url = "https://quotio.example/v1".into();
    model.compat = compat.map(|compat| {
        maho_ai::model::ModelCompat(compat.as_object().cloned().unwrap_or_default())
    });
    model
}

async fn capture_payload(model: &Model, context: &Context, options: StreamOptions) -> Value {
    let server = RecordingServer::start(completed_sse("resp_web_search_compat")).await;
    let mut model = model.clone();
    model.base_url = server.base_url();
    let events = collect(openai_responses::stream(&model, context, Some(options))).await;
    assert!(
        matches!(events.last(), Some(AssistantMessageEvent::Done { .. })),
        "the fixture completes the turn: {events:?}"
    );
    server.captured()
}

#[tokio::test]
async fn custom_responses_endpoints_strip_native_web_search_by_default() {
    let model = proxy_model(None);
    let context = hello_context();
    let mut options = responses_options();
    let on_payload: maho_ai::types::OnPayload = Arc::new(|payload: &Value, _model: &Model, _meta| {
        let mut next = payload.as_object().cloned().unwrap_or_default();
        next.insert("include".into(), json!(["reasoning.encrypted_content", "web_search_call.action.sources"]));
        next.insert("tool_choice".into(), json!({ "type": "web_search_preview" }));
        next.insert("tools".into(), json!([{ "type": "function", "name": "keeper" }, { "type": "web_search_preview" }]));
        Some(Value::Object(next))
    });
    options.request.on_payload = Some(on_payload);

    let payload = capture_payload(&model, &context, options).await;
    assert_eq!(payload["tools"], json!([{ "type": "function", "name": "keeper" }]));
    assert_eq!(payload["include"], json!(["reasoning.encrypted_content"]));
    assert!(payload.get("tool_choice").is_none());
}

#[tokio::test]
async fn custom_responses_endpoints_keep_native_web_search_when_they_opt_in() {
    let model = proxy_model(Some(json!({ "supportsWebSearchPreview": true })));
    let context = hello_context();
    let mut options = responses_options();
    let on_payload: maho_ai::types::OnPayload = Arc::new(|payload: &Value, _model: &Model, _meta| {
        let mut next = payload.as_object().cloned().unwrap_or_default();
        next.insert("include".into(), json!(["reasoning.encrypted_content", "web_search_call.action.sources"]));
        next.insert("tool_choice".into(), json!({ "type": "web_search_preview" }));
        next.insert("tools".into(), json!([{ "type": "function", "name": "keeper" }, { "type": "web_search_preview" }]));
        Some(Value::Object(next))
    });
    options.request.on_payload = Some(on_payload);

    let payload = capture_payload(&model, &context, options).await;
    assert_eq!(payload["tools"], json!([{ "type": "function", "name": "keeper" }, { "type": "web_search_preview" }]));
    assert_eq!(payload["include"], json!(["reasoning.encrypted_content", "web_search_call.action.sources"]));
    assert_eq!(payload["tool_choice"], json!({ "type": "web_search_preview" }));
}

#[tokio::test]
async fn azure_forwards_a_provider_specific_tool_choice_with_its_tools() {
    let server = RecordingServer::start(completed_sse("resp_azure")).await;
    let mut model = azure_model();
    model.base_url = server.base_url();
    let mut context = context_with(vec![user_message("Summarize this")]);
    context.tools = Some(vec![Tool {
        name: "read".into(),
        description: "Read a file".into(),
        parameters: json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
        freeform: None,
        constrained_sampling: None,
    }]);
    let mut options = azure_options(&[]);
    options.extra.insert("toolChoice".into(), json!("required"));

    let events = collect(azure_openai_responses::stream(&model, &context, Some(options))).await;
    assert!(matches!(events.last(), Some(AssistantMessageEvent::Done { .. })));

    let payload = server.captured();
    assert_eq!(payload["tool_choice"], json!("required"));
    assert_eq!(payload["tools"].as_array().expect("tools").len(), 1);
}

#[tokio::test]
async fn azure_forwards_a_provider_neutral_tool_choice_from_simple_options() {
    let server = RecordingServer::start(completed_sse("resp_azure_simple")).await;
    let mut model = azure_model();
    model.base_url = server.base_url();
    let mut context = context_with(vec![user_message("Summarize this")]);
    context.tools = Some(vec![Tool {
        name: "read".into(),
        description: "Read a file".into(),
        parameters: json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
        freeform: None,
        constrained_sampling: None,
    }]);
    let mut simple = SimpleStreamOptions::default();
    simple.stream.request.api_key = Some("test-key".into());
    simple.tool_choice = Some(ToolChoice::None);

    let events = collect(azure_openai_responses::stream_simple(&model, &context, Some(simple))).await;
    assert!(matches!(events.last(), Some(AssistantMessageEvent::Done { .. })));

    let payload = server.captured();
    assert_eq!(payload["tool_choice"], json!("none"));
    assert_eq!(payload["tools"].as_array().expect("tools").len(), 1);
}

#[test]
fn responses_tool_conversion_covers_function_freeform_and_grammar_tools() {
    let function_tool = Tool {
        name: "read".into(),
        description: "Read a file".into(),
        parameters: json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
        freeform: None,
        constrained_sampling: None,
    };
    let freeform_tool = Tool {
        name: "patch".into(),
        description: "Apply a patch".into(),
        parameters: json!({ "type": "object" }),
        freeform: Some(maho_ai::types::FreeformToolFormat {
            kind: "grammar".into(),
            syntax: "lark".into(),
            definition: "start: /[\\s\\S]*/".into(),
        }),
        constrained_sampling: None,
    };
    let grammar_tool = Tool {
        name: "query".into(),
        description: "Query".into(),
        parameters: json!({ "type": "object", "properties": { "q": { "type": "string" } }, "required": ["q"] }),
        freeform: None,
        constrained_sampling: Some(maho_ai::types::ConstrainedSampling::Config(
            maho_ai::types::ConstrainedSamplingConfig::Grammar {
                variants: BTreeMap::from([(
                    maho_ai::types::GrammarFormat::OpenaiLark,
                    "start: \"query\"".to_owned(),
                )]),
            },
        )),
    };

    let tools = convert_responses_tools(
        &[function_tool, freeform_tool, grammar_tool],
        &ConvertResponsesToolsOptions {
            supports_strict_mode: Some(true),
            supports_openai_grammar_tools: Some(true),
            ..ConvertResponsesToolsOptions::default()
        },
    )
    .expect("tools convert");

    assert_eq!(tools[0]["type"].as_str(), Some("function"));
    assert_eq!(tools[0]["strict"], json!(false));
    assert_eq!(tools[1]["type"].as_str(), Some("custom"));
    assert_eq!(tools[1]["format"]["type"].as_str(), Some("grammar"));
    assert_eq!(tools[2]["type"].as_str(), Some("custom"));
    assert_eq!(tools[2]["format"]["syntax"].as_str(), Some("lark"));
    assert_eq!(tools[2]["format"]["definition"].as_str(), Some("start: \"query\""));
}

#[test]
fn responses_message_conversion_keeps_the_system_role_and_image_inputs() {
    let mut model = model("openai-responses", "openai", "gpt-4o-mini");
    model.reasoning = false;
    let mut context = context_with(vec![Message::User(UserMessage {
        content: UserContent::Blocks(vec![
            ContentBlock::text("describe"),
            ContentBlock::Image(maho_ai::types::ImageContent {
                data: "AA==".into(),
                mime_type: "image/png".into(),
            }),
        ]),
        timestamp: 0,
    })]);

    let input = convert(&model, &context);
    assert_eq!(input[0]["role"].as_str(), Some("system"));
    assert_eq!(input[1]["role"].as_str(), Some("user"));
    assert_eq!(input[1]["content"][0]["type"].as_str(), Some("input_text"));
    assert_eq!(input[1]["content"][1]["type"].as_str(), Some("input_image"));
    assert_eq!(
        input[1]["content"][1]["image_url"].as_str(),
        Some("data:image/png;base64,AA==")
    );

    model.reasoning = true;
    context.system_prompt = Some("You are concise.".into());
    let input = convert(&model, &context);
    assert_eq!(input[0]["role"].as_str(), Some("developer"));
}

#[test]
fn responses_params_default_to_short_cache_and_omit_none_retention() {
    let model = model("openai-responses", "openai", "gpt-5.5");
    let context = hello_context();
    let mut options = responses_options();
    options.session_id = Some("session-1".into());
    let compat = openai_responses::get_compat(&model, None);
    let params =
        openai_responses::build_params(&model, &context, &options, &compat, &BTreeMap::new()).expect("params");
    assert_eq!(params["store"], json!(false));
    assert_eq!(params["stream"], json!(true));
    assert_eq!(params["prompt_cache_key"].as_str(), Some("session-1"));
    assert!(params.get("prompt_cache_retention").is_none());

    let mut long = responses_options();
    long.cache_retention = Some(CacheRetention::Long);
    let params = openai_responses::build_params(&model, &context, &long, &compat, &BTreeMap::new()).expect("params");
    assert_eq!(params["prompt_cache_retention"].as_str(), Some("24h"));

    let mut disabled = responses_options();
    disabled.cache_retention = Some(CacheRetention::None);
    disabled.session_id = Some("session-1".into());
    let params = openai_responses::build_params(&model, &context, &disabled, &compat, &BTreeMap::new()).expect("params");
    assert!(params.get("prompt_cache_key").is_none());
}

#[test]
fn responses_session_affinity_is_detected_and_applied_to_headers() {
    let mut model = model("openai-responses", "openai", "gpt-5.5");
    model.base_url = "https://api.openai.com/v1".into();
    assert_eq!(
        openai_responses::detect_session_affinity_format(&model),
        maho_ai::openai_responses_compat::SessionAffinityFormat::Openai
    );
    let compat = openai_responses::get_compat(&model, None);
    let headers = openai_responses::build_responses_headers(
        &model,
        &hello_context(),
        "sk-test",
        None,
        Some("session-1"),
        &compat,
    );
    assert_eq!(headers["session_id"].to_str().expect("ascii"), "session-1");
    assert_eq!(headers["x-client-request-id"].to_str().expect("ascii"), "session-1");
    assert_eq!(headers["authorization"].to_str().expect("ascii"), "Bearer sk-test");

    let mut openrouter = model.clone();
    openrouter.provider = "openrouter".into();
    openrouter.base_url = "https://openrouter.ai/api/v1".into();
    assert_eq!(
        openai_responses::detect_session_affinity_format(&openrouter),
        maho_ai::openai_responses_compat::SessionAffinityFormat::Openrouter
    );
    let compat = openai_responses::get_compat(&openrouter, None);
    let headers = openai_responses::build_responses_headers(
        &openrouter,
        &hello_context(),
        "sk-test",
        None,
        Some("session-1"),
        &compat,
    );
    assert_eq!(headers["x-session-id"].to_str().expect("ascii"), "session-1");
    assert!(headers.get("session_id").is_none());

    let mut no_session = model.clone();
    no_session.compat = Some(maho_ai::model::ModelCompat(
        json!({ "sessionAffinityFormat": "openai-nosession" }).as_object().cloned().unwrap_or_default(),
    ));
    let compat = openai_responses::get_compat(&no_session, None);
    let headers = openai_responses::build_responses_headers(
        &no_session,
        &hello_context(),
        "sk-test",
        None,
        Some("session-1"),
        &compat,
    );
    assert!(headers.get("session_id").is_none());
    assert_eq!(headers["x-client-request-id"].to_str().expect("ascii"), "session-1");
}

#[test]
fn responses_service_tier_pricing_scales_the_cost_fields() {
    let model = model("openai-responses", "openai", "gpt-5.5");
    let mut usage = Usage {
        cost: maho_ai::types::UsageCost { input: 2.0, output: 4.0, cache_read: 1.0, cache_write: 0.5, total: 0.0 },
        ..Usage::default()
    };
    openai_responses::apply_service_tier_pricing(&mut usage, Some("priority"), &model);
    assert_eq!(usage.cost.input, 5.0);
    assert_eq!(usage.cost.output, 10.0);
    assert_eq!(usage.cost.cache_read, 2.5);
    assert_eq!(usage.cost.cache_write, 1.25);
    assert_eq!(usage.cost.total, 18.75);

    let mut untouched = Usage {
        cost: maho_ai::types::UsageCost { input: 1.0, output: 1.0, cache_read: 1.0, cache_write: 1.0, total: 4.0 },
        ..Usage::default()
    };
    openai_responses::apply_service_tier_pricing(&mut untouched, None, &model);
    assert_eq!(untouched.cost.total, 4.0);
    assert_eq!(openai_responses::get_service_tier_cost_multiplier(&model, Some("flex")), 0.5);
}

#[tokio::test]
async fn openai_responses_surfaces_a_missing_api_key_as_a_typed_error() {
    let model = model("openai-responses", "openai", "gpt-5.5");
    let events = collect(openai_responses::stream(&model, &hello_context(), None)).await;
    let last = events.last().expect("a terminal event");
    let AssistantMessageEvent::Error { error, .. } = last else { panic!("expected an error event") };
    assert_eq!(error.error_message.as_deref(), Some("No API key for provider: openai"));
}

#[tokio::test]
async fn azure_responses_surfaces_a_missing_api_key_as_a_typed_error() {
    let model = azure_model();
    let events = collect(azure_openai_responses::stream(&model, &hello_context(), None)).await;
    let last = events.last().expect("a terminal event");
    let AssistantMessageEvent::Error { error, .. } = last else { panic!("expected an error event") };
    assert_eq!(error.error_message.as_deref(), Some("No API key for provider: azure-openai-responses"));
}

// =============================================================================
// test/openai-responses-terminal-event.test.ts
// =============================================================================

/// Every event senpi's `AssistantMessageEvent` gives a `partial` field, in push order.
fn partial_stop_reasons(events: &[AssistantMessageEvent]) -> Vec<StopReason> {
    events
        .iter()
        .filter_map(|event| match event {
            AssistantMessageEvent::Start { partial }
            | AssistantMessageEvent::TextStart { partial, .. }
            | AssistantMessageEvent::TextDelta { partial, .. }
            | AssistantMessageEvent::TextEnd { partial, .. }
            | AssistantMessageEvent::ThinkingStart { partial, .. }
            | AssistantMessageEvent::ThinkingDelta { partial, .. }
            | AssistantMessageEvent::ThinkingEnd { partial, .. }
            | AssistantMessageEvent::ToolcallStart { partial, .. }
            | AssistantMessageEvent::ToolcallDelta { partial, .. }
            | AssistantMessageEvent::ToolcallEnd { partial, .. } => Some(partial.stop_reason),
            AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. } => None,
        })
        .collect()
}

fn process(model: &Model, events: Vec<Value>) -> (AssistantMessage, Result<(), maho_ai::api::openai_responses_shared::ResponsesStreamError>) {
    let mut produced = output(model);
    let stream = maho_ai::types::AssistantMessageEventStream::assistant();
    let result = process_responses_events(events, &mut produced, &stream, model, &ResponsesStreamOptions::default());
    (produced, result)
}

fn early_eof_events() -> Vec<Value> {
    vec![
        json!({ "type": "response.created", "sequence_number": 0, "response": { "id": "resp_early_eof" } }),
        json!({
            "type": "response.output_item.added",
            "sequence_number": 1,
            "output_index": 0,
            "item": { "type": "reasoning", "id": "rs_early_eof", "summary": [] }
        }),
        json!({
            "type": "response.reasoning_text.delta",
            "sequence_number": 2,
            "output_index": 0,
            "content_index": 0,
            "item_id": "rs_early_eof",
            "delta": "partial reasoning before the stream ends"
        }),
    ]
}

fn phased_message_events(first: &str, second: &str, terminal: Option<&str>) -> Vec<Value> {
    let mut events = vec![
        json!({
            "type": "response.output_item.added",
            "sequence_number": 0,
            "output_index": 0,
            "item": { "type": "message", "id": "msg_phase", "role": "assistant", "status": "in_progress", "content": [], "phase": first }
        }),
        json!({
            "type": "response.output_item.done",
            "sequence_number": 1,
            "output_index": 0,
            "item": {
                "type": "message", "id": "msg_phase", "role": "assistant", "status": "completed",
                "content": [{ "type": "output_text", "text": "answer", "annotations": [] }],
                "phase": second
            }
        }),
    ];
    events.push(match terminal {
        Some("incomplete") => json!({
            "type": "response.incomplete",
            "sequence_number": 2,
            "response": { "id": "resp_phase", "status": "incomplete", "incomplete_details": { "reason": "max_output_tokens" } }
        }),
        Some(other) => panic!("unsupported terminal status {other}"),
        None => json!({
            "type": "response.completed",
            "sequence_number": 2,
            "response": { "id": "resp_phase", "status": "completed" }
        }),
    });
    events
}

#[test]
fn terminal_events_reject_a_stream_that_ends_before_a_terminal_response_event() {
    let model = model("openai-responses", "openai", "gpt-5-mini");
    let (_, result) = process(&model, early_eof_events());
    let error = result.expect_err("an early EOF is rejected");
    assert_eq!(error.message, "OpenAI Responses stream ended before a terminal response event");
}

#[tokio::test]
async fn terminal_events_emit_an_error_final_result_when_the_wrapper_stream_ends_early() {
    let payloads: Vec<String> = early_eof_events().iter().map(|event| event.to_string()).collect();
    let frames: Vec<(Option<&str>, &str)> = payloads.iter().map(|payload| (None, payload.as_str())).collect();
    let server = MockFixtureServer::start(Fixture::sse(frames)).await;
    let mut model = model("openai-responses", "openai", "gpt-5-mini");
    model.base_url = server.base_url().to_owned();

    let events = collect(openai_responses::stream(&model, &hello_context(), Some(responses_options()))).await;
    let AssistantMessageEvent::Start { partial } = events.first().expect("a start event") else {
        panic!("expected a start event")
    };
    assert_eq!(partial.stop_reason, StopReason::Pending);
    let last = events.last().expect("a terminal event");
    let AssistantMessageEvent::Error { error, .. } = last else { panic!("expected an error event: {last:?}") };
    assert_eq!(error.stop_reason, StopReason::Error);
    assert_eq!(error.error_message.as_deref(), Some("OpenAI Responses stream ended before a terminal response event"));
}

#[test]
fn terminal_events_track_message_phases() {
    for (phases, expected) in [
        (["commentary", "commentary"], vec![StopReason::Pending, StopReason::Pending]),
        (["final_answer", "final_answer"], vec![StopReason::Stop, StopReason::Stop]),
        (["commentary", "final_answer"], vec![StopReason::Pending, StopReason::Stop]),
    ] {
        let model = model("openai-responses", "openai", "gpt-5-mini");
        let (message, events) = run_events(&model, phased_message_events(phases[0], phases[1], None));
        assert_eq!(partial_stop_reasons(&events), expected, "phases {phases:?}");
        assert_eq!(message.stop_reason, StopReason::Stop);
    }
}

#[test]
fn terminal_events_replace_a_provisional_final_answer_stop_with_an_incomplete_reason() {
    let model = model("openai-responses", "openai", "gpt-5-mini");
    let (message, events) = run_events(&model, phased_message_events("final_answer", "final_answer", Some("incomplete")));
    assert_eq!(partial_stop_reasons(&events), vec![StopReason::Stop, StopReason::Stop]);
    assert_eq!(message.stop_reason, StopReason::Length);
}

#[test]
fn terminal_events_finalize_completed_as_stop() {
    let model = model("openai-responses", "openai", "gpt-5-mini");
    let (message, _) = process(
        &model,
        vec![json!({
            "type": "response.completed",
            "sequence_number": 0,
            "response": {
                "id": "resp_completed",
                "status": "completed",
                "usage": {
                    "input_tokens": 20,
                    "output_tokens": 7,
                    "total_tokens": 27,
                    "input_tokens_details": { "cached_tokens": 2, "cache_write_tokens": 3 }
                }
            }
        })],
    );
    assert_eq!(message.response_id.as_deref(), Some("resp_completed"));
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("completed"));
    assert_eq!(message.usage.input, 15);
    assert_eq!(message.usage.output, 7);
    assert_eq!(message.usage.cache_read, 2);
    assert_eq!(message.usage.cache_write, 3);
    assert_eq!(message.usage.total_tokens, 27);
}

fn incomplete_events(reason: &str) -> Vec<Value> {
    vec![json!({
        "type": "response.incomplete",
        "sequence_number": 0,
        "response": {
            "id": "resp_incomplete",
            "status": "incomplete",
            "incomplete_details": { "reason": reason },
            "usage": {
                "input_tokens": 30,
                "output_tokens": 12,
                "total_tokens": 42,
                "input_tokens_details": { "cached_tokens": 5 }
            }
        }
    })]
}

#[test]
fn terminal_events_finalize_incomplete_as_length_stops() {
    let model = model("openai-responses", "openai", "gpt-5-mini");
    let (message, _) = process(&model, incomplete_events("max_output_tokens"));
    assert_eq!(message.response_id.as_deref(), Some("resp_incomplete"));
    assert_eq!(message.stop_reason, StopReason::Length);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("incomplete.max_output_tokens"));
    assert_eq!(message.usage.input, 25);
    assert_eq!(message.usage.output, 12);
    assert_eq!(message.usage.cache_read, 5);
    assert_eq!(message.usage.cache_write, 0);
    assert_eq!(message.usage.total_tokens, 42);
}

#[test]
fn terminal_events_finalize_content_filtered_incomplete_as_non_retryable_errors() {
    let model = model("openai-responses", "openai", "gpt-5-mini");
    let (message, _) = process(&model, incomplete_events("content_filter"));
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("incomplete.content_filter"));
    assert_eq!(message.error_message.as_deref(), Some("Response incomplete: content_filter"));
}

#[test]
fn terminal_events_preserve_unknown_incomplete_reasons_as_non_retryable_errors() {
    let model = model("openai-responses", "openai", "gpt-5-mini");
    let (message, _) = process(&model, incomplete_events("max_time_limit"));
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("incomplete.max_time_limit"));
    assert_eq!(message.error_message.as_deref(), Some("Response incomplete: max_time_limit"));
}

#[test]
fn terminal_events_reject_failed_terminal_events_with_the_provider_error() {
    let model = model("openai-responses", "openai", "gpt-5-mini");
    let (message, result) = process(
        &model,
        vec![json!({
            "type": "response.failed",
            "sequence_number": 0,
            "response": { "id": "resp_failed", "status": "failed", "error": { "code": "server_error", "message": "boom" } }
        })],
    );
    let error = result.expect_err("a failed terminal event is rejected");
    assert_eq!(error.message, "server_error: boom");
    assert_eq!(message.raw_stop_reason.as_deref(), Some("failed"));
}

// =============================================================================
// test/assistant-message-frame.test.ts - the one case owned by the api/ wire code
// =============================================================================

#[tokio::test]
async fn assistant_message_frames_round_trip_responses_content_supplied_only_by_end_events() {
    use maho_ai::utils::assistant_message_frame::{reduce_assistant_message_frames, AssistantMessageFrameEncoder};

    let model = model("openai-responses", "openai", "test-model");
    let mut produced = output(&model);
    produced.timestamp = 1;

    let events = vec![
        json!({
            "type": "response.output_item.added",
            "sequence_number": 0,
            "output_index": 0,
            "item": { "type": "message", "id": "msg", "role": "assistant", "status": "in_progress", "content": [] }
        }),
        json!({
            "type": "response.output_item.done",
            "sequence_number": 1,
            "output_index": 0,
            "item": {
                "type": "message", "id": "msg", "role": "assistant", "status": "completed",
                "content": [{ "type": "output_text", "text": "final text", "annotations": [] }]
            }
        }),
        json!({
            "type": "response.output_item.added",
            "sequence_number": 2,
            "output_index": 1,
            "item": { "type": "function_call", "id": "fc", "call_id": "call", "name": "lookup", "arguments": "" }
        }),
        json!({
            "type": "response.output_item.done",
            "sequence_number": 3,
            "output_index": 1,
            "item": { "type": "function_call", "id": "fc", "call_id": "call", "name": "lookup", "arguments": "{\"query\":\"pi\"}" }
        }),
        json!({
            "type": "response.completed",
            "sequence_number": 4,
            "response": { "id": "response", "status": "completed", "output": [] }
        }),
    ];

    let stream = maho_ai::types::AssistantMessageEventStream::assistant();
    let mut encoder = AssistantMessageFrameEncoder::new();
    let mut frames = vec![
        encoder
            .encode(&AssistantMessageEvent::Start { partial: produced.clone() })
            .expect("the start event encodes")
            .expect("a start frame"),
    ];

    process_responses_events(events, &mut produced, &stream, &model, &ResponsesStreamOptions::default())
        .expect("stream processed");
    for event in stream.queue() {
        if let Some(frame) = encoder.encode(&event).expect("every pushed event encodes") {
            frames.push(frame);
        }
    }

    let reduced = reduce_assistant_message_frames(frames.iter()).expect("reduces").expect("a message");
    assert_eq!(reduced.content, produced.content);
}

// =============================================================================
// test/openai-config-update.test.ts
// =============================================================================

fn config_update_model(api: &str, provider: &str, id: &str) -> Model {
    let mut model = model(api, provider, id);
    model.reasoning = true;
    model.input = vec![InputModality::Text];
    model
}

fn convert_plain(model: &Model, messages: Vec<Message>) -> Vec<Value> {
    convert_responses_messages(
        model,
        &Context { system_prompt: None, messages, tools: None },
        &allowed(&ALLOWED_TOOL_CALL_PROVIDERS),
        &ConvertResponsesMessagesOptions::default(),
    )
}

fn configuration_update(effort: &str, timestamp: i64) -> Message {
    Message::ConfigurationUpdate(maho_ai::types::ConfigurationUpdateMessage {
        content: Vec::new(),
        effort: effort.to_owned(),
        timestamp,
    })
}

#[test]
fn configuration_updates_place_the_update_immediately_before_the_next_user_message() {
    let model = config_update_model("openai-responses", "openai", "gpt-6-astra");
    let mut produced = output(&model);
    produced.content = vec![ContentBlock::text("answer")];
    produced.stop_reason = StopReason::Stop;
    let messages = vec![
        Message::Assistant(Box::new(produced)),
        configuration_update("high", 2),
        Message::User(UserMessage { content: UserContent::Text("next".into()), timestamp: 3 }),
    ];

    let input = convert_plain(&model, messages);
    assert_eq!(input.len(), 3, "{input:?}");
    assert_eq!(input[0]["role"], json!("assistant"));
    assert_eq!(input[1]["type"], json!("configuration_update"));
    assert_eq!(input[1]["reasoning"], json!({ "effort": "high" }));
    assert_eq!(input[2]["role"], json!("user"));
}

#[test]
fn configuration_updates_replace_an_adjacent_update_rather_than_adding_another() {
    let model = config_update_model("openai-codex-responses", "chatgpt-subscription", "gpt-6-astra");
    let messages = vec![configuration_update("low", 1), configuration_update("high", 2), user_message("next")];

    let input = convert_plain(&model, messages);
    assert_eq!(input.len(), 2, "{input:?}");
    assert_eq!(input[0]["type"], json!("configuration_update"));
    assert_eq!(input[0]["reasoning"], json!({ "effort": "high" }));
    assert_eq!(input[1]["role"], json!("user"));
}

#[test]
fn configuration_updates_do_not_update_unsupported_models_or_providers() {
    let messages = vec![configuration_update("high", 1)];
    assert_eq!(convert_plain(&config_update_model("openai-responses", "openai", "gpt-5"), messages.clone()), Vec::<Value>::new());
    assert_eq!(
        convert_plain(&config_update_model("openai-responses", "opencode", "gpt-6-astra"), messages),
        Vec::<Value>::new()
    );
}

// =============================================================================
// test/openai-responses-compat.test.ts
// =============================================================================

/// senpi's `getModel(provider, id)` over the generated catalog.
fn builtin(provider: &str, id: &str) -> Model {
    maho_ai::models_generated::get_builtin_model(provider, id)
        .unwrap_or_else(|error| panic!("{provider}/{id}: {error}"))
        .clone()
}

fn compat_flag(model: &Model, key: &str) -> Option<Value> {
    model.compat.as_ref().and_then(|compat| compat.0.get(key).cloned())
}

fn with_compat(model: &Model, key: &str, value: Value) -> Model {
    let mut model = model.clone();
    let mut compat = model.compat.clone().unwrap_or_default().0;
    compat.insert(key.to_owned(), value);
    model.compat = Some(maho_ai::model::ModelCompat(compat));
    model
}

fn header_value(headers: &str, name: &str) -> Option<String> {
    headers.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim().to_owned())
    })
}

async fn capture_headers(model: &Model, context: &Context, options: StreamOptions) -> String {
    let server = RecordingServer::start(completed_sse("resp_headers")).await;
    let mut model = model.clone();
    model.base_url = server.base_url();
    let events = collect(openai_responses::stream(&model, context, Some(options))).await;
    assert!(
        matches!(events.last(), Some(AssistantMessageEvent::Done { .. })),
        "the fixture completes the turn: {events:?}"
    );
    server.captured_headers()
}

fn ping_tool() -> Tool {
    Tool {
        name: "ping".into(),
        description: "Ping".into(),
        parameters: json!({ "type": "object", "properties": { "value": { "type": "string" } }, "required": ["value"] }),
        freeform: None,
        constrained_sampling: None,
    }
}

#[tokio::test]
async fn compat_omits_reasoning_when_no_reasoning_is_requested() {
    let model = builtin("github-copilot", "gpt-5-mini");
    let payload = capture_payload(&model, &hello_context(), responses_options()).await;
    assert!(payload.get("reasoning").is_none(), "{payload}");
}

#[tokio::test]
async fn compat_omits_standalone_same_model_reasoning_replay_when_no_reasoning_is_requested() {
    let model = builtin("openai", "gpt-5.4");
    let mut previous = output(&model);
    previous.content = vec![
        ContentBlock::Thinking(ThinkingContent {
            thinking: "prior reasoning".into(),
            started_at: None,
            ended_at: None,
            thinking_signature: Some(
                json!({ "id": "rs_123", "type": "reasoning", "summary": [], "status": "completed" }).to_string(),
            ),
            redacted: None,
        }),
        ContentBlock::text("previous answer"),
    ];
    previous.stop_reason = StopReason::Stop;
    let context = Context {
        system_prompt: None,
        messages: vec![
            user_message("first turn"),
            Message::Assistant(Box::new(previous)),
            user_message("follow-up"),
        ],
        tools: None,
    };

    let payload = capture_payload(&model, &context, responses_options()).await;
    let items = payload["input"].as_array().expect("input");
    assert!(
        !items.iter().any(|item| item.get("type").and_then(Value::as_str) == Some("reasoning")),
        "no standalone reasoning replay: {payload}"
    );
}

#[tokio::test]
async fn compat_forwards_required_tool_choice() {
    let model = builtin("openai", "gpt-5.4");
    let mut context = hello_context();
    context.messages = vec![user_message("Do not call ping. Respond with text instead.")];
    context.tools = Some(vec![ping_tool()]);
    let mut options = responses_options();
    options.extra.insert("toolChoice".into(), json!("required"));

    let payload = capture_payload(&model, &context, options).await;
    assert_eq!(payload["tool_choice"], json!("required"));
    assert_eq!(payload["tools"][0]["name"], json!("ping"));
}

#[tokio::test]
async fn compat_sets_strict_mode_explicitly_for_cloudflare_responses_tools() {
    let model = builtin("cloudflare-ai-gateway", "gpt-5.6-sol");
    assert_eq!(compat_flag(&model, "supportsStrictMode"), Some(json!(true)));
    let mut context = hello_context();
    context.messages = vec![user_message("Use a tool.")];
    context.tools = Some(vec![
        Tool {
            name: "ordinary".into(),
            description: "An ordinary tool".into(),
            parameters: json!({
                "type": "object",
                "properties": { "path": { "type": "string" }, "offset": { "type": "number" } },
                "required": ["path"]
            }),
            freeform: None,
            constrained_sampling: None,
        },
        Tool {
            name: "constrained".into(),
            description: "A constrained tool".into(),
            parameters: json!({ "type": "object", "properties": { "value": { "type": "string" } }, "required": ["value"] }),
            freeform: None,
            constrained_sampling: Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::JsonSchema {
                strict: JsonSchemaStrictness::Prefer,
            })),
        },
    ]);

    let payload = capture_payload(&model, &context, responses_options()).await;
    let tools = payload["tools"].as_array().expect("tools");
    assert_eq!(tools[0]["name"], json!("ordinary"));
    assert_eq!(tools[0]["strict"], json!(false));
    assert_eq!(tools[1]["name"], json!("constrained"));
    assert_eq!(tools[1]["strict"], json!(true));
}

#[tokio::test]
async fn compat_sends_none_reasoning_effort_when_no_reasoning_is_requested() {
    for id in [
        "gpt-5.1",
        "gpt-5.2",
        "gpt-5.3-codex",
        "gpt-5.4",
        "gpt-5.4-mini",
        "gpt-5.4-nano",
        "gpt-5.5",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
    ] {
        let model = builtin("openai", id);
        let payload = capture_payload(&model, &hello_context(), responses_options()).await;
        assert_eq!(payload["reasoning"], json!({ "effort": "none" }), "{id}: {payload}");
    }
}

#[tokio::test]
async fn compat_omits_reasoning_effort_when_off_is_unsupported() {
    for id in ["gpt-5", "gpt-5-mini", "gpt-5-nano", "gpt-5-pro", "gpt-5.2-pro", "gpt-5.4-pro", "gpt-5.5-pro"] {
        let model = builtin("openai", id);
        let payload = capture_payload(&model, &hello_context(), responses_options()).await;
        assert!(payload.get("reasoning").is_none(), "{id}: {payload}");
    }
}

#[tokio::test]
async fn compat_sets_cache_affinity_headers_for_official_responses_requests_with_a_session_id() {
    let model = builtin("openai", "gpt-5.4");
    let mut options = responses_options();
    options.session_id = Some("session-123".into());

    let headers = capture_headers(&model, &hello_context(), options).await;
    assert_eq!(header_value(&headers, "session_id").as_deref(), Some("session-123"));
    assert_eq!(header_value(&headers, "x-client-request-id").as_deref(), Some("session-123"));
}

#[tokio::test]
async fn compat_clamps_prompt_cache_key_to_the_64_character_limit() {
    let model = builtin("openai", "gpt-5.4");
    let mut options = responses_options();
    options.session_id = Some("x".repeat(67));

    let payload = capture_payload(&model, &hello_context(), options).await;
    assert_eq!(payload["prompt_cache_key"], json!("x".repeat(64)));
}

#[tokio::test]
async fn compat_sets_cache_affinity_headers_for_proxy_responses_requests_with_a_session_id() {
    let mut model = builtin("openai", "gpt-5.4");
    model.provider = "opencode".into();
    model.base_url = "https://proxy.example.com/v1".into();
    let mut options = responses_options();
    options.session_id = Some("session-123".into());

    let headers = capture_headers(&model, &hello_context(), options).await;
    assert_eq!(header_value(&headers, "session_id").as_deref(), Some("session-123"));
    assert_eq!(header_value(&headers, "x-client-request-id").as_deref(), Some("session-123"));
}

#[tokio::test]
async fn compat_uses_openrouter_session_affinity_header_when_configured() {
    let model = with_compat(
        &{
            let mut model = builtin("openai", "gpt-5.4");
            model.provider = "proxy".into();
            model.base_url = "https://proxy.example.com/v1".into();
            model
        },
        "sessionAffinityFormat",
        json!("openrouter"),
    );
    let mut options = responses_options();
    options.session_id = Some("session-proxy".into());

    let headers = capture_headers(&model, &hello_context(), options.clone()).await;
    assert!(header_value(&headers, "session_id").is_none());
    assert!(header_value(&headers, "x-client-request-id").is_none());
    assert_eq!(header_value(&headers, "x-session-id").as_deref(), Some("session-proxy"));

    let payload = capture_payload(&model, &hello_context(), options).await;
    assert!(payload.get("session_id").is_none());
    assert_eq!(payload["prompt_cache_key"], json!("session-proxy"));
}

#[tokio::test]
async fn compat_auto_detects_openrouter_session_affinity_header() {
    let mut model = builtin("openai", "gpt-5.4");
    model.provider = "openrouter".into();
    model.base_url = "https://openrouter.ai/api/v1".into();
    let mut options = responses_options();
    options.session_id = Some("session-openrouter".into());

    let headers = capture_headers(&model, &hello_context(), options.clone()).await;
    assert!(header_value(&headers, "session_id").is_none());
    assert!(header_value(&headers, "x-client-request-id").is_none());
    assert_eq!(header_value(&headers, "x-session-id").as_deref(), Some("session-openrouter"));

    let payload = capture_payload(&model, &hello_context(), options).await;
    assert!(payload.get("session_id").is_none());
    assert_eq!(payload["prompt_cache_key"], json!("session-openrouter"));
}

#[tokio::test]
async fn compat_uses_no_session_format_when_configured() {
    let model = with_compat(
        &{
            let mut model = builtin("openai", "gpt-5.4");
            model.provider = "proxy".into();
            model.base_url = "https://proxy.example.com/v1".into();
            model
        },
        "sessionAffinityFormat",
        json!("openai-nosession"),
    );
    let mut options = responses_options();
    options.session_id = Some("session-proxy".into());

    let headers = capture_headers(&model, &hello_context(), options.clone()).await;
    assert!(header_value(&headers, "session_id").is_none());
    assert_eq!(header_value(&headers, "x-client-request-id").as_deref(), Some("session-proxy"));
    assert!(header_value(&headers, "x-session-id").is_none());

    let payload = capture_payload(&model, &hello_context(), options).await;
    assert!(payload.get("session_id").is_none());
    assert_eq!(payload["prompt_cache_key"], json!("session-proxy"));
}

#[tokio::test]
async fn compat_uses_no_session_format_for_opencode_responses_models() {
    let model = builtin("opencode", "gpt-5.4");
    assert_eq!(compat_flag(&model, "sessionAffinityFormat"), Some(json!("openai-nosession")));
    let mut options = responses_options();
    options.session_id = Some("session-opencode".into());

    let headers = capture_headers(&model, &hello_context(), options.clone()).await;
    assert!(header_value(&headers, "session_id").is_none());
    assert_eq!(header_value(&headers, "x-client-request-id").as_deref(), Some("session-opencode"));
    assert!(header_value(&headers, "x-session-id").is_none());

    let payload = capture_payload(&model, &hello_context(), options).await;
    assert_eq!(payload["prompt_cache_key"], json!("session-opencode"));
}

#[tokio::test]
async fn compat_can_omit_the_session_id_header_while_preserving_other_affinity_data() {
    let mut model = builtin("openai", "gpt-5.4");
    model.provider = "opencode".into();
    model.base_url = "https://proxy.example.com/v1".into();
    let model = with_compat(&model, "sessionAffinityFormat", json!("openai-nosession"));
    let mut options = responses_options();
    options.session_id = Some("session-123".into());

    let headers = capture_headers(&model, &hello_context(), options.clone()).await;
    assert!(header_value(&headers, "session_id").is_none());
    assert_eq!(header_value(&headers, "x-client-request-id").as_deref(), Some("session-123"));

    let payload = capture_payload(&model, &hello_context(), options).await;
    assert_eq!(payload["prompt_cache_key"], json!("session-123"));
}

#[tokio::test]
async fn compat_lets_explicit_headers_override_the_default_cache_affinity_headers() {
    let model = builtin("openai", "gpt-5.4");
    let mut options = responses_options();
    options.session_id = Some("session-123".into());
    options.request.headers = Some(ProviderHeaders::from([
        ("session_id".to_owned(), Some("override-session".to_owned())),
        ("x-client-request-id".to_owned(), Some("override-request".to_owned())),
    ]));

    let headers = capture_headers(&model, &hello_context(), options).await;
    assert_eq!(header_value(&headers, "session_id").as_deref(), Some("override-session"));
    assert_eq!(header_value(&headers, "x-client-request-id").as_deref(), Some("override-request"));
}

#[tokio::test]
async fn compat_omits_cache_affinity_headers_when_cache_retention_is_none() {
    let model = builtin("openai", "gpt-5.4");
    let mut options = responses_options();
    options.session_id = Some("session-123".into());
    options.cache_retention = Some(CacheRetention::None);

    let headers = capture_headers(&model, &hello_context(), options).await;
    assert!(header_value(&headers, "session_id").is_none());
    assert!(header_value(&headers, "x-client-request-id").is_none());
}

fn service_tier_sse(service_tier: &str, tokens: u64) -> Vec<u8> {
    let frame = json!({
        "type": "response.completed",
        "response": {
            "status": "completed",
            "service_tier": service_tier,
            "usage": {
                "input_tokens": tokens,
                "output_tokens": tokens,
                "total_tokens": tokens * 2,
                "input_tokens_details": { "cached_tokens": 0 }
            }
        }
    });
    format!("data: {frame}\n\ndata: [DONE]\n\n").into_bytes()
}

fn assert_close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() <= expected.abs() * 1e-9 + 1e-12, "{actual} != {expected}");
}

#[tokio::test]
async fn compat_applies_service_tier_cost_multipliers() {
    let cases = [
        ("gpt-5.4", "priority", 2.0),
        ("gpt-5.5", "priority", 2.5),
        ("gpt-5.5", "flex", 0.5),
        ("gpt-6-astra", "fast", 2.0),
    ];
    for (id, service_tier, multiplier) in cases {
        let model = builtin("openai", id);
        let server = RecordingServer::start(service_tier_sse(service_tier, 100_000)).await;
        let mut served = model.clone();
        served.base_url = server.base_url();
        let mut options = responses_options();
        options.extra.insert("serviceTier".into(), json!(service_tier));

        let events = collect(openai_responses::stream(&served, &hello_context(), Some(options))).await;
        let AssistantMessageEvent::Done { message, .. } = events.last().expect("a done event") else {
            panic!("{id} {service_tier}: expected a done event: {events:?}")
        };
        let token_scale = 100_000.0 / 1_000_000.0;
        assert_close(message.usage.cost.input, model.cost.input * multiplier * token_scale);
        assert_close(message.usage.cost.output, model.cost.output * multiplier * token_scale);
        assert_close(message.usage.cost.total, (model.cost.input + model.cost.output) * multiplier * token_scale);
    }
}

#[tokio::test]
async fn compat_sends_max_output_tokens_by_default() {
    let model = builtin("openai", "gpt-5.4");
    let mut options = responses_options();
    options.max_tokens = Some(1024);

    let payload = capture_payload(&model, &hello_context(), options).await;
    assert_eq!(payload["max_output_tokens"], json!(1024));
}

#[tokio::test]
async fn compat_omits_max_output_tokens_when_supports_max_output_tokens_is_false() {
    let model = with_compat(&builtin("openai", "gpt-5.4"), "supportsMaxOutputTokens", json!(false));
    let mut options = responses_options();
    options.max_tokens = Some(1024);

    let payload = capture_payload(&model, &hello_context(), options).await;
    assert!(payload.get("max_output_tokens").is_none(), "{payload}");
}

// =============================================================================
// test/openai-responses-thinking-matrix.test.ts
// =============================================================================

fn simple_options() -> SimpleStreamOptions {
    SimpleStreamOptions { stream: responses_options(), ..SimpleStreamOptions::default() }
}

async fn capture_simple_payload(model: &Model, context: &Context, simple: SimpleStreamOptions) -> Value {
    let server = RecordingServer::start(completed_sse("resp_simple_capture")).await;
    let mut model = model.clone();
    model.base_url = server.base_url();
    let events = collect(openai_responses::stream_simple(&model, context, Some(simple))).await;
    assert!(
        matches!(events.last(), Some(AssistantMessageEvent::Done { .. })),
        "the fixture completes the turn: {events:?}"
    );
    server.captured()
}

async fn capture_azure_simple_payload(model: &Model, context: &Context, simple: SimpleStreamOptions) -> Value {
    let server = RecordingServer::start(completed_sse("resp_azure_simple_capture")).await;
    let mut model = model.clone();
    model.base_url = server.base_url();
    let events = collect(azure_openai_responses::stream_simple(&model, context, Some(simple))).await;
    assert!(
        matches!(events.last(), Some(AssistantMessageEvent::Done { .. })),
        "the fixture completes the turn: {events:?}"
    );
    server.captured()
}

fn azure_simple_options() -> SimpleStreamOptions {
    SimpleStreamOptions { stream: azure_options(&[]), ..SimpleStreamOptions::default() }
}

fn thinking_context() -> Context {
    Context { system_prompt: None, messages: vec![user_message("Hello")], tools: None }
}

#[tokio::test]
async fn thinking_matrix_preserves_explicit_gpt_5_6_max_effort_mapping() {
    let model = builtin("openai", "gpt-5.6-sol");
    let simple = SimpleStreamOptions { reasoning: Some(ThinkingLevel::Max), ..simple_options() };

    let payload = capture_simple_payload(&model, &thinking_context(), simple).await;
    assert_eq!(payload["reasoning"], json!({ "effort": "max", "summary": "auto" }));
}

#[tokio::test]
async fn thinking_matrix_preserves_max_effort_for_a_map_less_model() {
    let mut model = builtin("openai", "gpt-5.6-sol");
    model.provider = "codex-lb".into();
    model.thinking_level_map = None;
    let simple = SimpleStreamOptions { reasoning: Some(ThinkingLevel::Max), ..simple_options() };

    let payload = capture_simple_payload(&model, &thinking_context(), simple).await;
    assert_eq!(payload["reasoning"], json!({ "effort": "max", "summary": "auto" }));
}

#[tokio::test]
async fn thinking_matrix_preserves_max_effort_for_a_map_less_model_on_azure_responses() {
    let mut model = builtin("azure-openai-responses", "gpt-5.6-sol");
    model.base_url = "http://127.0.0.1:9".into();
    model.thinking_level_map = None;
    let simple = SimpleStreamOptions { reasoning: Some(ThinkingLevel::Max), ..azure_simple_options() };

    let payload = capture_azure_simple_payload(&model, &thinking_context(), simple).await;
    // senpi's assertion is a `toMatchObject` subset match on `{ reasoning: { effort: "max" } }`.
    assert_eq!(payload["reasoning"]["effort"], json!("max"), "{payload}");
}

#[tokio::test]
async fn thinking_matrix_preserves_azures_explicit_gpt_5_6_max_effort_mapping() {
    let mut model = builtin("azure-openai-responses", "gpt-5.6-sol");
    model.base_url = "http://127.0.0.1:9".into();
    let simple = SimpleStreamOptions { reasoning: Some(ThinkingLevel::Max), ..azure_simple_options() };

    let payload = capture_azure_simple_payload(&model, &thinking_context(), simple).await;
    assert_eq!(payload["reasoning"], json!({ "effort": "max", "summary": "auto" }));
}

#[tokio::test]
async fn thinking_matrix_omits_azure_reasoning_when_thinking_cannot_be_disabled() {
    let mut model = builtin("azure-openai-responses", "gpt-5.6-sol");
    model.base_url = "http://127.0.0.1:9".into();

    let payload = capture_azure_simple_payload(&model, &thinking_context(), azure_simple_options()).await;
    assert!(payload.get("reasoning").is_none(), "{payload}");
}

#[tokio::test]
async fn thinking_matrix_omits_xai_reasoning_when_thinking_cannot_be_disabled() {
    let model = builtin("xai", "grok-4.5");

    let payload = capture_simple_payload(&model, &thinking_context(), simple_options()).await;
    assert!(payload.get("reasoning").is_none(), "{payload}");
}

#[tokio::test]
async fn thinking_matrix_does_not_send_an_unavailable_explicit_effort() {
    let model = builtin("openai", "gpt-5.1");
    let mut options = responses_options();
    options.extra.insert("reasoningEffort".into(), json!("minimal"));

    let payload = capture_payload(&model, &thinking_context(), options).await;
    assert!(payload.get("reasoning").is_none(), "{payload}");
}

#[tokio::test]
async fn thinking_matrix_does_not_send_an_unavailable_summary_default_effort() {
    let model = builtin("openai", "gpt-5-pro");
    let mut options = responses_options();
    options.extra.insert("reasoningSummary".into(), json!("auto"));

    let payload = capture_payload(&model, &thinking_context(), options).await;
    assert!(payload.get("reasoning").is_none(), "{payload}");
}

#[tokio::test]
async fn thinking_matrix_does_not_send_an_unavailable_explicit_azure_effort() {
    let mut model = builtin("azure-openai-responses", "gpt-5.5-pro");
    model.base_url = "http://127.0.0.1:9".into();
    let mut options = azure_options(&[]);
    options.extra.insert("reasoningEffort".into(), json!("minimal"));

    let payload = capture_azure_payload(&model, &thinking_context(), options).await;
    assert!(payload.get("reasoning").is_none(), "{payload}");
}

async fn capture_azure_payload(model: &Model, context: &Context, options: StreamOptions) -> Value {
    let server = RecordingServer::start(completed_sse("resp_azure_capture")).await;
    let mut model = model.clone();
    model.base_url = server.base_url();
    let events = collect(azure_openai_responses::stream(&model, context, Some(options))).await;
    assert!(
        matches!(events.last(), Some(AssistantMessageEvent::Done { .. })),
        "the fixture completes the turn: {events:?}"
    );
    server.captured()
}

// =============================================================================
// test/cache-retention.test.ts (OpenAI Responses cases)
// =============================================================================

fn env_options(pairs: &[(&str, &str)]) -> StreamOptions {
    let mut options = responses_options();
    options.request.env =
        Some(pairs.iter().map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect::<ProviderEnv>());
    options
}

#[tokio::test]
async fn cache_retention_sets_prompt_cache_retention_for_a_non_openai_base_url_by_default() {
    let mut model = builtin("openai", "gpt-4o-mini");
    model.base_url = "https://my-proxy.example.com/v1".into();

    let payload = capture_payload(&model, &hello_context(), env_options(&[("PI_CACHE_RETENTION", "long")])).await;
    assert_eq!(payload["prompt_cache_retention"], json!("24h"));
}

#[tokio::test]
async fn cache_retention_omits_prompt_cache_retention_when_long_retention_is_unsupported() {
    let model = with_compat(&builtin("openai", "gpt-4o-mini"), "supportsLongCacheRetention", json!(false));
    let mut options = responses_options();
    options.cache_retention = Some(CacheRetention::Long);
    options.session_id = Some("session-compat-false".into());

    let payload = capture_payload(&model, &hello_context(), options).await;
    assert!(payload.get("prompt_cache_retention").is_none(), "{payload}");
}

#[tokio::test]
async fn cache_retention_none_omits_prompt_cache_key_and_disables_implicit_writes() {
    let model = builtin("openai", "gpt-5.6-sol");
    let mut options = responses_options();
    options.cache_retention = Some(CacheRetention::None);
    options.session_id = Some("session-1".into());

    let payload = capture_payload(&model, &hello_context(), options).await;
    assert!(payload.get("prompt_cache_key").is_none(), "{payload}");
    assert!(payload.get("prompt_cache_retention").is_none(), "{payload}");
    assert_eq!(payload["prompt_cache_options"], json!({ "mode": "explicit" }));
}

#[tokio::test]
async fn cache_retention_omits_prompt_cache_options_for_models_that_reject_it() {
    let model = builtin("openai", "gpt-4o-mini");
    let mut options = responses_options();
    options.cache_retention = Some(CacheRetention::None);
    options.session_id = Some("session-1".into());

    let payload = capture_payload(&model, &hello_context(), options).await;
    assert!(payload.get("prompt_cache_key").is_none(), "{payload}");
    assert!(payload.get("prompt_cache_options").is_none(), "{payload}");
}

#[tokio::test]
async fn cache_retention_uses_the_supported_long_cache_field_per_model() {
    for (id, retention, cache_options) in [
        ("gpt-4o-mini", Some(json!("24h")), None),
        ("gpt-6-astra", None, Some(json!({ "ttl": "30m" }))),
    ] {
        let model = builtin("openai", id);
        let mut options = responses_options();
        options.cache_retention = Some(CacheRetention::Long);
        options.session_id = Some("session-2".into());

        let payload = capture_payload(&model, &hello_context(), options).await;
        assert_eq!(payload["prompt_cache_key"], json!("session-2"), "{id}");
        match retention {
            Some(value) => assert_eq!(payload["prompt_cache_retention"], value, "{id}"),
            None => assert!(payload.get("prompt_cache_retention").is_none(), "{id}: {payload}"),
        }
        match cache_options {
            Some(value) => assert_eq!(payload["prompt_cache_options"], value, "{id}"),
            None => assert!(payload.get("prompt_cache_options").is_none(), "{id}: {payload}"),
        }
    }
}

#[tokio::test]
async fn cache_retention_uses_the_model_default_when_options_omit_it() {
    let mut model = builtin("openai", "gpt-4o-mini");
    model.cache_retention = Some(CacheRetention::Long);
    let mut options = responses_options();
    options.session_id = Some("model-default-session".into());

    let payload = capture_payload(&model, &hello_context(), options).await;
    assert_eq!(payload["prompt_cache_key"], json!("model-default-session"));
    assert_eq!(payload["prompt_cache_retention"], json!("24h"));
}

// =============================================================================
// test/xai-responses.test.ts (Responses cases)
// =============================================================================

async fn capture_xai_request(model: &Model, context: &Context, options: StreamOptions) -> RecordingServer {
    let server = RecordingServer::start(completed_sse("resp_xai_test")).await;
    let mut served = model.clone();
    served.base_url = server.base_url();
    let events = collect(openai_responses::stream(&served, context, Some(options))).await;
    assert!(
        matches!(events.last(), Some(AssistantMessageEvent::Done { .. })),
        "the fixture completes the turn: {events:?}"
    );
    server
}

fn xai_options() -> StreamOptions {
    let mut options = responses_options();
    options.request.api_key = Some("xai-test-token".into());
    options
}

#[tokio::test]
async fn xai_uses_responses_with_bearer_auth_and_xai_compatible_request_fields() {
    let model = builtin("xai", "grok-4.5");
    let context = Context {
        system_prompt: Some("You are a careful coding assistant.".into()),
        messages: vec![user_message("hello")],
        tools: None,
    };
    let mut options = xai_options();
    options.session_id = Some("pi-session-123".into());
    options.cache_retention = Some(CacheRetention::Long);
    options.extra.insert("reasoningEffort".into(), json!("medium"));

    let server = capture_xai_request(&model, &context, options).await;
    assert_eq!(server.captured_path(), "/v1/responses");
    let headers = server.captured_headers();
    assert_eq!(header_value(&headers, "authorization").as_deref(), Some("Bearer xai-test-token"));
    assert_eq!(
        header_value(&headers, "user-agent").as_deref(),
        Some(maho_ai::utils::pi_user_agent::get_pi_user_agent().as_str())
    );
    assert_eq!(header_value(&headers, "session_id").as_deref(), Some("pi-session-123"));

    let payload = server.captured();
    assert_eq!(payload["model"], json!("grok-4.5"));
    assert_eq!(payload["store"], json!(false));
    assert_eq!(payload["stream"], json!(true));
    assert_eq!(payload["prompt_cache_key"], json!("pi-session-123"));
    assert_eq!(payload["reasoning"]["effort"], json!("medium"), "{payload}");
    assert_eq!(payload["include"], json!(["reasoning.encrypted_content"]));
    assert!(payload.get("prompt_cache_retention").is_none(), "{payload}");
    let input = payload["input"].as_array().expect("input");
    assert!(
        input.iter().any(|item| item["role"] == json!("developer")
            && item["content"] == json!("You are a careful coding assistant.")),
        "the system prompt becomes a developer message: {payload}"
    );
}

#[tokio::test]
async fn xai_requests_encrypted_reasoning_without_an_effort_override() {
    let model = builtin("xai", "grok-4.5");

    let server = capture_xai_request(&model, &hello_context(), xai_options()).await;
    let payload = server.captured();
    assert_eq!(payload["model"], json!("grok-4.5"));
    assert_eq!(payload["store"], json!(false));
    assert_eq!(payload["include"], json!(["reasoning.encrypted_content"]));
    assert!(payload.get("reasoning").is_none(), "{payload}");
}

#[tokio::test]
async fn xai_uses_responses_for_grok_4_6_with_xhigh_effort_and_encrypted_reasoning() {
    let model = builtin("xai", "grok-4.6");
    let context = Context {
        system_prompt: Some("You are a careful coding assistant.".into()),
        messages: vec![user_message("hello")],
        tools: None,
    };
    let mut options = xai_options();
    options.extra.insert("reasoningEffort".into(), json!("xhigh"));

    let server = capture_xai_request(&model, &context, options).await;
    assert_eq!(server.captured_path(), "/v1/responses");
    let payload = server.captured();
    assert_eq!(payload["model"], json!("grok-4.6"));
    assert_eq!(payload["store"], json!(false));
    assert_eq!(payload["stream"], json!(true));
    assert_eq!(payload["reasoning"]["effort"], json!("xhigh"), "{payload}");
    assert_eq!(payload["include"], json!(["reasoning.encrypted_content"]));
}

#[tokio::test]
async fn xai_uses_responses_for_grok_4_7_with_xhigh_effort_and_encrypted_reasoning() {
    let model = builtin("xai", "grok-4.7");
    let context = Context {
        system_prompt: Some("You are a careful coding assistant.".into()),
        messages: vec![user_message("hello")],
        tools: None,
    };
    let mut options = xai_options();
    options.extra.insert("reasoningEffort".into(), json!("xhigh"));

    let server = capture_xai_request(&model, &context, options).await;
    assert_eq!(server.captured_path(), "/v1/responses");
    let payload = server.captured();
    assert_eq!(payload["model"], json!("grok-4.7"));
    assert_eq!(payload["store"], json!(false));
    assert_eq!(payload["stream"], json!(true));
    assert_eq!(payload["reasoning"]["effort"], json!("xhigh"), "{payload}");
    assert_eq!(payload["include"], json!(["reasoning.encrypted_content"]));
}

#[tokio::test]
async fn xai_uses_responses_for_grok_4_3() {
    let model = builtin("xai", "grok-4.3");
    let mut options = xai_options();
    options.extra.insert("reasoningEffort".into(), json!("low"));

    let server = capture_xai_request(&model, &hello_context(), options).await;
    assert_eq!(server.captured_path(), "/v1/responses");
    let payload = server.captured();
    assert_eq!(payload["model"], json!("grok-4.3"));
    assert_eq!(payload["store"], json!(false));
    assert_eq!(payload["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(payload["reasoning"]["effort"], json!("low"), "{payload}");
}

#[tokio::test]
async fn xai_uses_pis_user_agent_by_default_for_responses_requests() {
    let mut model = builtin("xai", "grok-4.5");
    model.provider = "openai".into();
    model.base_url = "https://api.openai.com/v1".into();

    let server = capture_xai_request(&model, &hello_context(), responses_options()).await;
    let headers = server.captured_headers();
    assert_eq!(
        header_value(&headers, "user-agent").as_deref(),
        Some(maho_ai::utils::pi_user_agent::get_pi_user_agent().as_str())
    );
}

#[tokio::test]
async fn xai_lets_explicit_headers_override_the_default_responses_user_agent() {
    let model = builtin("xai", "grok-4.5");
    let mut options = xai_options();
    options.request.headers = Some(ProviderHeaders::from([(
        "User-Agent".to_owned(),
        Some("custom-agent".to_owned()),
    )]));

    let server = capture_xai_request(&model, &hello_context(), options).await;
    let headers = server.captured_headers();
    assert_eq!(header_value(&headers, "user-agent").as_deref(), Some("custom-agent"));
}

// =============================================================================
// test/pre-generation-error.test.ts - the two [OI] Responses adapters
// =============================================================================

#[tokio::test]
async fn pre_generation_error_surfaces_missing_auth_for_the_responses_adapters() {
    // senpi throws synchronously from `streamSimple`; the port's `stream_simple` returns the same
    // error as an already-settled error stream instead (no panic, same message).
    let openai = model("openai-responses", "test-provider", "test-model");
    let events = collect(openai_responses::stream_simple(&openai, &hello_context(), Some(SimpleStreamOptions::default()))).await;
    let AssistantMessageEvent::Error { error, .. } = events.last().expect("a terminal event") else {
        panic!("expected an error event: {events:?}")
    };
    assert_eq!(error.error_message.as_deref(), Some("No API key for provider: test-provider"));

    let azure = model("azure-openai-responses", "test-provider", "test-model");
    let events =
        collect(azure_openai_responses::stream_simple(&azure, &hello_context(), Some(SimpleStreamOptions::default()))).await;
    let AssistantMessageEvent::Error { error, .. } = events.last().expect("a terminal event") else {
        panic!("expected an error event: {events:?}")
    };
    assert_eq!(error.error_message.as_deref(), Some("No API key for provider: test-provider"));
}

// =============================================================================
// test/model-switch-replay-fixtures.ts + the Responses rows of
// test/model-switch-replay-characterization.test.ts and test/model-switch-replay-policy-table.test.ts
// =============================================================================

const PATCH: &str = "*** Begin Patch\n*** Update File: src/a.ts\n@@\n-old\n+new\n*** End Patch";

fn apply_patch_tool() -> Tool {
    Tool {
        name: "apply_patch".into(),
        description: "Apply a patch".into(),
        parameters: json!({
            "type": "object",
            "properties": { "input": { "type": "string" } },
            "required": ["input"]
        }),
        freeform: Some(FreeformToolFormat {
            kind: "grammar".into(),
            syntax: "lark".into(),
            definition: "start: \"patch\"".into(),
        }),
        constrained_sampling: None,
    }
}

fn patch_history(source_api: &str) -> Vec<Message> {
    let mut previous = output(&model(source_api, "openai", "gpt-source"));
    previous.content = vec![ContentBlock::ToolCall(ToolCall {
        id: "call_patch".into(),
        name: "apply_patch".into(),
        arguments: json!({ "input": PATCH }).as_object().cloned().unwrap_or_default(),
        incomplete: None,
        error_message: None,
        thought_signature: None,
        namespace: None,
    })];
    previous.stop_reason = StopReason::ToolUse;
    previous.timestamp = 1;
    vec![
        Message::Assistant(Box::new(previous)),
        Message::ToolResult(ToolResultMessage {
            tool_call_id: "call_patch".into(),
            tool_name: "apply_patch".into(),
            content: vec![ContentBlock::text("Done!")],
            details: None,
            usage: None,
            added_tool_names: None,
            is_error: false,
            timestamp: 2,
        }),
    ]
}

fn responses_convert(model: &Model, messages: Vec<Message>, tools: Option<Vec<Tool>>) -> Vec<Value> {
    convert_responses_messages(
        model,
        &Context { system_prompt: None, messages, tools },
        &allowed(&["openai"]),
        &ConvertResponsesMessagesOptions::default(),
    )
}

fn assert_custom_patch_replay(items: &[Value], model_id: &str) {
    assert_eq!(items.len(), 2, "{model_id}: {items:?}");
    assert_eq!(items[0]["type"], json!("custom_tool_call"), "{model_id}");
    assert_eq!(items[0]["call_id"], json!("call_patch"), "{model_id}");
    assert_eq!(items[0]["name"], json!("apply_patch"), "{model_id}");
    assert_eq!(items[0]["input"], json!(PATCH), "{model_id}");
    assert_eq!(items[1]["type"], json!("custom_tool_call_output"), "{model_id}");
    assert_eq!(items[1]["call_id"], json!("call_patch"), "{model_id}");
    assert_eq!(items[1]["name"], json!("apply_patch"), "{model_id}");
    assert_eq!(items[1]["output"], json!("Done!"), "{model_id}");
}

fn assert_function_patch_replay(items: &[Value], model_id: &str) {
    assert_eq!(items.len(), 2, "{model_id}: {items:?}");
    assert_eq!(items[0]["type"], json!("function_call"), "{model_id}");
    assert_eq!(items[0]["call_id"], json!("call_patch"), "{model_id}");
    assert_eq!(items[0]["name"], json!("apply_patch"), "{model_id}");
    assert_eq!(items[0]["arguments"], json!(json!({ "input": PATCH }).to_string()), "{model_id}");
    assert_eq!(items[1]["type"], json!("function_call_output"), "{model_id}");
    assert_eq!(items[1]["call_id"], json!("call_patch"), "{model_id}");
    assert_eq!(items[1]["output"], json!("Done!"), "{model_id}");
}

#[test]
fn model_switch_s5_serializes_apply_patch_according_to_the_current_tool_declaration() {
    let model = model("openai-responses", "openai", "gpt-target");
    let custom = responses_convert(&model, patch_history("openai-responses"), Some(vec![apply_patch_tool()]));
    let function = responses_convert(&model, patch_history("openai-responses"), None);

    assert_custom_patch_replay(&custom, "custom");
    assert_function_patch_replay(&function, "function");
}

#[test]
fn model_switch_s5b_upgrades_a_function_era_recording_to_custom_tool_call() {
    let model = model("openai-responses", "openai", "gpt-target");
    let replay = responses_convert(&model, patch_history("openai-completions"), Some(vec![apply_patch_tool()]));

    assert_custom_patch_replay(&replay, "upgrade");
}

#[test]
fn model_switch_s5b_stringifies_recorded_arguments_that_lack_an_input_string() {
    let model = model("openai-responses", "openai", "gpt-target");
    let history = patch_history("openai-completions");
    let Message::Assistant(mut assistant) = history[0].clone() else { panic!("assistant fixture") };
    assistant.content = vec![ContentBlock::ToolCall(ToolCall {
        id: "call_patch".into(),
        name: "apply_patch".into(),
        arguments: json!({ "patch": PATCH }).as_object().cloned().unwrap_or_default(),
        incomplete: None,
        error_message: None,
        thought_signature: None,
        namespace: None,
    })];
    let messages = vec![Message::Assistant(assistant), history[1].clone()];

    let replay = responses_convert(&model, messages, Some(vec![apply_patch_tool()]));

    assert_eq!(replay.len(), 2, "{replay:?}");
    assert_eq!(replay[0]["type"], json!("custom_tool_call"));
    assert_eq!(replay[0]["input"], json!(json!({ "patch": PATCH }).to_string()));
    assert_eq!(replay[1]["type"], json!("custom_tool_call_output"));
    assert_eq!(replay[1]["output"], json!("Done!"));
}

#[test]
fn model_switch_s5c_replays_mixed_edit_and_apply_patch_history_in_both_truth_table_branches() {
    let model = model("openai-responses", "openai", "gpt-target");
    let history = patch_history("openai-responses");
    let Message::Assistant(mut assistant) = history[0].clone() else { panic!("assistant fixture") };
    assistant.content = vec![
        ContentBlock::ToolCall(ToolCall {
            id: "call_edit".into(),
            name: "edit".into(),
            arguments: json!({ "path": "src/a.ts", "edits": [{ "oldText": "old", "newText": "new" }] })
                .as_object()
                .cloned()
                .unwrap_or_default(),
            incomplete: None,
            error_message: None,
            thought_signature: None,
            namespace: None,
        }),
        ContentBlock::ToolCall(ToolCall {
            id: "call_patch".into(),
            name: "apply_patch".into(),
            arguments: json!({ "input": PATCH }).as_object().cloned().unwrap_or_default(),
            incomplete: None,
            error_message: None,
            thought_signature: None,
            namespace: None,
        }),
    ];
    let mixed = vec![
        Message::Assistant(assistant),
        Message::ToolResult(ToolResultMessage {
            tool_call_id: "call_edit".into(),
            tool_name: "edit".into(),
            content: vec![ContentBlock::text("Edited")],
            details: None,
            usage: None,
            added_tool_names: None,
            is_error: false,
            timestamp: 2,
        }),
        history[1].clone(),
    ];

    let with_freeform = responses_convert(&model, mixed.clone(), Some(vec![apply_patch_tool()]));
    let without_freeform = responses_convert(&model, mixed, None);

    assert_eq!(with_freeform.len(), 4, "{with_freeform:?}");
    assert_eq!(with_freeform[0]["type"], json!("function_call"));
    assert_eq!(with_freeform[0]["call_id"], json!("call_edit"));
    assert_eq!(with_freeform[0]["name"], json!("edit"));
    assert_eq!(with_freeform[1]["type"], json!("custom_tool_call"));
    assert_eq!(with_freeform[1]["call_id"], json!("call_patch"));
    assert_eq!(with_freeform[1]["input"], json!(PATCH));
    assert_eq!(with_freeform[2]["type"], json!("function_call_output"));
    assert_eq!(with_freeform[2]["call_id"], json!("call_edit"));
    assert_eq!(with_freeform[2]["output"], json!("Edited"));
    assert_eq!(with_freeform[3]["type"], json!("custom_tool_call_output"));
    assert_eq!(with_freeform[3]["output"], json!("Done!"));

    assert_eq!(without_freeform.len(), 4, "{without_freeform:?}");
    assert_eq!(without_freeform[0]["type"], json!("function_call"));
    assert_eq!(without_freeform[0]["call_id"], json!("call_edit"));
    assert_eq!(without_freeform[1]["type"], json!("function_call"));
    assert_eq!(without_freeform[1]["call_id"], json!("call_patch"));
    assert_eq!(without_freeform[1]["arguments"], json!(json!({ "input": PATCH }).to_string()));
    assert_eq!(without_freeform[2]["type"], json!("function_call_output"));
    assert_eq!(without_freeform[3]["type"], json!("function_call_output"));
    assert_eq!(without_freeform[3]["call_id"], json!("call_patch"));
    assert_eq!(without_freeform[3]["output"], json!("Done!"));
}

#[test]
fn model_switch_policy_row_azure_responses_applies_the_shared_converter_truth_table() {
    let model = model("azure-openai-responses", "azure-openai", "gpt-target");
    let custom = responses_convert(&model, patch_history("openai-responses"), Some(vec![apply_patch_tool()]));
    let function = responses_convert(&model, patch_history("openai-responses"), None);

    assert_custom_patch_replay(&custom, "azure custom");
    assert_function_patch_replay(&function, "azure function");
}

#[test]
fn model_switch_policy_row_codex_responses_applies_the_shared_converter_truth_table() {
    let model = model("openai-codex-responses", "openai", "gpt-target");
    let custom = responses_convert(&model, patch_history("openai-responses"), Some(vec![apply_patch_tool()]));
    let function = responses_convert(&model, patch_history("openai-responses"), None);

    assert_custom_patch_replay(&custom, "codex custom");
    assert_function_patch_replay(&function, "codex function");
}

// =============================================================================
// test/provider-error-body-regression.test.ts (the openai-responses tier)
// =============================================================================

#[tokio::test]
async fn provider_error_body_keeps_the_prefix_and_surfaces_the_body_for_openai_responses() {
    let fixture = Fixture::json(axum::http::StatusCode::FORBIDDEN, json!({ "error": "blocked by gateway WAF" }).to_string());
    let server = MockFixtureServer::start(fixture).await;
    let mut model = model("openai-responses", "openai", "gpt-test");
    model.base_url = server.base_url().to_owned();

    let events = collect(openai_responses::stream(&model, &hello_context(), Some(responses_options()))).await;
    let last = events.last().expect("a terminal event");
    let AssistantMessageEvent::Error { error, .. } = last else { panic!("expected an error event: {last:?}") };
    assert_eq!(error.stop_reason, StopReason::Error);
    let message = error.error_message.clone().expect("an error message");
    assert!(message.contains("OpenAI API error (403)"), "{message}");
    assert!(message.contains("blocked by gateway WAF"), "{message}");
}

// =============================================================================
// test/fetch-option.test.ts (the two OpenAI Responses adapters)
// =============================================================================

#[tokio::test]
async fn fetch_option_passes_fetch_through_to_the_responses_adapters() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-custom-fetch", reqwest::header::HeaderValue::from_static("1"));
    let client = reqwest::Client::builder().default_headers(headers).build().expect("custom client");

    for api in ["openai-responses", "azure-openai-responses"] {
        let server = RecordingServer::start(completed_sse("resp_fetch_option")).await;
        let mut model = if api == "azure-openai-responses" { azure_model() } else { model(api, "openai", "gpt-5") };
        model.base_url = server.base_url();
        let mut options = responses_options();
        options.request.fetch = Some(client.clone());
        let events = if api == "azure-openai-responses" {
            collect(azure_openai_responses::stream(&model, &hello_context(), Some(options))).await
        } else {
            collect(openai_responses::stream(&model, &hello_context(), Some(options))).await
        };
        assert!(matches!(events.last(), Some(AssistantMessageEvent::Done { .. })), "{api}: {events:?}");
        let recorded = server.captured_headers();
        assert_eq!(header_value(&recorded, "x-custom-fetch").as_deref(), Some("1"), "{api} used the injected client");
    }
}

// =============================================================================
// test/openai-responses-websocket-expiry.test.ts
// =============================================================================

struct RecordingSocket {
    ready_state: Option<u8>,
    closes: Mutex<Vec<(u16, String)>>,
}

impl RecordingSocket {
    fn new(ready_state: Option<u8>) -> Arc<Self> {
        Arc::new(Self { ready_state, closes: Mutex::new(Vec::new()) })
    }

    fn closes(&self) -> Vec<(u16, String)> {
        self.closes.lock().expect("closes lock").clone()
    }
}

impl openai_responses::CachedWebSocket for RecordingSocket {
    fn ready_state(&self) -> Option<u8> {
        self.ready_state
    }

    fn close(&self, code: u16, reason: &str) {
        self.closes.lock().expect("closes lock").push((code, reason.to_owned()));
    }
}

/// Drains the runtime so the spawned expiry task reaches its next await point (registering its
/// timer). With the clock paused this is deterministic: the task is either runnable (it runs on the
/// next yield) or parked on a timer that only `advance` moves.
async fn settle() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn websocket_expiry_re_arms_while_busy_then_evicts_once_idle() {
    let socket = RecordingSocket::new(Some(1));
    let entry = Arc::new(openai_responses::CachedWebSocketConnection::new(socket.clone()));
    openai_responses::cache_websocket_session("sess-busy-rearm", entry.clone());
    openai_responses::schedule_session_websocket_expiry("sess-busy-rearm", entry.clone());
    settle().await;

    tokio::time::advance(Duration::from_millis(openai_responses::SESSION_WEBSOCKET_CACHE_TTL_MS + 1)).await;
    settle().await;
    assert!(socket.closes().is_empty(), "a busy entry survives the first TTL: {:?}", socket.closes());

    entry.set_busy(false);
    tokio::time::advance(Duration::from_millis(openai_responses::SESSION_WEBSOCKET_CACHE_TTL_MS + 1)).await;
    settle().await;
    assert_eq!(socket.closes(), vec![(1000, String::from("idle_timeout"))]);
}

#[tokio::test(start_paused = true)]
async fn websocket_expiry_evicts_a_busy_entry_whose_socket_died() {
    let socket = RecordingSocket::new(Some(3));
    let entry = Arc::new(openai_responses::CachedWebSocketConnection::new(socket.clone()));
    openai_responses::cache_websocket_session("sess-busy-dead", entry.clone());
    openai_responses::schedule_session_websocket_expiry("sess-busy-dead", entry.clone());
    settle().await;

    tokio::time::advance(Duration::from_millis(openai_responses::SESSION_WEBSOCKET_CACHE_TTL_MS + 1)).await;
    settle().await;
    assert_eq!(socket.closes(), vec![(1000, String::from("idle_timeout_dead"))]);
}

#[tokio::test(start_paused = true)]
async fn websocket_expiry_closes_and_drops_an_idle_entry_at_the_ttl() {
    let socket = RecordingSocket::new(Some(1));
    let entry = Arc::new(openai_responses::CachedWebSocketConnection::new(socket.clone()));
    entry.set_busy(false);
    openai_responses::cache_websocket_session("sess-idle", entry.clone());
    openai_responses::schedule_session_websocket_expiry("sess-idle", entry.clone());
    settle().await;

    tokio::time::advance(Duration::from_millis(openai_responses::SESSION_WEBSOCKET_CACHE_TTL_MS + 1)).await;
    settle().await;
    assert_eq!(socket.closes(), vec![(1000, String::from("idle_timeout"))]);
}
