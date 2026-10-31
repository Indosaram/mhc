//! Ports of senpi test/mcp/modern-tasks.test.ts and modern-tasks-http.test.ts.
//!
//! Integration tests for MCP task handles, polling, cancellation, timeout caps,
//! deduplicated input requests, presenter loss, and HTTP header routing.

use axum::{Json, Router, response::IntoResponse, routing::post};
use futures::future::BoxFuture;
use maho_core::mcp_mrtr_presentations::McpMrtrPresentation;
use maho_ext_api::{AbortSignal, ExtensionUi};
use maho_ext_mcp::{
    catalog::McpToolCatalogEntry,
    config_schema::{McpServerConfig, Transport},
    connection::ServerConnection,
    expose::call::call_mcp_tool,
    log::McpLogger,
    modern::{
        elicitation::MrtrInputResponse,
        errors::{McpMrtrError, McpMrtrErrorCode, as_mcp_mrtr_error},
        request::{McpRequestOptions, mcp_request_with_clock},
        tasks::{MCP_TASK_EXTENSION, MCP_TASK_TIMEOUT_ENV, McpTaskOptions, follow_mcp_task},
    },
    request_origin::{McpRequestOrigin, create_mcp_request_origin},
    transport_sdk::{McpClient, McpTransportSpec},
    wrap::{MCP_ELAPSED_CLOCK, McpClock},
};
use serde_json::{Map, Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

// ---------------------------------------------------------------------------
// Helpers and Fixtures
// ---------------------------------------------------------------------------

fn task(status: &str, fields: Value) -> Map<String, Value> {
    let mut base = json!({
        "resultType": "complete",
        "taskId": "task-4195",
        "status": status,
        "createdAt": "2026-07-28T00:00:00Z",
        "lastUpdatedAt": "2026-07-28T00:00:00Z",
        "ttlMs": Value::Null,
        "pollIntervalMs": 400_000.0,
    });
    if let Value::Object(fields) = fields {
        for (k, v) in fields {
            base[k] = v;
        }
    }
    base.as_object().unwrap().clone()
}

fn handle() -> Map<String, Value> {
    task("working", json!({"resultType": "task"}))
}

fn final_payload() -> Value {
    json!({"content": [{"type": "text", "text": "task output"}]})
}

fn form_input(message: &str) -> Value {
    json!({
        "method": "elicitation/create",
        "params": {
            "message": message,
            "requestedSchema": {
                "type": "object",
                "properties": {
                    "value": {"type": "string"}
                }
            }
        }
    })
}

async fn init_clock() {
    // The clock base is process-wide and may postdate this paused runtime's start; move past it first.
    tokio::time::advance(Duration::from_secs(3600)).await;
}

async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}

async fn advance(ms: u64) {
    tokio::time::advance(Duration::from_millis(ms)).await;
    settle().await;
}

// ---------------------------------------------------------------------------
// Scripted Task Sender for follow_mcp_task unit tests
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct RecordedTaskCall {
    method: &'static str,
    params: Value,
    signal: Option<AbortSignal>,
}

struct ScriptedTaskSender {
    calls: Arc<Mutex<Vec<RecordedTaskCall>>>,
    call_tx: tokio::sync::mpsc::Sender<RecordedTaskCall>,
    responses: Arc<Mutex<Vec<Result<Map<String, Value>, McpMrtrError>>>>,
}

impl ScriptedTaskSender {
    fn new(
        responses: Vec<Result<Map<String, Value>, McpMrtrError>>,
    ) -> (Self, tokio::sync::mpsc::Receiver<RecordedTaskCall>) {
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        (
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                call_tx: tx,
                responses: Arc::new(Mutex::new(responses)),
            },
            rx,
        )
    }

    fn calls(&self) -> Vec<RecordedTaskCall> {
        self.calls.lock().unwrap().clone()
    }

    fn send_fn(
        &self,
    ) -> impl Fn(
        &'static str,
        Value,
        Option<AbortSignal>,
    ) -> BoxFuture<'static, Result<Map<String, Value>, McpMrtrError>>
    + Send
    + Sync
    + 'static {
        let calls = self.calls.clone();
        let call_tx = self.call_tx.clone();
        let responses = self.responses.clone();
        move |method: &'static str, params: Value, signal: Option<AbortSignal>| {
            let call = RecordedTaskCall {
                method,
                params,
                signal,
            };
            calls.lock().unwrap().push(call.clone());
            let _ = call_tx.try_send(call);
            let mut guard = responses.lock().unwrap();
            let next = if !guard.is_empty() {
                guard.remove(0)
            } else {
                Ok(Map::new())
            };
            drop(guard);
            Box::pin(async move { next })
        }
    }
}

// ---------------------------------------------------------------------------
// Scripted HTTP Server for mcp_request and call_mcp_tool tests
// ---------------------------------------------------------------------------

struct RecordedHttpRequest {
    method: String,
    params: Value,
    headers: axum::http::HeaderMap,
    reply_tx: tokio::sync::oneshot::Sender<Result<Value, (axum::http::StatusCode, Value)>>,
}

#[derive(Clone, Debug)]
struct RecordedHttpRequestSummary {
    method: String,
    params: Value,
    headers: axum::http::HeaderMap,
}

struct ScriptedHttpServer {
    url: url::Url,
    request_rx: tokio::sync::mpsc::Receiver<RecordedHttpRequest>,
    calls: Arc<Mutex<Vec<RecordedHttpRequestSummary>>>,
    stop_tx: Option<tokio::sync::oneshot::Sender<()>>,
    server_task: tokio::task::JoinHandle<()>,
}

impl ScriptedHttpServer {
    async fn start() -> Self {
        let (request_tx, request_rx) = tokio::sync::mpsc::channel(32);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let call_log = calls.clone();
        let app = Router::new().route(
            "/mcp",
            post(
                move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
                    let request_tx = request_tx.clone();
                    let call_log = call_log.clone();
                    async move {
                        let id = body.get("id").cloned();
                        let method = body["method"].as_str().unwrap_or_default().to_string();
                        let params = body.get("params").cloned().unwrap_or(Value::Null);
                        call_log.lock().unwrap().push(RecordedHttpRequestSummary {
                            method: method.clone(),
                            params: params.clone(),
                            headers: headers.clone(),
                        });
                        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                        let recorded = RecordedHttpRequest {
                            method,
                            params,
                            headers,
                            reply_tx,
                        };
                        let _ = request_tx.send(recorded).await;
                        match reply_rx.await {
                            Ok(Ok(result)) => {
                                if let Some(id) = id {
                                    Json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
                                        .into_response()
                                } else {
                                    axum::http::StatusCode::ACCEPTED.into_response()
                                }
                            }
                            Ok(Err((status, error))) => {
                                if let Some(id) = id {
                                    (
                                        status,
                                        Json(json!({"jsonrpc": "2.0", "id": id, "error": error})),
                                    )
                                        .into_response()
                                } else {
                                    status.into_response()
                                }
                            }
                            Err(_) => axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                        }
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let address = listener.local_addr().expect("local addr");
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stop_rx.await;
                })
                .await
                .expect("serve");
        });
        Self {
            url: format!("http://{address}/mcp").parse().expect("url"),
            request_rx,
            calls,
            stop_tx: Some(stop_tx),
            server_task,
        }
    }

    async fn next_request(&mut self) -> RecordedHttpRequest {
        tokio::time::timeout(Duration::from_secs(5), self.request_rx.recv())
            .await
            .expect("server received request in time")
            .expect("server received request")
    }

    fn calls(&self) -> Vec<RecordedHttpRequestSummary> {
        self.calls.lock().unwrap().clone()
    }

    async fn close(mut self) {
        if let Some(stop) = self.stop_tx.take() {
            let _ = stop.send(());
        }
        let _ = tokio::time::timeout(Duration::from_secs(2), self.server_task).await;
    }
}

impl RecordedHttpRequest {
    fn reply_ok(self, result: Value) {
        let _ = self.reply_tx.send(Ok(result));
    }
}

async fn fixture_client(
    url: &url::Url,
    root: &std::path::Path,
    tasks_enabled: bool,
) -> Arc<McpClient> {
    let logger = Arc::new(Mutex::new(
        McpLogger::new("fixture", root, None).expect("logger"),
    ));
    let client = McpClient::materialize(
        "fixture",
        &McpTransportSpec::Http {
            url: url.clone(),
            headers: Default::default(),
        },
        logger,
    )
    .await
    .expect("materialize client");
    client.protocol.set_version("2026-07-28");
    let mut extensions = json!({});
    if tasks_enabled {
        extensions = json!({ MCP_TASK_EXTENSION: {} });
    }
    *client.server_capabilities.write().await = json!({
        "tools": {},
        "extensions": extensions,
    });
    *client.server_info.write().await = json!({
        "name": "fixture",
        "version": "1",
    });
    client
}

struct NullUi;
impl ExtensionUi for NullUi {
    fn select<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: maho_ext_api::ExtensionUiDialogOptions,
    ) -> maho_ext_api::UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: maho_ext_api::ExtensionUiDialogOptions,
    ) -> maho_ext_api::UiFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn input<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
        _: maho_ext_api::ExtensionUiDialogOptions,
    ) -> maho_ext_api::UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn notify(&self, _: &str, _: maho_ext_api::NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(
        &self,
        _: &str,
        _: Option<maho_ext_api::WidgetContent>,
        _: maho_ext_api::ExtensionWidgetOptions,
    ) {
    }
    fn set_header(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_footer(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(
        &self,
        _: maho_ext_api::ComponentFactory,
        _: maho_ext_api::CustomUiOptions,
    ) -> maho_ext_api::ExtensionFuture<'_, maho_ext_api::JsonValue> {
        Box::pin(async { Err(maho_ext_api::ExtensionFailure::new("no custom UI")) })
    }
    fn theme(&self) -> maho_ext_api::Theme {
        maho_ext_api::Theme::default()
    }
}

// ---------------------------------------------------------------------------
// Tests from modern-tasks.test.ts
// ---------------------------------------------------------------------------

/// Upstream `test/mcp/modern-tasks.test.ts`:
/// "keeps a task pending beyond the RPC operation deadline and unwraps only its terminal result"
#[tokio::test(start_paused = true)]
async fn keeps_a_task_pending_beyond_the_rpc_operation_deadline_and_unwraps_only_its_terminal_result()
 {
    init_clock().await;
    let mut server = ScriptedHttpServer::start().await;
    let root = tempfile::tempdir().expect("tempdir");
    let client = fixture_client(&server.url, root.path(), true).await;
    let updates = Arc::new(Mutex::new(Vec::new()));
    let updates_clone = updates.clone();
    let (working_tx, working_rx) = tokio::sync::oneshot::channel::<()>();
    let working_tx = Arc::new(Mutex::new(Some(working_tx)));

    let options = McpRequestOptions {
        max_total_timeout: Some(Duration::from_millis(2000)),
        on_progress: Some(Arc::new(move |progress: Value| {
            if let Some(msg) = progress.get("message").and_then(Value::as_str) {
                updates_clone.lock().unwrap().push(msg.to_string());
            }
            if let Some(tx) = working_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
        })),
        ..Default::default()
    };

    let client_clone = client.clone();
    let pending = tokio::spawn(async move {
        mcp_request_with_clock(
            &client_clone,
            "tools/call",
            json!({"name": "test"}),
            Duration::from_millis(1000),
            &options,
            Arc::new(MCP_ELAPSED_CLOCK),
        )
        .await
    });

    // 1. Initial tools/call
    let req1 = server.next_request().await;
    assert_eq!(req1.method, "tools/call");
    assert!(
        req1.params["_meta"]["io.modelcontextprotocol/clientCapabilities"]["extensions"]
            .get(MCP_TASK_EXTENSION)
            .is_some()
    );
    req1.reply_ok(json!(handle()));

    // Wait for working progress callback
    working_rx.await.expect("working progress");
    assert!(updates.lock().unwrap()[0].contains("task-4195"));

    // 2. Advance 400,000ms (way past the 2000ms RPC operation deadline!)
    advance(400_000).await;

    // 3. tasks/get
    let req2 = server.next_request().await;
    assert_eq!(req2.method, "tasks/get");
    assert_eq!(req2.params["taskId"], "task-4195");
    req2.reply_ok(json!(task("completed", json!({"result": final_payload()}))));

    let result = pending.await.expect("join").expect("success");
    assert_eq!(result, final_payload());
    assert_eq!(server.calls().len(), 2);
    server.close().await;
}

/// Upstream `test/mcp/modern-tasks.test.ts`:
/// "sends tasks/cancel on abort and clears timers"
#[tokio::test(start_paused = true)]
async fn sends_tasks_cancel_on_abort_and_clears_timers() {
    init_clock().await;
    let (sender, mut rx) = ScriptedTaskSender::new(vec![Ok(Map::new())]);
    let caller = AbortSignal::default();
    let (working_tx, working_rx) = tokio::sync::oneshot::channel::<()>();
    let working_tx = Arc::new(Mutex::new(Some(working_tx)));

    let initial = handle();
    let caller_clone = caller.clone();
    let send_fn = sender.send_fn();
    let input_fn = |_batch, _sig| Box::pin(async { Ok(Map::new()) }) as BoxFuture<'static, _>;
    let progress_fn = move |_id: &str, _status: &str, _msg: Option<&str>| {
        if let Some(tx) = working_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
        Ok(())
    };

    let pending = tokio::spawn(async move {
        follow_mcp_task(
            &initial,
            McpTaskOptions {
                clock: &MCP_ELAPSED_CLOCK,
                signal: caller_clone,
                timeout_ms: 30.0 * 60_000.0,
                send: &send_fn,
                inputs: &input_fn,
                progress: &progress_fn,
            },
        )
        .await
    });

    working_rx.await.expect("working progress");
    caller.abort();

    let call = rx.recv().await.expect("tasks/cancel received");
    assert_eq!(call.method, "tasks/cancel");
    assert_eq!(call.params["taskId"], "task-4195");

    let err = pending.await.expect("join").expect_err("should cancel");
    assert_eq!(err.code, McpMrtrErrorCode::Cancelled);
    assert_eq!(sender.calls().len(), 1);
}

/// Upstream `test/mcp/modern-tasks.test.ts`:
/// "sends tasks/cancel on cap and clears timers"
#[tokio::test(start_paused = true)]
async fn sends_tasks_cancel_on_cap_and_clears_timers() {
    init_clock().await;
    let (sender, mut rx) = ScriptedTaskSender::new(vec![Ok(Map::new())]);
    let caller = AbortSignal::default();
    let (working_tx, working_rx) = tokio::sync::oneshot::channel::<()>();
    let working_tx = Arc::new(Mutex::new(Some(working_tx)));

    let initial = handle();
    let send_fn = sender.send_fn();
    let input_fn = |_batch, _sig| Box::pin(async { Ok(Map::new()) }) as BoxFuture<'static, _>;
    let progress_fn = move |_id: &str, _status: &str, _msg: Option<&str>| {
        if let Some(tx) = working_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
        Ok(())
    };

    let pending = tokio::spawn(async move {
        follow_mcp_task(
            &initial,
            McpTaskOptions {
                clock: &MCP_ELAPSED_CLOCK,
                signal: caller,
                timeout_ms: 30.0 * 60_000.0,
                send: &send_fn,
                inputs: &input_fn,
                progress: &progress_fn,
            },
        )
        .await
    });

    working_rx.await.expect("working progress");
    advance(30 * 60_000).await;

    let call = rx.recv().await.expect("tasks/cancel received");
    assert_eq!(call.method, "tasks/cancel");
    assert_eq!(call.params["taskId"], "task-4195");

    let err = pending.await.expect("join").expect_err("should timeout");
    assert_eq!(err.code, McpMrtrErrorCode::TaskTimeout);
    assert_eq!(sender.calls().len(), 1);
}

/// Upstream `test/mcp/modern-tasks.test.ts`:
/// "uses the configured cap and names both the task and setting in the error"
#[tokio::test(start_paused = true)]
async fn uses_the_configured_cap_and_names_both_the_task_and_setting_in_the_error() {
    init_clock().await;
    // DIVERGENCE: upstream stubs SENPI_MCP_TASK_TIMEOUT_MS; one test binary runs in parallel and the workspace
    // forbids `unsafe` env writes, so the configured cap is passed as the value that setting resolves to.
    let configured_cap = 500.0;

    let (sender, _rx) = ScriptedTaskSender::new(vec![Ok(Map::new())]);
    let caller = AbortSignal::default();
    let (working_tx, working_rx) = tokio::sync::oneshot::channel::<()>();
    let working_tx = Arc::new(Mutex::new(Some(working_tx)));

    let initial = handle();
    let send_fn = sender.send_fn();
    let input_fn = |_batch, _sig| Box::pin(async { Ok(Map::new()) }) as BoxFuture<'static, _>;
    let progress_fn = move |_id: &str, _status: &str, _msg: Option<&str>| {
        if let Some(tx) = working_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
        Ok(())
    };

    let pending = tokio::spawn(async move {
        follow_mcp_task(
            &initial,
            McpTaskOptions {
                clock: &MCP_ELAPSED_CLOCK,
                signal: caller,
                timeout_ms: configured_cap,
                send: &send_fn,
                inputs: &input_fn,
                progress: &progress_fn,
            },
        )
        .await
    });

    working_rx.await.expect("working progress");
    advance(500).await;

    let err = pending.await.expect("join").expect_err("should timeout");
    assert_eq!(err.code, McpMrtrErrorCode::TaskTimeout);
    assert!(
        err.message.contains("task-4195") && err.message.contains(MCP_TASK_TIMEOUT_ENV),
        "Error message must name both the task and setting: {}",
        err.message
    );
}

/// Upstream `test/mcp/modern-tasks.test.ts`:
/// "terminates input-required tasks without a presenter, never fabricating a decline"
#[tokio::test(start_paused = true)]
async fn terminates_input_required_tasks_without_a_presenter_never_fabricating_a_decline() {
    init_clock().await;
    let initial = task("input_required", json!({"resultType": "task"}));
    let task_get_reply = task(
        "input_required",
        json!({"inputRequests": {"question": form_input("synthetic question")}}),
    );
    let (sender, mut rx) = ScriptedTaskSender::new(vec![Ok(task_get_reply), Ok(Map::new())]);
    let caller = AbortSignal::default();

    let send_fn = sender.send_fn();
    // No presenter available: inputs callback returns ProcessingUnavailable
    let input_fn = |_batch, _sig| {
        Box::pin(async { Err(McpMrtrError::new(McpMrtrErrorCode::ProcessingUnavailable)) })
            as BoxFuture<'static, _>
    };
    let progress_fn = |_id: &str, _status: &str, _msg: Option<&str>| Ok(());

    let pending = tokio::spawn(async move {
        follow_mcp_task(
            &initial,
            McpTaskOptions {
                clock: &MCP_ELAPSED_CLOCK,
                signal: caller,
                timeout_ms: 30.0 * 60_000.0,
                send: &send_fn,
                inputs: &input_fn,
                progress: &progress_fn,
            },
        )
        .await
    });

    advance(400_000).await;

    let call1 = rx.recv().await.expect("tasks/get");
    assert_eq!(call1.method, "tasks/get");

    let call2 = rx.recv().await.expect("tasks/cancel");
    assert_eq!(call2.method, "tasks/cancel");

    let err = pending.await.expect("join").expect_err("should reject");
    assert_eq!(err.code, McpMrtrErrorCode::ProcessingUnavailable);

    let recorded = sender.calls();
    let methods: Vec<&str> = recorded.iter().map(|c| c.method).collect();
    assert_eq!(methods, vec!["tasks/get", "tasks/cancel"]);
    for c in &recorded {
        let serialized = serde_json::to_string(&c.params).unwrap();
        assert!(
            !serialized.contains("decline"),
            "Calls must never fabricate a decline: {serialized}"
        );
    }
}

async fn answers_through_presenter_once_per_key(_kind: &str) {
    init_clock().await;
    let input = task(
        "input_required",
        json!({
            "inputRequests": {"question": form_input("synthetic question")},
            "pollIntervalMs": 10.0,
        }),
    );
    let responses = vec![
        Ok(input.clone()),
        Ok(Map::new()),    // tasks/update response
        Ok(input.clone()), // duplicate key on next poll
        Ok(task("completed", json!({"result": final_payload()}))),
    ];
    let (sender, mut rx) = ScriptedTaskSender::new(responses);
    let caller = AbortSignal::default();

    let present_count = Arc::new(AtomicUsize::new(0));
    let present_count_clone = present_count.clone();

    let send_fn = sender.send_fn();
    let input_fn = move |batch: Map<String, Value>, _sig: AbortSignal| {
        let count = present_count_clone.clone();
        Box::pin(async move {
            count.fetch_add(1, Ordering::SeqCst);
            let mut res = Map::new();
            if let Some(requests) = batch.get("inputRequests").and_then(Value::as_object) {
                for key in requests.keys() {
                    res.insert(
                        key.clone(),
                        serde_json::to_value(MrtrInputResponse::Accept {
                            content: json!({"approved": true}),
                        })
                        .unwrap(),
                    );
                }
            }
            Ok(res)
        }) as BoxFuture<'static, _>
    };
    let progress_fn = |_id: &str, _status: &str, _msg: Option<&str>| Ok(());

    let initial = handle();
    let pending = tokio::spawn(async move {
        follow_mcp_task(
            &initial,
            McpTaskOptions {
                clock: &MCP_ELAPSED_CLOCK,
                signal: caller,
                timeout_ms: 30.0 * 60_000.0,
                send: &send_fn,
                inputs: &input_fn,
                progress: &progress_fn,
            },
        )
        .await
    });

    // Initial handle has pollIntervalMs: 400_000
    advance(400_000).await;
    let call1 = rx.recv().await.expect("first tasks/get");
    assert_eq!(call1.method, "tasks/get");

    // tasks/update is sent
    let call2 = rx.recv().await.expect("tasks/update");
    assert_eq!(call2.method, "tasks/update");
    assert_eq!(
        call2.params["inputResponses"]["question"]["action"],
        "accept"
    );

    // Next poll interval is 10ms
    advance(10).await;
    let call3 = rx.recv().await.expect("second tasks/get (duplicate)");
    assert_eq!(call3.method, "tasks/get");

    // The repeated key is already answered, so NO tasks/update is sent!
    // Next poll interval is 10ms
    advance(10).await;
    let call4 = rx.recv().await.expect("third tasks/get (completed)");
    assert_eq!(call4.method, "tasks/get");

    let result = pending.await.expect("join").expect("should succeed");
    assert_eq!(result, final_payload().as_object().unwrap().clone());

    let updates: Vec<_> = sender
        .calls()
        .into_iter()
        .filter(|c| c.method == "tasks/update")
        .collect();
    assert_eq!(updates.len(), 1, "Must only send tasks/update once per key");
    assert_eq!(present_count.load(Ordering::SeqCst), 1);
}

/// Upstream `test/mcp/modern-tasks.test.ts`:
/// "answers through the terminal presenter once per task input key"
#[tokio::test(start_paused = true)]
async fn answers_through_the_terminal_presenter_once_per_task_input_key() {
    answers_through_presenter_once_per_key("terminal").await;
}

/// Upstream `test/mcp/modern-tasks.test.ts`:
/// "answers through the private presenter once per task input key"
#[tokio::test(start_paused = true)]
async fn answers_through_the_private_presenter_once_per_task_input_key() {
    answers_through_presenter_once_per_key("private").await;
}

/// Upstream `test/mcp/modern-tasks.test.ts`:
/// "surfaces terminal failed without an empty success"
#[tokio::test(start_paused = true)]
async fn surfaces_terminal_failed_without_an_empty_success() {
    init_clock().await;
    let (sender, _rx) = ScriptedTaskSender::new(vec![]);
    let initial = task(
        "failed",
        json!({"resultType": "task", "error": {"code": -32603, "message": "failure"}}),
    );
    let send_fn = sender.send_fn();
    let input_fn = |_batch, _sig| Box::pin(async { Ok(Map::new()) }) as BoxFuture<'static, _>;
    let progress_fn = |_id: &str, _status: &str, _msg: Option<&str>| Ok(());

    let result = follow_mcp_task(
        &initial,
        McpTaskOptions {
            clock: &MCP_ELAPSED_CLOCK,
            signal: AbortSignal::default(),
            timeout_ms: 30.0 * 60_000.0,
            send: &send_fn,
            inputs: &input_fn,
            progress: &progress_fn,
        },
    )
    .await;

    let err = result.expect_err("failed task must error");
    assert_eq!(err.code, McpMrtrErrorCode::TaskFailed);
    assert_eq!(err.remote_code, Some(-32603));
    assert_eq!(sender.calls().len(), 0);
}

/// Upstream `test/mcp/modern-tasks.test.ts`:
/// "surfaces terminal cancelled without an empty success"
#[tokio::test(start_paused = true)]
async fn surfaces_terminal_cancelled_without_an_empty_success() {
    init_clock().await;
    let (sender, _rx) = ScriptedTaskSender::new(vec![]);
    let initial = task("cancelled", json!({"resultType": "task"}));
    let send_fn = sender.send_fn();
    let input_fn = |_batch, _sig| Box::pin(async { Ok(Map::new()) }) as BoxFuture<'static, _>;
    let progress_fn = |_id: &str, _status: &str, _msg: Option<&str>| Ok(());

    let result = follow_mcp_task(
        &initial,
        McpTaskOptions {
            clock: &MCP_ELAPSED_CLOCK,
            signal: AbortSignal::default(),
            timeout_ms: 30.0 * 60_000.0,
            send: &send_fn,
            inputs: &input_fn,
            progress: &progress_fn,
        },
    )
    .await;

    let err = result.expect_err("cancelled task must error");
    assert_eq!(err.code, McpMrtrErrorCode::TaskCancelled);
    assert_eq!(sender.calls().len(), 0);
}

/// Upstream `test/mcp/modern-tasks.test.ts`:
/// "rejects unsolicited handles without negotiating the extension"
#[tokio::test(start_paused = true)]
async fn rejects_unsolicited_handles_without_negotiating_the_extension() {
    init_clock().await;
    let mut server = ScriptedHttpServer::start().await;
    let root = tempfile::tempdir().expect("tempdir");
    // tasks_enabled = false
    let client = fixture_client(&server.url, root.path(), false).await;

    let client_clone = client.clone();
    let pending = tokio::spawn(async move {
        mcp_request_with_clock(
            &client_clone,
            "tools/call",
            json!({"name": "test"}),
            Duration::from_millis(1000),
            &Default::default(),
            Arc::new(MCP_ELAPSED_CLOCK),
        )
        .await
    });

    let req = server.next_request().await;
    assert_eq!(req.method, "tools/call");
    // Server replies with a task handle, but client never negotiated tasks extension
    req.reply_ok(json!(handle()));

    let err = pending.await.expect("join").expect_err("should reject");
    let mrtr = as_mcp_mrtr_error(&err).expect("MRTR error");
    assert_eq!(mrtr.code, McpMrtrErrorCode::InvalidResult);
    assert_eq!(server.calls().len(), 1);
    server.close().await;
}

/// Upstream `test/mcp/modern-tasks.test.ts`:
/// "cancels a handle even when abort races its response delivery"
#[tokio::test(start_paused = true)]
async fn cancels_a_handle_even_when_abort_races_its_response_delivery() {
    init_clock().await;
    let (sender, mut rx) = ScriptedTaskSender::new(vec![Ok(Map::new())]);
    let caller = AbortSignal::default();

    // Abort races response delivery (caller aborts immediately upon receiving handle)
    caller.abort();

    let initial = handle();
    let send_fn = sender.send_fn();
    let input_fn = |_batch, _sig| Box::pin(async { Ok(Map::new()) }) as BoxFuture<'static, _>;
    let progress_fn = |_id: &str, _status: &str, _msg: Option<&str>| Ok(());

    let pending = tokio::spawn(async move {
        follow_mcp_task(
            &initial,
            McpTaskOptions {
                clock: &MCP_ELAPSED_CLOCK,
                signal: caller,
                timeout_ms: 30.0 * 60_000.0,
                send: &send_fn,
                inputs: &input_fn,
                progress: &progress_fn,
            },
        )
        .await
    });

    let call = rx.recv().await.expect("tasks/cancel");
    assert_eq!(call.method, "tasks/cancel");
    assert_eq!(call.params["taskId"], "task-4195");

    let err = pending.await.expect("join").expect_err("should reject");
    assert_eq!(err.code, McpMrtrErrorCode::Cancelled);
    let calls = sender.calls();
    assert_eq!(calls.last().unwrap().method, "tasks/cancel");
}

/// Upstream `test/mcp/modern-tasks.test.ts`:
/// "losing the presenter removes elicitation from subsequent task polls"
#[tokio::test(start_paused = true)]
async fn losing_the_presenter_removes_elicitation_from_subsequent_task_polls() {
    init_clock().await;
    let mut server = ScriptedHttpServer::start().await;
    let root = tempfile::tempdir().expect("tempdir");
    let client = fixture_client(&server.url, root.path(), true).await;

    let presentation_controller = maho_ai::utils::abort::AbortController::new();
    let pres_signal = presentation_controller.signal();
    let origin_signal = AbortSignal::default();
    let dummy_conn = Arc::new(ServerConnection::new(
        "fixture",
        McpServerConfig {
            enabled: Some(true),
            transport: Some(Transport::Http),
            url: Some(server.url.to_string()),
            ..Default::default()
        },
        None,
        Arc::new(Mutex::new(
            McpLogger::new("fixture", root.path(), None).unwrap(),
        )),
    ));
    let presentation = McpMrtrPresentation {
        ui: Arc::new(NullUi),
        signal: pres_signal,
        admit: None,
        presented: None,
        private_forms: None,
    };
    let origin = create_mcp_request_origin(McpRequestOrigin::new(
        1,
        dummy_conn,
        origin_signal,
        Some(presentation),
        None,
        false,
        "fixture".into(),
        None,
        Arc::new(|| true),
        Arc::new(|_| Box::pin(async { Ok(json!({})) })),
    ));

    let options = McpRequestOptions {
        origin: Some(origin),
        ..Default::default()
    };

    let client_clone = client.clone();
    let pending = tokio::spawn(async move {
        mcp_request_with_clock(
            &client_clone,
            "tools/call",
            json!({"name": "test"}),
            Duration::from_millis(1000),
            &options,
            Arc::new(MCP_ELAPSED_CLOCK),
        )
        .await
    });

    // 1. Initial tools/call
    let req1 = server.next_request().await;
    assert_eq!(req1.method, "tools/call");
    assert!(
        req1.params["_meta"]["io.modelcontextprotocol/clientCapabilities"]
            .get("elicitation")
            .is_some()
    );
    req1.reply_ok(json!(handle()));

    // Lose the presentation while waiting
    settle().await;
    presentation_controller.abort(None);

    advance(400_000).await;

    // 2. tasks/get poll
    let req2 = server.next_request().await;
    assert_eq!(req2.method, "tasks/get");
    let caps = &req2.params["_meta"]["io.modelcontextprotocol/clientCapabilities"];
    assert_eq!(
        caps,
        &json!({"extensions": {MCP_TASK_EXTENSION: {}}}),
        "Losing presenter must remove elicitation from subsequent task polls"
    );
    req2.reply_ok(json!(task("completed", json!({"result": final_payload()}))));

    let result = pending.await.expect("join").expect("success");
    assert_eq!(result, final_payload());
    server.close().await;
}

/// Upstream `test/mcp/modern-tasks.test.ts`:
/// "uses bounded backoff without a hint and adopts a changed server interval"
#[tokio::test(start_paused = true)]
async fn uses_bounded_backoff_without_a_hint_and_adopts_a_changed_server_interval() {
    init_clock().await;
    let responses = vec![
        Ok(task("working", json!({"pollIntervalMs": Value::Null}))),
        Ok(task("working", json!({"pollIntervalMs": 7.0}))),
        Ok(task(
            "completed",
            json!({"result": {"content": [{"type": "text", "text": "done"}], "isError": true}}),
        )),
    ];
    let (sender, mut rx) = ScriptedTaskSender::new(responses);
    let initial = task(
        "working",
        json!({"resultType": "task", "pollIntervalMs": Value::Null}),
    );
    let send_fn = sender.send_fn();
    let input_fn = |_batch, _sig| Box::pin(async { Ok(Map::new()) }) as BoxFuture<'static, _>;
    let progress_fn = |_id: &str, _status: &str, _msg: Option<&str>| Ok(());

    let pending = tokio::spawn(async move {
        follow_mcp_task(
            &initial,
            McpTaskOptions {
                clock: &MCP_ELAPSED_CLOCK,
                signal: AbortSignal::default(),
                timeout_ms: 30.0 * 60_000.0,
                send: &send_fn,
                inputs: &input_fn,
                progress: &progress_fn,
            },
        )
        .await
    });

    // 1. Initial backoff is 1000ms
    advance(1000).await;
    let call1 = rx.recv().await.expect("first poll (1000ms)");
    assert_eq!(call1.method, "tasks/get");

    // 2. Second backoff is 2000ms
    advance(2000).await;
    let call2 = rx.recv().await.expect("second poll (2000ms)");
    assert_eq!(call2.method, "tasks/get");

    // 3. Adopted server interval is 7ms
    advance(7).await;
    let call3 = rx.recv().await.expect("third poll (7ms)");
    assert_eq!(call3.method, "tasks/get");

    let result = pending.await.expect("join").expect("success");
    assert_eq!(result.get("isError"), Some(&Value::Bool(true)));
    assert_eq!(sender.calls().len(), 3);
}

/// Upstream `test/mcp/modern-tasks.test.ts`:
/// "disconnect while waiting removes the poll and cap timers and close observer"
#[tokio::test(start_paused = true)]
async fn disconnect_while_waiting_removes_the_poll_and_cap_timers_and_close_observer() {
    init_clock().await;
    let mut server = ScriptedHttpServer::start().await;
    let root = tempfile::tempdir().expect("tempdir");
    let client = fixture_client(&server.url, root.path(), true).await;

    let client_clone = client.clone();
    let pending = tokio::spawn(async move {
        mcp_request_with_clock(
            &client_clone,
            "tools/call",
            json!({"name": "test"}),
            Duration::from_millis(1000),
            &Default::default(),
            Arc::new(MCP_ELAPSED_CLOCK),
        )
        .await
    });

    let req = server.next_request().await;
    assert_eq!(req.method, "tools/call");
    req.reply_ok(json!(handle()));

    settle().await;

    // Disconnect client while waiting in the 400_000ms delay.
    // Client close observers registered via on_mcp_client_close are notified via client.closed.
    client.closed.send_replace(true);
    let _ = client.close().await;

    let err = pending
        .await
        .expect("join")
        .expect_err("should reject on close");
    let mrtr = as_mcp_mrtr_error(&err).expect("MRTR error");
    assert_eq!(mrtr.code, McpMrtrErrorCode::ContextChanged);

    // No tasks/cancel is sent on disconnect (context changed)
    assert_eq!(server.calls().len(), 1);
    server.close().await;
}

/// Upstream `test/mcp/modern-tasks.test.ts`:
/// "a stalled poll has its own request timeout and cancels the task without replaying tools/call"
#[tokio::test(start_paused = true)]
async fn a_stalled_poll_has_its_own_request_timeout_and_cancels_the_task_without_replaying_tools_call()
 {
    init_clock().await;
    let mut server = ScriptedHttpServer::start().await;
    let root = tempfile::tempdir().expect("tempdir");
    let client = fixture_client(&server.url, root.path(), true).await;

    let client_clone = client.clone();
    let pending = tokio::spawn(async move {
        mcp_request_with_clock(
            &client_clone,
            "tools/call",
            json!({"name": "test"}),
            Duration::from_millis(1000), // 1000ms request timeout
            &Default::default(),
            Arc::new(MCP_ELAPSED_CLOCK),
        )
        .await
    });

    // 1. tools/call
    let req1 = server.next_request().await;
    assert_eq!(req1.method, "tools/call");
    req1.reply_ok(json!(handle()));

    // Advance through poll interval (400_000ms)
    advance(400_000).await;

    // 2. tasks/get arrives, but server stalls (never replies)
    let req2 = server.next_request().await;
    assert_eq!(req2.method, "tasks/get");

    // Advance 1000ms to trigger the poll attempt's own request timeout
    advance(1000).await;

    // 3. tasks/cancel is sent because the poll attempt timed out
    let req3 = server.next_request().await;
    assert_eq!(req3.method, "tasks/cancel");
    assert_eq!(req3.params["taskId"], "task-4195");
    req3.reply_ok(json!({}));

    // Drop stalled req2 reply channel
    drop(req2);

    let err = pending.await.expect("join").expect_err("should reject");
    let mrtr = as_mcp_mrtr_error(&err).expect("MRTR error");
    assert_eq!(mrtr.code, McpMrtrErrorCode::TransportFailed);

    let methods: Vec<String> = server.calls().into_iter().map(|c| c.method).collect();
    assert_eq!(methods, vec!["tools/call", "tasks/get", "tasks/cancel"]);
    server.close().await;
}

/// Upstream `test/mcp/modern-tasks.test.ts`:
/// "the cap interrupts pending task consent without leaking its timers"
#[tokio::test(start_paused = true)]
async fn the_cap_interrupts_pending_task_consent_without_leaking_its_timers() {
    init_clock().await;
    // DIVERGENCE: see `uses_the_configured_cap_and_names_both_the_task_and_setting_in_the_error`.
    let cap_ms = 500_000.0;

    let task_get_reply = task(
        "input_required",
        json!({"inputRequests": {"question": form_input("synthetic question")}}),
    );
    let (sender, mut rx) = ScriptedTaskSender::new(vec![Ok(task_get_reply), Ok(Map::new())]);
    let caller = AbortSignal::default();

    let (shown_tx, shown_rx) = tokio::sync::oneshot::channel::<()>();
    let shown_tx = Arc::new(Mutex::new(Some(shown_tx)));

    let send_fn = sender.send_fn();
    // Presenter shows consent prompt and hangs until signal is cancelled (aborted by cap)
    let input_fn = move |_batch: Map<String, Value>, signal: AbortSignal| {
        let shown_tx = shown_tx.clone();
        Box::pin(async move {
            if let Some(tx) = shown_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
            signal.cancelled().await;
            Err(McpMrtrError::new(McpMrtrErrorCode::Cancelled))
        }) as BoxFuture<'static, _>
    };
    let progress_fn = |_id: &str, _status: &str, _msg: Option<&str>| Ok(());

    let initial = handle();
    let pending = tokio::spawn(async move {
        follow_mcp_task(
            &initial,
            McpTaskOptions {
                clock: &MCP_ELAPSED_CLOCK,
                signal: caller,
                timeout_ms: cap_ms,
                send: &send_fn,
                inputs: &input_fn,
                progress: &progress_fn,
            },
        )
        .await
    });

    // 1. Advance through initial 400_000ms delay
    advance(400_000).await;
    let call1 = rx.recv().await.expect("tasks/get");
    assert_eq!(call1.method, "tasks/get");

    // Wait until presenter is shown
    shown_rx.await.expect("presenter shown");

    // 2. Advance remaining 100_000ms to hit the 500_000ms cap
    advance(100_000).await;

    // 3. tasks/cancel is sent on cap timeout
    let call2 = rx.recv().await.expect("tasks/cancel");
    assert_eq!(call2.method, "tasks/cancel");
    assert_eq!(call2.params["taskId"], "task-4195");

    let err = pending.await.expect("join").expect_err("should timeout");
    assert_eq!(err.code, McpMrtrErrorCode::TaskTimeout);
}

// ---------------------------------------------------------------------------
// Tests from modern-tasks-http.test.ts
// ---------------------------------------------------------------------------

async fn routes_task_and_displays_status_through_tool_row(mode: &str) {
    init_clock().await;
    let id = "synthetic-task";
    let task_handle = json!({
        "resultType": "task",
        "taskId": id,
        "status": "working",
        "statusMessage": "processing batch",
        "createdAt": "2026-07-28T00:00:00Z",
        "lastUpdatedAt": "2026-07-28T00:00:00Z",
        "ttlMs": Value::Null,
        "pollIntervalMs": 0.0,
    });

    let mut server = ScriptedHttpServer::start().await;
    let root = tempfile::tempdir().expect("tempdir");
    let client = fixture_client(&server.url, root.path(), true).await;

    let updates = Arc::new(Mutex::new(Vec::new()));
    let updates_clone = updates.clone();
    let caller = AbortSignal::default();
    let caller_clone = caller.clone();
    let mode_str = mode.to_string();

    let on_update = move |update: maho_ext_api::ToolResult| {
        for content in update.content {
            if let Some(text) = content.text() {
                updates_clone.lock().unwrap().push(text.to_owned());
            }
        }
        if mode_str == "abort" {
            caller_clone.abort();
        }
        Ok(())
    };

    let entry = McpToolCatalogEntry {
        server: "fixture".into(),
        tool: "test".into(),
        schema: json!({"type": "object"}),
        output_schema: None,
        modern: true,
        description: None,
        annotations: None,
        request_timeout: Duration::from_millis(1000),
        client: Some(client.clone()),
        runtime: None,
        connection: None,
        ensure_connected: None,
        ensure_fresh: None,
        agent_dir: None,
        artifacts: None,
        output_guard: None,
        request_origin: None,
    };

    let entry_clone = entry;
    let caller_arg = caller.clone();
    let client_for_notif = client.clone();
    let pending = tokio::spawn(async move {
        call_mcp_tool(
            &entry_clone,
            json!({}),
            "fixture",
            &caller_arg,
            Some(&on_update),
        )
        .await
    });

    // 1. Initial tools/call
    let req1 = server.next_request().await;
    assert_eq!(req1.method, "tools/call");
    let token = req1.params["_meta"]["progressToken"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let _ = client_for_notif.notifications.send(json!({
        "method": "notifications/progress",
        "params": {
            "progressToken": token,
            "progress": 0,
            "message": format!("MCP task {id}: working - processing batch"),
        }
    }));
    req1.reply_ok(task_handle);

    settle().await;

    // 2. Followup request (tasks/get or tasks/cancel)
    let followup = server.next_request().await;
    let expected_method = if mode == "complete" {
        "tasks/get"
    } else {
        "tasks/cancel"
    };
    assert_eq!(followup.method, expected_method);
    assert_eq!(
        followup
            .headers
            .get("mcp-name")
            .and_then(|v| v.to_str().ok()),
        Some(id)
    );
    assert_eq!(
        followup
            .headers
            .get("mcp-method")
            .and_then(|v| v.to_str().ok()),
        Some(expected_method)
    );
    assert!(
        followup.params["_meta"]["io.modelcontextprotocol/clientCapabilities"]["extensions"]
            .get(MCP_TASK_EXTENSION)
            .is_some()
    );

    if mode == "complete" {
        followup.reply_ok(json!({
            "resultType": "complete",
            "taskId": id,
            "status": "completed",
            "createdAt": "2026-07-28T00:00:00Z",
            "lastUpdatedAt": "2026-07-28T00:00:00Z",
            "ttlMs": Value::Null,
            "result": {"content": [{"type": "text", "text": "done"}]},
        }));
        let res = pending.await.expect("join").expect("should succeed");
        assert_eq!(res["content"][0]["text"], "done");
    } else {
        followup.reply_ok(json!({}));
        let err = pending.await.expect("join").expect_err("should reject");
        assert!(
            err.to_string().contains("MCP_MRTR_CANCELLED") || err.to_string().contains("canceled"),
            "Error must reflect cancellation: {err}"
        );
    }

    let up = updates.lock().unwrap();
    assert_eq!(up.len(), 1);
    assert!(
        up[0].contains(&format!("MCP task {id}: working - processing batch")),
        "Progress updates must include task status: {:?}",
        up
    );
    drop(up);

    server.close().await;
}

/// Upstream `test/mcp/modern-tasks-http.test.ts`:
/// "routes task complete and displays status through the tool row"
#[tokio::test(start_paused = true)]
async fn routes_task_complete_and_displays_status_through_the_tool_row() {
    routes_task_and_displays_status_through_tool_row("complete").await;
}

/// Upstream `test/mcp/modern-tasks-http.test.ts`:
/// "routes task abort and displays status through the tool row"
#[tokio::test(start_paused = true)]
async fn routes_task_abort_and_displays_status_through_the_tool_row() {
    routes_task_and_displays_status_through_tool_row("abort").await;
}
