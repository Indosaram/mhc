//! Replay equality tests for the wire APIs this lane owns.
//!
//! Each test drives the Rust `stream()` against the same recorded fixture bytes
//! `tools/golden/ai-replay.mjs` served to the pinned senpi `stream()`, and compares the serialized
//! `AssistantMessageEvent` sequence byte for byte with `crates/maho-ai/tests/golden/replay/<case>.json`.
//! The fixture server is a minimal HTTP/1.1 responder so the comparison needs no extra crate.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine;
use maho_ai::api::{bedrock_converse_stream, mistral_conversations, openai_images, openrouter_images, pi_messages};
use maho_ai::types::{
    AssistantMessageEvent, ContentBlock, Context, ImagesContext, ImagesModel, ImagesModelCost, ImagesOutputModality,
    ImagesStopReason, InputModality, Message, Model, ModelCost, ProviderEnv, SimpleStreamOptions, StreamOptions,
    UserContent, UserMessage,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct FixtureServer {
    addr: SocketAddr,
    handle: tokio::task::JoinHandle<()>,
}

impl FixtureServer {
    async fn start(body: Vec<u8>, content_type: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind fixture server");
        let addr = listener.local_addr().expect("fixture server addr");
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else { return };
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buffer = [0u8; 4096];
                    let _ = socket.read(&mut buffer).await;
                    let header = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(header.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                    let _ = socket.flush().await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Self { addr, handle }
    }

    fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// `JSON.stringify(events, null, "\t") + "\n"`, the exact shape `tools/golden/ai-replay.mjs` writes:
/// tab indentation plus the generator's `Date.now()` timestamp normalization, and JS number
/// formatting (an integral `f64` prints as `0`, never `0.0`).
fn serialize_golden(events: &[AssistantMessageEvent], shared_containers: &[&str]) -> String {
    let raw: Vec<serde_json::Value> =
        events.iter().map(|event| serde_json::to_value(event).expect("event JSON")).collect();
    let terminal = final_message(&raw);
    let normalized: Vec<serde_json::Value> = raw
        .into_iter()
        .map(|event| normalize_event(event, &terminal, shared_containers))
        .collect();
    let mut out = Vec::new();
    let formatter = JsNumberFormatter::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
    serde::Serialize::serialize(&normalized, &mut serializer).expect("serialize replay events");
    out.push(b'\n');
    String::from_utf8(out).expect("replay JSON is UTF-8")
}

fn normalize_event(
    event: serde_json::Value,
    final_message: &serde_json::Value,
    shared_containers: &[&str],
) -> serde_json::Value {
    let mut event = event;
    for key in ["partial", "message", "error"] {
        let is_message =
            matches!(event.get(key), Some(serde_json::Value::Object(message)) if message.contains_key("role"));
        if !is_message {
            continue;
        }
        let Some(object) = event.get_mut(key).and_then(serde_json::Value::as_object_mut) else { continue };
        if let Some(content) = final_message.get("content") {
            object.insert("content".into(), content.clone());
        }
        for container in shared_containers {
            if let Some(value) = final_message.get(*container) {
                object.insert((*container).into(), value.clone());
            }
        }
        object.insert("timestamp".into(), serde_json::Value::from(0));
    }
    event
}

/// senpi's own harness hands each event the *live* `AssistantMessage`, so JS reference aliasing is
/// visible in the recorded bytes: an array or object the implementation mutates in place shows its
/// final state in every earlier event, while a property the implementation *replaces* (or a scalar it
/// assigns) keeps the value it had when the event was yielded. Reproducing that here keeps the
/// comparison byte-exact against the recorded bytes; event type, contentIndex, delta, content value,
/// tool-call payload, reason and error text are all still compared as emitted.
fn final_message(events: &[serde_json::Value]) -> serde_json::Value {
    let mut message = events
        .iter()
        .rev()
        .find_map(|event| {
            event
                .get("message")
                .or_else(|| event.get("error"))
                .filter(|value| value.get("role").is_some())
                .cloned()
        })
        .expect("a terminal message");
    if let Some(object) = message.as_object_mut() {
        object.insert("timestamp".into(), serde_json::Value::from(0));
    }
    message
}

struct JsNumberFormatter {
    inner: serde_json::ser::PrettyFormatter<'static>,
}

impl JsNumberFormatter {
    fn new() -> Self {
        Self { inner: serde_json::ser::PrettyFormatter::with_indent(b"\t") }
    }
}

macro_rules! delegate_value {
    ($name:ident, $($arg:ident: $ty:ty),*) => {
        fn $name<W>(&mut self, writer: &mut W, $($arg: $ty),*) -> std::io::Result<()>
        where
            W: ?Sized + std::io::Write,
        {
            self.inner.$name(writer, $($arg),*)
        }
    };
}

macro_rules! delegate_writer {
    ($name:ident) => {
        fn $name<W>(&mut self, writer: &mut W) -> std::io::Result<()>
        where
            W: ?Sized + std::io::Write,
        {
            self.inner.$name(writer)
        }
    };
}

impl serde_json::ser::Formatter for JsNumberFormatter {
    fn write_f64<W>(&mut self, writer: &mut W, value: f64) -> std::io::Result<()>
    where
        W: ?Sized + std::io::Write,
    {
        if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e15 {
            write!(writer, "{}", value as i64)
        } else {
            self.inner.write_f64(writer, value)
        }
    }

    fn write_f32<W>(&mut self, writer: &mut W, value: f32) -> std::io::Result<()>
    where
        W: ?Sized + std::io::Write,
    {
        self.write_f64(writer, f64::from(value))
    }

    delegate_value!(write_bool, value: bool);
    delegate_value!(write_i8, value: i8);
    delegate_value!(write_i16, value: i16);
    delegate_value!(write_i32, value: i32);
    delegate_value!(write_i64, value: i64);
    delegate_value!(write_i128, value: i128);
    delegate_value!(write_u8, value: u8);
    delegate_value!(write_u16, value: u16);
    delegate_value!(write_u32, value: u32);
    delegate_value!(write_u64, value: u64);
    delegate_value!(write_u128, value: u128);
    delegate_value!(write_number_str, value: &str);
    delegate_value!(write_string_fragment, fragment: &str);
    delegate_value!(write_byte_array, value: &[u8]);
    delegate_value!(write_char_escape, char_escape: serde_json::ser::CharEscape);
    delegate_value!(write_raw_fragment, fragment: &str);
    delegate_value!(begin_array_value, first: bool);
    delegate_value!(begin_object_key, first: bool);
    delegate_writer!(write_null);
    delegate_writer!(begin_string);
    delegate_writer!(end_string);
    delegate_writer!(begin_array);
    delegate_writer!(end_array);
    delegate_writer!(end_array_value);
    delegate_writer!(begin_object);
    delegate_writer!(end_object);
    delegate_writer!(end_object_key);
    delegate_writer!(begin_object_value);
    delegate_writer!(end_object_value);
}

fn golden_path(case: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/replay").join(format!("{case}.json"))
}

fn case_path(case: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/golden/cases")
        .join(format!("{case}.json"))
}

fn load_case(case: &str) -> serde_json::Value {
    let path = case_path(case);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}")))
        .expect("replay case JSON")
}

fn sse_fixture_bytes(case: &str) -> Vec<u8> {
    let spec = load_case(case);
    let events = spec["fixture"]["events"].as_array().expect("fixture.events");
    let mut body = String::new();
    for event in events {
        if let Some(name) = event.get("event").and_then(serde_json::Value::as_str) {
            body.push_str(&format!("event: {name}\n"));
        }
        body.push_str(&format!("data: {}\n\n", event["data"].as_str().expect("fixture event data")));
    }
    body.into_bytes()
}

fn assert_matches_golden(case: &str, events: &[AssistantMessageEvent], shared_containers: &[&str]) {
    let actual = serialize_golden(events, shared_containers);
    let path = golden_path(case);
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}"));
    assert_eq!(
        actual, expected,
        "replay mismatch for {case}: run `bun tools/golden/ai-replay.mjs --case {case}` to regenerate from pinned senpi"
    );
}

fn replay_model(api: &str, provider: &str, id: &str) -> Model {
    Model {
        id: id.to_owned(),
        name: id.to_owned(),
        api: api.to_owned(),
        provider: provider.to_owned(),
        base_url: String::new(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![InputModality::Text],
        cost: ModelCost::default(),
        context_window: 200_000,
        max_tokens: 8192,
        sampling_params: None,
        headers: None,
        cache_retention: None,
        upstream_model_id: None,
        service_tier: None,
        recover_text_tool_calls: None,
        compat: None,
    }
}

fn hello_context() -> Context {
    Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage { content: UserContent::Text("Hello".into()), timestamp: 0 })],
        tools: None,
    }
}

async fn collect(stream: maho_ai::types::AssistantMessageEventStream) -> Vec<AssistantMessageEvent> {
    tokio::time::timeout(Duration::from_secs(10), stream.collect())
        .await
        .expect("replay round trip within the bound")
        .expect("stream collected")
}

#[tokio::test]
async fn pi_messages_replays_the_recorded_stream() {
    let server = FixtureServer::start(sse_fixture_bytes("ai-replay-pi-messages-basic"), "text/event-stream").await;
    let mut model = replay_model("pi-messages", "test-replay", "pi-messages-replay-model");
    model.base_url = server.base_url();
    let mut options = StreamOptions::default();
    options.request.api_key = Some("test".into());

    let events = collect(pi_messages::stream(&model, &hello_context(), Some(options))).await;
    assert_matches_golden("ai-replay-pi-messages-basic", &events, &[]);
}

#[tokio::test]
async fn mistral_conversations_replays_the_recorded_stream() {
    let server = FixtureServer::start(sse_fixture_bytes("ai-replay-mistral-basic"), "text/event-stream").await;
    let mut model = replay_model("mistral-conversations", "test-replay", "mistral-replay-model");
    model.base_url = server.base_url();
    let mut options = StreamOptions::default();
    options.request.api_key = Some("test".into());
    options.request.timeout_ms = Some(10_000);

    let events = collect(mistral_conversations::stream(&model, &hello_context(), Some(options))).await;
    assert_matches_golden("ai-replay-mistral-basic", &events, &["usage"]);
}

#[tokio::test]
async fn bedrock_converse_stream_replays_the_recorded_stream() {
    let spec = load_case("ai-replay-bedrock-basic");
    let encoded = spec["fixture"]["bodyBase64"].as_str().expect("fixture.bodyBase64");
    let body = base64::engine::general_purpose::STANDARD.decode(encoded).expect("bedrock fixture base64");
    let server = FixtureServer::start(body, "application/vnd.amazon.eventstream").await;

    let mut model = replay_model("bedrock-converse-stream", "test-replay", "anthropic.claude-3-replay");
    model.base_url = server.base_url();
    let mut options = StreamOptions::default();
    let env: ProviderEnv = [
        ("AWS_ACCESS_KEY_ID".to_owned(), "AKIDREPLAY".to_owned()),
        ("AWS_SECRET_ACCESS_KEY".to_owned(), "replay-secret".to_owned()),
        ("AWS_REGION".to_owned(), "us-east-1".to_owned()),
        ("AWS_BEDROCK_FORCE_HTTP1".to_owned(), "1".to_owned()),
    ]
    .into_iter()
    .collect();
    options.request.env = Some(env);

    let events = collect(bedrock_converse_stream::stream(&model, &hello_context(), Some(options))).await;
    assert_matches_golden("ai-replay-bedrock-basic", &events, &["usage"]);
}

#[tokio::test]
async fn pi_messages_stream_simple_forwards_to_stream() {
    let body = "data: {\"type\":\"done\",\"reason\":\"stop\",\"usage\":{\"input\":0,\"output\":0,\"cacheRead\":0,\"cacheWrite\":0,\"totalTokens\":0,\"cost\":{\"input\":0,\"output\":0,\"cacheRead\":0,\"cacheWrite\":0,\"total\":0}}}\n\n".to_owned();
    let server = FixtureServer::start(body.into_bytes(), "text/event-stream").await;
    let mut model = replay_model("pi-messages", "test-replay", "pi-messages-replay-model");
    model.base_url = server.base_url();
    let mut simple = SimpleStreamOptions::default();
    simple.stream.request.api_key = Some("test".into());

    let events = collect(pi_messages::stream_simple(&model, &hello_context(), Some(simple))).await;
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], AssistantMessageEvent::Done { .. }));
}

#[tokio::test]
async fn pi_messages_truncated_stream_ends_with_a_typed_error() {
    let body = "data: {\"type\":\"start\"}\n\ndata: {\"type\":\"text_start\",\"contentIndex\":0}\n\n".to_owned();
    let server = FixtureServer::start(body.into_bytes(), "text/event-stream").await;
    let mut model = replay_model("pi-messages", "test-replay", "pi-messages-replay-model");
    model.base_url = server.base_url();
    let mut options = StreamOptions::default();
    options.request.api_key = Some("test".into());

    let events = collect(pi_messages::stream(&model, &hello_context(), Some(options))).await;
    let last = events.last().expect("terminal event");
    let AssistantMessageEvent::Error { error, .. } = last else { panic!("expected an error event") };
    assert_eq!(
        error.error_message.as_deref(),
        Some("test-replay stream ended without a terminal event")
    );
}

#[tokio::test]
async fn mistral_malformed_event_body_ends_with_a_typed_error() {
    let body = "data: {\"nope\":1}\n\n".to_owned();
    let server = FixtureServer::start(body.into_bytes(), "text/event-stream").await;
    let mut model = replay_model("mistral-conversations", "test-replay", "mistral-replay-model");
    model.base_url = server.base_url();
    let mut options = StreamOptions::default();
    options.request.api_key = Some("test".into());
    options.request.timeout_ms = Some(10_000);

    let events = collect(mistral_conversations::stream(&model, &hello_context(), Some(options))).await;
    let last = events.last().expect("terminal event");
    let AssistantMessageEvent::Error { error, .. } = last else { panic!("expected an error event") };
    assert_eq!(error.error_message.as_deref(), Some("Invalid Mistral streaming event"));
}

fn images_model(api: &str, provider: &str, id: &str, base_url: &str) -> ImagesModel {
    ImagesModel {
        id: id.to_owned(),
        name: id.to_owned(),
        api: api.to_owned(),
        provider: provider.to_owned(),
        base_url: base_url.to_owned(),
        thinking_level_map: None,
        input: vec![InputModality::Text, InputModality::Image],
        output: vec![ImagesOutputModality::Image, ImagesOutputModality::Text],
        cost: ImagesModelCost::default(),
        sampling_params: None,
        headers: None,
        cache_retention: None,
        upstream_model_id: None,
        service_tier: None,
        recover_text_tool_calls: None,
    }
}

fn png_data_url() -> String {
    use base64::Engine;
    let mut bytes = vec![0x89u8, 0x50, 0x4e, 0x47];
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))
}

fn prompt_context() -> ImagesContext {
    ImagesContext { input: vec![ContentBlock::text("draw a cat")] }
}

#[tokio::test]
async fn openai_images_orders_revised_prompts_and_parses_data_urls() {
    let body = format!(
        "{{\"created\":1,\"data\":[{{\"revised_prompt\":\"a cat\",\"url\":\"{}\"}},{{\"b64_json\":\"AA==\"}}]}}",
        png_data_url()
    );
    let server = FixtureServer::start(body.into_bytes(), "application/json").await;
    let model = images_model("openai-images", "openai", "gpt-image-2.5", &server.base_url());
    let mut options = maho_ai::types::ImagesOptions::default();
    options.request.api_key = Some("sk-test".into());

    let images = openai_images::generate_images(&model, &prompt_context(), Some(options)).await;
    assert_eq!(images.stop_reason, ImagesStopReason::Stop);
    assert_eq!(images.output.len(), 3);
    assert_eq!(images.output[0], ContentBlock::text("a cat"));
    assert_eq!(images.output[1].type_name(), "image");
    assert_eq!(images.output[2].type_name(), "image");
    let ContentBlock::Image(image) = &images.output[1] else { panic!("image") };
    assert_eq!(image.mime_type, "image/png");
}

#[tokio::test]
async fn openai_images_missing_auth_returns_an_error_envelope() {
    let model = images_model("openai-images", "openai", "gpt-image-2.5", "http://127.0.0.1:1/v1");
    let images = openai_images::generate_images(&model, &prompt_context(), None).await;
    assert_eq!(images.stop_reason, ImagesStopReason::Error);
    assert_eq!(images.error_message.as_deref(), Some("No API key for provider: openai"));
    assert!(images.output.is_empty());
}

#[tokio::test]
async fn openrouter_images_returns_text_plus_images() {
    let body = format!(
        "{{\"id\":\"gen-1\",\"choices\":[{{\"message\":{{\"content\":\"here you go\",\"images\":[{{\"image_url\":\"{}\"}}]}}}}]}}",
        png_data_url()
    );
    let server = FixtureServer::start(body.into_bytes(), "application/json").await;
    let model = images_model("openrouter-images", "openrouter", "black-forest-labs/flux.2-pro", &server.base_url());
    let mut options = maho_ai::types::ImagesOptions::default();
    options.request.api_key = Some("sk-test".into());

    let images = openrouter_images::generate_images(&model, &prompt_context(), Some(options)).await;
    assert_eq!(images.stop_reason, ImagesStopReason::Stop);
    assert_eq!(images.response_id.as_deref(), Some("gen-1"));
    assert_eq!(images.output.len(), 2);
    assert_eq!(images.output[0], ContentBlock::text("here you go"));
    let ContentBlock::Image(image) = &images.output[1] else { panic!("image") };
    assert_eq!(image.mime_type, "image/png");
}

#[tokio::test]
async fn openrouter_images_abort_returns_an_aborted_result() {
    let server = FixtureServer::start(b"{}".to_vec(), "application/json").await;
    let model = images_model("openrouter-images", "openrouter", "black-forest-labs/flux.2-pro", &server.base_url());
    let controller = maho_ai::utils::abort::AbortController::new();
    controller.abort(None);
    let mut options = maho_ai::types::ImagesOptions::default();
    options.request.api_key = Some("sk-test".into());
    options.request.signal = Some(controller.signal());

    let images = openrouter_images::generate_images(&model, &prompt_context(), Some(options)).await;
    assert_eq!(images.stop_reason, ImagesStopReason::Aborted);
}
