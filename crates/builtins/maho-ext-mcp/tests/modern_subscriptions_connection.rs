//! Port of senpi test/mcp/4193-subscriptions.test.ts and modern/sdk-bridge.test.ts:
//! connection-level listen opt-ins, undeclared filters, disposal aborting the HTTP listen stream,
//! renewal re-establishing subscriptions, and routing cases against ModernSubscriptions / sdk_bridge.

use axum::{Json, Router, body::Bytes, http::HeaderMap, response::IntoResponse, routing::post};
use maho_ext_mcp::{
    catalog_cache::collect_server_catalog_for_cache,
    config_schema::{Auth, McpServerConfig, Transport},
    connection::ServerConnection,
    log::McpLogger,
    modern::{
        sdk_bridge::{modern_initialize_result, normalize_modern_result},
        subscriptions::{ModernSubscriptions, SubscriptionSend},
        wire::MODERN_PROTOCOL_VERSION,
    },
    resources::ensure_mcp_resource_subscriptions,
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone, Debug)]
struct WireRequest {
    method: String,
    message: Value,
    headers: HeaderMap,
}

#[derive(Clone)]
struct ListenStreamHandle {
    id: Value,
    notify_tx: tokio::sync::mpsc::UnboundedSender<String>,
    dropped_rx: Arc<Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
}

impl ListenStreamHandle {
    async fn wait_dropped(&self) {
        let rx = self
            .dropped_rx
            .lock()
            .unwrap()
            .take()
            .expect("already awaited");
        tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .expect("stream dropped in time")
            .expect("stream drop channel");
    }
}

struct DropSignal<S> {
    stream: S,
    signal: Option<tokio::sync::oneshot::Sender<()>>,
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

struct SubscriptionsFixture {
    url: String,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    requests: Arc<Mutex<Vec<WireRequest>>>,
    request_notify: Arc<tokio::sync::Notify>,
    streams: Arc<Mutex<Vec<ListenStreamHandle>>>,
    stream_notify: Arc<tokio::sync::Notify>,
    tools: Arc<Mutex<Vec<Value>>>,
    root: tempfile::TempDir,
}

impl SubscriptionsFixture {
    async fn start(capabilities: Option<Value>, tools: Option<Vec<Value>>) -> Self {
        let requests: Arc<Mutex<Vec<WireRequest>>> = Arc::default();
        let request_notify = Arc::new(tokio::sync::Notify::new());
        let streams: Arc<Mutex<Vec<ListenStreamHandle>>> = Arc::default();
        let stream_notify = Arc::new(tokio::sync::Notify::new());

        let capabilities = capabilities.unwrap_or_else(|| {
            json!({
                "tools": {"listChanged": true},
                "prompts": {"listChanged": true},
                "resources": {"listChanged": true, "subscribe": true},
            })
        });
        let tools = Arc::new(Mutex::new(tools.unwrap_or_else(|| {
            vec![json!({"name": "echo", "inputSchema": {"type": "object"}})]
        })));

        let router_requests = requests.clone();
        let router_req_notify = request_notify.clone();
        let router_streams = streams.clone();
        let router_stream_notify = stream_notify.clone();
        let router_tools = tools.clone();

        let app = Router::new().route(
            "/mcp",
            post(move |headers: HeaderMap, body: Bytes| {
                let requests = router_requests.clone();
                let req_notify = router_req_notify.clone();
                let streams = router_streams.clone();
                let stream_notify = router_stream_notify.clone();
                let tools = router_tools.clone();
                let capabilities = capabilities.clone();

                async move {
                    let message: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                    let method = message
                        .get("method")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();

                    requests.lock().unwrap().push(WireRequest {
                        method: method.clone(),
                        message: message.clone(),
                        headers,
                    });
                    req_notify.notify_waiters();

                    if method == "subscriptions/listen" {
                        let (notify_tx, notify_rx) =
                            tokio::sync::mpsc::unbounded_channel::<String>();
                        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel::<()>();

                        streams.lock().unwrap().push(ListenStreamHandle {
                            id: message.get("id").cloned().unwrap_or(Value::Null),
                            notify_tx,
                            dropped_rx: Arc::new(Mutex::new(Some(dropped_rx))),
                        });
                        stream_notify.notify_waiters();

                        let initial = format!(
                            "data: {}\n\n",
                            json!({
                                "jsonrpc": "2.0",
                                "method": "notifications/subscriptions/acknowledged",
                                "params": {
                                    "_meta": {
                                        "io.modelcontextprotocol/subscriptionId": message["id"]
                                    },
                                    "notifications": message["params"]["notifications"]
                                }
                            })
                        );

                        let stream = futures::stream::unfold(
                            (Some(initial), notify_rx),
                            |(first, mut rx)| async move {
                                if let Some(initial_chunk) = first {
                                    Some((
                                        Ok::<_, std::io::Error>(Bytes::from(initial_chunk)),
                                        (None, rx),
                                    ))
                                } else {
                                    match rx.recv().await {
                                        Some(msg) => Some((
                                            Ok::<_, std::io::Error>(Bytes::from(format!(
                                                "data: {msg}\n\n"
                                            ))),
                                            (None, rx),
                                        )),
                                        None => None,
                                    }
                                }
                            },
                        );

                        let body = DropSignal {
                            stream,
                            signal: Some(dropped_tx),
                        };

                        return axum::response::Response::builder()
                            .header("Content-Type", "text/event-stream")
                            .body(axum::body::Body::from_stream(body))
                            .unwrap();
                    }

                    let id = message.get("id").cloned().unwrap_or(Value::Null);
                    let result = match method.as_str() {
                        "server/discover" => json!({
                            "resultType": "complete",
                            "supportedVersions": [MODERN_PROTOCOL_VERSION],
                            "capabilities": capabilities,
                            "_meta": {
                                "io.modelcontextprotocol/serverInfo": {
                                    "name": "synthetic-peer",
                                    "version": "1"
                                }
                            },
                            "instructions": "Use these tools"
                        }),
                        "tools/list" => {
                            let current = tools.lock().unwrap().clone();
                            json!({
                                "resultType": "complete",
                                "tools": current
                            })
                        }
                        "prompts/list" => json!({
                            "resultType": "complete",
                            "prompts": []
                        }),
                        "resources/list" => json!({
                            "resultType": "complete",
                            "resources": [{"uri": "test://data", "name": "data"}]
                        }),
                        "resources/templates/list" => json!({
                            "resultType": "complete",
                            "resourceTemplates": []
                        }),
                        "tools/call" => json!({
                            "resultType": "complete",
                            "content": []
                        }),
                        _ => json!({"resultType": "complete"}),
                    };

                    Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });

        let root = tempfile::tempdir().unwrap();
        Self {
            url: format!("http://{address}/mcp"),
            stop: Some(stop),
            requests,
            request_notify,
            streams,
            stream_notify,
            tools,
            root,
        }
    }

    fn create_connection(&self, name: &str) -> Arc<ServerConnection> {
        let config = McpServerConfig {
            enabled: Some(true),
            transport: Some(Transport::Http),
            url: Some(self.url.clone()),
            auth: Some(Auth::Disabled(false)),
            connect_timeout_ms: Some(5000.0),
            ..Default::default()
        };
        let logger = Arc::new(Mutex::new(
            McpLogger::new(name, self.root.path(), None).unwrap(),
        ));
        ServerConnection::new(name, config, None, logger)
    }

    fn set_tools(&self, tools: Vec<Value>) {
        *self.tools.lock().unwrap() = tools;
    }

    async fn wait_for_request(&self, method: &str) -> WireRequest {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let notified = self.request_notify.notified();
                if let Some(found) = self
                    .requests
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|r| r.method == method)
                    .cloned()
                {
                    return found;
                }
                notified.await;
            }
        })
        .await
        .expect("timed out waiting for request")
    }

    async fn wait_for_stream(&self) -> ListenStreamHandle {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let notified = self.stream_notify.notified();
                if let Some(stream) = self.streams.lock().unwrap().last().cloned() {
                    return stream;
                }
                notified.await;
            }
        })
        .await
        .expect("timed out waiting for stream")
    }

    async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

/// Upstream: 4193 a connected modern catalog opens subscriptions/listen with list-change opt-ins
#[tokio::test]
async fn connected_modern_catalog_opens_subscriptions_listen_with_list_change_opt_ins() {
    let fixture = SubscriptionsFixture::start(None, None).await;
    let connection = fixture.create_connection("fixture");
    let client = connection.connect().await.unwrap();

    let timeout = Duration::from_secs(2);
    let _ = client
        .request("tools/list", json!({}), timeout)
        .await
        .unwrap();
    let _ = client
        .request("prompts/list", json!({}), timeout)
        .await
        .unwrap();
    let _ = client
        .request("resources/list", json!({}), timeout)
        .await
        .unwrap();

    let _ = fixture.wait_for_request("subscriptions/listen").await;

    let methods: Vec<String> = fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.method.clone())
        .collect();
    assert!(methods.contains(&"server/discover".into()));
    assert!(methods.contains(&"tools/list".into()));
    assert!(methods.contains(&"subscriptions/listen".into()));

    let listen = fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .find(|r| r.method == "subscriptions/listen")
        .cloned()
        .expect("listen request");

    let notifications = &listen.message["params"]["notifications"];
    assert_eq!(notifications["toolsListChanged"], json!(true));
    assert_eq!(notifications["promptsListChanged"], json!(true));
    assert_eq!(notifications["resourcesListChanged"], json!(true));

    connection.dispose().await.unwrap();
    fixture.stop().await;
}

/// Upstream: 4193 omits undeclared filters and keeps calls independent of the listen lifetime
#[tokio::test]
async fn omits_undeclared_filters_and_keeps_calls_independent_of_the_listen_lifetime() {
    let capabilities = json!({
        "tools": {},
        "resources": {},
        "prompts": {},
    });
    let fixture = SubscriptionsFixture::start(Some(capabilities), None).await;
    let connection = fixture.create_connection("fixture");
    let client = connection.connect().await.unwrap();

    let timeout = Duration::from_secs(2);
    let _ = client
        .request("tools/list", json!({}), timeout)
        .await
        .unwrap();
    let _ = client
        .request("resources/list", json!({}), timeout)
        .await
        .unwrap();
    ensure_mcp_resource_subscriptions(client.clone(), &[json!({"uri": "test://data"})], timeout)
        .await;
    let _ = client
        .request("tools/call", json!({"name": "echo"}), timeout)
        .await
        .unwrap();

    let methods: Vec<String> = fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.method.clone())
        .collect();
    assert!(!methods.contains(&"subscriptions/listen".into()));
    assert!(!methods.contains(&"resources/subscribe".into()));

    connection.dispose().await.unwrap();
    fixture.stop().await;
}

/// Upstream: 4193 disposal aborts the outstanding HTTP listen stream
#[tokio::test]
async fn disposal_aborts_the_outstanding_http_listen_stream() {
    let fixture = SubscriptionsFixture::start(None, None).await;
    let connection = fixture.create_connection("fixture");
    let client = connection.connect().await.unwrap();

    let timeout = Duration::from_secs(2);
    let _ = client
        .request("tools/list", json!({}), timeout)
        .await
        .unwrap();

    let stream = fixture.wait_for_stream().await;
    connection.dispose().await.unwrap();
    stream.wait_dropped().await;

    fixture.stop().await;
}

/// Upstream: 4193 connection renewal closes its old stream and reestablishes subscriptions
#[tokio::test]
async fn connection_renewal_closes_its_old_stream_and_reestablishes_subscriptions() {
    let fixture = SubscriptionsFixture::start(None, None).await;
    let connection = fixture.create_connection("fixture");
    let client = connection.connect().await.unwrap();

    let timeout = Duration::from_secs(2);
    let _ = client
        .request("tools/list", json!({}), timeout)
        .await
        .unwrap();

    let first_stream = fixture.wait_for_stream().await;
    let first_id = first_stream.id.clone();

    let renewed_client = connection.renew().await.unwrap();
    first_stream.wait_dropped().await;

    let _ = renewed_client
        .request("tools/list", json!({}), timeout)
        .await
        .unwrap();

    let renewed_stream = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let notified = fixture.stream_notify.notified();
            let streams = fixture.streams.lock().unwrap().clone();
            if streams.len() >= 2 {
                return streams.last().unwrap().clone();
            }
            notified.await;
        }
    })
    .await
    .unwrap();

    assert_ne!(renewed_stream.id, first_id);

    connection.dispose().await.unwrap();
    fixture.stop().await;
}

/// Upstream: 4193 %s invalidates and refetches the existing catalog (notifications/tools/list_changed)
#[tokio::test]
async fn notification_invalidates_and_refetches_existing_catalog() {
    let initial_tools = vec![json!({"name": "before", "inputSchema": {"type": "object"}})];
    let fixture = SubscriptionsFixture::start(None, Some(initial_tools)).await;
    let connection = fixture.create_connection("fixture");
    let client = connection.connect().await.unwrap();

    let timeout = Duration::from_secs(5);
    let mut tools_changed = connection.on_tools_changed();

    let catalog = collect_server_catalog_for_cache(&connection, timeout, "synthetic")
        .await
        .unwrap();
    assert_eq!(catalog.tools.len(), 1);
    assert_eq!(catalog.tools[0]["name"], "before");

    let stream = fixture.wait_for_stream().await;

    // Server modifies tools
    fixture.set_tools(vec![
        json!({"name": "after", "inputSchema": {"type": "object"}}),
    ]);

    // Push notification over SSE stream
    let notification_body = json!({
        "jsonrpc": "2.0",
        "method": "notifications/tools/list_changed",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/subscriptionId": stream.id
            }
        }
    });
    stream
        .notify_tx
        .send(notification_body.to_string())
        .unwrap();

    let _ = tokio::time::timeout(Duration::from_secs(3), tools_changed.recv())
        .await
        .unwrap()
        .unwrap();

    let refreshed_catalog = collect_server_catalog_for_cache(&connection, timeout, "synthetic")
        .await
        .unwrap();
    assert_eq!(refreshed_catalog.tools.len(), 1);
    assert_eq!(refreshed_catalog.tools[0]["name"], "after");

    let methods: Vec<String> = fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.method.clone())
        .collect();
    assert!(!methods.contains(&"resources/subscribe".into()));

    let tool_call = client
        .request("tools/call", json!({"name": "after"}), timeout)
        .await
        .unwrap();
    assert_eq!(tool_call["content"], json!([]));

    connection.dispose().await.unwrap();
    fixture.stop().await;
}

/// Upstream: populates SDK state through local initialize while keeping all wire traffic modern
#[tokio::test]
async fn populates_sdk_state_through_local_initialize_while_keeping_all_wire_traffic_modern() {
    let fixture = SubscriptionsFixture::start(None, None).await;
    let connection = fixture.create_connection("fixture");
    let client = connection.connect().await.unwrap();

    let timeout = Duration::from_secs(2);
    let caps = client.server_capabilities.read().await.clone();
    assert_eq!(caps["tools"]["listChanged"], json!(true));

    let info = client.server_info.read().await.clone();
    assert_eq!(info["name"], json!("synthetic-peer"));

    let instructions = client.instructions.read().await.clone();
    assert_eq!(instructions.as_deref(), Some("Use these tools"));

    let _ = client
        .request("tools/list", json!({}), timeout)
        .await
        .unwrap();
    let _ = client
        .request("tools/call", json!({"name": "echo"}), timeout)
        .await
        .unwrap();

    let stream = fixture.wait_for_stream().await;

    connection.dispose().await.unwrap();
    stream.wait_dropped().await;

    let requests = fixture.requests.lock().unwrap().clone();
    for req in &requests {
        assert_eq!(
            req.headers
                .get("MCP-Protocol-Version")
                .and_then(|v| v.to_str().ok()),
            Some(MODERN_PROTOCOL_VERSION)
        );
        assert!(req.headers.get("Mcp-Session-Id").is_none());
        assert_eq!(
            req.message["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
            json!(MODERN_PROTOCOL_VERSION)
        );
    }

    fixture.stop().await;
}

/// Upstream: keeps SDK notification and elicitation handlers on the same client
/// (routing aspect: delivers notifications only after their subscription is acknowledged)
#[tokio::test]
async fn routing_delivers_notification_only_after_subscription_acknowledged() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let send: SubscriptionSend = Arc::new(move |msg: Value| {
        let tx = tx.clone();
        Box::pin(async move {
            let _ = tx.send(msg);
            Ok(())
        })
    });
    let capabilities = json!({
        "tools": {"listChanged": true},
        "prompts": {},
        "resources": {},
    });
    let subscriptions = ModernSubscriptions::new(send, capabilities, false);

    subscriptions.outgoing(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}));
    let listen_msg = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .expect("listen message");
    let subscription_id = listen_msg["id"].clone();

    // Before acknowledgment: notification is rejected
    let unacknowledged = json!({
        "jsonrpc": "2.0",
        "method": "notifications/tools/list_changed",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/subscriptionId": subscription_id
            }
        }
    });
    assert!(!subscriptions.incoming(&unacknowledged));

    // Acknowledge the subscription
    let ack = json!({
        "jsonrpc": "2.0",
        "method": "notifications/subscriptions/acknowledged",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/subscriptionId": subscription_id
            },
            "notifications": {
                "toolsListChanged": true
            }
        }
    });
    assert!(!subscriptions.incoming(&ack));

    // After acknowledgment: notification for acknowledged id is delivered
    let acknowledged = json!({
        "jsonrpc": "2.0",
        "method": "notifications/tools/list_changed",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/subscriptionId": subscription_id
            }
        }
    });
    assert!(subscriptions.incoming(&acknowledged));

    // Notification for wrong / unacknowledged id is rejected
    let wrong = json!({
        "jsonrpc": "2.0",
        "method": "notifications/tools/list_changed",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/subscriptionId": "other-id"
            }
        }
    });
    assert!(!subscriptions.incoming(&wrong));

    subscriptions.dispose();
}

/// Upstream: refuses unsupported discovery versions and malformed SDK capabilities
#[test]
fn refuses_unsupported_discovery_versions_and_malformed_sdk_capabilities() {
    let unsupported_version = json!({
        "supportedVersions": ["future"],
        "capabilities": {"tools": {}},
    });
    let err = modern_initialize_result(&unsupported_version, MODERN_PROTOCOL_VERSION)
        .expect_err("unsupported version must fail");
    assert!(err.0.contains("does not support"));

    let malformed_caps = json!({
        "supportedVersions": [MODERN_PROTOCOL_VERSION],
        "capabilities": {"tools": "invalid"},
    });
    let err2 = modern_initialize_result(&malformed_caps, MODERN_PROTOCOL_VERSION)
        .expect_err("malformed capabilities must fail");
    assert!(err2.0.contains("malformed"));
}

/// Upstream: normalize_modern_result strips null nextCursor from list responses
#[test]
fn normalizes_modern_result_stripping_null_next_cursor() {
    let with_null = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "tools": [],
            "nextCursor": null
        }
    });
    let normalized = normalize_modern_result(with_null);
    assert!(normalized["result"].get("nextCursor").is_none());

    let with_cursor = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "tools": [],
            "nextCursor": "page-2"
        }
    });
    let normalized2 = normalize_modern_result(with_cursor);
    assert_eq!(normalized2["result"]["nextCursor"], json!("page-2"));
}
