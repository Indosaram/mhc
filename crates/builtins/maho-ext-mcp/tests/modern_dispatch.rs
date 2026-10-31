//! Ports of senpi src/core/extensions/builtin/mcp/modern/dispatch.test.ts ("dual-era factory dispatch") against
//! modern/dispatch.rs, modern/era.rs and modern/client.rs, plus the createMcpRequestClient / elicitationEnabled
//! plumbing (modern/client.ts) the dispatch path drives. Every HTTP exchange goes to a local axum stub that records
//! the method, body and headers of each request (upstream stubs global `fetch`).
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use maho_ext_mcp::{
    config_schema::{Auth, AuthMode, McpServerConfig, Transport},
    elicitation::McpFormReadiness,
    form_types::McpFormUnsubscribe,
    log::McpLogger,
    modern::{client::create_mcp_request_client, wire::MODERN_PROTOCOL_VERSION},
    transport::{connect_mcp_transport, create_mcp_transport, shutdown_mcp_transport},
    transport_sdk::{McpClient, McpTransportSpec},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, atomic::Ordering},
    time::Duration,
};
use tokio::sync::mpsc;

/// Bound for every test body: only loopback I/O happens inside.
const BOUND: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, PartialEq)]
struct Exchange {
    method: String,
    body: String,
    headers: Vec<(String, String)>,
}
impl Exchange {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
    fn rpc_method(&self) -> String {
        serde_json::from_str::<Value>(&self.body).unwrap_or(Value::Null)["method"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }
}
type Wire = Arc<Mutex<Vec<Exchange>>>;

fn record(wire: &Wire, method: &Method, headers: &HeaderMap, body: &str) {
    let headers = headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                value.to_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    wire.lock().unwrap().push(Exchange {
        method: method.as_str().to_owned(),
        body: body.to_owned(),
        headers,
    });
}

async fn serve(router: Router) -> (url::Url, tokio::sync::oneshot::Sender<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
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

/// Upstream `config`: an HTTP server with 2000 ms connect/request timeouts.
fn config(url: &url::Url) -> McpServerConfig {
    McpServerConfig {
        transport: Some(Transport::Http),
        url: Some(url.to_string()),
        args: Some(vec![]),
        enabled: Some(true),
        idle_timeout_min: Some(10.0),
        connect_timeout_ms: Some(2000.0),
        request_timeout_ms: Some(2000.0),
        startup_timeout_ms: Some(250.0),
        ..Default::default()
    }
}

fn logger(name: &str, root: &std::path::Path) -> Arc<Mutex<McpLogger>> {
    Arc::new(Mutex::new(McpLogger::new(name, root, None).unwrap()))
}

fn ring(logger: &Arc<Mutex<McpLogger>>) -> Vec<String> {
    logger.lock().unwrap().get_ring_buffer()
}

/// Upstream `discovery`.
fn discovery() -> Value {
    json!({
        "resultType": "complete",
        "supportedVersions": [MODERN_PROTOCOL_VERSION],
        "capabilities": {"tools": {}},
        "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "modern", "version": "1"}},
        "instructions": "Modern instructions",
    })
}

async fn modern_server(
    State(wire): State<Wire>,
    method: Method,
    headers: HeaderMap,
    body: String,
) -> Response {
    record(&wire, &method, &headers, &body);
    let request: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let result = if request["method"] == "server/discover" {
        discovery()
    } else {
        json!({"resultType": "complete", "tools": []})
    };
    Json(json!({"jsonrpc": "2.0", "id": request["id"], "result": result})).into_response()
}

/// dispatch.test.ts "discovers a modern server once, connects without legacy traffic, and logs its real version".
#[tokio::test]
async fn discovers_a_modern_server_once_connects_without_legacy_traffic_and_logs_its_real_version()
{
    tokio::time::timeout(BOUND, async {
        let wire: Wire = Arc::default();
        let (url, stop) = serve(
            Router::new()
                .route("/mcp", any(modern_server))
                .with_state(wire.clone()),
        )
        .await;
        let root = tempfile::tempdir().unwrap();
        let logger = logger("modern", root.path());
        let mut configured = config(&url);
        configured.auth = Some(Auth::Mode(AuthMode::Bearer));
        configured.bearer_token_env = Some("FIXTURE".into());
        configured.headers = Some(BTreeMap::from([
            ("Mcp-Name".to_owned(), "stale".to_owned()),
            ("Mcp-Session-Id".to_owned(), "stale".to_owned()),
            ("MCP-Protocol-Version".to_owned(), "stale".to_owned()),
        ]));
        let env = BTreeMap::from([("FIXTURE".to_owned(), "fixture".to_owned())]);
        let connection =
            create_mcp_transport("modern", &configured, Some(&env), logger.clone()).unwrap();

        let (first, second) = tokio::join!(connection.materialize(), connection.materialize());
        assert!(Arc::ptr_eq(&first.unwrap(), &second.unwrap()));
        let client = connect_mcp_transport(&connection).await.unwrap();
        assert_eq!(
            client.instructions.read().await.as_deref(),
            Some("Modern instructions")
        );
        let tools = client
            .request("tools/list", json!({}), Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(tools["tools"], json!([]));
        let methods = |wire: &Wire| {
            wire.lock()
                .unwrap()
                .iter()
                .map(Exchange::rpc_method)
                .collect::<Vec<_>>()
        };
        assert_eq!(methods(&wire), ["server/discover", "tools/list"]);
        for exchange in wire.lock().unwrap().iter() {
            assert_eq!(exchange.method, "POST");
            assert_eq!(exchange.header("Authorization"), Some("Bearer fixture"));
            assert_eq!(
                exchange.header("MCP-Protocol-Version"),
                Some(MODERN_PROTOCOL_VERSION)
            );
            let rpc_method = exchange.rpc_method();
            assert_eq!(exchange.header("Mcp-Method"), Some(rpc_method.as_str()));
            assert_eq!(exchange.header("Mcp-Session-Id"), None);
            assert_eq!(exchange.header("Mcp-Name"), None);
        }
        assert_eq!(
            ring(&logger)
                .iter()
                .filter(|line| line.contains("era=modern version=2026-07-28"))
                .count(),
            1
        );
        shutdown_mcp_transport(&connection).await.unwrap();
        assert_eq!(methods(&wire), ["server/discover", "tools/list"]);
        let _ = stop.send(());
    })
    .await
    .expect("modern dispatch test exceeded its bound");
}

/// How the legacy stub answers `server/discover` (the upstream `it.each([400, 404, -32022])` parameter, plus a
/// generic JSON-RPC error for the plumbing tests).
#[derive(Clone, Copy, PartialEq)]
enum Discover {
    Status(u16),
    UnsupportedVersion,
    MethodNotFound,
}

#[derive(Clone)]
struct LegacyStub {
    wire: Wire,
    gets: mpsc::UnboundedSender<()>,
    discover: Discover,
}

async fn legacy_server(
    State(stub): State<LegacyStub>,
    method: Method,
    headers: HeaderMap,
    body: String,
) -> Response {
    record(&stub.wire, &method, &headers, &body);
    let request: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    if request["method"] == "server/discover" {
        return match stub.discover {
            Discover::Status(status) => StatusCode::from_u16(status).unwrap().into_response(),
            Discover::UnsupportedVersion => (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": request["id"],
                    "error": {
                        "code": -32022,
                        "message": "UnsupportedProtocolVersionError",
                        "data": {"supported": ["2025-06-18", "2025-03-26"]},
                    },
                })),
            )
                .into_response(),
            Discover::MethodNotFound => Json(json!({
                "jsonrpc": "2.0",
                "id": request["id"],
                "error": {"code": -32601, "message": "Method not found"},
            }))
            .into_response(),
        };
    }
    if method != Method::POST {
        if method == Method::GET {
            let _ = stub.gets.send(());
        }
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    if request["method"] == "notifications/initialized" {
        return StatusCode::ACCEPTED.into_response();
    }
    let result = if request["method"] == "initialize" {
        let version = if stub.discover == Discover::UnsupportedVersion {
            json!("2025-06-18")
        } else {
            request["params"]["protocolVersion"].clone()
        };
        json!({
            "protocolVersion": version,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "legacy", "version": "1"},
        })
    } else {
        json!({"tools": []})
    };
    (
        [("Mcp-Session-Id", "fixture-session")],
        Json(json!({"jsonrpc": "2.0", "id": request["id"], "result": result})),
    )
        .into_response()
}

async fn legacy_stub(
    discover: Discover,
) -> (
    url::Url,
    tokio::sync::oneshot::Sender<()>,
    Wire,
    mpsc::UnboundedReceiver<()>,
) {
    let wire: Wire = Arc::default();
    let (gets, received) = mpsc::unbounded_channel();
    let (url, stop) = serve(Router::new().route("/mcp", any(legacy_server)).with_state(
        LegacyStub {
            wire: wire.clone(),
            gets,
            discover,
        },
    ))
    .await;
    (url, stop, wire, received)
}

/// The recorded exchanges, minus the session-teardown DELETE: `close` bounds that request by a wall-clock
/// timeout, so its arrival is not ordered against the snapshot.
fn snapshot(wire: &Wire) -> Vec<Exchange> {
    wire.lock()
        .unwrap()
        .iter()
        .filter(|exchange| exchange.method != "DELETE")
        .cloned()
        .collect()
}

/// The standalone GET notification stream starts asynchronously after `initialize`; waiting for it orders it
/// before the next request in both runs.
async fn standalone_get(gets: &mut mpsc::UnboundedReceiver<()>) {
    tokio::time::timeout(Duration::from_secs(5), gets.recv())
        .await
        .expect("standalone GET stream")
        .expect("stub alive");
}

/// dispatch.test.ts "keeps legacy requests byte-identical after an HTTP %s discovery rejection" (senpi#4402).
async fn keeps_legacy_requests_byte_identical(discover: Discover) {
    tokio::time::timeout(BOUND, async {
        let (url, stop, wire, mut gets) = legacy_stub(discover).await;
        let root = tempfile::tempdir().unwrap();
        let logger = logger("legacy", root.path());

        // Baseline: the legacy client without any discovery probe (upstream `materializeMcpTransport`).
        let baseline = McpClient::materialize(
            "legacy",
            &McpTransportSpec::Http {
                url: url.clone(),
                headers: BTreeMap::new(),
            },
            logger.clone(),
        )
        .await
        .unwrap();
        baseline.initialize(Duration::from_secs(2)).await.unwrap();
        standalone_get(&mut gets).await;
        baseline
            .request("tools/list", json!({}), Duration::from_secs(2))
            .await
            .unwrap();
        let expected = snapshot(&wire);
        baseline.close().await.unwrap();
        wire.lock().unwrap().clear();

        let connection =
            create_mcp_transport("legacy", &config(&url), None, logger.clone()).unwrap();
        let client = connect_mcp_transport(&connection).await.unwrap();
        standalone_get(&mut gets).await;
        client
            .request("tools/list", json!({}), Duration::from_secs(2))
            .await
            .unwrap();
        if discover == Discover::UnsupportedVersion {
            assert!(
                ring(&logger)
                    .iter()
                    .any(|line| line.contains("era=legacy version=2025-06-18"))
            );
        }
        let actual = snapshot(&wire);
        shutdown_mcp_transport(&connection).await.unwrap();

        assert_eq!(actual[0].rpc_method(), "server/discover");
        assert_eq!(actual[1..], expected[..]);
        let _ = stop.send(());
    })
    .await
    .expect("legacy dispatch test exceeded its bound");
}

/// dispatch.test.ts "keeps legacy requests byte-identical after an HTTP %s discovery rejection" (status 400).
#[tokio::test]
async fn keeps_legacy_requests_byte_identical_after_an_http_400_discovery_rejection() {
    keeps_legacy_requests_byte_identical(Discover::Status(400)).await;
}

/// dispatch.test.ts "keeps legacy requests byte-identical after an HTTP %s discovery rejection" (status 404).
#[tokio::test]
async fn keeps_legacy_requests_byte_identical_after_an_http_404_discovery_rejection() {
    keeps_legacy_requests_byte_identical(Discover::Status(404)).await;
}

/// dispatch.test.ts "keeps legacy requests byte-identical after an HTTP %s discovery rejection" (-32022 with only
/// legacy versions supported).
#[tokio::test]
async fn keeps_legacy_requests_byte_identical_after_an_http_32022_discovery_rejection() {
    keeps_legacy_requests_byte_identical(Discover::UnsupportedVersion).await;
}

async fn modern_error_server(
    State(wire): State<Wire>,
    method: Method,
    headers: HeaderMap,
    body: String,
) -> Response {
    record(&wire, &method, &headers, &body);
    let request: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "jsonrpc": "2.0",
            "id": request["id"],
            "error": {"code": -32021, "message": "Missing capability"},
        })),
    )
        .into_response()
}

/// dispatch.test.ts "does not downgrade a recognized modern error to SDK initialize".
#[tokio::test]
async fn does_not_downgrade_a_recognized_modern_error_to_sdk_initialize() {
    tokio::time::timeout(BOUND, async {
        let wire: Wire = Arc::default();
        let (url, stop) = serve(
            Router::new()
                .route("/mcp", any(modern_error_server))
                .with_state(wire.clone()),
        )
        .await;
        let root = tempfile::tempdir().unwrap();
        let connection =
            create_mcp_transport("modern", &config(&url), None, logger("modern", root.path()))
                .unwrap();
        let Err(error) = connect_mcp_transport(&connection).await else {
            panic!("a recognized modern error must not connect");
        };
        assert!(
            error.message.contains("modern discovery failed"),
            "{}",
            error.message
        );
        let methods = wire
            .lock()
            .unwrap()
            .iter()
            .map(Exchange::rpc_method)
            .collect::<Vec<_>>();
        assert_eq!(methods, ["server/discover"]);
        shutdown_mcp_transport(&connection).await.unwrap();
        let _ = stop.send(());
    })
    .await
    .expect("modern error test exceeded its bound");
}

/// createMcpRequestClient / elicitationEnabled plumbing (modern/client.ts: `new Client(..., { capabilities:
/// mcpClientCapabilities(legacyEligible) })` with `legacyEligible = legacyMcpFormEligible(uiProvider,
/// elicitationEnabled)`): through the real dispatch path, a disabled connection advertises no elicitation
/// capability in `initialize`, an enabled one advertises `{ elicitation: {} }`. Not an upstream it() case.
#[tokio::test]
async fn elicitation_enabled_reaches_the_legacy_initialize_capabilities_through_dispatch() {
    tokio::time::timeout(BOUND, async {
        for (enabled, capabilities) in [(false, json!({})), (true, json!({"elicitation": {}}))] {
            let (url, stop, wire, _gets) = legacy_stub(Discover::MethodNotFound).await;
            let root = tempfile::tempdir().unwrap();
            let mut connection =
                create_mcp_transport("forms", &config(&url), None, logger("forms", root.path()))
                    .unwrap();
            connection.elicitation_enabled = enabled;
            let client = connect_mcp_transport(&connection).await.unwrap();
            assert_eq!(client.protocol.legacy_form_eligible(), Some(enabled));
            let initialize = wire
                .lock()
                .unwrap()
                .iter()
                .find(|exchange| exchange.rpc_method() == "initialize")
                .cloned()
                .expect("initialize request");
            let body: Value = serde_json::from_str(&initialize.body).unwrap();
            assert_eq!(
                body["params"]["capabilities"], capabilities,
                "enabled={enabled}"
            );
            shutdown_mcp_transport(&connection).await.unwrap();
            let _ = stop.send(());
        }
    })
    .await
    .expect("elicitation plumbing test exceeded its bound");
}

fn readiness(enforced: bool, eligible: bool) -> McpFormReadiness {
    McpFormReadiness {
        enforced: Arc::new(move || enforced),
        eligible: Arc::new(move || eligible),
        subscribe: Arc::new(
            |_listener: Arc<dyn Fn() + Send + Sync>| -> McpFormUnsubscribe { Box::new(|| {}) },
        ),
    }
}

/// createMcpRequestClient plumbing (modern/client.ts): `protocol.privateForms = uiProvider?.enforced?.() === true`
/// and `protocol.legacyFormEligible = legacyMcpFormEligible(uiProvider, elicitationEnabled)`, where a disabled
/// connection is never eligible and a provider's `eligible` wins otherwise. Not an upstream it() case.
#[tokio::test]
async fn create_mcp_request_client_records_the_form_policy_before_the_first_request() {
    let root = tempfile::tempdir().unwrap();
    let client = McpClient::materialize(
        "forms",
        &McpTransportSpec::Http {
            url: "http://127.0.0.1:1/mcp".parse().unwrap(),
            headers: BTreeMap::new(),
        },
        logger("forms", root.path()),
    )
    .await
    .unwrap();
    let cases = [
        (None, true, false, Some(true)),
        (None, false, false, Some(false)),
        (Some(readiness(true, true)), true, true, Some(true)),
        (Some(readiness(true, true)), false, true, Some(false)),
        (Some(readiness(false, false)), true, false, Some(false)),
    ];
    for (provider, enabled, private_forms, eligible) in cases {
        create_mcp_request_client(&client, None, provider.as_ref(), enabled);
        assert_eq!(
            client.protocol.private_forms.load(Ordering::Acquire),
            private_forms
        );
        assert_eq!(client.protocol.legacy_form_eligible(), eligible);
    }
}
