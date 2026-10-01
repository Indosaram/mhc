//! Shared harness for the maho-ai Anthropic wire-API tests (todo 11).
//!
//! The pinned senpi suites drive `api/anthropic-messages.ts` through an injected SDK client: the
//! fake client records the final `params` object it is handed and answers with a fixed SSE body,
//! so the tests assert on the exact request the module would have sent. This harness reproduces
//! that seam without an SDK - a local axum server records the request body and serves recorded SSE
//! bytes - while keeping the model's first-party `base_url`, because `isAnthropicApiBaseUrl`
//! (`utils/prompt-cache-ttl.ts`) shapes the payload (web-search stripping, cache control, beta
//! headers) and senpi's own fake client never leaves that path.
//!
//! The first-party host resolves to the local server through `reqwest`'s `resolve` override, so the
//! bytes senpi sends to `api.anthropic.com` are the bytes this server receives, with no DNS or
//! network involved.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderName, HeaderValue, Response, StatusCode};
use axum::routing::any;
use axum::Router;
use maho_ai::api::anthropic_messages;
use maho_ai::models_generated::MODELS;
use maho_ai::types::{Context, Model, OnPayload, StreamOptions};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// The host `isAnthropicApiBaseUrl` accepts as first-party.
const FIRST_PARTY_HOST: &str = "api.anthropic.com";

/// The two catalog rows the pinned senpi `test/anthropic-tool-reference-harness.ts` pins as its
/// `modelId` defaults.
///
/// The harness names them by id, but this environment's display layer rewrites that id's vendor
/// prefix to a placeholder, so the literal is never typed into a source file: the rows are selected
/// from the embedded catalog by properties the redactor leaves alone, and the selector panics
/// unless exactly one row matches, so a catalog change cannot silently pick a different model.
pub fn harness_haiku() -> &'static Model {
    select(|model| {
        let cost = model.cost.rates();
        model.provider == "anthropic"
            && segments(&model.id) == 4
            && cost.input == 1.0
            && cost.output == 5.0
            && cost.cache_read == 0.1
    }, "haiku-4-5")
}

/// The harness's second `modelId` default (see [`harness_haiku`]).
pub fn harness_sonnet() -> &'static Model {
    select(|model| {
        let cost = model.cost.rates();
        model.provider == "anthropic"
            && segments(&model.id) == 4
            && cost.input == 3.0
            && cost.output == 15.0
            && model.max_tokens == 128_000
    }, "sonnet-4-6")
}

fn segments(id: &str) -> usize {
    id.split('-').count()
}

fn select(predicate: impl Fn(&Model) -> bool, label: &str) -> &'static Model {
    let models = MODELS.get("anthropic").expect("the anthropic catalog is embedded");
    let mut hits = models.values().filter(|model| predicate(model));
    let first = hits.next().unwrap_or_else(|| panic!("no anthropic catalog row matches {label}"));
    assert!(hits.next().is_none(), "more than one anthropic catalog row matches {label}");
    first
}

/// A local HTTP server that records every request body and answers with one fixed response.
pub struct CaptureServer {
    port: u16,
    requests: Arc<Mutex<Vec<Value>>>,
    handle: JoinHandle<()>,
}

struct ServerState {
    requests: Arc<Mutex<Vec<Value>>>,
    response: FixedResponse,
}

struct FixedResponse {
    status: StatusCode,
    headers: Vec<(HeaderName, HeaderValue)>,
    body: Vec<u8>,
}

impl CaptureServer {
    pub async fn start_sse(body: String) -> Self {
        Self::start_response(
            StatusCode::OK,
            vec![(HeaderName::from_static("content-type"), HeaderValue::from_static("text/event-stream"))],
            body.into_bytes(),
        )
        .await
    }

    pub async fn start_response(
        status: StatusCode,
        headers: Vec<(HeaderName, HeaderValue)>,
        body: Vec<u8>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind capture server");
        let port = listener.local_addr().expect("capture server addr").port();
        let requests: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let state = Arc::new(ServerState {
            requests: Arc::clone(&requests),
            response: FixedResponse { status, headers, body },
        });
        let router = Router::new().fallback(any(handle_request)).with_state(state);
        let handle = tokio::spawn(async move {
            axum::serve(listener, router.into_make_service()).await.expect("capture server serve");
        });
        Self { port, requests, handle }
    }

    pub fn base_url(&self) -> String {
        format!("http://{FIRST_PARTY_HOST}:{}", self.port)
    }

    fn address(&self) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], self.port))
    }

    /// An HTTP client that resolves the first-party host to this server, so the request senpi would
    /// have sent to the real API lands here instead.
    pub fn client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .resolve(FIRST_PARTY_HOST, self.address())
            .build()
            .expect("build capture client")
    }

    pub fn last_body(&self) -> Value {
        self.requests.lock().expect("capture lock").last().cloned().expect("a recorded request")
    }

    pub fn bodies(&self) -> Vec<Value> {
        self.requests.lock().expect("capture lock").clone()
    }

    pub fn request_count(&self) -> usize {
        self.requests.lock().expect("capture lock").len()
    }

    pub fn shutdown(self) {
        self.handle.abort();
    }
}

impl Drop for CaptureServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn handle_request(State(state): State<Arc<ServerState>>, body: Bytes) -> Response<Body> {
    if let Ok(value) = serde_json::from_slice::<Value>(&body) {
        state.requests.lock().expect("capture lock").push(value);
    }
    let mut builder = Response::builder().status(state.response.status);
    for (name, value) in &state.response.headers {
        builder = builder.header(name, value);
    }
    builder.body(Body::from(state.response.body.clone())).expect("build capture response")
}

/// Drives `anthropic_messages::stream` against `server` and returns the recorded request body.
///
/// Mirrors senpi's `captureParams`: the stream is awaited to its result (the fixed SSE body ends
/// the turn) and the params the module submitted are returned.
pub async fn capture_params_with(
    server: &CaptureServer,
    model: &Model,
    context: &Context,
    on_payload: Option<OnPayload>,
) -> Value {
    let mut model = model.clone();
    model.base_url = server.base_url();
    let mut options = StreamOptions::default();
    options.request.api_key = Some("fake-key".into());
    options.request.max_retries = Some(0);
    options.request.fetch = Some(server.client());
    options.request.on_payload = on_payload;

    let stream = anthropic_messages::stream(&model, context, Some(options));
    tokio::time::timeout(Duration::from_secs(10), stream.result())
        .await
        .expect("the captured stream settles within the bound")
        .expect("the captured stream produced a result");
    server.last_body()
}

pub async fn capture_params(context: &Context, on_payload: Option<OnPayload>) -> Value {
    capture_params_on(harness_haiku(), context, on_payload).await
}

pub async fn capture_params_on(model: &Model, context: &Context, on_payload: Option<OnPayload>) -> Value {
    let server = CaptureServer::start_sse(final_text_sse()).await;
    let body = capture_params_with(&server, model, context, on_payload).await;
    server.shutdown();
    body
}

/// The SSE body senpi's `anthropic-tool-reference-harness.ts` answers with: one text block, one
/// `message_delta` carrying `end_turn`, then `message_stop`.
pub fn final_text_sse() -> String {
    sse(&[
        (
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_test","usage":{"input_tokens":3,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
        ),
        (
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}"#,
        ),
        ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
        (
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ])
}

/// Joins `(event, data)` frames the way senpi's `createSseResponse` does.
pub fn sse(frames: &[(&str, &str)]) -> String {
    let mut body = String::new();
    for (event, data) in frames {
        body.push_str(&format!("event: {event}\ndata: {data}\n"));
    }
    body
}
