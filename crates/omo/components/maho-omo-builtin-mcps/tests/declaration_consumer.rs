mod support;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use maho_ext_mcp::config_schema::ServerConfigWire;
use maho_ext_mcp::log::McpLogger;
use maho_ext_mcp::transport::{connect_mcp_transport, create_mcp_transport};
use maho_omo_builtin_mcps::{CONTEXT7_API_KEY_ENV, create_context7_declaration};
use serde_json::{Value, json};

const CONNECT_TIMEOUT_MS: f64 = 2_000.0;

struct FakeMcpServer {
    address: std::net::SocketAddr,
    requests: Arc<AtomicUsize>,
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl FakeMcpServer {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&requests);
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let router = Router::new().route(
                "/mcp",
                post(move |headers: HeaderMap, Json(value): Json<Value>| {
                    let counter = Arc::clone(&counter);
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        if value.get("id").is_none() {
                            return StatusCode::ACCEPTED.into_response();
                        }
                        let authenticated = headers.contains_key("authorization");
                        let result = match value["method"].as_str() {
                            Some("initialize") => json!({
                                "protocolVersion": "2025-11-25",
                                "capabilities": {},
                                "serverInfo": {"name": "builtin-mcps-fixture", "version": "1"},
                            }),
                            Some("tools/list") => json!({
                                "tools": [{
                                    "name": "context7-echo",
                                    "description": if authenticated { "authenticated" } else { "anonymous" },
                                    "inputSchema": {"type": "object"},
                                }],
                            }),
                            _ => json!({}),
                        };
                        Json(json!({"jsonrpc": "2.0", "id": value.get("id").cloned().unwrap_or(Value::Null), "result": result})).into_response()
                    }
                }),
            );
            axum::serve(listener, router).with_graceful_shutdown(async {
                let _ = stopped.await;
            }).await.expect("serve");
        });
        Self { address, requests, stop, task }
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    async fn shutdown(self) {
        let _ = self.stop.send(());
        tokio::time::timeout(Duration::from_secs(3), self.task).await.expect("server stops").expect("server task");
    }
}

struct Harness {
    server: FakeMcpServer,
    env: BTreeMap<String, String>,
    connection: maho_ext_mcp::transport::McpTransportConnection,
    _logs: tempfile::TempDir,
}

impl Harness {
    async fn start(api_key: Option<&str>) -> Self {
        let server = FakeMcpServer::start().await;
        let env: BTreeMap<String, String> = api_key
            .map(|value| BTreeMap::from([(CONTEXT7_API_KEY_ENV.to_owned(), value.to_owned())]))
            .unwrap_or_default();
        let mut declaration = create_context7_declaration(&env);
        declaration.url = Some(format!("http://{}/mcp", server.address));
        let mut wire = ServerConfigWire::from(&declaration);
        // Test-harness bound only: the declaration itself carries no connect timeout.
        wire.connect_timeout_ms = Some(CONNECT_TIMEOUT_MS);

        let logs = tempfile::tempdir().expect("log dir");
        let logger = Arc::new(Mutex::new(McpLogger::new("context7", logs.path(), None).expect("logger")));
        let connection = create_mcp_transport("context7", &wire, Some(&env), logger).expect("transport");
        Self { server, env, connection, _logs: logs }
    }

    async fn tools(&self) -> Value {
        let client = connect_mcp_transport(&self.connection).await.expect("connect");
        let tools = client.request("tools/list", json!({}), Duration::from_secs(2)).await.expect("tools/list")["tools"].clone();
        client.close().await.expect("close");
        tools
    }

    async fn shutdown(self) {
        self.server.shutdown().await;
    }
}

#[tokio::test]
async fn given_the_anonymous_context7_declaration_when_a_local_mcp_server_serves_it_then_the_declared_transport_connects_without_a_bearer_header() {
    let harness = Harness::start(None).await;

    let tools = harness.tools().await;

    assert_eq!(tools[0]["name"], json!("context7-echo"));
    assert_eq!(tools[0]["description"], json!("anonymous"), "no CONTEXT7_API_KEY means no Authorization header");
    harness.shutdown().await;
}

#[tokio::test]
async fn given_the_bearer_context7_declaration_when_a_local_mcp_server_serves_it_then_the_token_is_resolved_from_the_environment_at_connect_time() {
    let harness = Harness::start(Some("ctx7sk-live-secret")).await;

    let tools = harness.tools().await;

    assert_eq!(tools[0]["name"], json!("context7-echo"));
    assert_eq!(tools[0]["description"], json!("authenticated"), "bearerTokenEnv is read at connect time");
    assert!(!harness.env.is_empty());
    harness.shutdown().await;
}

#[tokio::test]
async fn given_the_declared_lazy_lifecycle_when_the_transport_is_created_then_no_request_reaches_the_server_until_it_is_used() {
    let harness = Harness::start(None).await;

    assert!(harness.connection.client().is_err(), "lazy: creating the transport opens no connection");
    assert_eq!(harness.server.requests(), 0, "lazy: nothing connects before a tool call needs it");

    let _ = harness.tools().await;
    assert!(harness.server.requests() > 0, "the declared transport does connect when it is used");
    harness.shutdown().await;
}

#[test]
fn given_the_wire_form_of_the_declaration_when_compared_then_it_carries_upstreams_field_names() {
    let wire = ServerConfigWire::from(&create_context7_declaration(&BTreeMap::new()));
    let value = serde_json::to_value(&wire).expect("wire json");

    assert_eq!(value["type"], json!("http"));
    assert_eq!(value["url"], json!("https://mcp.context7.com/mcp"));
    assert_eq!(value["enabled"], json!(true));
    assert_eq!(value["auth"], json!(false));
    assert_eq!(value["lifecycle"], json!("lazy"));
    assert_eq!(value["exposure"], json!("search"));
    assert!(value.get("bearerTokenEnv").is_none());

    let authenticated = serde_json::to_value(ServerConfigWire::from(&create_context7_declaration(&BTreeMap::from([
        (CONTEXT7_API_KEY_ENV.to_owned(), "ctx7sk-live-secret".to_owned()),
    ]))))
    .expect("wire json");
    assert_eq!(authenticated["auth"], json!("bearer"));
    assert_eq!(authenticated["bearerTokenEnv"], json!(CONTEXT7_API_KEY_ENV));
    assert!(!authenticated.to_string().contains("ctx7sk-live-secret"));
}
