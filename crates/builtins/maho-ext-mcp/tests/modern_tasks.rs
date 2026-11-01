//! Ports of senpi test/mcp/modern-tasks.test.ts and modern-tasks-http.test.ts.
//!
//! Upstream drives the engine with a manual `Clock` and an in-process script transport. The port keeps the
//! manual clock (`ManualClock`, upstream's fixture `Clock`) and serves the script over loopback HTTP through
//! the real modern route (`McpClient::speak_modern` + `ModernHttpTransport`, upstream's `ModernSdkBridge`).
//! Tokio time is paused and frozen (`freeze_time`), so the attempt timeouts never fire while loopback I/O is in
//! flight; only the manual clock moves the engine's deadlines, poll intervals and task cap.

use crate::support::clock::{ManualClock, freeze_time, unique_binding, within};
use axum::{Json, Router, response::IntoResponse, routing::post};
use futures::future::BoxFuture;
use maho_core::mcp_mrtr_presentations::McpMrtrPresentation;
use maho_ext_api::{AbortSignal, ExtensionUi, ToolContent, ToolError, ToolResult};
use maho_ext_mcp::{
    catalog::McpToolCatalogEntry,
    config_schema::{Auth, McpServerConfig, Transport},
    connection::ServerConnection,
    errors::McpError,
    expose::call::call_mcp_tool,
    form_types::{
        McpFormInput, McpFormOutcome, McpFormOutcomeKind, McpFormOutcomeSource, McpFormPresentation,
    },
    log::McpLogger,
    modern::{
        errors::{McpMrtrError, McpMrtrErrorCode, as_mcp_mrtr_error},
        request::mcp_request_with_clock,
        tasks::{MCP_TASK_EXTENSION, MCP_TASK_TIMEOUT_ENV},
        wire::MODERN_PROTOCOL_VERSION,
    },
    request_origin::{
        McpRequestOptions, McpRequestOrigin, create_mcp_request_origin, mcp_request_options,
    },
    transport_sdk::McpClient,
};
use serde_json::{Map, Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

// ---------------------------------------------------------------------------
// Fixtures (upstream test/mcp/fixtures/mrtr-lifecycle.ts with `tasks = true`)
// ---------------------------------------------------------------------------

fn task(status: &str, fields: Value) -> Value {
    let mut base = json!({
        "resultType": "complete",
        "taskId": "task-4195",
        "status": status,
        "createdAt": "2026-07-28T00:00:00Z",
        "lastUpdatedAt": "2026-07-28T00:00:00Z",
        "ttlMs": Value::Null,
        "pollIntervalMs": 400_000,
    });
    if let Value::Object(fields) = fields {
        for (key, value) in fields {
            if value.is_null() && key == "pollIntervalMs" {
                // Upstream `pollIntervalMs: undefined` drops the hint entirely.
                base.as_object_mut().unwrap().remove(&key);
            } else {
                base[key] = value;
            }
        }
    }
    base
}

fn handle() -> Value {
    task("working", json!({"resultType": "task"}))
}

fn final_payload() -> Value {
    json!({"content": [{"type": "text", "text": "task output"}]})
}

fn form_input() -> Value {
    json!({
        "method": "elicitation/create",
        "params": {
            "message": "synthetic question",
            "requestedSchema": {"type": "object", "properties": {"value": {"type": "string"}}},
        },
    })
}

/// One scripted reply: a JSON-RPC result, or no reply at all (upstream `undefined`: the request stalls).
type Reply = Option<Value>;

#[derive(Clone, Debug)]
struct Call {
    method: String,
    params: Value,
    headers: axum::http::HeaderMap,
}

/// Upstream `ScriptTransport` over loopback HTTP: `server/discover` advertises a modern server (with the tasks
/// extension when `tasks`), every application request (`tools/call`, `tasks/*`) is recorded and answered with the
/// next scripted result, and `None` leaves it unanswered. Other traffic is answered and not recorded.
struct Script {
    url: url::Url,
    calls: Arc<Mutex<Vec<Call>>>,
    received: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Call>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Script {
    async fn start(results: Vec<Reply>, tasks: bool) -> Self {
        let mut capabilities = json!({"tools": {}});
        if tasks {
            capabilities["extensions"] = json!({ MCP_TASK_EXTENSION: {} });
        }
        let discovery = json!({
            "resultType": "complete",
            "supportedVersions": [MODERN_PROTOCOL_VERSION],
            "capabilities": capabilities,
            "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "fixture", "version": "1"}},
        });
        let calls = Arc::new(Mutex::new(Vec::new()));
        let results = Arc::new(Mutex::new(VecDeque::from(results)));
        let (received_tx, received) = tokio::sync::mpsc::unbounded_channel();
        let recorded = calls.clone();
        let app = Router::new().route(
            "/mcp",
            post(
                move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
                    let recorded = recorded.clone();
                    let results = results.clone();
                    let received = received_tx.clone();
                    let discovery = discovery.clone();
                    async move {
                        let method = body["method"].as_str().unwrap_or_default().to_owned();
                        let id = body.get("id").cloned();
                        if method != "tools/call" && !method.starts_with("tasks/") {
                            let Some(id) = id else {
                                return axum::http::StatusCode::ACCEPTED.into_response();
                            };
                            let result = if method == "server/discover" {
                                discovery
                            } else {
                                json!({})
                            };
                            return Json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
                                .into_response();
                        }
                        let call = Call {
                            method: method.clone(),
                            params: body.get("params").cloned().unwrap_or(Value::Null),
                            headers,
                        };
                        recorded.lock().unwrap().push(call.clone());
                        let reply = results.lock().unwrap().pop_front().flatten();
                        let _ = received.send(call);
                        let Some(id) = id else {
                            return axum::http::StatusCode::ACCEPTED.into_response();
                        };
                        match reply {
                            Some(result) => {
                                Json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
                                    .into_response()
                            }
                            None => std::future::pending().await,
                        }
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let address = listener.local_addr().expect("local addr");
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await;
        });
        Self {
            url: format!("http://{address}/mcp").parse().expect("url"),
            calls,
            received: tokio::sync::Mutex::new(received),
            stop: Some(stop),
        }
    }

    /// Upstream `await f.transport.received.promise`.
    async fn next_call(&self) -> Call {
        let mut received = self.received.lock().await;
        within("the script to receive a request", received.recv())
            .await
            .expect("script open")
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn methods(&self) -> Vec<String> {
        self.calls().into_iter().map(|call| call.method).collect()
    }

    fn close(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

/// A connected modern client (real era probe, `ModernHttpTransport` route) and the connection that owns it, so a
/// request origin can name the physical client (upstream fakes `connection.client` instead).
async fn connect(
    script: &Script,
    root: &std::path::Path,
) -> (Arc<ServerConnection>, Arc<McpClient>) {
    let connection = ServerConnection::new(
        "fixture",
        McpServerConfig {
            enabled: Some(true),
            transport: Some(Transport::Http),
            url: Some(script.url.to_string()),
            auth: Some(Auth::Disabled(false)),
            ..Default::default()
        },
        None,
        Arc::new(Mutex::new(
            McpLogger::new("fixture", root, None).expect("logger"),
        )),
    );
    let client = within("the modern connection", connection.connect())
        .await
        .expect("connected client");
    assert!(
        maho_ext_mcp::modern::request::is_modern_mcp_client(&client),
        "the script must negotiate the modern era"
    );
    (connection, client)
}

/// Upstream `f.origin(presentation)` with an optional private presenter (`privateFormsEnforced`).
fn origin(
    connection: Arc<ServerConnection>,
    presentation: Option<McpMrtrPresentation>,
    private_form: Option<McpFormPresentation>,
) -> Arc<McpRequestOrigin> {
    let enforced = private_form.is_some();
    create_mcp_request_origin(McpRequestOrigin::new(
        unique_binding(),
        connection,
        AbortSignal::default(),
        presentation,
        private_form,
        enforced,
        "test".into(),
        None,
        Arc::new(|| true),
        Arc::new(|_| Box::pin(async { Ok::<_, McpError>(Value::Null) })),
    ))
}

/// Upstream `mrtrTui()`: answers Submit (else the first choice) and "answer" for text input.
struct TaskUi;
impl ExtensionUi for TaskUi {
    fn select<'a>(
        &'a self,
        _: &'a str,
        choices: &'a [String],
        _: maho_ext_api::ExtensionUiDialogOptions,
    ) -> maho_ext_api::UiFuture<'a, Option<String>> {
        let answer = if choices.iter().any(|choice| choice == "Submit") {
            Some("Submit".to_owned())
        } else {
            choices.first().cloned()
        };
        Box::pin(async move { answer })
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
        Box::pin(async { Some("answer".to_owned()) })
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

fn presentation(ui: TaskUi) -> (McpMrtrPresentation, maho_ai::utils::abort::AbortController) {
    let lifetime = maho_ai::utils::abort::AbortController::new();
    (
        McpMrtrPresentation {
            ui: Arc::new(ui),
            signal: lifetime.signal(),
            admit: None,
            presented: None,
            private_forms: None,
        },
        lifetime,
    )
}

/// Runs `tools/call` through the MRTR engine on `clock`, like upstream `f.client.callTool`.
fn call_tool(
    client: &Arc<McpClient>,
    clock: &ManualClock,
    timeout: Duration,
    options: McpRequestOptions,
) -> tokio::task::JoinHandle<Result<Value, McpError>> {
    let (client, clock) = (client.clone(), clock.clone());
    tokio::spawn(async move {
        mcp_request_with_clock(
            &client,
            "tools/call",
            json!({"name": "test"}),
            timeout,
            &options,
            Arc::new(clock),
        )
        .await
    })
}

/// Every script negotiates the tasks extension unless a test says otherwise (upstream `fixture(results, true)`).
const TASKS: bool = true;

/// The SDK default request timeout the upstream fixture leaves in place when a call passes none.
const SDK_TIMEOUT: Duration = Duration::from_secs(60);

fn mrtr_code(result: Result<Value, McpError>) -> McpMrtrErrorCode {
    let error = result.expect_err("the call must fail");
    as_mcp_mrtr_error(&error)
        .unwrap_or_else(|| panic!("not an MRTR error: {error:?}"))
        .code
}

/// A progress callback that signals its first update (upstream `working.resolve()`).
fn on_working(
    updates: Arc<Mutex<Vec<Value>>>,
) -> (
    Arc<dyn Fn(Value) + Send + Sync>,
    tokio::sync::oneshot::Receiver<()>,
) {
    let (working, worked) = tokio::sync::oneshot::channel();
    let working = Mutex::new(Some(working));
    (
        Arc::new(move |progress: Value| {
            updates.lock().unwrap().push(progress);
            if let Some(working) = working.lock().unwrap().take() {
                let _ = working.send(());
            }
        }),
        worked,
    )
}

// ---------------------------------------------------------------------------
// modern-tasks.test.ts
// ---------------------------------------------------------------------------

/// Upstream: "keeps a task pending beyond the RPC operation deadline and unwraps only its terminal result"
#[tokio::test(start_paused = true)]
async fn keeps_a_task_pending_beyond_the_rpc_operation_deadline_and_unwraps_only_its_terminal_result()
 {
    let _frozen = freeze_time();
    let clock = ManualClock::new();
    let script = Script::start(
        vec![
            Some(handle()),
            Some(task("completed", json!({"result": final_payload()}))),
        ],
        TASKS,
    )
    .await;
    let root = tempfile::tempdir().expect("tempdir");
    let (_connection, client) = connect(&script, root.path()).await;
    let updates = Arc::new(Mutex::new(Vec::new()));
    let (on_progress, worked) = on_working(updates.clone());
    let pending = call_tool(
        &client,
        &clock,
        Duration::from_millis(1000),
        McpRequestOptions {
            max_total_timeout: Some(Duration::from_millis(2000)),
            on_progress: Some(on_progress),
            ..Default::default()
        },
    );
    within("the working progress", worked)
        .await
        .expect("working progress");
    clock.advance(400_000.0);
    let result = within("the task result", pending)
        .await
        .expect("join")
        .expect("task completes");
    assert_eq!(result, final_payload());
    let calls = script.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].method, "tasks/get");
    assert_eq!(calls[1].params["taskId"], "task-4195");
    assert_eq!(
        calls[0].params["_meta"]["io.modelcontextprotocol/clientCapabilities"]["extensions"]
            [MCP_TASK_EXTENSION],
        json!({})
    );
    let first = updates.lock().unwrap()[0].clone();
    assert!(
        first["message"]
            .as_str()
            .is_some_and(|message| message.contains("task-4195")),
        "{first}"
    );
    within("every timer to clear", clock.settled()).await;
    script.close();
}

/// Upstream: "sends tasks/cancel on %s and clears timers"
async fn sends_tasks_cancel_and_clears_timers(reason: &str) {
    let _frozen = freeze_time();
    let clock = ManualClock::new();
    let script = Script::start(vec![Some(handle()), Some(json!({}))], TASKS).await;
    let root = tempfile::tempdir().expect("tempdir");
    let (_connection, client) = connect(&script, root.path()).await;
    let caller = AbortSignal::default();
    let (on_progress, worked) = on_working(Arc::default());
    let pending = call_tool(
        &client,
        &clock,
        SDK_TIMEOUT,
        McpRequestOptions {
            signal: Some(caller.clone()),
            on_progress: Some(on_progress),
            ..Default::default()
        },
    );
    within("the working progress", worked)
        .await
        .expect("working progress");
    if reason == "abort" {
        caller.abort();
    } else {
        clock.advance(30.0 * 60_000.0);
    }
    let code = mrtr_code(within("the cancelled call", pending).await.expect("join"));
    assert_eq!(
        code,
        if reason == "abort" {
            McpMrtrErrorCode::Cancelled
        } else {
            McpMrtrErrorCode::TaskTimeout
        }
    );
    let calls = script.calls();
    assert_eq!(calls[1].method, "tasks/cancel");
    assert_eq!(calls[1].params["taskId"], "task-4195");
    // Upstream `transport.cancellations`: the modern HTTP route never POSTs notifications/cancelled.
    assert!(
        !script
            .methods()
            .iter()
            .any(|method| method == "notifications/cancelled")
    );
    within("every timer to clear", clock.settled()).await;
    script.close();
}

#[tokio::test(start_paused = true)]
async fn sends_tasks_cancel_on_abort_and_clears_timers() {
    sends_tasks_cancel_and_clears_timers("abort").await;
}

#[tokio::test(start_paused = true)]
async fn sends_tasks_cancel_on_cap_and_clears_timers() {
    sends_tasks_cancel_and_clears_timers("cap").await;
}

/// Upstream: "uses the configured cap and names both the task and setting in the error"
///
/// DIVERGENCE: upstream stubs `SENPI_MCP_TASK_TIMEOUT_MS=500`. One test binary runs every module in parallel and
/// the workspace forbids the `unsafe` environment write, so the cap is exercised through `follow_mcp_task`'s
/// `timeout_ms` (the value `mcp_task_timeout_ms` resolves the setting to); the error names both, as upstream.
#[tokio::test(start_paused = true)]
async fn uses_the_configured_cap_and_names_both_the_task_and_setting_in_the_error() {
    use maho_ext_mcp::modern::tasks::{McpTaskOptions, follow_mcp_task};
    let clock = ManualClock::new();
    let sent = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let record = sent.clone();
    let send = move |method: &'static str, _params: Value, _signal: Option<AbortSignal>| {
        record.lock().unwrap().push(method);
        Box::pin(async { Ok(Map::new()) })
            as BoxFuture<'static, Result<Map<String, Value>, McpMrtrError>>
    };
    let inputs = |_batch: Map<String, Value>, _signal: AbortSignal| {
        Box::pin(async { Ok(Map::new()) })
            as BoxFuture<'static, Result<Map<String, Value>, McpMrtrError>>
    };
    let (working, worked) = tokio::sync::oneshot::channel::<()>();
    let working = Mutex::new(Some(working));
    let progress = move |_: &str, _: &str, _: Option<&str>| -> Result<(), McpMrtrError> {
        if let Some(working) = working.lock().unwrap().take() {
            let _ = working.send(());
        }
        Ok(())
    };
    let initial = handle().as_object().cloned().expect("handle object");
    let follower_clock = clock.clone();
    let pending = tokio::spawn(async move {
        follow_mcp_task(
            &initial,
            McpTaskOptions {
                clock: &follower_clock,
                signal: AbortSignal::default(),
                timeout_ms: 500.0,
                send: &send,
                inputs: &inputs,
                progress: &progress,
            },
        )
        .await
    });
    within("the working progress", worked)
        .await
        .expect("working progress");
    clock.advance(500.0);
    let error = within("the capped task", pending)
        .await
        .expect("join")
        .expect_err("the cap must stop the task");
    assert_eq!(error.code, McpMrtrErrorCode::TaskTimeout);
    let pattern = regex::Regex::new(&format!("task-4195.*{MCP_TASK_TIMEOUT_ENV}")).unwrap();
    assert!(pattern.is_match(&error.message), "{}", error.message);
    assert_eq!(*sent.lock().unwrap(), vec!["tasks/cancel"]);
    within("every timer to clear", clock.settled()).await;
}

/// Upstream: "terminates input-required tasks without a presenter, never fabricating a decline"
#[tokio::test(start_paused = true)]
async fn terminates_input_required_tasks_without_a_presenter_never_fabricating_a_decline() {
    let _frozen = freeze_time();
    let clock = ManualClock::new();
    let script = Script::start(
        vec![
            Some(task("input_required", json!({"resultType": "task"}))),
            Some(task(
                "input_required",
                json!({"inputRequests": {"question": form_input()}}),
            )),
            Some(json!({})),
        ],
        TASKS,
    )
    .await;
    let root = tempfile::tempdir().expect("tempdir");
    let (_connection, client) = connect(&script, root.path()).await;
    let (on_progress, worked) = on_working(Arc::default());
    let pending = call_tool(
        &client,
        &clock,
        SDK_TIMEOUT,
        McpRequestOptions {
            on_progress: Some(on_progress),
            ..Default::default()
        },
    );
    within("the working progress", worked)
        .await
        .expect("working progress");
    clock.advance(400_000.0);
    let code = mrtr_code(within("the stopped call", pending).await.expect("join"));
    assert_eq!(code, McpMrtrErrorCode::ProcessingUnavailable);
    assert_eq!(
        script.methods(),
        ["tools/call", "tasks/get", "tasks/cancel"]
    );
    let wire = serde_json::to_string(
        &script
            .calls()
            .into_iter()
            .map(|call| call.params)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert!(!wire.contains("decline"), "{wire}");
    within("every timer to clear", clock.settled()).await;
    script.close();
}

/// Upstream: "answers through the %s presenter once per task input key"
async fn answers_through_the_presenter_once_per_task_input_key(kind: &str) {
    let _frozen = freeze_time();
    let clock = ManualClock::new();
    let input = task(
        "input_required",
        json!({"inputRequests": {"question": form_input()}, "pollIntervalMs": 10}),
    );
    let script = Script::start(
        vec![
            Some(handle()),
            Some(input.clone()),
            Some(json!({})),
            Some(input),
            Some(task("completed", json!({"result": final_payload()}))),
        ],
        TASKS,
    )
    .await;
    let root = tempfile::tempdir().expect("tempdir");
    let (connection, client) = connect(&script, root.path()).await;
    let (presentation, _lifetime) = presentation(TaskUi);
    let presented = Arc::new(AtomicUsize::new(0));
    let private_form = (kind == "private").then(|| {
        let presented = presented.clone();
        McpFormPresentation {
            signal: maho_ai::utils::abort::AbortController::new().signal(),
            ready: Arc::new(|| true),
            present: Arc::new(move |_input: McpFormInput| {
                presented.fetch_add(1, Ordering::SeqCst);
                Box::pin(async {
                    McpFormOutcome {
                        kind: McpFormOutcomeKind::Result(
                            json!({"action": "accept", "content": {"approved": true}}),
                        ),
                        source: McpFormOutcomeSource::PresenterAction,
                    }
                })
            }),
            synchronize: None,
        }
    });
    let mut options = mcp_request_options(
        Some(origin(connection, Some(presentation), private_form)),
        None,
    );
    options.on_progress = Some(Arc::new(|_| {}));
    let first_poll = clock.next_scheduled(400_000.0);
    let pending = call_tool(&client, &clock, SDK_TIMEOUT, options);
    within("the first poll to be scheduled", first_poll).await;
    let after_input = clock.next_scheduled(10.0);
    clock.advance(400_000.0);
    within("the poll after the input", after_input).await;
    let after_duplicate = clock.next_scheduled(10.0);
    clock.advance(10.0);
    // The repeated key is already answered and must not cause another tasks/update.
    within("the poll after the duplicate", after_duplicate).await;
    clock.advance(10.0);
    let result = within("the task result", pending)
        .await
        .expect("join")
        .expect("task completes");
    assert_eq!(result, final_payload());
    let updates = script
        .calls()
        .into_iter()
        .filter(|call| call.method == "tasks/update")
        .collect::<Vec<_>>();
    assert_eq!(updates.len(), 1);
    assert_eq!(
        updates[0].params["inputResponses"]["question"]["action"],
        "accept"
    );
    assert_eq!(
        presented.load(Ordering::SeqCst),
        usize::from(kind == "private")
    );
    within("every timer to clear", clock.settled()).await;
    script.close();
}

#[tokio::test(start_paused = true)]
async fn answers_through_the_terminal_presenter_once_per_task_input_key() {
    answers_through_the_presenter_once_per_task_input_key("terminal").await;
}

#[tokio::test(start_paused = true)]
async fn answers_through_the_private_presenter_once_per_task_input_key() {
    answers_through_the_presenter_once_per_task_input_key("private").await;
}

/// Upstream: "surfaces terminal %s without an empty success"
async fn surfaces_terminal_status_without_an_empty_success(status: &str) {
    let _frozen = freeze_time();
    let clock = ManualClock::new();
    let script = Script::start(
        vec![Some(task(
            status,
            json!({"resultType": "task", "error": {"code": -32603, "message": "failure"}}),
        ))],
        TASKS,
    )
    .await;
    let root = tempfile::tempdir().expect("tempdir");
    let (_connection, client) = connect(&script, root.path()).await;
    let result = within(
        "the terminal call",
        call_tool(&client, &clock, SDK_TIMEOUT, McpRequestOptions::default()),
    )
    .await
    .expect("join");
    assert_eq!(
        mrtr_code(result),
        if status == "failed" {
            McpMrtrErrorCode::TaskFailed
        } else {
            McpMrtrErrorCode::TaskCancelled
        }
    );
    within("every timer to clear", clock.settled()).await;
    script.close();
}

#[tokio::test(start_paused = true)]
async fn surfaces_terminal_failed_without_an_empty_success() {
    surfaces_terminal_status_without_an_empty_success("failed").await;
}

#[tokio::test(start_paused = true)]
async fn surfaces_terminal_cancelled_without_an_empty_success() {
    surfaces_terminal_status_without_an_empty_success("cancelled").await;
}

/// Upstream: "rejects unsolicited handles without negotiating the extension"
#[tokio::test(start_paused = true)]
async fn rejects_unsolicited_handles_without_negotiating_the_extension() {
    let _frozen = freeze_time();
    let clock = ManualClock::new();
    let script = Script::start(vec![Some(handle())], false).await;
    let root = tempfile::tempdir().expect("tempdir");
    let (_connection, client) = connect(&script, root.path()).await;
    let result = within(
        "the rejected call",
        call_tool(&client, &clock, SDK_TIMEOUT, McpRequestOptions::default()),
    )
    .await
    .expect("join");
    assert_eq!(mrtr_code(result), McpMrtrErrorCode::InvalidResult);
    assert_eq!(script.calls().len(), 1);
    script.close();
}

/// Upstream: "cancels a handle even when abort races its response delivery"
///
/// Upstream aborts right after the transport delivers the handle. The port aborts as the progress callback
/// sees the handle's first status, which is the earliest point the response is observable outside the engine.
#[tokio::test(start_paused = true)]
async fn cancels_a_handle_even_when_abort_races_its_response_delivery() {
    let _frozen = freeze_time();
    let clock = ManualClock::new();
    let script = Script::start(vec![Some(handle()), Some(json!({}))], TASKS).await;
    let root = tempfile::tempdir().expect("tempdir");
    let (_connection, client) = connect(&script, root.path()).await;
    let caller = AbortSignal::default();
    let aborter = caller.clone();
    let result = within(
        "the cancelled call",
        call_tool(
            &client,
            &clock,
            SDK_TIMEOUT,
            McpRequestOptions {
                signal: Some(caller),
                on_progress: Some(Arc::new(move |_| aborter.abort())),
                ..Default::default()
            },
        ),
    )
    .await
    .expect("join");
    assert_eq!(mrtr_code(result), McpMrtrErrorCode::Cancelled);
    assert_eq!(
        script.methods().last().map(String::as_str),
        Some("tasks/cancel")
    );
    within("every timer to clear", clock.settled()).await;
    script.close();
}

/// Upstream: "losing the presenter removes elicitation from subsequent task polls"
#[tokio::test(start_paused = true)]
async fn losing_the_presenter_removes_elicitation_from_subsequent_task_polls() {
    let _frozen = freeze_time();
    let clock = ManualClock::new();
    let script = Script::start(
        vec![
            Some(handle()),
            Some(task("completed", json!({"result": final_payload()}))),
        ],
        TASKS,
    )
    .await;
    let root = tempfile::tempdir().expect("tempdir");
    let (connection, client) = connect(&script, root.path()).await;
    let (presentation, lifetime) = presentation(TaskUi);
    let options = mcp_request_options(Some(origin(connection, Some(presentation), None)), None);
    let scheduled = clock.next_scheduled(400_000.0);
    let pending = call_tool(&client, &clock, SDK_TIMEOUT, options);
    within("the poll to be scheduled", scheduled).await;
    // Upstream `ui.lose()`: the abort is observed synchronously by the next poll's capabilities.
    lifetime.abort(None);
    clock.advance(400_000.0);
    within("the task result", pending)
        .await
        .expect("join")
        .expect("task completes");
    let poll = &script.calls()[1];
    assert_eq!(poll.method, "tasks/get");
    assert_eq!(
        poll.params["_meta"]["io.modelcontextprotocol/clientCapabilities"],
        json!({"extensions": {MCP_TASK_EXTENSION: {}}})
    );
    script.close();
}

/// Upstream: "uses bounded backoff without a hint and adopts a changed server interval"
#[tokio::test(start_paused = true)]
async fn uses_bounded_backoff_without_a_hint_and_adopts_a_changed_server_interval() {
    let _frozen = freeze_time();
    let clock = ManualClock::new();
    let script = Script::start(vec![
        Some(task(
            "working",
            json!({"resultType": "task", "pollIntervalMs": Value::Null}),
        )),
        Some(task("working", json!({"pollIntervalMs": Value::Null}))),
        Some(task("working", json!({"pollIntervalMs": 7}))),
        Some(task(
            "completed",
            json!({"result": {"content": [{"type": "text", "text": "task output"}], "isError": true}}),
        )),
    ], TASKS)
    .await;
    let root = tempfile::tempdir().expect("tempdir");
    let (_connection, client) = connect(&script, root.path()).await;
    let first = clock.next_scheduled(1000.0);
    let pending = call_tool(&client, &clock, SDK_TIMEOUT, McpRequestOptions::default());
    within("the first backoff", first).await;
    let second = clock.next_scheduled(2000.0);
    clock.advance(1000.0);
    within("the second backoff", second).await;
    let third = clock.next_scheduled(7.0);
    clock.advance(2000.0);
    within("the adopted interval", third).await;
    clock.advance(7.0);
    let result = within("the task result", pending)
        .await
        .expect("join")
        .expect("task completes");
    assert_eq!(result["isError"], true);
    within("every timer to clear", clock.settled()).await;
    script.close();
}

/// Upstream: "disconnect while waiting removes the poll and cap timers and close observer"
///
/// Upstream `client.close()` fires the SDK `onclose`; the port's close observer is the client's `closed`
/// channel (`on_mcp_client_close`), which `McpClient::close` sets. The observer is removed when its task ends
/// with the request; there is no `onclose` slot to compare.
#[tokio::test(start_paused = true)]
async fn disconnect_while_waiting_removes_the_poll_and_cap_timers_and_close_observer() {
    let _frozen = freeze_time();
    let clock = ManualClock::new();
    let script = Script::start(vec![Some(handle())], TASKS).await;
    let root = tempfile::tempdir().expect("tempdir");
    let (_connection, client) = connect(&script, root.path()).await;
    let scheduled = clock.next_scheduled(400_000.0);
    let pending = call_tool(&client, &clock, SDK_TIMEOUT, McpRequestOptions::default());
    within("the poll to be scheduled", scheduled).await;
    within("the client to close", client.close())
        .await
        .expect("close");
    let code = mrtr_code(within("the stopped call", pending).await.expect("join"));
    assert_eq!(code, McpMrtrErrorCode::ContextChanged);
    within("every timer to clear", clock.settled()).await;
    assert_eq!(script.calls().len(), 1);
    script.close();
}

/// Upstream: "a stalled poll has its own request timeout and cancels the task without replaying tools/call"
///
/// Upstream advances fake timers by the 1000 ms request timeout; the attempt timeout is a tokio timer here, so the
/// paused tokio clock is advanced by the same amount.
#[tokio::test(start_paused = true)]
async fn a_stalled_poll_has_its_own_request_timeout_and_cancels_the_task_without_replaying_tools_call()
 {
    let _frozen = freeze_time();
    let clock = ManualClock::new();
    let script = Script::start(vec![Some(handle()), None, Some(json!({}))], TASKS).await;
    let root = tempfile::tempdir().expect("tempdir");
    let (_connection, client) = connect(&script, root.path()).await;
    let scheduled = clock.next_scheduled(400_000.0);
    let pending = call_tool(
        &client,
        &clock,
        Duration::from_millis(1000),
        McpRequestOptions::default(),
    );
    within("the poll to be scheduled", scheduled).await;
    assert_eq!(script.next_call().await.method, "tools/call");
    clock.advance(400_000.0);
    assert_eq!(script.next_call().await.method, "tasks/get");
    tokio::time::advance(Duration::from_millis(1000)).await;
    let error = within("the stalled call", pending)
        .await
        .expect("join")
        .expect_err("the stalled poll must fail");
    assert!(!error.retriable);
    assert_eq!(
        as_mcp_mrtr_error(&error).map(|error| error.code),
        Some(McpMrtrErrorCode::TransportFailed)
    );
    assert_eq!(
        script.methods(),
        ["tools/call", "tasks/get", "tasks/cancel"]
    );
    within("every timer to clear", clock.settled()).await;
    script.close();
}

/// Upstream: "the cap interrupts pending task consent without leaking its timers"
///
/// DIVERGENCE: the 500000 ms cap is upstream's stubbed `SENPI_MCP_TASK_TIMEOUT_MS`; see
/// `uses_the_configured_cap_and_names_both_the_task_and_setting_in_the_error`. The cap is driven through
/// `follow_mcp_task` with a consent callback that, like upstream's TUI select, waits for its abort signal.
#[tokio::test(start_paused = true)]
async fn the_cap_interrupts_pending_task_consent_without_leaking_its_timers() {
    use maho_ext_mcp::modern::tasks::{McpTaskOptions, follow_mcp_task};
    let clock = ManualClock::new();
    let replies = Arc::new(Mutex::new(VecDeque::from([
        task(
            "input_required",
            json!({"inputRequests": {"question": form_input()}}),
        ),
        json!({}),
    ])));
    let sent = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let record = sent.clone();
    let send = move |method: &'static str, _params: Value, _signal: Option<AbortSignal>| {
        record.lock().unwrap().push(method);
        let reply = replies.lock().unwrap().pop_front().unwrap_or_default();
        Box::pin(async move { Ok(reply.as_object().cloned().unwrap_or_default()) })
            as BoxFuture<'static, Result<Map<String, Value>, McpMrtrError>>
    };
    let (shown, was_shown) = tokio::sync::oneshot::channel::<()>();
    let shown = Mutex::new(Some(shown));
    let inputs = move |_batch: Map<String, Value>, signal: AbortSignal| {
        if let Some(shown) = shown.lock().unwrap().take() {
            let _ = shown.send(());
        }
        Box::pin(async move {
            signal.cancelled().await;
            Err(McpMrtrError::new(McpMrtrErrorCode::Cancelled))
        }) as BoxFuture<'static, Result<Map<String, Value>, McpMrtrError>>
    };
    let progress = |_: &str, _: &str, _: Option<&str>| -> Result<(), McpMrtrError> { Ok(()) };
    let scheduled = clock.next_scheduled(400_000.0);
    let initial = handle().as_object().cloned().expect("handle object");
    let follower_clock = clock.clone();
    let pending = tokio::spawn(async move {
        follow_mcp_task(
            &initial,
            McpTaskOptions {
                clock: &follower_clock,
                signal: AbortSignal::default(),
                timeout_ms: 500_000.0,
                send: &send,
                inputs: &inputs,
                progress: &progress,
            },
        )
        .await
    });
    within("the poll to be scheduled", scheduled).await;
    clock.advance(400_000.0);
    within("the consent prompt", was_shown)
        .await
        .expect("consent shown");
    clock.advance(100_000.0);
    let error = within("the capped task", pending)
        .await
        .expect("join")
        .expect_err("the cap must stop the task");
    assert_eq!(error.code, McpMrtrErrorCode::TaskTimeout);
    assert_eq!(sent.lock().unwrap().last().copied(), Some("tasks/cancel"));
    within("every timer to clear", clock.settled()).await;
}

// ---------------------------------------------------------------------------
// modern-tasks-http.test.ts
// ---------------------------------------------------------------------------

/// Upstream: "routes task %s and displays status through the tool row"
async fn routes_task_and_displays_status_through_the_tool_row(mode: &str) {
    let _frozen = freeze_time();
    let id = "synthetic-task";
    let working = json!({
        "taskId": id,
        "status": "working",
        "statusMessage": "processing batch",
        "createdAt": "2026-07-28T00:00:00Z",
        "lastUpdatedAt": "2026-07-28T00:00:00Z",
        "ttlMs": Value::Null,
        "pollIntervalMs": 0,
    });
    let mut handle = working.clone();
    handle["resultType"] = json!("task");
    let mut completed = working;
    completed["status"] = json!("completed");
    completed["result"] = json!({"content": [{"type": "text", "text": "done"}]});
    let script = Script::start(
        vec![
            Some(handle),
            Some(if mode == "complete" {
                completed
            } else {
                json!({})
            }),
        ],
        TASKS,
    )
    .await;
    let root = tempfile::tempdir().expect("tempdir");
    let (_connection, client) = connect(&script, root.path()).await;
    let caller = AbortSignal::default();
    let updates = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorded = updates.clone();
    let aborter = caller.clone();
    let abort = mode == "abort";
    let on_update = move |update: ToolResult| -> Result<(), ToolError> {
        for content in update.content {
            if let ToolContent::Text { text, .. } = content {
                recorded.lock().unwrap().push(text);
            }
        }
        if abort {
            aborter.abort();
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
        client: Some(client),
        runtime: None,
        connection: None,
        ensure_connected: None,
        ensure_fresh: None,
        agent_dir: None,
        artifacts: None,
        output_guard: None,
        request_origin: None,
    };
    let result = within(
        "the tool call",
        call_mcp_tool(&entry, json!({}), "fixture", &caller, Some(&on_update)),
    )
    .await;
    if mode == "complete" {
        let result = result.expect("task completes");
        assert_eq!(result["content"], json!([{"type": "text", "text": "done"}]));
    } else {
        let error = result.expect_err("the aborted call must fail");
        assert!(
            error.to_string() == McpMrtrErrorCode::Cancelled.message(),
            "{error}"
        );
    }
    let updates = updates.lock().unwrap().clone();
    assert_eq!(updates.len(), 1, "{updates:?}");
    assert!(
        updates[0].contains(&format!("MCP task {id}: working - processing batch")),
        "{updates:?}"
    );
    let calls = script.calls();
    let followup = &calls[1];
    assert_eq!(
        followup.method,
        if mode == "complete" {
            "tasks/get"
        } else {
            "tasks/cancel"
        }
    );
    assert_eq!(
        followup
            .headers
            .get("mcp-name")
            .and_then(|value| value.to_str().ok()),
        Some(id)
    );
    assert_eq!(
        followup
            .headers
            .get("mcp-method")
            .and_then(|value| value.to_str().ok()),
        Some(followup.method.as_str())
    );
    assert_eq!(
        followup.params["_meta"]["io.modelcontextprotocol/clientCapabilities"]["extensions"]
            [MCP_TASK_EXTENSION],
        json!({})
    );
    script.close();
}

#[tokio::test(start_paused = true)]
async fn routes_task_complete_and_displays_status_through_the_tool_row() {
    routes_task_and_displays_status_through_the_tool_row("complete").await;
}

#[tokio::test(start_paused = true)]
async fn routes_task_abort_and_displays_status_through_the_tool_row() {
    routes_task_and_displays_status_through_the_tool_row("abort").await;
}
