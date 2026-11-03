//! senpi-mcp rows 10 and 39: private-form readiness on pooled MCP connections.
//!
//! Row 10 ports the lease readiness listeners of upstream
//! `src/core/extensions/builtin/mcp/shared-connection.ts` (`#readinessListeners`, `#readinessSubscriptions` and
//! the aggregate `elicitationUiProvider` `enforced` / `eligible` / `subscribe`). No upstream test covers them
//! directly, so these tests drive the upstream code paths (`attach`, `release`, the per-lease subscription and
//! `dispose`) through `SharedMcpConnection`.
//!
//! Row 39 ports `test/mcp/presenter-legacy-readiness.test.ts`: a legacy connection renegotiates its
//! `initialize` capabilities when the host grants and later revokes a private-form presenter.

use axum::{Json, Router, response::IntoResponse, routing::post};
use maho_ai::utils::abort::AbortController;
use maho_core::{
    mcp_form_types::{
        McpFormBinding, McpFormInput, McpFormOutcome, McpFormOutcomeKind, McpFormOutcomeSource,
        McpFormPresentation, McpFormUnsubscribe, private_mcp_forms,
    },
    mcp_mrtr_presentations::register_mcp_mrtr_presentation,
};
use maho_ext_api::{AbortSignal, ExtensionUi};
use maho_ext_mcp::{
    config_schema::{Auth, McpServerConfig, Transport},
    connection::{ServerConnection, ServerConnectionState},
    errors::McpError,
    host_registry::HostMcpRegistry,
    log::McpLogger,
    request_origin::{McpRequestOrigin, create_mcp_request_origin, mcp_request_options},
    service::McpService,
    shared_connection::{SharedLeaseOptions, SharedMcpConnection},
    startup_race::McpStartupRaceResult,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

type Listener = Arc<dyn Fn() + Send + Sync>;

/// A host presenter grant for one session: `granted` decides whether the binding captures a ready presenter,
/// and every change notifies the binding's subscribers (upstream presenter authority `acquire` / `revoke`).
#[derive(Default)]
struct Presenter {
    granted: AtomicBool,
    listeners: Mutex<Vec<(u64, Listener)>>,
    next: AtomicU64,
    subscribes: AtomicUsize,
    unsubscribes: AtomicUsize,
    shown: AtomicUsize,
}

impl Presenter {
    fn set_granted(&self, granted: bool) {
        self.granted.store(granted, Ordering::SeqCst);
        let listeners = self
            .listeners
            .lock()
            .unwrap()
            .iter()
            .map(|(_, listener)| listener.clone())
            .collect::<Vec<_>>();
        for listener in listeners {
            listener();
        }
    }
}

/// An `ExtensionUi` registered with an enforced private-form binding backed by `presenter`. The presenter
/// answers every form with a human `accept` and empty content.
fn private_ui(presenter: &Arc<Presenter>) -> (Arc<dyn ExtensionUi>, AbortController) {
    let host = AbortController::new();
    let shown = presenter.clone();
    let presentation = McpFormPresentation {
        signal: host.signal(),
        ready: Arc::new(|| true),
        present: Arc::new(move |_: McpFormInput| {
            shown.shown.fetch_add(1, Ordering::SeqCst);
            futures::FutureExt::boxed(async {
                McpFormOutcome {
                    kind: McpFormOutcomeKind::Result(json!({"action": "accept", "content": {}})),
                    source: McpFormOutcomeSource::PresenterAction,
                }
            })
        }),
        synchronize: None,
    };
    let captured = presenter.clone();
    let subscribed = presenter.clone();
    let subscribe: Arc<dyn Fn(Listener) -> McpFormUnsubscribe + Send + Sync> =
        Arc::new(move |listener: Listener| -> McpFormUnsubscribe {
            let id = subscribed.next.fetch_add(1, Ordering::SeqCst);
            subscribed.subscribes.fetch_add(1, Ordering::SeqCst);
            subscribed.listeners.lock().unwrap().push((id, listener));
            let presenter = subscribed.clone();
            Box::new(move || {
                presenter
                    .listeners
                    .lock()
                    .unwrap()
                    .retain(|(entry, _)| *entry != id);
                presenter.unsubscribes.fetch_add(1, Ordering::SeqCst);
            })
        });
    let binding = McpFormBinding {
        epoch: "epoch".into(),
        signal: host.signal(),
        capture: Arc::new(move || {
            captured
                .granted
                .load(Ordering::SeqCst)
                .then(|| presentation.clone())
        }),
        dispose: Arc::new(|| {}),
        version: None,
        subscribe: Some(subscribe),
    };
    let ui: Arc<dyn ExtensionUi> =
        Arc::new(crate::command_notify_redaction::RecordingUi::default());
    register_mcp_mrtr_presentation(ui.clone(), host.signal(), None, Some(binding));
    (ui, host)
}

fn logger(name: &str, root: &Path) -> Arc<Mutex<McpLogger>> {
    Arc::new(Mutex::new(
        McpLogger::new(name, root, None).expect("logger"),
    ))
}

/// A pooled connection that never opens a transport in these tests.
fn pooled(root: &Path) -> Arc<SharedMcpConnection> {
    let config = McpServerConfig {
        enabled: Some(true),
        transport: Some(Transport::Http),
        url: Some("http://127.0.0.1:1/mcp".into()),
        auth: Some(Auth::Disabled(false)),
        ..Default::default()
    };
    let connection = ServerConnection::new("readiness", config, None, logger("readiness", root));
    SharedMcpConnection::new(
        connection,
        root.into(),
        "hash".into(),
        Duration::from_secs(1),
    )
}

/// Subscribes a counting listener to the connection's aggregate readiness.
fn count_readiness(shared: &Arc<SharedMcpConnection>) -> (Arc<AtomicUsize>, McpFormUnsubscribe) {
    let fired = Arc::new(AtomicUsize::new(0));
    let counted = fired.clone();
    let listener: Listener = Arc::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
    });
    (fired, shared.subscribe_readiness(listener))
}

fn private_options(ui: &Arc<dyn ExtensionUi>) -> SharedLeaseOptions {
    SharedLeaseOptions {
        elicitation_ui: Some(ui.clone()),
        elicitation_enabled: true,
        project_policy: None,
    }
}

/// shared-connection.ts `attach`: every attach notifies the connection's readiness listeners
/// (`for (const listener of this.#readinessListeners) listener()`).
#[tokio::test]
async fn readiness_listeners_fire_when_a_lease_attaches() {
    let root = tempfile::tempdir().expect("root");
    let shared = pooled(root.path());
    let (fired, _off) = count_readiness(&shared);
    let first = shared.attach(1, "key".into()).expect("first lease");
    assert_eq!(fired.load(Ordering::SeqCst), 1);
    let second = shared.attach(2, "key".into()).expect("second lease");
    assert_eq!(fired.load(Ordering::SeqCst), 2);
    first.dispose();
    second.dispose();
    shared.dispose().await.expect("dispose");
}

/// shared-connection.ts `attach`: a lease whose provider has `subscribe` is subscribed once, and its presenter
/// readiness changes fan out to every connection readiness listener.
#[tokio::test]
async fn owner_presenter_readiness_changes_fan_out_to_connection_listeners() {
    let root = tempfile::tempdir().expect("root");
    let shared = pooled(root.path());
    let presenter = Arc::new(Presenter::default());
    let (ui, _host) = private_ui(&presenter);
    let (fired, _off) = count_readiness(&shared);
    let lease = shared.attach(1, "key".into()).expect("lease");
    shared.set_owner_options(1, private_options(&ui));
    assert_eq!(presenter.subscribes.load(Ordering::SeqCst), 1);
    assert!(shared.readiness_enforced());
    assert!(!shared.readiness_eligible(), "no presenter is granted yet");

    let before = fired.load(Ordering::SeqCst);
    presenter.set_granted(true);
    assert_eq!(fired.load(Ordering::SeqCst), before + 1);
    assert!(shared.readiness_eligible());
    presenter.set_granted(false);
    assert_eq!(fired.load(Ordering::SeqCst), before + 2);
    assert!(!shared.readiness_eligible());

    lease.dispose();
    shared.dispose().await.expect("dispose");
}

/// shared-connection.ts `release`: the released lease's subscription is removed, and the remaining leases are
/// told readiness changed; releasing the last lease notifies nobody.
#[tokio::test]
async fn releasing_a_lease_unsubscribes_it_and_notifies_only_while_others_remain() {
    let root = tempfile::tempdir().expect("root");
    let shared = pooled(root.path());
    let (left_presenter, right_presenter) = (
        Arc::new(Presenter::default()),
        Arc::new(Presenter::default()),
    );
    let (left_ui, _left_host) = private_ui(&left_presenter);
    let (right_ui, _right_host) = private_ui(&right_presenter);
    let (fired, _off) = count_readiness(&shared);
    let left = shared.attach(1, "key".into()).expect("left lease");
    shared.set_owner_options(1, private_options(&left_ui));
    let right = shared.attach(2, "key".into()).expect("right lease");
    shared.set_owner_options(2, private_options(&right_ui));

    let before = fired.load(Ordering::SeqCst);
    left.dispose();
    assert_eq!(left_presenter.unsubscribes.load(Ordering::SeqCst), 1);
    assert_eq!(
        fired.load(Ordering::SeqCst),
        before + 1,
        "the remaining lease is told readiness changed"
    );
    left_presenter.set_granted(true);
    assert_eq!(
        fired.load(Ordering::SeqCst),
        before + 1,
        "a released owner's presenter no longer reaches the connection"
    );

    right.dispose();
    assert_eq!(right_presenter.unsubscribes.load(Ordering::SeqCst), 1);
    assert_eq!(
        fired.load(Ordering::SeqCst),
        before + 1,
        "releasing the last lease notifies nobody"
    );
    shared.dispose().await.expect("dispose");
}

/// shared-connection.ts aggregate `eligible`: with any enforcing lease, any eligible live lease permits
/// negotiation; a released lease no longer counts.
#[tokio::test]
async fn enforced_eligibility_aggregates_over_live_leases() {
    let root = tempfile::tempdir().expect("root");
    let shared = pooled(root.path());
    assert!(!shared.readiness_enforced());
    assert!(!shared.readiness_eligible(), "no lease, no negotiation");

    let (ready, waiting) = (
        Arc::new(Presenter::default()),
        Arc::new(Presenter::default()),
    );
    let (ready_ui, _ready_host) = private_ui(&ready);
    let (waiting_ui, _waiting_host) = private_ui(&waiting);
    let first = shared.attach(1, "key".into()).expect("first lease");
    shared.set_owner_options(1, private_options(&ready_ui));
    assert!(shared.readiness_enforced());
    assert!(!shared.readiness_eligible());
    ready.set_granted(true);
    assert!(shared.readiness_eligible());

    let second = shared.attach(2, "key".into()).expect("second lease");
    shared.set_owner_options(2, private_options(&waiting_ui));
    assert!(
        shared.readiness_eligible(),
        "any ready owner permits negotiation"
    );
    first.dispose();
    assert!(shared.readiness_enforced());
    assert!(
        !shared.readiness_eligible(),
        "the released ready owner no longer counts"
    );
    second.dispose();
    shared.dispose().await.expect("dispose");
}

/// shared-connection.ts aggregate `eligible` without enforcement: every live lease must be eligible
/// (`leases.length > 0 && leases.every(eligible)`, `elicitationEnabled !== false`).
#[tokio::test]
async fn unenforced_eligibility_requires_every_live_lease() {
    let root = tempfile::tempdir().expect("root");
    let shared = pooled(root.path());
    let plain = |elicitation_enabled| SharedLeaseOptions {
        elicitation_ui: None,
        elicitation_enabled,
        project_policy: None,
    };
    let enabled = shared.attach(1, "key".into()).expect("enabled lease");
    shared.set_owner_options(1, plain(true));
    assert!(!shared.readiness_enforced());
    assert!(shared.readiness_eligible());
    let disabled = shared.attach(2, "key".into()).expect("disabled lease");
    shared.set_owner_options(2, plain(false));
    assert!(!shared.readiness_eligible());
    disabled.dispose();
    assert!(shared.readiness_eligible());
    enabled.dispose();
    assert!(!shared.readiness_eligible());
    shared.dispose().await.expect("dispose");
}

/// shared-connection.ts aggregate `subscribe`: the returned function removes the listener.
#[tokio::test]
async fn readiness_unsubscribe_stops_delivery() {
    let root = tempfile::tempdir().expect("root");
    let shared = pooled(root.path());
    let (fired, off) = count_readiness(&shared);
    let first = shared.attach(1, "key".into()).expect("first lease");
    assert_eq!(fired.load(Ordering::SeqCst), 1);
    off();
    let second = shared.attach(2, "key".into()).expect("second lease");
    assert_eq!(fired.load(Ordering::SeqCst), 1);
    first.dispose();
    second.dispose();
    shared.dispose().await.expect("dispose");
}

/// shared-connection.ts `dispose`: every lease is disposed, so every owner's readiness subscription ends.
#[tokio::test]
async fn dispose_ends_every_owner_readiness_subscription() {
    let root = tempfile::tempdir().expect("root");
    let shared = pooled(root.path());
    let (left_presenter, right_presenter) = (
        Arc::new(Presenter::default()),
        Arc::new(Presenter::default()),
    );
    let (left_ui, _left_host) = private_ui(&left_presenter);
    let (right_ui, _right_host) = private_ui(&right_presenter);
    let left = shared.attach(1, "key".into()).expect("left lease");
    shared.set_owner_options(1, private_options(&left_ui));
    let right = shared.attach(2, "key".into()).expect("right lease");
    shared.set_owner_options(2, private_options(&right_ui));
    shared.dispose().await.expect("dispose");
    assert_eq!(left_presenter.unsubscribes.load(Ordering::SeqCst), 1);
    assert_eq!(right_presenter.unsubscribes.load(Ordering::SeqCst), 1);
    assert!(left_presenter.listeners.lock().unwrap().is_empty());
    assert!(right_presenter.listeners.lock().unwrap().is_empty());
    drop((left, right));
}

/// A legacy streamable-HTTP MCP server that records each `initialize`'s client capabilities and answers
/// `tools/call` with an SSE stream carrying one `elicitation/create` request; the client's answer is echoed
/// back as the tool result (upstream presenter-legacy-readiness.test.ts `fetch` stub).
struct LegacyFormStub {
    url: String,
    initializations: Arc<Mutex<Vec<Value>>>,
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

type Pending = Arc<Mutex<Option<(Value, tokio::sync::mpsc::UnboundedSender<String>)>>>;

fn sse(value: &Value) -> String {
    format!("data: {value}\n\n")
}

impl LegacyFormStub {
    async fn start() -> Self {
        let initializations = Arc::new(Mutex::new(Vec::new()));
        let pending: Pending = Arc::default();
        let recorded = initializations.clone();
        let app = Router::new().route(
            "/mcp",
            post(move |Json(request): Json<Value>| {
                let (recorded, pending) = (recorded.clone(), pending.clone());
                async move {
                    let method = request.get("method").and_then(Value::as_str);
                    if method == Some("server/discover") {
                        return Json(json!({"jsonrpc": "2.0", "id": request["id"],
                            "error": {"code": -32601, "message": "Method not found"}}))
                        .into_response();
                    }
                    let Some(method) = method else {
                        let (id, stream) = pending
                            .lock()
                            .unwrap()
                            .take()
                            .expect("Unexpected form result");
                        let text = serde_json::to_string(&request["result"]).expect("result text");
                        let _ = stream.send(sse(&json!({"jsonrpc": "2.0", "id": id,
                            "result": {"content": [{"type": "text", "text": text}]}})));
                        drop(stream);
                        return axum::http::StatusCode::ACCEPTED.into_response();
                    };
                    if request.get("id").is_none() {
                        return axum::http::StatusCode::ACCEPTED.into_response();
                    }
                    if method == "tools/call" {
                        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel::<String>();
                        sender
                            .send(sse(&json!({"jsonrpc": "2.0", "id": "question",
                                "method": "elicitation/create",
                                "params": {"message": "Synthetic consent",
                                    "requestedSchema": {"type": "object", "properties": {}}}})))
                            .expect("stream open");
                        *pending.lock().unwrap() = Some((request["id"].clone(), sender));
                        let body = futures::stream::unfold(receiver, |mut receiver| async move {
                            receiver
                                .recv()
                                .await
                                .map(|chunk| (Ok::<_, std::io::Error>(chunk), receiver))
                        });
                        return axum::response::Response::builder()
                            .header("content-type", "text/event-stream")
                            .body(axum::body::Body::from_stream(body))
                            .expect("sse response");
                    }
                    let result = match method {
                        "initialize" => {
                            recorded
                                .lock()
                                .unwrap()
                                .push(request["params"]["capabilities"].clone());
                            json!({"protocolVersion": "2025-11-25", "capabilities": {"tools": {}},
                                "serverInfo": {"name": "fixture", "version": "1"}})
                        }
                        "tools/list" => {
                            json!({"tools": [{"name": "fixture", "inputSchema": {"type": "object"}}]})
                        }
                        "prompts/list" => json!({"prompts": []}),
                        "resources/list" => json!({"resources": []}),
                        "resources/templates/list" => json!({"resourceTemplates": []}),
                        _ => json!({}),
                    };
                    Json(json!({"jsonrpc": "2.0", "id": request["id"], "result": result}))
                        .into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("stub listener");
        let address = listener.local_addr().expect("stub address");
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .expect("stub server");
        });
        Self {
            url: format!("http://{address}/mcp"),
            initializations,
            stop,
            task,
        }
    }

    fn initializations(&self) -> Vec<Value> {
        self.initializations.lock().unwrap().clone()
    }

    async fn close(self) {
        let _ = self.stop.send(());
        tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .expect("stub stops in time")
            .expect("stub task");
    }
}

/// Upstream `transition`: subscribe first, trigger, then wait (bounded) for the connection to reach
/// `connected` again.
async fn transition(connection: &Arc<ServerConnection>, trigger: impl FnOnce()) {
    let mut states = connection.on_state_change();
    trigger();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match states.recv().await {
                Ok(event) if event.state == ServerConnectionState::Connected => return,
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    panic!("connection state channel closed")
                }
            }
        }
    })
    .await
    .expect("Legacy connection did not renegotiate");
}

async fn attach(service: &mut McpService, agent_dir: &Path) {
    service
        .attach_session(agent_dir, agent_dir, &BTreeMap::new(), true, &[])
        .await
        .expect("attach");
    assert_eq!(
        service
            .wait_for_deferred_attach(Duration::from_secs(10))
            .await,
        McpStartupRaceResult::Settled
    );
}

async fn renegotiates_late_grants_and_revocation(shared: bool) {
    let stub = LegacyFormStub::start().await;
    let agent = tempfile::tempdir().expect("agent dir");
    std::fs::write(
        agent.path().join("mcp.json"),
        json!({"mcpServers": {"fixture": {"type": "http", "url": stub.url, "auth": false,
            "lifecycle": "eager", "connectTimeoutMs": 5000}}})
        .to_string(),
    )
    .expect("mcp.json");
    let registry = Arc::new(HostMcpRegistry::default());
    let presenter = Arc::new(Presenter::default());
    let (ui, _host) = private_ui(&presenter);
    let mut service = McpService::new(registry.clone(), 391);
    service.set_elicitation_ui(Some(ui.clone()));
    attach(&mut service, agent.path()).await;
    let connection = service.connections["fixture"]
        .entry
        .lock()
        .await
        .connection
        .clone();

    let peer_presenter = Arc::new(Presenter::default());
    let (peer_ui, _peer_host) = private_ui(&peer_presenter);
    let mut peer = shared.then(|| McpService::new(registry.clone(), 392));
    if let Some(peer) = &mut peer {
        peer.set_elicitation_ui(Some(peer_ui.clone()));
        attach(peer, agent.path()).await;
        assert_eq!(registry.size(), 1);
    }

    assert_eq!(stub.initializations(), vec![json!({})]);
    transition(&connection, || presenter.set_granted(true)).await;
    assert_eq!(
        stub.initializations(),
        vec![json!({}), json!({"elicitation": {}})]
    );

    let binding = private_mcp_forms(&ui).expect("private binding");
    let current = connection.clone();
    let origin = create_mcp_request_origin(McpRequestOrigin::new(
        1,
        connection.clone(),
        connection.request_lifetime_signal(),
        None,
        (binding.capture)(),
        true,
        "fixture".into(),
        None,
        Arc::new(move || current.state() == ServerConnectionState::Connected),
        Arc::new(|_: AbortSignal| {
            futures::FutureExt::boxed(async { Ok::<_, McpError>(Value::Null) })
        }),
    ));
    let client = connection.client().expect("connected client");
    let reply = tokio::time::timeout(
        Duration::from_secs(10),
        client.request_with_options(
            "tools/call",
            json!({"name": "fixture"}),
            Duration::from_secs(10),
            &mcp_request_options(Some(origin), None),
        ),
    )
    .await
    .expect("tool call settles")
    .expect("tool call");
    assert_eq!(
        reply["content"],
        json!([{"type": "text",
            "text": serde_json::to_string(&json!({"action": "accept", "content": {}})).unwrap()}])
    );
    assert_eq!(presenter.shown.load(Ordering::SeqCst), 1);
    assert!(
        (private_mcp_forms(&peer_ui).expect("peer binding").capture)().is_none(),
        "the peer session holds no presenter grant"
    );

    transition(&connection, || presenter.set_granted(false)).await;
    assert_eq!(
        stub.initializations(),
        vec![json!({}), json!({"elicitation": {}}), json!({})]
    );

    if let Some(mut peer) = peer {
        peer.dispose().await.expect("peer dispose");
    }
    service.dispose().await.expect("dispose");
    registry.dispose().await.expect("registry dispose");
    stub.close().await;
}

/// presenter-legacy-readiness.test.ts: "renegotiates late grants and revocation on a legacy connection
/// (shared=false)".
#[tokio::test]
async fn renegotiates_late_grants_and_revocation_on_a_legacy_connection_shared_false() {
    renegotiates_late_grants_and_revocation(false).await;
}

/// presenter-legacy-readiness.test.ts: "renegotiates late grants and revocation on a legacy connection
/// (shared=true)".
#[tokio::test]
async fn renegotiates_late_grants_and_revocation_on_a_legacy_connection_shared_true() {
    renegotiates_late_grants_and_revocation(true).await;
}
