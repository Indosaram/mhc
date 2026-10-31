//! Ports of senpi packages/coding-agent/test/mcp/4193-subscriptions.test.ts and
//! src/core/extensions/builtin/mcp/modern/sdk-bridge.test.ts (#4193 connection-owned `subscriptions/listen`).
//!
//! Upstream drives a real `ServerConnection` against a synthetic node HTTP peer (fixtures/subscriptions-http.ts);
//! this port does the same with `ServerConnection` against a local axum peer that records every POST and keeps each
//! listen stream open until the client drops it. senpi's `ModernSdkBridge` is the port's `McpClient` (local
//! initialize from discovery, `ModernSubscriptions` routing, the stateless `ModernHttpTransport`), so the bridge
//! cases run through the same connection surface; pure routing runs on `ModernSubscriptions` directly.

use axum::{
    Router,
    body::{Body, Bytes},
    http::HeaderMap,
    response::Response,
    routing::post,
};
use maho_ext_mcp::{
    catalog_cache::collect_server_catalog_for_cache,
    config_schema::{Auth, McpServerConfig, Transport},
    connection::{ServerConnection, ServerConnectionToolsChangedCause},
    log::McpLogger,
    modern::{
        era::DiscoveryResult,
        sdk_bridge::modern_initialize_result,
        subscriptions::{ModernSubscriptions, SubscriptionSend},
        wire::MODERN_PROTOCOL_VERSION,
    },
    resources::ensure_mcp_resource_subscriptions,
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Notify, mpsc, oneshot};

const WAIT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const SUBSCRIPTION_KEY: &str = "io.modelcontextprotocol/subscriptionId";

#[derive(Clone, Debug)]
struct WireRequest {
    method: String,
    message: Value,
    headers: HeaderMap,
}

/// One open `subscriptions/listen` response (upstream `fixture.streams[i]`).
#[derive(Clone)]
struct ListenStream {
    message: Value,
    /// Upstream `stream.response.write(...)`: each string is written as one SSE `data:` event.
    write: mpsc::UnboundedSender<String>,
    dropped: Arc<Mutex<Option<oneshot::Receiver<()>>>>,
}
impl ListenStream {
    fn id(&self) -> Value {
        self.message["id"].clone()
    }
    /// The client has not yet dropped this stream (upstream `cancelStream` not called).
    fn is_open(&self) -> bool {
        let mut dropped = self.dropped.lock().unwrap();
        matches!(
            dropped.as_mut().map(oneshot::Receiver::try_recv),
            Some(Err(oneshot::error::TryRecvError::Empty))
        )
    }
    /// Upstream `response.once("close", ...)`: resolves when the client drops the stream.
    async fn closed(&self) {
        let receiver = self
            .dropped
            .lock()
            .unwrap()
            .take()
            .expect("closed awaited once");
        tokio::time::timeout(WAIT, receiver)
            .await
            .expect("listen stream closed in time")
            .expect("drop signal");
    }
}

/// Fires once when the server-side SSE body is dropped (the client closed the response).
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

/// Upstream `openFixture(options)`, plus the sdk-bridge discovery fields.
#[derive(Default)]
struct FixtureOptions {
    tools: Option<Vec<Value>>,
    tool_result: Option<Value>,
    capabilities: Option<Value>,
    server_info: Option<Value>,
    instructions: Option<&'static str>,
    /// sdk-bridge.test.ts lists tools with `nextCursor: null`.
    null_next_cursor: bool,
}

struct Fixture {
    url: String,
    stop: Option<oneshot::Sender<()>>,
    requests: Arc<Mutex<Vec<WireRequest>>>,
    streams: Arc<Mutex<Vec<ListenStream>>>,
    changed: Arc<Notify>,
    tools: Arc<Mutex<Vec<Value>>>,
    root: tempfile::TempDir,
}

impl Fixture {
    async fn open(options: FixtureOptions) -> Self {
        let requests: Arc<Mutex<Vec<WireRequest>>> = Arc::default();
        let streams: Arc<Mutex<Vec<ListenStream>>> = Arc::default();
        let changed = Arc::new(Notify::new());
        let tools = Arc::new(Mutex::new(options.tools.unwrap_or_else(|| {
            vec![json!({"name": "echo", "inputSchema": {"type": "object"}})]
        })));
        let mut discovery = json!({
            "resultType": "complete",
            "supportedVersions": ["2026-07-28"],
            "capabilities": options.capabilities.unwrap_or_else(|| json!({
                "tools": {"listChanged": true},
                "prompts": {"listChanged": true},
                "resources": {"listChanged": true, "subscribe": true},
            })),
            "_meta": {"io.modelcontextprotocol/serverInfo": options.server_info.unwrap_or_else(|| json!({"name": "synthetic-peer", "version": "1"}))},
        });
        if let Some(instructions) = options.instructions {
            discovery["instructions"] = json!(instructions);
        }
        let tool_result = options.tool_result.unwrap_or_else(|| json!({}));
        let null_next_cursor = options.null_next_cursor;

        let state = (
            requests.clone(),
            streams.clone(),
            changed.clone(),
            tools.clone(),
        );
        let app = Router::new().route(
            "/mcp",
            post(move |headers: HeaderMap, body: Bytes| {
                let (requests, streams, changed, tools) = state.clone();
                let (discovery, tool_result) = (discovery.clone(), tool_result.clone());
                async move {
                    let message: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                    let method = message["method"].as_str().unwrap_or_default().to_owned();
                    requests.lock().unwrap().push(WireRequest {
                        method: method.clone(),
                        message: message.clone(),
                        headers,
                    });
                    if method == "subscriptions/listen" {
                        let (write, written) = mpsc::unbounded_channel::<String>();
                        let (dropped_sender, dropped) = oneshot::channel();
                        let acknowledged = json!({
                            "jsonrpc": "2.0",
                            "method": "notifications/subscriptions/acknowledged",
                            "params": {
                                "_meta": {SUBSCRIPTION_KEY: message["id"]},
                                "notifications": message["params"]["notifications"],
                            },
                        });
                        streams.lock().unwrap().push(ListenStream {
                            message,
                            write,
                            dropped: Arc::new(Mutex::new(Some(dropped))),
                        });
                        changed.notify_waiters();
                        // The acknowledgment first, then every server write, until the client drops it.
                        let stream = futures::stream::unfold(
                            (Some(acknowledged.to_string()), written),
                            |(first, mut written)| async move {
                                let data = match first {
                                    Some(first) => first,
                                    None => written.recv().await?,
                                };
                                Some((
                                    Ok::<_, std::io::Error>(Bytes::from(format!(
                                        "data: {data}\n\n"
                                    ))),
                                    (None, written),
                                ))
                            },
                        );
                        return Response::builder()
                            .header("Content-Type", "text/event-stream")
                            .body(Body::from_stream(DropSignal {
                                stream: Box::pin(stream),
                                signal: Some(dropped_sender),
                            }))
                            .unwrap();
                    }
                    changed.notify_waiters();
                    let mut result = match method.as_str() {
                        "server/discover" => discovery,
                        "tools/list" => {
                            let mut result = json!({"resultType": "complete", "tools": tools.lock().unwrap().clone()});
                            if null_next_cursor {
                                result["nextCursor"] = Value::Null;
                            }
                            result
                        }
                        "prompts/list" => json!({"resultType": "complete", "prompts": []}),
                        "resources/list" => {
                            json!({"resultType": "complete", "resources": [{"uri": "test://data", "name": "data"}]})
                        }
                        "resources/templates/list" => {
                            json!({"resultType": "complete", "resourceTemplates": []})
                        }
                        "tools/call" => json!({"resultType": "complete", "content": []}),
                        _ => json!({"resultType": "complete"}),
                    };
                    if method == "tools/call"
                        && let (Some(result), Some(extra)) =
                            (result.as_object_mut(), tool_result.as_object())
                    {
                        result.extend(extra.clone());
                    }
                    Response::builder()
                        .header("Content-Type", "application/json")
                        .body(Body::from(
                            json!({"jsonrpc": "2.0", "id": message["id"], "result": result})
                                .to_string(),
                        ))
                        .unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel::<()>();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });
        Self {
            url: format!("http://{address}/mcp"),
            stop: Some(stop),
            requests,
            streams,
            changed,
            tools,
            root: tempfile::tempdir().unwrap(),
        }
    }

    /// Upstream `new ServerConnection({ config: normalizeServer({ type: "http", url, auth: false }) })`.
    fn connection(&self) -> Arc<ServerConnection> {
        ServerConnection::new(
            "configured-fixture",
            McpServerConfig {
                enabled: Some(true),
                transport: Some(Transport::Http),
                url: Some(self.url.clone()),
                auth: Some(Auth::Disabled(false)),
                connect_timeout_ms: Some(10_000.0),
                ..Default::default()
            },
            None,
            Arc::new(Mutex::new(
                McpLogger::new("triage", self.root.path(), None).unwrap(),
            )),
        )
    }

    fn methods(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.method.clone())
            .collect()
    }

    /// Waits (bounded) until a recorded listen stream satisfies `accept`; returns the last such stream.
    async fn stream_where(&self, accept: impl Fn(&[ListenStream]) -> bool) -> ListenStream {
        tokio::time::timeout(WAIT, async {
            loop {
                let changed = self.changed.notified();
                {
                    let streams = self.streams.lock().unwrap();
                    if accept(&streams) {
                        return streams.last().cloned().expect("a stream");
                    }
                }
                changed.await;
            }
        })
        .await
        .expect("listen stream in time")
    }

    /// Upstream `fixture.next("subscriptions/listen")` after `count` earlier streams.
    async fn listen(&self, count: usize) -> ListenStream {
        self.stream_where(|streams| streams.len() > count).await
    }

    fn close(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

async fn connect(fixture: &Fixture) -> Arc<ServerConnection> {
    let connection = fixture.connection();
    tokio::time::timeout(WAIT, connection.connect())
        .await
        .expect("connect in time")
        .expect("connect");
    connection
}

/// 4193-subscriptions.test.ts "4193 a connected modern catalog opens subscriptions/listen with list-change opt-ins".
#[tokio::test]
async fn a_connected_modern_catalog_opens_subscriptions_listen_with_list_change_opt_ins() {
    let fixture = Fixture::open(FixtureOptions::default()).await;
    let connection = connect(&fixture).await;
    let client = connection.client().unwrap();
    for method in ["tools/list", "prompts/list", "resources/list"] {
        client
            .request(method, json!({}), REQUEST_TIMEOUT)
            .await
            .unwrap();
    }
    // Connect and all initial lists are complete: no timer or network polling guesses.
    let listen = fixture.listen(0).await;
    let methods = fixture.methods();
    assert!(methods.contains(&"server/discover".to_owned()));
    assert!(methods.contains(&"tools/list".to_owned()));
    assert!(methods.contains(&"subscriptions/listen".to_owned()));
    let first = fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .find(|request| request.method == "subscriptions/listen")
        .cloned()
        .expect("listen request");
    assert_eq!(listen.message["method"], "subscriptions/listen");
    let notifications = &first.message["params"]["notifications"];
    assert_eq!(notifications["toolsListChanged"], true);
    assert_eq!(notifications["promptsListChanged"], true);
    assert_eq!(notifications["resourcesListChanged"], true);
    connection.dispose().await.unwrap();
    fixture.close();
}

/// 4193-subscriptions.test.ts "4193 omits undeclared filters and keeps calls independent of the listen lifetime".
#[tokio::test]
async fn omits_undeclared_filters_and_keeps_calls_independent_of_the_listen_lifetime() {
    let fixture = Fixture::open(FixtureOptions {
        capabilities: Some(json!({"tools": {}, "resources": {}, "prompts": {}})),
        ..Default::default()
    })
    .await;
    let connection = connect(&fixture).await;
    let client = connection.client().unwrap();
    client
        .request("tools/list", json!({}), REQUEST_TIMEOUT)
        .await
        .unwrap();
    client
        .request("resources/list", json!({}), REQUEST_TIMEOUT)
        .await
        .unwrap();
    ensure_mcp_resource_subscriptions(
        client.clone(),
        &[json!({"uri": "test://data"})],
        REQUEST_TIMEOUT,
    )
    .await;
    client
        .request("tools/call", json!({"name": "echo"}), REQUEST_TIMEOUT)
        .await
        .unwrap();
    let methods = fixture.methods();
    assert!(!methods.contains(&"subscriptions/listen".to_owned()));
    assert!(!methods.contains(&"resources/subscribe".to_owned()));
    connection.dispose().await.unwrap();
    fixture.close();
}

/// Shared body of 4193-subscriptions.test.ts "4193 %s invalidates and refetches the existing catalog".
async fn invalidates_and_refetches_the_existing_catalog(method: &str) {
    let fixture = Fixture::open(FixtureOptions {
        tools: Some(vec![
            json!({"name": "before", "inputSchema": {"type": "object"}}),
        ]),
        ..Default::default()
    })
    .await;
    let connection = connect(&fixture).await;
    collect_server_catalog_for_cache(&connection, REQUEST_TIMEOUT, "synthetic")
        .await
        .unwrap();
    fixture.listen(0).await;
    ensure_mcp_resource_subscriptions(
        connection.client().unwrap(),
        &[json!({"uri": "test://data"})],
        REQUEST_TIMEOUT,
    )
    .await;
    // Observe the final resource-filter stream on the server, not a timed polling guess.
    let stream = fixture
        .stream_where(|streams| {
            streams.last().is_some_and(|stream| {
                stream.message["params"]["notifications"]["resourceSubscriptions"]
                    .as_array()
                    .is_some_and(|uris| !uris.is_empty())
            })
        })
        .await;
    let mut changes = connection.on_tools_changed();
    *fixture.tools.lock().unwrap() =
        vec![json!({"name": "after", "inputSchema": {"type": "object"}})];
    let mut params = json!({"_meta": {SUBSCRIPTION_KEY: stream.id()}});
    if method == "notifications/resources/updated" {
        params["uri"] = json!("test://data");
    }
    stream
        .write
        .send(json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string())
        .unwrap();
    // Upstream refetches from `onToolsChanged`.
    tokio::time::timeout(WAIT, async {
        loop {
            if changes.recv().await.unwrap().cause
                == ServerConnectionToolsChangedCause::Notification
            {
                break;
            }
        }
    })
    .await
    .expect("tools changed in time");
    let catalog = collect_server_catalog_for_cache(&connection, REQUEST_TIMEOUT, "synthetic")
        .await
        .unwrap();
    let names: Vec<&str> = catalog
        .tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(names, ["after"]);
    assert!(
        !fixture
            .methods()
            .contains(&"resources/subscribe".to_owned())
    );
    // The open stream must not occupy the application request path.
    let called = connection
        .client()
        .unwrap()
        .request("tools/call", json!({"name": "after"}), REQUEST_TIMEOUT)
        .await
        .unwrap();
    assert_eq!(called["content"], json!([]));
    connection.dispose().await.unwrap();
    fixture.close();
}

/// 4193-subscriptions.test.ts "4193 %s invalidates and refetches the existing catalog" (tools list_changed).
#[tokio::test]
async fn tools_list_changed_invalidates_and_refetches_the_existing_catalog() {
    invalidates_and_refetches_the_existing_catalog("notifications/tools/list_changed").await;
}

/// 4193-subscriptions.test.ts "4193 %s invalidates and refetches the existing catalog" (prompts list_changed).
#[tokio::test]
async fn prompts_list_changed_invalidates_and_refetches_the_existing_catalog() {
    invalidates_and_refetches_the_existing_catalog("notifications/prompts/list_changed").await;
}

/// 4193-subscriptions.test.ts "4193 %s invalidates and refetches the existing catalog" (resources list_changed).
#[tokio::test]
async fn resources_list_changed_invalidates_and_refetches_the_existing_catalog() {
    invalidates_and_refetches_the_existing_catalog("notifications/resources/list_changed").await;
}

/// 4193-subscriptions.test.ts "4193 %s invalidates and refetches the existing catalog" (resources updated).
#[tokio::test]
async fn resources_updated_invalidates_and_refetches_the_existing_catalog() {
    invalidates_and_refetches_the_existing_catalog("notifications/resources/updated").await;
}

/// 4193-subscriptions.test.ts "4193 disposal aborts the outstanding HTTP listen stream".
#[tokio::test]
async fn disposal_aborts_the_outstanding_http_listen_stream() {
    let fixture = Fixture::open(FixtureOptions::default()).await;
    let connection = connect(&fixture).await;
    connection
        .client()
        .unwrap()
        .request("tools/list", json!({}), REQUEST_TIMEOUT)
        .await
        .unwrap();
    let stream = fixture.listen(0).await;
    connection.dispose().await.unwrap();
    stream.closed().await;
    fixture.close();
}

/// 4193-subscriptions.test.ts "4193 connection renewal closes its old stream and reestablishes subscriptions".
#[tokio::test]
async fn connection_renewal_closes_its_old_stream_and_reestablishes_subscriptions() {
    let fixture = Fixture::open(FixtureOptions::default()).await;
    let connection = connect(&fixture).await;
    connection
        .client()
        .unwrap()
        .request("tools/list", json!({}), REQUEST_TIMEOUT)
        .await
        .unwrap();
    let original = fixture.listen(0).await;
    let renewed_client = tokio::time::timeout(WAIT, connection.renew())
        .await
        .expect("renew in time")
        .unwrap();
    original.closed().await;
    let before = fixture.streams.lock().unwrap().len();
    renewed_client
        .request("tools/list", json!({}), REQUEST_TIMEOUT)
        .await
        .unwrap();
    let renewed = fixture.listen(before).await;
    assert_ne!(renewed.id(), original.id());
    connection.dispose().await.unwrap();
    fixture.close();
}

fn bridge_discovery() -> Value {
    json!({
        "resultType": "complete",
        "supportedVersions": [MODERN_PROTOCOL_VERSION],
        "capabilities": {"tools": {"listChanged": true}, "resources": {}, "prompts": {}},
        "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "modern-only", "version": "1", "title": "Modern"}},
        "instructions": "Use these tools",
    })
}

/// sdk-bridge.test.ts "populates SDK state through local initialize while keeping all wire traffic modern".
/// The connection factory's `server/discover` probe is the only pre-request traffic (upstream `sent == []` after
/// connect, with discovery injected). The upstream exact order `[listen, tools/list, tools/call]` comes from the
/// listen POST being issued synchronously before tools/list; in Rust the listen POST is a spawned task racing the
/// tools/list POST on another connection, so the port asserts the exact multiset instead.
#[tokio::test]
async fn populates_sdk_state_through_local_initialize_while_keeping_all_wire_traffic_modern() {
    let discovery = bridge_discovery();
    let fixture = Fixture::open(FixtureOptions {
        capabilities: Some(discovery["capabilities"].clone()),
        server_info: Some(discovery["_meta"]["io.modelcontextprotocol/serverInfo"].clone()),
        instructions: Some("Use these tools"),
        tool_result: Some(json!({"content": [{"type": "text", "text": "ok"}]})),
        null_next_cursor: true,
        ..Default::default()
    })
    .await;
    let connection = connect(&fixture).await;
    let client = connection.client().unwrap();
    assert_eq!(fixture.methods(), ["server/discover"]);
    assert_eq!(
        *client.server_capabilities.read().await,
        discovery["capabilities"]
    );
    assert_eq!(
        *client.server_info.read().await,
        discovery["_meta"]["io.modelcontextprotocol/serverInfo"]
    );
    assert_eq!(
        client.instructions.read().await.as_deref(),
        Some("Use these tools")
    );
    let listed = client
        .request("tools/list", json!({}), REQUEST_TIMEOUT)
        .await
        .unwrap();
    assert_eq!(listed["tools"][0]["name"], "echo");
    assert!(listed.get("nextCursor").is_none());
    let called = client
        .request("tools/call", json!({"name": "echo"}), REQUEST_TIMEOUT)
        .await
        .unwrap();
    assert_eq!(called["content"][0]["text"], "ok");
    let stream = fixture.listen(0).await;
    assert!(stream.is_open(), "neither call waits for the stream's end");

    connection.dispose().await.unwrap();
    // #4193: advertised list changes require a listen stream, but neither call waits for its end.
    stream.closed().await;
    let requests = fixture.requests.lock().unwrap().clone();
    let mut methods: Vec<&str> = requests
        .iter()
        .map(|request| request.method.as_str())
        .collect();
    methods.sort_unstable();
    assert_eq!(
        methods,
        [
            "server/discover",
            "subscriptions/listen",
            "tools/call",
            "tools/list"
        ]
    );
    assert_eq!(
        stream.message["params"]["notifications"],
        json!({"toolsListChanged": true})
    );
    for request in &requests {
        assert_eq!(
            request
                .headers
                .get("MCP-Protocol-Version")
                .and_then(|value| value.to_str().ok()),
            Some(MODERN_PROTOCOL_VERSION)
        );
        assert_eq!(
            request.message["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
            MODERN_PROTOCOL_VERSION
        );
        assert!(!request.headers.contains_key("Mcp-Session-Id"));
    }
    fixture.close();
}

/// sdk-bridge.test.ts "keeps SDK notification and elicitation handlers on the same client" (notification half):
/// a list change reaches the handlers only after its subscription is acknowledged. The elicitation half is N-A:
/// the port's bridge (`McpClient`) answers every modern server-to-client request with -32601, as upstream's real
/// modern transports do (transport.ts rejects id-bearing server requests; stdio-probe.ts replies -32601).
#[tokio::test]
async fn keeps_sdk_notification_handlers_on_the_same_client() {
    let (sender, mut sent) = mpsc::unbounded_channel::<Value>();
    let send: SubscriptionSend = Arc::new(
        move |message: Value| -> futures::future::BoxFuture<'static, Result<(), String>> {
            let _ = sender.send(message);
            Box::pin(async { Ok(()) })
        },
    );
    let discovery = bridge_discovery();
    let subscriptions = ModernSubscriptions::new(send, discovery["capabilities"].clone(), false);
    subscriptions.outgoing(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}));
    let listen = tokio::time::timeout(WAIT, sent.recv())
        .await
        .expect("listen sent in time")
        .expect("listen");
    assert_eq!(listen["method"], "subscriptions/listen");
    let subscription_id = listen["id"].clone();
    assert!(subscription_id.is_string());
    let changed = json!({
        "jsonrpc": "2.0",
        "method": "notifications/tools/list_changed",
        "params": {"_meta": {SUBSCRIPTION_KEY: subscription_id}},
    });
    assert!(!subscriptions.incoming(&changed), "not yet acknowledged");
    assert!(!subscriptions.incoming(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/subscriptions/acknowledged",
        "params": {"_meta": {SUBSCRIPTION_KEY: subscription_id}, "notifications": {"toolsListChanged": true}},
    })));
    // #4193: deliver a notification only after its subscription is acknowledged.
    assert!(subscriptions.incoming(&changed));
    subscriptions.dispose();
    let cancelled = tokio::time::timeout(WAIT, sent.recv())
        .await
        .expect("cancel sent in time")
        .expect("cancel");
    assert_eq!(cancelled["method"], "notifications/cancelled");
    assert_eq!(cancelled["params"]["requestId"], subscription_id);
}

/// sdk-bridge.test.ts "refuses unsupported discovery versions and malformed SDK capabilities".
#[test]
fn refuses_unsupported_discovery_versions_and_malformed_sdk_capabilities() {
    let discovery = |patch: Value| -> DiscoveryResult {
        let mut discovery = bridge_discovery().as_object().cloned().unwrap();
        discovery.extend(patch.as_object().cloned().unwrap());
        discovery
    };
    let unsupported = modern_initialize_result(
        &discovery(json!({"supportedVersions": ["future"]})),
        MODERN_PROTOCOL_VERSION,
    )
    .expect_err("unsupported discovery version");
    assert!(unsupported.0.contains("does not support"));
    assert!(
        modern_initialize_result(
            &discovery(json!({"capabilities": {"tools": "invalid"}})),
            MODERN_PROTOCOL_VERSION,
        )
        .is_err()
    );
}
