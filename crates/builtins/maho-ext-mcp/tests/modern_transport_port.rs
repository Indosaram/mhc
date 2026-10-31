//! Port of senpi packages/coding-agent/src/core/extensions/builtin/mcp/modern/transport.test.ts
//! ("modern HTTP MCP transport") against `ModernHttpTransport`, driven through a local axum stub that records
//! every POST (headers + body) through a channel.
//!
//! API mapping: upstream `onmessage` receives both notifications and the final reply; the Rust `send` returns the
//! reply addressed to the request and hands notifications to `on_message`. Upstream `onerror` + a rejected `send`
//! is the single `Err` returned by `send`. Upstream `close()` aborting in-flight work maps to dropping (aborting)
//! the `send` future; the Rust transport has no `close`/`onclose`, so "onclose only once" and "send after close
//! rejects" have no equivalent (N-A).

use axum::{
    Router,
    body::{Body, Bytes},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::post,
};
use maho_ext_mcp::{
    errors::McpErrorKind,
    modern::{
        errors::{McpMrtrErrorCode, as_mcp_mrtr_error},
        transport::ModernHttpTransport,
        wire::{MODERN_PROTOCOL_VERSION, ModernClientInfo},
    },
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};

const WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
struct Recorded {
    headers: HeaderMap,
    body: Value,
}

async fn serve(router: Router) -> (url::Url, oneshot::Sender<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    (format!("http://{address}/mcp").parse().unwrap(), stop)
}

/// A stub answering every POST with `respond()` and recording the request.
fn recording(
    respond: impl Fn() -> Response + Clone + Send + Sync + 'static,
) -> (Router, mpsc::UnboundedReceiver<Recorded>) {
    let (sender, receiver) = mpsc::unbounded_channel();
    let router = Router::new().route(
        "/mcp",
        post(move |headers: HeaderMap, body: Bytes| {
            let sender = sender.clone();
            let respond = respond.clone();
            async move {
                let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let _ = sender.send(Recorded { headers, body });
                respond()
            }
        }),
    );
    (router, receiver)
}

fn json_response(status: StatusCode, value: Value) -> Response {
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(Body::from(value.to_string()))
        .unwrap()
}

fn sse_response(body: Body) -> Response {
    Response::builder()
        .header("Content-Type", "text/event-stream")
        .body(body)
        .unwrap()
}

fn client_info() -> ModernClientInfo {
    ModernClientInfo {
        name: "senpi".into(),
        version: "test".into(),
    }
}

fn client_capabilities() -> Value {
    json!({"elicitation": {"form": {}}})
}

fn complete_result() -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "result": {"resultType": "complete", "tools": []}})
}

/// Upstream `transport(fetch, options)`.
fn transport(
    url: url::Url,
    headers: &[(&str, &str)],
    protocol_version: Option<&str>,
) -> ModernHttpTransport {
    ModernHttpTransport::new(
        "fixture",
        url,
        headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect::<BTreeMap<_, _>>(),
        client_info(),
        client_capabilities(),
        protocol_version.unwrap_or(MODERN_PROTOCOL_VERSION).into(),
        None,
        Arc::new(|_| {}),
    )
    .unwrap()
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

async fn next(receiver: &mut mpsc::UnboundedReceiver<Recorded>) -> Recorded {
    tokio::time::timeout(WAIT, receiver.recv())
        .await
        .expect("request recorded in time")
        .expect("recorder open")
}

/// Signals once when the wrapped response body (or handler future) is dropped: the observable equivalent of
/// upstream's `ReadableStream` `cancel` / aborted fetch signal on the server side.
struct DropSignal<S> {
    stream: S,
    signal: Option<oneshot::Sender<()>>,
}
impl<S: futures::Stream + Unpin> futures::Stream for DropSignal<S> {
    type Item = S::Item;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::pin::Pin::new(&mut self.stream).poll_next(cx)
    }
}
impl<S> Drop for DropSignal<S> {
    fn drop(&mut self) {
        if let Some(signal) = self.signal.take() {
            let _ = signal.send(());
        }
    }
}

/// An SSE body that yields `first` and then stays open until dropped; `dropped` fires on drop.
fn open_sse(first: &'static str, dropped: oneshot::Sender<()>) -> Body {
    let stream = futures::stream::unfold(Some(first), |state| async move {
        match state {
            Some(chunk) => Some((Ok::<_, std::io::Error>(Bytes::from(chunk)), None)),
            None => futures::future::pending().await,
        }
    });
    Body::from_stream(DropSignal {
        stream: Box::pin(stream),
        signal: Some(dropped),
    })
}

/// Shared assertions of upstream "sends matching metadata and headers for version %s".
async fn sends_matching_metadata_and_headers(protocol_version: Option<&str>) {
    let (router, mut requests) = recording(|| json_response(StatusCode::OK, complete_result()));
    let (url, stop) = serve(router).await;
    let client = transport(
        url,
        &[
            ("Authorization", "Bearer fixture"),
            ("mcp-protocol-version", "wrong"),
            ("Mcp-Session-Id", "forbidden"),
        ],
        protocol_version,
    );
    let received = Mutex::new(Vec::new());
    let on_message = |message: Value| received.lock().unwrap().push(message);

    let reply = client
        .send(
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
                "params": {"cursor": "next", "_meta": {"progressToken": 7, "io.modelcontextprotocol/protocolVersion": "wrong"}},
            }),
            None,
            &on_message,
        )
        .await
        .unwrap();

    let request = next(&mut requests).await;
    assert!(
        requests.try_recv().is_err(),
        "exactly one POST (fetch called once)"
    );
    let version = protocol_version.unwrap_or("2026-07-28");
    assert_eq!(
        request.body["params"],
        json!({
            "cursor": "next",
            "_meta": {
                "progressToken": 7,
                "io.modelcontextprotocol/protocolVersion": version,
                "io.modelcontextprotocol/clientInfo": {"name": "senpi", "version": "test"},
                "io.modelcontextprotocol/clientCapabilities": client_capabilities(),
            },
        })
    );
    assert_eq!(
        header(&request.headers, "MCP-Protocol-Version"),
        request.body["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"].as_str()
    );
    assert_eq!(header(&request.headers, "Mcp-Method"), Some("tools/list"));
    assert_eq!(header(&request.headers, "Mcp-Session-Id"), None);
    assert_eq!(
        header(&request.headers, "Authorization"),
        Some("Bearer fixture")
    );
    assert_eq!(
        header(&request.headers, "Content-Type"),
        Some("application/json")
    );
    assert!(
        header(&request.headers, "Accept")
            .unwrap_or_default()
            .contains("text/event-stream")
    );
    // Upstream `received` is called with the result: the Rust reply channel is the return value.
    assert_eq!(reply, Some(complete_result()));
    assert!(received.lock().unwrap().is_empty());
    let _ = stop.send(());
}

/// transport.test.ts "sends matching metadata and headers for version %s" (undefined).
#[tokio::test]
async fn sends_matching_metadata_and_headers_for_version_undefined() {
    sends_matching_metadata_and_headers(None).await;
}

/// transport.test.ts "sends matching metadata and headers for version %s" ("2026-08-01").
#[tokio::test]
async fn sends_matching_metadata_and_headers_for_version_2026_08_01() {
    sends_matching_metadata_and_headers(Some("2026-08-01")).await;
}

/// Shared body of upstream "routes %s with the canonical name encoding".
async fn routes_with_the_canonical_name_encoding(
    method: &str,
    params: Value,
    expected: Option<&str>,
) {
    let (router, mut requests) = recording(|| json_response(StatusCode::OK, complete_result()));
    let (url, stop) = serve(router).await;
    let client = transport(url, &[("Mcp-Name", "wrong"), ("Mcp-Method", "wrong")], None);

    let _ = client
        .send(
            &json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}),
            None,
            &|_| {},
        )
        .await;

    let request = next(&mut requests).await;
    assert_eq!(header(&request.headers, "Mcp-Name"), expected);
    assert_eq!(header(&request.headers, "Mcp-Method"), Some(method));
    let _ = stop.send(());
}

/// transport.test.ts "routes %s with the canonical name encoding" (tools/call, "echo").
#[tokio::test]
async fn routes_tools_call_echo_with_the_canonical_name_encoding() {
    routes_with_the_canonical_name_encoding("tools/call", json!({"name": "echo"}), Some("echo"))
        .await;
}

/// transport.test.ts "routes %s with the canonical name encoding" (prompts/get, Hangul name).
#[tokio::test]
async fn routes_prompts_get_hangul_with_the_canonical_name_encoding() {
    routes_with_the_canonical_name_encoding(
        "prompts/get",
        json!({"name": "\u{D55C}\u{AE00}"}),
        Some("=?base64?7ZWc6riA?="),
    )
    .await;
}

/// transport.test.ts "routes %s with the canonical name encoding" (resources/read, uri wins over name).
#[tokio::test]
async fn routes_resources_read_uri_with_the_canonical_name_encoding() {
    routes_with_the_canonical_name_encoding(
        "resources/read",
        json!({"uri": "x:\u{00FF}", "name": "ignored"}),
        Some("=?base64?eDrDvw==?="),
    )
    .await;
}

/// transport.test.ts "routes %s with the canonical name encoding" (tools/call, "base64:literal").
#[tokio::test]
async fn routes_tools_call_base64_prefix_literal_with_the_canonical_name_encoding() {
    routes_with_the_canonical_name_encoding(
        "tools/call",
        json!({"name": "base64:literal"}),
        Some("base64:literal"),
    )
    .await;
}

/// transport.test.ts "routes %s with the canonical name encoding" (#4192: exact sentinel pattern is encoded).
#[tokio::test]
async fn routes_tools_call_sentinel_literal_with_the_canonical_name_encoding() {
    routes_with_the_canonical_name_encoding(
        "tools/call",
        json!({"name": "=?base64?literal?="}),
        Some("=?base64?PT9iYXNlNjQ/bGl0ZXJhbD89?="),
    )
    .await;
}

/// transport.test.ts "routes %s with the canonical name encoding" (tools/list carries no Mcp-Name).
#[tokio::test]
async fn routes_tools_list_without_a_name_with_the_canonical_name_encoding() {
    routes_with_the_canonical_name_encoding("tools/list", json!({"name": "ignored"}), None).await;
}

/// transport.test.ts "ignores unmatched cancellation and POSTs responses without session traffic".
#[tokio::test]
async fn ignores_unmatched_cancellation_and_posts_responses_without_session_traffic() {
    let (router, mut requests) = recording(|| {
        Response::builder()
            .status(StatusCode::ACCEPTED)
            .body(Body::empty())
            .unwrap()
    });
    let (url, stop) = serve(router).await;
    let client = transport(url, &[], None);

    let cancelled = client
        .send(
            &json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 1}}),
            None,
            &|_| {},
        )
        .await
        .unwrap();
    let replied = client
        .send(
            &json!({"jsonrpc": "2.0", "id": "server-request", "result": {}}),
            None,
            &|_| {},
        )
        .await
        .unwrap();
    assert_eq!((cancelled, replied), (None, None));

    // Both sends completed, so every POST the stub will ever see is already recorded.
    let request = next(&mut requests).await;
    assert!(requests.try_recv().is_err(), "fetch called exactly once");
    assert_eq!(
        request.body,
        json!({"jsonrpc": "2.0", "id": "server-request", "result": {}})
    );
    assert!(!request.headers.contains_key("Mcp-Session-Id"));
    let _ = stop.send(());
}

/// transport.test.ts "consumes chunked SSE with multiline data and delivers the final response". The body is
/// streamed in 7-byte chunks, so multi-byte UTF-8 sequences and CRLF pairs straddle chunk boundaries.
#[tokio::test]
async fn consumes_chunked_sse_with_multiline_data_and_delivers_the_final_response() {
    let notification =
        json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {"progress": 1}});
    let text = format!(
        ": keepalive\r\nevent: message\r\ndata: {notification}\r\n\r\n\
         event: message\r\ndata: {{\"jsonrpc\":\"2.0\",\"id\":1,\r\ndata: \"result\":{{\"resultType\":\"complete\",\"text\":\"\u{D55C}\u{AE00}\"}}}}\r\n\r\n"
    );
    let bytes = text.into_bytes();
    let (router, _requests) = recording(move || {
        let chunks: Vec<Result<Bytes, std::io::Error>> = bytes
            .chunks(7)
            .map(|chunk| Ok(Bytes::copy_from_slice(chunk)))
            .collect();
        Response::builder()
            .header("Content-Type", "text/event-stream; charset=utf-8")
            .body(Body::from_stream(futures::stream::iter(chunks)))
            .unwrap()
    });
    let (url, stop) = serve(router).await;
    let client = transport(url, &[], None);
    let messages = Mutex::new(Vec::new());
    let on_message = |message: Value| messages.lock().unwrap().push(message);

    let reply = client
        .send(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "echo"}}),
            None,
            &on_message,
        )
        .await
        .unwrap();

    let mut delivered = messages.lock().unwrap().clone();
    delivered.extend(reply);
    assert_eq!(
        delivered,
        vec![
            notification,
            json!({"jsonrpc": "2.0", "id": 1, "result": {"resultType": "complete", "text": "\u{D55C}\u{AE00}"}}),
        ]
    );
    let _ = stop.send(());
}

/// transport.test.ts "finishes an SSE request when its final response arrives before stream EOF": the send
/// settles on the reply although the stream stays open, and the stream is then cancelled exactly once.
#[tokio::test]
async fn finishes_an_sse_request_when_its_final_response_arrives_before_stream_eof() {
    let (dropped_sender, dropped) = oneshot::channel();
    let dropped_sender = Arc::new(Mutex::new(Some(dropped_sender)));
    let (router, _requests) = recording(move || {
        let signal = dropped_sender.lock().unwrap().take().expect("one POST");
        sse_response(open_sse(
            "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"resultType\":\"complete\",\"tools\":[]}}\n\n",
            signal,
        ))
    });
    let (url, stop) = serve(router).await;
    let client = transport(url, &[], None);
    let received = Mutex::new(Vec::new());
    let on_message = |message: Value| received.lock().unwrap().push(message);

    let reply = tokio::time::timeout(
        WAIT,
        client.send(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            None,
            &on_message,
        ),
    )
    .await
    .expect("send settles before stream EOF")
    .unwrap();

    assert_eq!(reply, Some(complete_result()));
    assert!(received.lock().unwrap().is_empty());
    tokio::time::timeout(WAIT, dropped)
        .await
        .expect("SSE stream cancelled")
        .expect("drop signal");
    let _ = stop.send(());
}

/// Shared body of upstream "reports HTTP and malformed response failures through onerror and send".
async fn reports_http_and_malformed_response_failures(
    respond: impl Fn() -> Response + Clone + Send + Sync + 'static,
) {
    let (router, _requests) = recording(respond);
    let (url, stop) = serve(router).await;
    let client = transport(url, &[], None);
    let delivered = Mutex::new(Vec::new());
    let on_message = |message: Value| delivered.lock().unwrap().push(message);

    let failure = client
        .send(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            None,
            &on_message,
        )
        .await
        .expect_err("send rejects");

    // Upstream reports every case once as McpMrtrError("MCP_MRTR_TRANSPORT_FAILED").
    assert_eq!(
        as_mcp_mrtr_error(&failure).map(|error| error.code),
        Some(McpMrtrErrorCode::TransportFailed)
    );
    assert!(delivered.lock().unwrap().is_empty());
    let _ = stop.send(());
}

/// transport.test.ts "reports HTTP and malformed response failures through onerror and send" (HTTP 503 text).
#[tokio::test]
async fn reports_http_503_failures_through_send() {
    reports_http_and_malformed_response_failures(|| {
        Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .body(Body::from("unavailable"))
            .unwrap()
    })
    .await;
}

/// transport.test.ts "reports HTTP and malformed response failures through onerror and send" (JSON that is
/// not a JSON-RPC message).
#[tokio::test]
async fn reports_non_json_rpc_json_failures_through_send() {
    reports_http_and_malformed_response_failures(|| {
        json_response(StatusCode::OK, json!({"invalid": true}))
    })
    .await;
}

/// transport.test.ts "reports HTTP and malformed response failures through onerror and send" (SSE data that is
/// not JSON).
#[tokio::test]
async fn reports_malformed_sse_data_failures_through_send() {
    reports_http_and_malformed_response_failures(|| sse_response(Body::from("data: invalid\n\n")))
        .await;
}

/// transport.test.ts "reports HTTP and malformed response failures through onerror and send" (SSE ends with only
/// a params-less progress notification, before the response).
#[tokio::test]
async fn reports_sse_eof_before_the_response_through_send() {
    reports_http_and_malformed_response_failures(|| {
        sse_response(Body::from(
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n",
        ))
    })
    .await;
}

/// transport.test.ts "delivers JSON-RPC errors as messages rather than transport errors": an HTTP 400 carrying a
/// JSON-RPC error is a message (code kept, data and remote text stripped), never a transport failure.
#[tokio::test]
async fn delivers_json_rpc_errors_as_messages_rather_than_transport_errors() {
    let (router, _requests) = recording(|| {
        json_response(
            StatusCode::BAD_REQUEST,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {"code": -32022, "message": "REMOTE_ERROR_CANARY", "data": {"supported": ["PRIVATE_DATA_CANARY"]}},
            }),
        )
    });
    let (url, stop) = serve(router).await;
    let client = transport(url, &[], None);
    let delivered = Mutex::new(Vec::new());
    let on_message = |message: Value| delivered.lock().unwrap().push(message);

    let reply = client
        .send(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            None,
            &on_message,
        )
        .await
        .expect("not a transport error")
        .expect("the error is delivered as the reply message");

    assert_eq!(reply["jsonrpc"], "2.0");
    assert_eq!(reply["id"], 1);
    assert_eq!(reply["error"]["code"], -32022);
    assert!(reply["error"].get("data").is_none());
    assert!(!reply.to_string().contains("CANARY"));
    assert!(delivered.lock().unwrap().is_empty());
    let _ = stop.send(());
}

/// transport.test.ts "aborts active POSTs on close and invokes onclose only once". Rust has no `close`: dropping
/// the in-flight `send` is the abort, observed as the server losing the request (its handler future is dropped).
/// The onclose-once and send-after-close assertions have no Rust surface (N-A).
#[tokio::test]
async fn aborts_active_posts_on_close() {
    struct OnDrop(Option<oneshot::Sender<()>>);
    impl Drop for OnDrop {
        fn drop(&mut self) {
            if let Some(signal) = self.0.take() {
                let _ = signal.send(());
            }
        }
    }
    let (entered_sender, entered) = oneshot::channel::<()>();
    let (aborted_sender, aborted) = oneshot::channel::<()>();
    let senders = Arc::new(Mutex::new(Some((entered_sender, aborted_sender))));
    let router = Router::new().route(
        "/mcp",
        post(move |_body: Bytes| {
            let senders = senders.clone();
            async move {
                let (entered, aborted) = senders.lock().unwrap().take().expect("one POST");
                let _guard = OnDrop(Some(aborted));
                let _ = entered.send(());
                futures::future::pending::<()>().await;
                StatusCode::OK
            }
        }),
    );
    let (url, stop) = serve(router).await;
    let client = Arc::new(transport(url, &[], None));

    let pending = tokio::spawn(async move {
        client
            .send(
                &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
                None,
                &|_| {},
            )
            .await
    });
    tokio::time::timeout(WAIT, entered)
        .await
        .expect("POST entered")
        .expect("entered signal");

    pending.abort();
    assert!(pending.await.expect_err("send aborted").is_cancelled());
    tokio::time::timeout(WAIT, aborted)
        .await
        .expect("server observed the aborted POST")
        .expect("abort signal");
    let _ = stop.send(());
}

/// transport.test.ts "cancels an active SSE reader on close". Rust has no `close`: dropping the in-flight `send`
/// while it reads SSE cancels the stream exactly once (the server-side body is dropped).
#[tokio::test]
async fn cancels_an_active_sse_reader_on_close() {
    let (dropped_sender, dropped) = oneshot::channel();
    let dropped_sender = Arc::new(Mutex::new(Some(dropped_sender)));
    let (router, _requests) = recording(move || {
        let signal = dropped_sender.lock().unwrap().take().expect("one POST");
        sse_response(open_sse(
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{\"progressToken\":1,\"progress\":0}}\n\n",
            signal,
        ))
    });
    let (url, stop) = serve(router).await;
    let client = Arc::new(transport(url, &[], None));
    let (reading_sender, reading) = oneshot::channel::<()>();
    let reading_sender = Mutex::new(Some(reading_sender));

    let pending = tokio::spawn(async move {
        let on_message = move |_: Value| {
            if let Some(signal) = reading_sender.lock().unwrap().take() {
                let _ = signal.send(());
            }
        };
        client
            .send(
                &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
                None,
                &on_message,
            )
            .await
    });
    tokio::time::timeout(WAIT, reading)
        .await
        .expect("SSE reader delivered the notification")
        .expect("reading signal");

    pending.abort();
    assert!(pending.await.expect_err("send aborted").is_cancelled());
    tokio::time::timeout(WAIT, dropped)
        .await
        .expect("SSE stream cancelled")
        .expect("drop signal");
    let _ = stop.send(());
}

/// transport.ts `send`: HTTP 401/403 cancels the body and rejects with
/// `AuthError("MCP server needs authentication; use /mcp auth-start.", { phase: "auth" })`.
async fn status_maps_to_auth_error(status: StatusCode) {
    let (router, _requests) = recording(move || {
        Response::builder()
            .status(status)
            .body(Body::empty())
            .unwrap()
    });
    let (url, stop) = serve(router).await;
    let client = transport(url, &[], None);
    let failure = client
        .send(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            None,
            &|_| {},
        )
        .await
        .expect_err("auth failure");
    assert_eq!(failure.kind, McpErrorKind::Auth);
    assert_eq!(failure.phase.as_deref(), Some("auth"));
    assert!(!failure.retriable);
    assert_eq!(
        failure.message,
        "MCP server needs authentication; use /mcp auth-start."
    );
    assert!(as_mcp_mrtr_error(&failure).is_none());
    let _ = stop.send(());
}

/// transport.ts 401 -> AuthError mapping (no upstream it(); present in src/modern/transport.rs).
#[tokio::test]
async fn http_401_maps_to_an_auth_error() {
    status_maps_to_auth_error(StatusCode::UNAUTHORIZED).await;
}

/// transport.ts 403 -> AuthError mapping (no upstream it(); present in src/modern/transport.rs).
#[tokio::test]
async fn http_403_maps_to_an_auth_error() {
    status_maps_to_auth_error(StatusCode::FORBIDDEN).await;
}
