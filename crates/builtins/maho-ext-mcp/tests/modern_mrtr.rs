//! Tests ported faithfully from:
//! - ~/.cache/sync-5152/senpi-target/packages/coding-agent/test/mcp/modern-mrtr.test.ts
//! - ~/.cache/sync-5152/senpi-target/packages/coding-agent/test/mcp/modern-mrtr-boundaries.test.ts
//! - ~/.cache/sync-5152/senpi-target/packages/coding-agent/test/mcp/modern-mrtr-lifecycle.test.ts
//!
//! Covering the multi-round-trip request (MRTR) engine, continuation lifecycles, boundaries, and deadlines.

use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::post,
};
use maho_ai::utils::abort::AbortSignal as HostSignal;
use maho_core::mcp_mrtr_presentations::McpMrtrPresentation;
use maho_ext_api::AbortSignal;
use maho_ext_api::*;
use maho_ext_mcp::{
    config_schema::{Auth, McpServerConfig, Transport},
    connection::ServerConnection,
    errors::{McpError, McpErrorKind},
    health::{with_mcp_retriable_failed_send_retry, with_mcp_session_expiry_retry},
    log::McpLogger,
    modern::{
        consent_queue::queued_form,
        elicitation::{MrtrInputResponse, collect_mrtr_form},
        errors::{McpMrtrError, McpMrtrErrorCode, as_mcp_mrtr_error},
        tools::call_modern_tool,
    },
    request_origin::{
        McpRequestOptions, McpRequestOrigin, create_mcp_request_origin, mcp_request_options,
    },
    transport_sdk::McpClient,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

fn form_input(message: &str) -> Value {
    json!({
        "method": "elicitation/create",
        "params": {
            "message": message,
            "requestedSchema": {
                "type": "object",
                "properties": {
                    "value": { "type": "string" }
                }
            }
        }
    })
}

fn input_result(state: &str, inputs: Value) -> Value {
    json!({
        "resultType": "input_required",
        "requestState": state,
        "inputRequests": inputs,
    })
}

fn default_input_result() -> Value {
    input_result(
        " opaque \u{2603} state ",
        json!({ "question": form_input("synthetic question") }),
    )
}

#[derive(Clone)]
struct MrtrStub {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    results: Arc<Mutex<Vec<Value>>>,
    continuation_status: Arc<Mutex<Option<u16>>>,
    output_schema: Arc<Mutex<Option<Value>>>,
    sse: Arc<Mutex<bool>>,
    notifications: Arc<Mutex<Vec<Value>>>,
    extra_methods: Arc<Mutex<Vec<String>>>,
    hang: Arc<AtomicBool>,
    stop: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
}

impl MrtrStub {
    async fn start(results: Vec<Value>, sse: bool) -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let results = Arc::new(Mutex::new(results));
        let continuation_status = Arc::new(Mutex::new(None));
        let output_schema = Arc::new(Mutex::new(None));
        let sse = Arc::new(Mutex::new(sse));
        let notifications = Arc::new(Mutex::new(Vec::new()));
        let extra_methods = Arc::new(Mutex::new(Vec::new()));
        let hang = Arc::new(AtomicBool::new(false));

        let req_clone = requests.clone();
        let res_clone = results.clone();
        let status_clone = continuation_status.clone();
        let schema_clone = output_schema.clone();
        let sse_clone = sse.clone();
        let notif_clone = notifications.clone();
        let extra_clone = extra_methods.clone();
        let hang_clone = hang.clone();

        let app = Router::new().route(
            "/mcp",
            post(move |headers: HeaderMap, Json(request): Json<Value>| {
                let requests = req_clone.clone();
                let results = res_clone.clone();
                let continuation_status = status_clone.clone();
                let output_schema = schema_clone.clone();
                let sse = sse_clone.clone();
                let notifications = notif_clone.clone();
                let extra_methods = extra_clone.clone();
                let hang = hang_clone.clone();

                async move {
                    if hang.load(Ordering::SeqCst) {
                        std::future::pending::<()>().await;
                    }

                    let id = request.get("id").cloned().unwrap_or(Value::Null);
                    let method = request
                        .get("method")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let params = request.get("params").cloned().unwrap_or(Value::Null);

                    if let Some(meta) = params.get("_meta") {
                        let proto_version = meta
                            .get("io.modelcontextprotocol/protocolVersion")
                            .and_then(Value::as_str);
                        let client_caps =
                            meta.get("io.modelcontextprotocol/clientCapabilities");
                        if proto_version != Some("2026-07-28")
                            || !client_caps.is_some_and(Value::is_object)
                        {
                            return Json(json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "error": { "code": -32020, "message": "invalid metadata" }
                            }))
                            .into_response();
                        }
                    }

                    if method == "server/discover" {
                        return Json(json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "resultType": "complete",
                                "supportedVersions": ["2026-07-28"],
                                "capabilities": {
                                    "tools": {},
                                    "resources": {},
                                    "prompts": {}
                                },
                                "_meta": {
                                    "io.modelcontextprotocol/serverInfo": {
                                        "name": "synthetic",
                                        "version": "1"
                                    }
                                }
                            }
                        }))
                        .into_response();
                    }

                    if method == "tools/list"
                        && !extra_methods.lock().unwrap().iter().any(|m| m == "tools/list")
                    {
                        let schema = output_schema.lock().unwrap().clone();
                        let mut tool = json!({
                            "name": "fixture",
                            "inputSchema": { "type": "object" }
                        });
                        if let Some(schema) = schema {
                            tool["outputSchema"] = schema;
                        }
                        return Json(json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "tools": [tool]
                            }
                        }))
                        .into_response();
                    }

                    if method == "prompts/list"
                        && !extra_methods.lock().unwrap().iter().any(|m| m == method)
                    {
                        return Json(json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": { "prompts": [{ "name": "fixture", "description": "Synthetic MRTR form" }] }
                        }))
                        .into_response();
                    }

                    if method == "resources/list"
                        && !extra_methods.lock().unwrap().iter().any(|m| m == method)
                    {
                        return Json(json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": { "resources": [{ "name": "fixture", "uri": "test://fixture" }] }
                        }))
                        .into_response();
                    }

                    if method == "resources/templates/list"
                        && !extra_methods.lock().unwrap().iter().any(|m| m == method)
                    {
                        return Json(json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": { "resourceTemplates": [] }
                        }))
                        .into_response();
                    }

                    if method == "ping" {
                        return Json(json!({ "jsonrpc": "2.0", "id": id, "result": {} }))
                            .into_response();
                    }

                    let is_app = ["tools/call", "resources/read", "prompts/get"].contains(&method)
                        || extra_methods.lock().unwrap().iter().any(|m| m == method);
                    if !is_app {
                        return Json(json!({ "jsonrpc": "2.0", "id": id, "result": {} }))
                            .into_response();
                    }

                    let mut reqs = requests.lock().unwrap();
                    let round = reqs.len();
                    reqs.push(request.clone());
                    drop(reqs);

                    let status_code = *continuation_status.lock().unwrap();
                    if let Some(code) = status_code
                        && params.get("requestState").is_some()
                    {
                        let status = StatusCode::from_u16(code).unwrap_or(StatusCode::UNAUTHORIZED);
                        return (
                            status,
                            [(
                                header::WWW_AUTHENTICATE,
                                "Bearer error=\"insufficient_scope\"",
                            )],
                        )
                            .into_response();
                    }

                    let res_list = results.lock().unwrap().clone();
                    let intermediate = res_list.get(round).cloned();
                    let reply_result = if let Some(res) = intermediate {
                        res
                    } else if method == "resources/read" {
                        let uri = params
                            .get("uri")
                            .and_then(Value::as_str)
                            .unwrap_or("test://resource");
                        json!({
                            "resultType": "complete",
                            "contents": [{ "uri": uri, "text": "final resource" }]
                        })
                    } else if method == "prompts/get" {
                        json!({
                            "resultType": "complete",
                            "messages": [{ "role": "user", "content": { "type": "text", "text": "final prompt" } }]
                        })
                    } else {
                        json!({
                            "resultType": "complete",
                            "content": [{ "type": "text", "text": "final tool" }]
                        })
                    };

                    let reply = if let Some(err) = reply_result.get("error") {
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": err
                        })
                    } else {
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": reply_result
                        })
                    };

                    let is_sse = *sse.lock().unwrap();
                    if is_sse {
                        let notifs = notifications.lock().unwrap().clone();
                        let mut body = String::new();
                        for n in notifs {
                            body.push_str(&format!(
                                "data: {}\n\n",
                                serde_json::to_string(&n).unwrap()
                            ));
                        }
                        body.push_str(&format!(
                            "data: {}\n\n",
                            serde_json::to_string(&reply).unwrap()
                        ));
                        ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response()
                    } else {
                        Json(reply).into_response()
                    }
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mrtr stub listener");
        let address = listener.local_addr().expect("mrtr stub address");
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stop_rx.await;
                })
                .await
                .ok();
        });

        Self {
            url: format!("http://{address}/mcp"),
            requests,
            results,
            continuation_status,
            output_schema,
            sse,
            notifications,
            extra_methods,
            hang,
            stop: Arc::new(Mutex::new(Some(stop_tx))),
        }
    }

    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }

    fn close(&self) {
        if let Some(stop) = self.stop.lock().unwrap().take() {
            let _ = stop.send(());
        }
    }
}

type AsyncSelectFn = Arc<
    dyn for<'a> Fn(&'a str, &'a [String], ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>>
        + Send
        + Sync,
>;

struct MrtrUi {
    select_fn: Option<Arc<dyn Fn(&str, &[String]) -> Option<String> + Send + Sync>>,
    async_select_fn: Option<AsyncSelectFn>,
    input_fn: Option<Arc<dyn Fn(&str) -> Option<String> + Send + Sync>>,
    select_titles: Arc<Mutex<Vec<String>>>,
}

impl MrtrUi {
    fn default_ui() -> Arc<Self> {
        Arc::new(Self {
            select_fn: Some(Arc::new(|_title, choices| {
                if choices.iter().any(|c| c == "Submit") {
                    Some("Submit".into())
                } else {
                    choices.first().cloned()
                }
            })),
            async_select_fn: None,
            input_fn: Some(Arc::new(|_| Some("human answer".into()))),
            select_titles: Arc::new(Mutex::new(Vec::new())),
        })
    }

    fn with_select(
        select_fn: impl Fn(&str, &[String]) -> Option<String> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            select_fn: Some(Arc::new(select_fn)),
            async_select_fn: None,
            input_fn: Some(Arc::new(|_| Some("human answer".into()))),
            select_titles: Arc::new(Mutex::new(Vec::new())),
        })
    }

    fn with_handlers(
        select_fn: impl Fn(&str, &[String]) -> Option<String> + Send + Sync + 'static,
        input_fn: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            select_fn: Some(Arc::new(select_fn)),
            async_select_fn: None,
            input_fn: Some(Arc::new(input_fn)),
            select_titles: Arc::new(Mutex::new(Vec::new())),
        })
    }

    fn with_async_select(
        select_fn: impl for<'a> Fn(
            &'a str,
            &'a [String],
            ExtensionUiDialogOptions,
        ) -> UiFuture<'a, Option<String>>
        + Send
        + Sync
        + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            select_fn: None,
            async_select_fn: Some(Arc::new(select_fn)),
            input_fn: Some(Arc::new(|_| Some("human answer".into()))),
            select_titles: Arc::new(Mutex::new(Vec::new())),
        })
    }
}

impl ExtensionUi for MrtrUi {
    fn select<'a>(
        &'a self,
        title: &'a str,
        choices: &'a [String],
        options: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        self.select_titles.lock().unwrap().push(title.to_string());
        if let Some(func) = &self.async_select_fn {
            return func(title, choices, options);
        }
        let res = self.select_fn.as_ref().and_then(|f| f(title, choices));
        Box::pin(async move { res })
    }

    fn confirm<'a>(
        &'a self,
        _title: &'a str,
        _message: &'a str,
        _options: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        Box::pin(async { false })
    }

    fn input<'a>(
        &'a self,
        title: &'a str,
        _placeholder: Option<&'a str>,
        _options: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        let res = self.input_fn.as_ref().and_then(|f| f(title));
        Box::pin(async move { res })
    }

    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Err("custom UI must not be used".into()) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}

fn make_origin_with_binding(
    binding: usize,
    connection: Arc<ServerConnection>,
    ui: Option<Arc<dyn ExtensionUi>>,
    signal: Option<AbortSignal>,
) -> Arc<McpRequestOrigin> {
    let presentation = ui.map(|ui| McpMrtrPresentation {
        ui,
        signal: HostSignal::default(),
        admit: None,
        presented: None,
        private_forms: None,
    });
    create_mcp_request_origin(McpRequestOrigin::new(
        binding,
        connection,
        signal.unwrap_or_default(),
        presentation,
        None,
        false,
        "fixture".into(),
        None,
        Arc::new(|| true),
        Arc::new(|_: AbortSignal| {
            futures::FutureExt::boxed(async { Ok::<_, McpError>(json!({})) })
        }),
    ))
}

fn make_origin(
    connection: Arc<ServerConnection>,
    ui: Option<Arc<dyn ExtensionUi>>,
    signal: Option<AbortSignal>,
) -> Arc<McpRequestOrigin> {
    make_origin_with_binding(1, connection, ui, signal)
}

async fn connect_mrtr(
    server: &MrtrStub,
    ui: Option<Arc<dyn ExtensionUi>>,
) -> (
    Arc<ServerConnection>,
    Arc<McpClient>,
    Arc<McpRequestOrigin>,
    tempfile::TempDir,
) {
    let root = tempfile::tempdir().unwrap();
    let config = McpServerConfig {
        enabled: Some(true),
        transport: Some(Transport::Http),
        url: Some(server.url.clone()),
        auth: Some(Auth::Disabled(false)),
        ..Default::default()
    };
    let connection = ServerConnection::new(
        "fixture",
        config,
        None,
        Arc::new(Mutex::new(
            McpLogger::new("fixture", root.path(), None).unwrap(),
        )),
    );
    let client = connection.connect().await.expect("connected client");
    let origin = make_origin(connection.clone(), ui, None);
    (connection, client, origin, root)
}

// -------------------------------------------------------------------------------------------------
// Suite 1: modern-mrtr.test.ts ports
// -------------------------------------------------------------------------------------------------

/// Upstream case: `submitted continuation is not reauthorized or replayed after HTTP 401` in `modern-mrtr.test.ts`
#[tokio::test]
async fn submitted_continuation_is_not_reauthorized_or_replayed_after_http_401() {
    test_submitted_continuation_rejection_after_http(401).await;
}

/// Upstream case: `submitted continuation is not reauthorized or replayed after HTTP 403` in `modern-mrtr.test.ts`
#[tokio::test]
async fn submitted_continuation_is_not_reauthorized_or_replayed_after_http_403() {
    test_submitted_continuation_rejection_after_http(403).await;
}

async fn test_submitted_continuation_rejection_after_http(status: u16) {
    let server = MrtrStub::start(vec![default_input_result()], false).await;
    *server.continuation_status.lock().unwrap() = Some(status);
    let (connection, client, origin, _root) =
        connect_mrtr(&server, Some(MrtrUi::default_ui())).await;

    let res = with_mcp_session_expiry_retry(&connection, || {
        let connection = connection.clone();
        let client = client.clone();
        let origin = origin.clone();
        async move {
            with_mcp_retriable_failed_send_retry(&connection, || {
                let client = client.clone();
                let origin = origin.clone();
                async move {
                    client
                        .request_with_options(
                            "tools/call",
                            json!({"name": "fixture"}),
                            Duration::from_secs(5),
                            &mcp_request_options(Some(origin), None),
                        )
                        .await
                }
            })
            .await
        }
    })
    .await;

    let err = res.expect_err("expected auth failure");
    assert_eq!(err.kind, McpErrorKind::Auth);
    assert!(!err.retriable);

    let reqs = server.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(
        reqs[1]["params"]["inputResponses"]["question"]["action"],
        "accept"
    );
    server.close();
}

/// Upstream case: `tools/call waits for input_required then returns only complete content (SSE=false)` in `modern-mrtr.test.ts`
#[tokio::test]
async fn tools_call_waits_for_input_required_then_returns_only_complete_content_json() {
    test_tools_call_waits_for_input_required(false).await;
}

/// Upstream case: `tools/call waits for input_required then returns only complete content (SSE=true)` in `modern-mrtr.test.ts`
#[tokio::test]
async fn tools_call_waits_for_input_required_then_returns_only_complete_content_sse() {
    test_tools_call_waits_for_input_required(true).await;
}

async fn test_tools_call_waits_for_input_required(sse: bool) {
    let server = MrtrStub::start(vec![default_input_result()], sse).await;
    let (_conn, client, origin, _root) = connect_mrtr(&server, Some(MrtrUi::default_ui())).await;

    let result = client
        .request_with_options(
            "tools/call",
            json!({"name": "fixture", "arguments": {"value": 1}}),
            Duration::from_secs(5),
            &mcp_request_options(Some(origin), None),
        )
        .await
        .expect("successful tool call");

    assert_eq!(
        result["content"],
        json!([{"type": "text", "text": "final tool"}])
    );
    let reqs = server.requests();
    assert_eq!(reqs.len(), 2);
    assert_ne!(reqs[0]["id"], reqs[1]["id"]);
    assert_eq!(reqs[1]["params"]["name"], "fixture");
    assert_eq!(reqs[1]["params"]["arguments"]["value"], 1);
    assert_eq!(reqs[1]["params"]["requestState"], " opaque \u{2603} state ");
    assert_eq!(
        reqs[1]["params"]["inputResponses"]["question"]["action"],
        "accept"
    );
    assert_eq!(
        reqs[1]["params"]["inputResponses"]["question"]["content"]["value"],
        "human answer"
    );
    assert_eq!(
        reqs[0]["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"]["elicitation"],
        json!({})
    );
    server.close();
}

/// Upstream case: `resources/read completes MRTR before flattening contents` in `modern-mrtr.test.ts`
#[tokio::test]
async fn resources_read_completes_mrtr_before_flattening_contents() {
    let server = MrtrStub::start(vec![default_input_result()], false).await;
    let (_conn, client, origin, _root) = connect_mrtr(&server, Some(MrtrUi::default_ui())).await;

    let result = client
        .request_with_options(
            "resources/read",
            json!({"uri": "test://resource"}),
            Duration::from_secs(5),
            &mcp_request_options(Some(origin), None),
        )
        .await
        .expect("successful resources/read");

    assert_eq!(
        result["contents"],
        json!([{"uri": "test://resource", "text": "final resource"}])
    );
    assert_eq!(server.requests().len(), 2);
    server.close();
}

/// Upstream case: `prompts/get completes MRTR before changing the editor` in `modern-mrtr.test.ts`
#[tokio::test]
async fn prompts_get_completes_mrtr_before_changing_the_editor() {
    let server = MrtrStub::start(vec![default_input_result()], false).await;
    let (_conn, client, origin, _root) = connect_mrtr(&server, Some(MrtrUi::default_ui())).await;

    let result = client
        .request_with_options(
            "prompts/get",
            json!({"name": "fixture"}),
            Duration::from_secs(5),
            &mcp_request_options(Some(origin), None),
        )
        .await
        .expect("successful prompts/get");

    assert_eq!(
        result["messages"][0]["content"],
        json!({"type": "text", "text": "final prompt"})
    );
    assert_eq!(server.requests().len(), 2);
    server.close();
}

/// Upstream case: `each round replaces state and responses instead of merging` in `modern-mrtr.test.ts`
#[tokio::test]
async fn each_round_replaces_state_and_responses_instead_of_merging() {
    let intermediate = vec![
        input_result("S1", json!({"question": form_input("q1")})),
        json!({
            "resultType": "input_required",
            "inputRequests": {}
        }),
        json!({
            "resultType": "input_required",
            "requestState": ""
        }),
    ];
    let server = MrtrStub::start(intermediate, false).await;
    let (_conn, client, origin, _root) = connect_mrtr(&server, Some(MrtrUi::default_ui())).await;

    let _ = client
        .request_with_options(
            "tools/call",
            json!({"name": "fixture"}),
            Duration::from_secs(5),
            &mcp_request_options(Some(origin), None),
        )
        .await
        .expect("callTool succeeds");

    let reqs = server.requests();
    assert_eq!(reqs.len(), 4);
    assert_eq!(reqs[2]["params"]["inputResponses"], json!({}));
    assert!(reqs[2]["params"].get("requestState").is_none());
    assert_eq!(reqs[3]["params"]["requestState"], "");
    assert!(reqs[3]["params"].get("inputResponses").is_none());
    server.close();
}

/// Upstream case: `eight input rounds permit nine attempts and then stop` in `modern-mrtr.test.ts`
#[tokio::test]
async fn eight_input_rounds_permit_nine_attempts_and_then_stop() {
    let intermediates = (0..9).map(|_| default_input_result()).collect();
    let server = MrtrStub::start(intermediates, false).await;
    let (_conn, client, origin, _root) = connect_mrtr(&server, Some(MrtrUi::default_ui())).await;

    let err = client
        .request_with_options(
            "tools/call",
            json!({"name": "fixture"}),
            Duration::from_secs(5),
            &mcp_request_options(Some(origin), None),
        )
        .await
        .expect_err("should exceed round limit");

    let mrtr = as_mcp_mrtr_error(&err).expect("mcp mrtr error");
    assert_eq!(mrtr.code, McpMrtrErrorCode::RoundLimit);
    assert_eq!(server.requests().len(), 9);
    server.close();
}

/// Upstream case: `unsupported result types never become empty success (%s)` in `modern-mrtr.test.ts`
#[tokio::test]
async fn unsupported_result_types_never_become_empty_success() {
    for result_type in [json!("task"), json!("unknown"), json!(null), json!(1)] {
        let server = MrtrStub::start(vec![json!({ "resultType": result_type })], false).await;
        let (_conn, client, origin, _root) =
            connect_mrtr(&server, Some(MrtrUi::default_ui())).await;

        let err = client
            .request_with_options(
                "tools/call",
                json!({"name": "fixture"}),
                Duration::from_secs(5),
                &mcp_request_options(Some(origin), None),
            )
            .await
            .expect_err("should reject unsupported resultType");

        let mrtr = as_mcp_mrtr_error(&err).expect("mcp mrtr error");
        assert_eq!(mrtr.code, McpMrtrErrorCode::InvalidResult);
        server.close();
    }
}

/// Upstream case: `malformed input is rejected before UI (%j)` in `modern-mrtr.test.ts`
#[tokio::test]
async fn malformed_input_is_rejected_before_ui() {
    let cases = vec![
        json!({ "resultType": "input_required" }),
        json!({ "resultType": "input_required", "requestState": 1 }),
        json!({ "resultType": "input_required", "inputRequests": [] }),
        json!({ "resultType": "complete", "requestState": "private" }),
    ];
    for intermediate in cases {
        let server = MrtrStub::start(vec![intermediate], false).await;
        let (_conn, client, origin, _root) =
            connect_mrtr(&server, Some(MrtrUi::default_ui())).await;

        let err = client
            .request_with_options(
                "tools/call",
                json!({"name": "fixture"}),
                Duration::from_secs(5),
                &mcp_request_options(Some(origin), None),
            )
            .await
            .expect_err("should reject malformed input");

        assert!(!err.retriable);
        server.close();
    }
}

/// Upstream case: `undeclared %s stops locally without fabricated results` in `modern-mrtr.test.ts`
#[tokio::test]
async fn undeclared_input_stops_locally_without_fabricated_results() {
    for method in ["roots/list", "sampling/createMessage"] {
        let intermediate = input_result(
            "state",
            json!({
                "input": {
                    "method": method,
                    "params": {}
                }
            }),
        );
        let server = MrtrStub::start(vec![intermediate], false).await;
        let (_conn, client, origin, _root) =
            connect_mrtr(&server, Some(MrtrUi::default_ui())).await;

        let err = client
            .request_with_options(
                "tools/call",
                json!({"name": "fixture"}),
                Duration::from_secs(5),
                &mcp_request_options(Some(origin), None),
            )
            .await
            .expect_err("should reject undeclared capability");

        let mrtr = as_mcp_mrtr_error(&err).expect("mcp mrtr error");
        assert_eq!(mrtr.code, McpMrtrErrorCode::UnsupportedInput);
        assert_eq!(server.requests().len(), 1);
        server.close();
    }
}

/// Upstream case: `server input keys cannot mutate object prototypes` in `modern-mrtr.test.ts`
#[tokio::test]
async fn server_input_keys_cannot_mutate_object_prototypes() {
    let inputs = json!({
        "__proto__": form_input("proto question"),
        "constructor": form_input("constructor question"),
        "toString": form_input("toString question"),
    });
    let server = MrtrStub::start(vec![input_result("state", inputs)], false).await;
    let (_conn, client, origin, _root) = connect_mrtr(&server, Some(MrtrUi::default_ui())).await;

    let _ = client
        .request_with_options(
            "tools/call",
            json!({"name": "fixture"}),
            Duration::from_secs(5),
            &mcp_request_options(Some(origin), None),
        )
        .await
        .expect("callTool succeeds");

    let reqs = server.requests();
    assert_eq!(reqs.len(), 2);
    let responses = reqs[1]["params"]["inputResponses"].as_object().unwrap();
    assert!(responses.contains_key("__proto__"));
    assert!(responses.contains_key("constructor"));
    assert!(responses.contains_key("toString"));
    server.close();
}

/// Upstream case: `no human terminates locally without decline or a retry` in `modern-mrtr.test.ts`
#[tokio::test]
async fn no_human_terminates_locally_without_decline_or_a_retry() {
    let server = MrtrStub::start(vec![default_input_result()], false).await;
    let (_conn, client, origin, _root) = connect_mrtr(&server, None).await;

    let err = client
        .request_with_options(
            "tools/call",
            json!({"name": "fixture"}),
            Duration::from_secs(5),
            &mcp_request_options(Some(origin), None),
        )
        .await
        .expect_err("should terminate when human presentation is missing");

    let mrtr = as_mcp_mrtr_error(&err).expect("mcp mrtr error");
    assert_eq!(mrtr.code, McpMrtrErrorCode::ProcessingUnavailable);
    assert_eq!(server.requests().len(), 1);
    assert_eq!(
        server.requests()[0]["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"],
        json!({})
    );
    server.close();
}

/// Upstream case: `stdio uses the same continuation contract` in `modern-mrtr.test.ts`
#[tokio::test]
async fn stdio_uses_the_same_continuation_contract() {
    let root = tempfile::tempdir().unwrap();
    let script = r#"
const readline = require('node:readline');
process.stderr.write("PRIVATE_STDERR_CANARY\n");
const send = x => process.stdout.write(JSON.stringify(x) + '\n');
let round = 0;
readline.createInterface({ input: process.stdin }).on('line', line => {
    const x = JSON.parse(line);
    if (x.id === 'mcp-stdio-era-probe') {
        send({
            jsonrpc: '2.0',
            id: x.id,
            result: {
                resultType: 'complete',
                supportedVersions: ['2026-07-28'],
                capabilities: { tools: {} },
                _meta: { 'io.modelcontextprotocol/serverInfo': { name: 'stdio-test', version: '1' } }
            }
        });
    } else if (x.method === 'tools/call') {
        if (round === 0) {
            round++;
            send({
                jsonrpc: '2.0',
                id: x.id,
                result: {
                    resultType: 'input_required',
                    requestState: 'opaque',
                    inputRequests: {
                        q: {
                            method: 'elicitation/create',
                            params: {
                                message: 'synthetic question',
                                requestedSchema: { type: 'object', properties: { value: { type: 'string' } } }
                            }
                        }
                    }
                }
            });
        } else {
            send({
                jsonrpc: '2.0',
                id: x.id,
                result: {
                    resultType: 'complete',
                    content: [{ type: 'text', text: 'final tool' }]
                }
            });
        }
    }
});
"#;
    let config = McpServerConfig {
        enabled: Some(true),
        transport: Some(Transport::Stdio),
        command: Some(crate::support::node()),
        args: Some(vec!["-e".into(), script.into()]),
        ..Default::default()
    };
    let logger = Arc::new(Mutex::new(
        McpLogger::new("stdio-test", root.path(), None).unwrap(),
    ));
    let mut transport =
        maho_ext_mcp::transport::create_mcp_transport("stdio-test", &config, None, logger).unwrap();
    transport.elicitation_ui = Some(MrtrUi::default_ui());
    let client = maho_ext_mcp::transport::connect_mcp_transport(&transport)
        .await
        .unwrap();

    let origin = make_origin_with_binding(
        1,
        ServerConnection::new("stdio-test", config.clone(), None, transport.logger.clone()),
        Some(MrtrUi::default_ui()),
        None,
    );
    let result = client
        .request_with_options(
            "tools/call",
            json!({"name": "fixture"}),
            Duration::from_secs(5),
            &mcp_request_options(Some(origin), None),
        )
        .await
        .unwrap();
    assert_eq!(
        result["content"],
        json!([{"type": "text", "text": "final tool"}])
    );

    let log_path = root.path().join("stdio-test.log");
    if log_path.exists() {
        let content = std::fs::read_to_string(&log_path).unwrap_or_default();
        assert!(
            !content.contains("PRIVATE_STDERR_CANARY"),
            "stderr canary leaked into logger"
        );
    }
    maho_ext_mcp::transport::shutdown_mcp_transport(&transport)
        .await
        .ok();
}

/// Upstream case: `modern diagnostic messages are suppressed before consent reaches the logger` in `modern-mrtr.test.ts`
#[tokio::test]
async fn modern_diagnostic_messages_are_suppressed_before_consent_reaches_the_logger() {
    let server = MrtrStub::start(vec![default_input_result()], true).await;
    *server.notifications.lock().unwrap() = vec![json!({
        "jsonrpc": "2.0",
        "method": "notifications/message",
        "params": {
            "level": "warning",
            "data": "PRIVATE_SERVER_LOG_CANARY",
            "logger": "PRIVATE_LOGGER_CANARY"
        }
    })];

    let (_conn, client, origin, root) = connect_mrtr(&server, Some(MrtrUi::default_ui())).await;
    let _ = client
        .request_with_options(
            "tools/call",
            json!({"name": "fixture"}),
            Duration::from_secs(5),
            &mcp_request_options(Some(origin), None),
        )
        .await
        .expect("callTool succeeds");

    let log_path = root.path().join("fixture.log");
    if log_path.exists() {
        let content = std::fs::read_to_string(&log_path).unwrap_or_default();
        assert!(!content.contains("CANARY"));
    }
    server.close();
}

/// Upstream case: `input_required is rejected on noncapable %s` in `modern-mrtr.test.ts`
#[tokio::test]
async fn input_required_is_rejected_on_noncapable_methods() {
    let methods = [
        "resources/list",
        "resources/templates/list",
        "prompts/list",
        "tools/list",
        "completion/complete",
    ];
    for method in methods {
        let server = MrtrStub::start(vec![default_input_result()], false).await;
        server
            .extra_methods
            .lock()
            .unwrap()
            .push(method.to_string());
        let (_conn, client, origin, _root) =
            connect_mrtr(&server, Some(MrtrUi::default_ui())).await;

        let err = client
            .request_with_options(
                method,
                json!({}),
                Duration::from_secs(5),
                &mcp_request_options(Some(origin), None),
            )
            .await
            .expect_err("should reject input_required on non-capable method");

        let mrtr = as_mcp_mrtr_error(&err).expect("mcp mrtr error");
        assert_eq!(mrtr.code, McpMrtrErrorCode::InvalidResult);
        assert_eq!(server.requests().len(), 1);
        server.close();
    }
}

/// Upstream case: `tools/call validates outputSchema only after complete (valid=%s)` in `modern-mrtr.test.ts`
#[tokio::test]
async fn tools_call_validates_output_schema_only_after_complete() {
    for valid in [true, false] {
        let intermediate = vec![
            default_input_result(),
            json!({
                "resultType": "complete",
                "content": [],
                "structuredContent": { "value": if valid { json!(42) } else { json!("invalid") } }
            }),
        ];
        let server = MrtrStub::start(intermediate, false).await;
        let schema = json!({
            "type": "object",
            "properties": { "value": { "type": "number" } },
            "required": ["value"]
        });
        *server.output_schema.lock().unwrap() = Some(schema.clone());

        let (_conn, client, origin, _root) =
            connect_mrtr(&server, Some(MrtrUi::default_ui())).await;

        let call_res = call_modern_tool(
            &client,
            json!({"name": "fixture"}),
            Some(&schema),
            Duration::from_secs(5),
            &mcp_request_options(Some(origin), None),
        )
        .await;

        if valid {
            let res = call_res.expect("should pass validation");
            assert_eq!(res.get("structuredContent"), Some(&json!({"value": 42})));
        } else {
            assert!(call_res.is_err(), "should fail validation");
        }
        assert_eq!(server.requests().len(), 2);
        server.close();
    }
}

/// Upstream case: `oversized input is rejected before UI` in `modern-mrtr.test.ts`
#[tokio::test]
async fn oversized_input_is_rejected_before_ui() {
    let cases = vec![
        json!({
            "resultType": "input_required",
            "requestState": "x".repeat(65537)
        }),
        input_result(
            "state",
            Value::Object(
                (0..33)
                    .map(|i| (i.to_string(), form_input("test")))
                    .collect(),
            ),
        ),
        input_result(
            "state",
            json!({
                "value": {
                    "method": "elicitation/create",
                    "params": {
                        "message": "x".repeat(16385),
                        "requestedSchema": { "type": "object", "properties": {} }
                    }
                }
            }),
        ),
    ];
    for intermediate in cases {
        let server = MrtrStub::start(vec![intermediate], false).await;
        let (_conn, client, origin, _root) =
            connect_mrtr(&server, Some(MrtrUi::default_ui())).await;

        let err = client
            .request_with_options(
                "tools/call",
                json!({"name": "fixture"}),
                Duration::from_secs(5),
                &mcp_request_options(Some(origin), None),
            )
            .await
            .expect_err("should reject oversized input");

        let mrtr = as_mcp_mrtr_error(&err).expect("mcp mrtr error");
        assert_eq!(mrtr.code, McpMrtrErrorCode::InputLimit);
        server.close();
    }
}

/// Upstream case: `caller continuation fields cannot be reused by an application retry` in `modern-mrtr.test.ts`
#[tokio::test]
async fn caller_continuation_fields_cannot_be_reused_by_an_application_retry() {
    let server = MrtrStub::start(vec![], false).await;
    let (_conn, client, origin, _root) = connect_mrtr(&server, Some(MrtrUi::default_ui())).await;

    let err = client
        .request_with_options(
            "tools/call",
            json!({"name": "fixture", "requestState": "injected"}),
            Duration::from_secs(5),
            &mcp_request_options(Some(origin), None),
        )
        .await
        .expect_err("should reject requestState in caller params");

    let mrtr = as_mcp_mrtr_error(&err).expect("mcp mrtr error");
    assert_eq!(mrtr.code, McpMrtrErrorCode::InvalidInput);
    assert_eq!(server.requests().len(), 0);
    server.close();
}

/// Upstream case: `unsolicited URL inputs terminate without navigation or fabricated decline` in `modern-mrtr.test.ts`
#[tokio::test]
async fn unsolicited_url_inputs_terminate_without_navigation_or_fabricated_decline() {
    let intermediate = input_result(
        "state",
        json!({
            "url": {
                "method": "elicitation/create",
                "params": {
                    "mode": "url",
                    "message": "private",
                    "url": "https://invalid.test",
                    "elicitationId": "id"
                }
            }
        }),
    );
    let server = MrtrStub::start(vec![intermediate], false).await;
    let shown = Arc::new(AtomicBool::new(false));
    let was_shown = shown.clone();
    let ui = MrtrUi::with_select(move |_title, _choices| {
        was_shown.store(true, Ordering::SeqCst);
        Some("Continue".into())
    });

    let (_conn, client, origin, _root) = connect_mrtr(&server, Some(ui)).await;
    let err = client
        .request_with_options(
            "tools/call",
            json!({"name": "fixture"}),
            Duration::from_secs(5),
            &mcp_request_options(Some(origin), None),
        )
        .await
        .expect_err("should reject URL mode input");

    let mrtr = as_mcp_mrtr_error(&err).expect("mcp mrtr error");
    assert_eq!(mrtr.code, McpMrtrErrorCode::UnsupportedInput);
    assert!(!shown.load(Ordering::SeqCst));
    assert_eq!(server.requests().len(), 1);
    server.close();
}

// -------------------------------------------------------------------------------------------------
// Suite 2: modern-mrtr-boundaries.test.ts ports
// -------------------------------------------------------------------------------------------------

/// Upstream case: `rejects an already-aborted unpresented form without manufacturing a cancel` in `modern-mrtr-boundaries.test.ts`
#[tokio::test]
async fn rejects_an_already_aborted_unpresented_form_without_manufacturing_a_cancel() {
    let signal = AbortSignal::default();
    signal.abort();
    let run_called = Arc::new(AtomicBool::new(false));
    let was_called = run_called.clone();
    let res = queued_form(1, &signal, || async move {
        was_called.store(true, Ordering::SeqCst);
        Ok::<_, McpMrtrError>(())
    })
    .await;
    assert_eq!(
        res.err().map(|e| e.code),
        Some(McpMrtrErrorCode::ProcessingUnavailable)
    );
    assert!(!run_called.load(Ordering::SeqCst));
}

/// Upstream case: `rejects a queued form aborted before presentation without an action` in `modern-mrtr-boundaries.test.ts`
#[tokio::test]
async fn rejects_a_queued_form_aborted_before_presentation_without_an_action() {
    let binding = 200;
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();

    let first = tokio::spawn(async move {
        queued_form(binding, &AbortSignal::default(), || async move {
            let _ = entered_tx.send(());
            let _ = release_rx.await;
            Ok::<_, McpMrtrError>(MrtrInputResponse::Cancel)
        })
        .await
    });

    let _ = entered_rx.await;

    let second_signal = AbortSignal::default();
    let second_abort = second_signal.clone();
    let second_called = Arc::new(AtomicBool::new(false));
    let second_was_called = second_called.clone();

    let second = tokio::spawn(async move {
        queued_form(binding, &second_signal, || async move {
            second_was_called.store(true, Ordering::SeqCst);
            Ok::<_, McpMrtrError>(MrtrInputResponse::Cancel)
        })
        .await
    });

    second_abort.abort();

    let second_res = second.await.unwrap();
    assert_eq!(
        second_res.err().map(|e| e.code),
        Some(McpMrtrErrorCode::ProcessingUnavailable)
    );
    assert!(!second_called.load(Ordering::SeqCst));

    let _ = release_tx.send(());
    let first_res = first.await.unwrap();
    assert!(first_res.is_ok());
}

/// Upstream case: `prioritizes the round ceiling over an oversized ninth intermediate result` in `modern-mrtr-boundaries.test.ts`
#[tokio::test]
async fn prioritizes_the_round_ceiling_over_an_oversized_ninth_intermediate_result() {
    let mut intermediates: Vec<Value> = (0..8).map(|_| input_result("round", json!({}))).collect();
    intermediates.push(input_result(&"x".repeat(70000), json!({})));

    let server = MrtrStub::start(intermediates, false).await;
    let (_conn, client, origin, _root) = connect_mrtr(&server, Some(MrtrUi::default_ui())).await;

    let err = client
        .request_with_options(
            "tools/call",
            json!({"name": "test"}),
            Duration::from_secs(5),
            &mcp_request_options(Some(origin), None),
        )
        .await
        .expect_err("should hit round limit");

    let mrtr = as_mcp_mrtr_error(&err).expect("mcp mrtr error");
    assert_eq!(mrtr.code, McpMrtrErrorCode::RoundLimit);
    assert_eq!(server.requests().len(), 9);
    server.close();
}

/// Upstream case: `preserves a numeric remote error code without exposing its message` in `modern-mrtr-boundaries.test.ts`
#[tokio::test]
async fn preserves_a_numeric_remote_error_code_without_exposing_its_message() {
    let server = MrtrStub::start(
        vec![json!({
            "error": {
                "code": -32023,
                "message": "PRIVATE_REMOTE_MESSAGE_CANARY"
            }
        })],
        false,
    )
    .await;
    let (_conn, client, origin, _root) = connect_mrtr(&server, Some(MrtrUi::default_ui())).await;

    let err = client
        .request_with_options(
            "tools/call",
            json!({"name": "test"}),
            Duration::from_secs(5),
            &mcp_request_options(Some(origin), None),
        )
        .await
        .expect_err("should fail with transport error");

    let mrtr = as_mcp_mrtr_error(&err).expect("mcp mrtr error");
    assert_eq!(mrtr.code, McpMrtrErrorCode::TransportFailed);
    assert_eq!(mrtr.remote_code, Some(-32023));
    assert!(err.message.contains("-32023"));
    assert!(!err.message.contains("CANARY"));
    server.close();
}

// -------------------------------------------------------------------------------------------------
// Suite 3: modern-mrtr-lifecycle.test.ts ports
// -------------------------------------------------------------------------------------------------

/// Upstream case: `uses the SDK default for both attempt and operation budgets when timeout is omitted` in `modern-mrtr-lifecycle.test.ts`
#[tokio::test(start_paused = true)]
async fn uses_the_default_for_both_attempt_and_operation_budgets_when_timeout_is_omitted() {
    let server = MrtrStub::start(vec![], false).await;
    server.hang.store(true, Ordering::SeqCst);
    let (_conn, client, origin, _root) = connect_mrtr(&server, Some(MrtrUi::default_ui())).await;

    let pending = tokio::spawn(async move {
        client
            .request_with_options(
                "tools/call",
                json!({"name": "test"}),
                Duration::from_secs(60),
                &mcp_request_options(Some(origin), None),
            )
            .await
    });

    tokio::time::advance(Duration::from_millis(840_000)).await;

    let err = pending.await.unwrap().expect_err("should time out");
    let mrtr = as_mcp_mrtr_error(&err).expect("mcp mrtr error");
    assert_eq!(mrtr.code, McpMrtrErrorCode::Timeout);
    server.close();
}

/// Upstream case: `a stopped multi-input batch (%s) never dispatches partial answers` in `modern-mrtr-lifecycle.test.ts`
#[tokio::test]
async fn a_stopped_multi_input_batch_never_dispatches_partial_answers() {
    for stop_action in ["Decline", "Cancel"] {
        let server = MrtrStub::start(
            vec![
                input_result(
                    "batch",
                    json!({
                        "first": form_input("first"),
                        "second": form_input("second"),
                        "third": form_input("third"),
                    }),
                ),
                json!({
                    "resultType": "complete",
                    "content": [{ "type": "text", "text": "must not dispatch" }]
                }),
            ],
            false,
        )
        .await;

        let titles = Arc::new(Mutex::new(Vec::new()));
        let recorded_titles = titles.clone();
        let stop_str = stop_action.to_string();

        let ui = MrtrUi::with_handlers(
            move |title, choices| {
                recorded_titles.lock().unwrap().push(title.to_string());
                if title.contains("second") && choices.iter().any(|c| c == "Continue") {
                    return Some(stop_str.clone());
                }
                if choices.iter().any(|c| c == "Submit") {
                    Some("Submit".into())
                } else {
                    choices.first().cloned()
                }
            },
            |_| Some("UNSENT_VALUE".into()),
        );

        let (_conn, client, origin, _root) = connect_mrtr(&server, Some(ui)).await;
        let err = client
            .request_with_options(
                "tools/call",
                json!({"name": "test"}),
                Duration::from_secs(5),
                &mcp_request_options(Some(origin), None),
            )
            .await
            .expect_err("should cancel");

        let mrtr = as_mcp_mrtr_error(&err).expect("mcp mrtr error");
        assert_eq!(mrtr.code, McpMrtrErrorCode::Cancelled);

        let seen = titles.lock().unwrap();
        assert!(!seen.iter().any(|t| t.contains("third")));
        assert_eq!(server.requests().len(), 1);
        server.close();
    }
}

/// Upstream case: `supplied maxTotalTimeout is a duration from a nonzero start` in `modern-mrtr-lifecycle.test.ts`
#[tokio::test(start_paused = true)]
async fn supplied_max_total_timeout_is_a_duration_from_a_nonzero_start() {
    let server = MrtrStub::start(
        vec![
            default_input_result(),
            json!({
                "resultType": "complete",
                "content": [{ "type": "text", "text": "final" }]
            }),
        ],
        false,
    )
    .await;

    let ui = MrtrUi::with_async_select(|_title, choices, _opts| {
        let choices = choices.to_vec();
        Box::pin(async move {
            if choices.iter().any(|c| c == "Submit") {
                tokio::time::advance(Duration::from_millis(1250)).await;
                return Some("Submit".into());
            }
            choices.first().cloned()
        })
    });

    let (_conn, client, origin, _root) = connect_mrtr(&server, Some(ui)).await;
    let mut options = mcp_request_options(Some(origin), None);
    options.max_total_timeout = Some(Duration::from_millis(2000));

    let res = client
        .request_with_options(
            "tools/call",
            json!({"name": "test"}),
            Duration::from_millis(1000),
            &options,
        )
        .await
        .expect("call succeeds within maxTotalTimeout");

    assert_eq!(res["content"], json!([{"type": "text", "text": "final"}]));
    assert_eq!(server.requests().len(), 2);
    server.close();
}

/// Upstream case: `omitted maxTotalTimeout uses the bounded operation duration` in `modern-mrtr-lifecycle.test.ts`
#[tokio::test(start_paused = true)]
async fn omitted_max_total_timeout_uses_the_bounded_operation_duration() {
    let server = MrtrStub::start(vec![], false).await;
    server.hang.store(true, Ordering::SeqCst);
    let (_conn, client, origin, _root) = connect_mrtr(&server, Some(MrtrUi::default_ui())).await;

    let pending = tokio::spawn(async move {
        client
            .request_with_options(
                "tools/call",
                json!({"name": "test"}),
                Duration::from_millis(1000),
                &mcp_request_options(Some(origin), None),
            )
            .await
    });

    tokio::time::advance(Duration::from_millis(309_000)).await;

    let err = pending.await.unwrap().expect_err("should time out");
    let mrtr = as_mcp_mrtr_error(&err).expect("mcp mrtr error");
    assert_eq!(mrtr.code, McpMrtrErrorCode::Timeout);
    server.close();
}

/// Upstream case: `progress near the deadline cannot extend the logical operation` in `modern-mrtr-lifecycle.test.ts`
#[tokio::test(start_paused = true)]
async fn progress_near_the_deadline_cannot_extend_the_logical_operation() {
    let server = MrtrStub::start(vec![], false).await;
    server.hang.store(true, Ordering::SeqCst);
    let (_conn, client, origin, _root) = connect_mrtr(&server, Some(MrtrUi::default_ui())).await;

    let mut options = mcp_request_options(Some(origin), None);
    options.max_total_timeout = Some(Duration::from_millis(2000));

    let pending = tokio::spawn(async move {
        client
            .request_with_options(
                "tools/call",
                json!({"name": "test"}),
                Duration::from_millis(1000),
                &options,
            )
            .await
    });

    tokio::time::advance(Duration::from_millis(1999)).await;
    tokio::time::advance(Duration::from_millis(1)).await;

    let err = pending.await.unwrap().expect_err("should time out");
    let mrtr = as_mcp_mrtr_error(&err).expect("mcp mrtr error");
    assert_eq!(mrtr.code, McpMrtrErrorCode::Timeout);
    server.close();
}

/// Upstream case: `operation deadline expires while queued for TUI consent` in `modern-mrtr-lifecycle.test.ts`
#[tokio::test(start_paused = true)]
async fn operation_deadline_expires_while_queued_for_tui_consent() {
    let server = MrtrStub::start(vec![default_input_result(), default_input_result()], false).await;
    let (shown_tx, shown_rx) = tokio::sync::oneshot::channel::<()>();
    let shown_tx = Arc::new(Mutex::new(Some(shown_tx)));
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));

    let shown_clone = shown_tx.clone();
    let release_clone = release_rx.clone();

    let ui = MrtrUi::with_async_select(move |_title, _choices, opts| {
        let shown_tx = shown_clone.clone();
        let release_clone = release_clone.clone();
        Box::pin(async move {
            if let Some(tx) = shown_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
            let rx = release_clone.lock().await.take();
            if let Some(rx) = rx {
                tokio::select! {
                    _ = rx => {},
                    _ = async {
                        if let Some(sig) = opts.signal {
                            sig.cancelled().await;
                        } else {
                            std::future::pending::<()>().await;
                        }
                    } => {},
                }
            }
            Some("Submit".into())
        })
    });

    let (connection, client, _orig, _root) = connect_mrtr(&server, Some(ui.clone())).await;

    let binding = 555;
    let origin1 = make_origin_with_binding(binding, connection.clone(), Some(ui.clone()), None);
    let origin2 = make_origin_with_binding(binding, connection.clone(), Some(ui), None);

    let client1 = client.clone();
    let mut opts1 = mcp_request_options(Some(origin1), None);
    opts1.max_total_timeout = Some(Duration::from_millis(10000));
    let first = tokio::spawn(async move {
        client1
            .request_with_options(
                "tools/call",
                json!({"name": "test"}),
                Duration::from_millis(1000),
                &opts1,
            )
            .await
    });

    let _ = shown_rx.await;

    let client2 = client.clone();
    let mut opts2 = mcp_request_options(Some(origin2), None);
    opts2.max_total_timeout = Some(Duration::from_millis(100));
    let second = tokio::spawn(async move {
        client2
            .request_with_options(
                "tools/call",
                json!({"name": "test"}),
                Duration::from_millis(1000),
                &opts2,
            )
            .await
    });

    tokio::time::advance(Duration::from_millis(100)).await;

    let second_err = second.await.unwrap().expect_err("second should time out");
    let mrtr = as_mcp_mrtr_error(&second_err).expect("mcp mrtr error");
    assert_eq!(mrtr.code, McpMrtrErrorCode::Timeout);

    let _ = release_tx.send(());
    first.abort();
    server.close();
}

/// Upstream case: `caller abort wins before continuation submission` in `modern-mrtr-lifecycle.test.ts`
#[tokio::test]
async fn caller_abort_wins_before_continuation_submission() {
    let server = MrtrStub::start(vec![default_input_result()], false).await;
    let caller = AbortSignal::default();
    let caller_abort = caller.clone();

    let ui = MrtrUi::with_select(move |_title, choices| {
        if choices.iter().any(|c| c == "Submit") {
            caller_abort.abort();
            return Some("Submit".into());
        }
        choices.first().cloned()
    });

    let (_conn, client, origin, _root) = connect_mrtr(&server, Some(ui)).await;
    let options = mcp_request_options(Some(origin), Some(caller));

    let err = client
        .request_with_options(
            "tools/call",
            json!({"name": "test"}),
            Duration::from_secs(5),
            &options,
        )
        .await
        .expect_err("should be cancelled by caller abort");

    let mrtr = as_mcp_mrtr_error(&err).expect("mcp mrtr error");
    assert_eq!(mrtr.code, McpMrtrErrorCode::Cancelled);
    assert_eq!(server.requests().len(), 1);
    server.close();
}

/// Upstream case: `raw request uses the same MRTR adapter and final custom schema` in `modern-mrtr-lifecycle.test.ts`
#[tokio::test]
async fn raw_request_uses_the_same_mrtr_adapter_and_final_custom_schema() {
    let intermediate = vec![
        json!({
            "resultType": "input_required",
            "requestState": ""
        }),
        json!({
            "final": 42
        }),
    ];
    let server = MrtrStub::start(intermediate, false).await;
    let (_conn, client, origin, _root) = connect_mrtr(&server, Some(MrtrUi::default_ui())).await;

    let res = client
        .request_with_options(
            "tools/call",
            json!({"name": "test"}),
            Duration::from_secs(5),
            &mcp_request_options(Some(origin), None),
        )
        .await
        .expect("raw request completes");

    assert_eq!(res["final"], 42);
    let reqs = server.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[1]["params"]["requestState"], "");
    server.close();
}
