//! Port of senpi test/mcp/4191-modern-schema-validation.test.ts (senpi#4191): validate the real modern wire
//! data, not an object-shaped substitute. The peer is a loopback stub that mirrors fixtures/mrtr-2026-server.ts
//! (`MrtrScript`); `connect` mirrors fixtures/mrtr-connection.ts over HTTP; `console.warn` is the process
//! stderr seam (`maho_ext_api::diagnostic`).

use axum::{Json, Router, routing::post};
use maho_ext_api::{AbortSignal, ToolCall, ToolContent, ToolDefinition};
use maho_ext_mcp::{
    catalog::{
        McpCatalogCallback, McpCatalogRuntime, McpToolCatalogEntry, cached_mcp_catalog_entries,
        collect_tool_catalog,
    },
    catalog_cache::{
        McpCachedServerCatalog, collect_server_catalog_for_cache, read_mcp_catalog_cache,
        write_mcp_cached_server,
    },
    config_schema::{Auth, McpServerConfig, Transport},
    connection::ServerConnection,
    errors::McpError,
    expose::register::build_mcp_tool_definitions,
    guard::output_guard::McpOutputArtifacts,
    idle::McpConnectionLifecycle,
    log::McpLogger,
    modern::tool_schema::{SchemaErrorCode, compile_modern_schema},
    request_origin::McpRequestOptions,
    tool_client::{call_mcp_tool_request, list_mcp_tools},
};
use serde_json::{Map, Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

const TIMEOUT: Duration = Duration::from_secs(10);

/// fixtures/mrtr-2026-server.ts `MrtrScript.reply` for a non-human HTTP peer.
fn reply(
    request: &Value,
    results: &[Value],
    tools: &[Value],
    requests: &Mutex<Vec<Value>>,
) -> Value {
    let meta = &request["params"]["_meta"];
    if meta["io.modelcontextprotocol/protocolVersion"] != "2026-07-28"
        || !meta["io.modelcontextprotocol/clientCapabilities"].is_object()
    {
        return json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32020,"message":"invalid metadata"}});
    }
    let result = match request["method"].as_str().unwrap_or_default() {
        "server/discover" => json!({
            "resultType": "complete",
            "supportedVersions": ["2026-07-28"],
            "capabilities": {"tools": {}, "resources": {}, "prompts": {}},
            "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "synthetic", "version": "1"}},
        }),
        "tools/list" => json!({"tools": tools}),
        "prompts/list" => {
            json!({"prompts": [{"name": "fixture", "description": "Synthetic MRTR form"}]})
        }
        "resources/list" => json!({"resources": [{"name": "fixture", "uri": "test://fixture"}]}),
        "resources/templates/list" => json!({"resourceTemplates": []}),
        method @ ("tools/call" | "resources/read" | "prompts/get") => {
            let round = {
                let mut requests = requests.lock().unwrap();
                requests.push(request.clone());
                requests.len() - 1
            };
            match results.get(round) {
                Some(result) => result.clone(),
                None if method == "resources/read" => {
                    json!({"resultType":"complete","contents":[{"uri":request["params"]["uri"],"text":"final resource"}]})
                }
                None if method == "prompts/get" => {
                    json!({"resultType":"complete","messages":[{"role":"user","content":{"type":"text","text":"final prompt"}}]})
                }
                None => {
                    json!({"resultType":"complete","content":[{"type":"text","text":"final tool"}]})
                }
            }
        }
        _ => json!({}),
    };
    json!({"jsonrpc": "2.0", "id": request["id"], "result": result})
}

struct Peer {
    connection: Arc<ServerConnection>,
    /// `script.requests`: application requests only.
    requests: Arc<Mutex<Vec<Value>>>,
    root: tempfile::TempDir,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl Peer {
    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
    async fn close(mut self) {
        self.connection.dispose().await.expect("dispose");
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .expect("stub stops in time")
            .expect("stub task");
    }
}

/// fixtures/mrtr-connection.ts `connect(results, false, false)`; `tools` is `script.tools` (`None` keeps the
/// script's default `fixture` tool).
async fn connect(results: Vec<Value>, tools: Option<Vec<Value>>) -> Peer {
    let tools = tools
        .unwrap_or_else(|| vec![json!({"name": "fixture", "inputSchema": {"type": "object"}})]);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (seen, results, tools) = (requests.clone(), Arc::new(results), Arc::new(tools));
    let app = Router::new().route(
        "/mcp",
        post(move |Json(request): Json<Value>| {
            let (seen, results, tools) = (seen.clone(), results.clone(), tools.clone());
            async move { Json(reply(&request, &results, &tools, &seen)) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("address");
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .expect("stub server");
    });
    let root = tempfile::tempdir().expect("root");
    let logger = Arc::new(Mutex::new(
        McpLogger::new("fixture", root.path(), None).expect("logger"),
    ));
    let connection = ServerConnection::new(
        "fixture",
        McpServerConfig {
            enabled: Some(true),
            transport: Some(Transport::Http),
            url: Some(format!("http://{address}/mcp")),
            auth: Some(Auth::Disabled(false)),
            connect_timeout_ms: Some(5000.0),
            ..Default::default()
        },
        None,
        logger,
    );
    connection.connect().await.expect("connect");
    Peer {
        connection,
        requests,
        root,
        stop: Some(stop),
        task,
    }
}

/// Serializes the tests that swap the process-wide stderr writer (upstream `vi.spyOn(console, "warn")`).
static STDERR_CAPTURE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Warnings {
    text: Arc<Mutex<String>>,
    previous: Option<maho_tui::process_stdio::StreamWrite>,
    _serial: tokio::sync::MutexGuard<'static, ()>,
}
impl Warnings {
    async fn capture() -> Self {
        let serial = STDERR_CAPTURE.lock().await;
        let text = Arc::new(Mutex::new(String::new()));
        let sink = text.clone();
        let previous = maho_tui::process_stdio::set_stderr_writer(Arc::new(move |line: &str| {
            sink.lock().unwrap().push_str(line)
        }));
        Self {
            text,
            previous: Some(previous),
            _serial: serial,
        }
    }
    /// `expect(warn).toHaveBeenCalledWith(expect.stringContaining(needle))`.
    fn assert_any_contains(&self, needle: &str) {
        let text = self.text.lock().unwrap().clone();
        assert!(
            text.lines().any(|line| line.contains(needle)),
            "no warning contains {needle:?}: {text:?}"
        );
    }
}
impl Drop for Warnings {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            maho_tui::process_stdio::set_stderr_writer(previous);
        }
    }
}

fn definitions(entries: &[McpToolCatalogEntry], root: &tempfile::TempDir) -> Vec<ToolDefinition> {
    build_mcp_tool_definitions(
        entries,
        root.path().into(),
        Arc::new(McpOutputArtifacts::default()),
        None,
    )
}

/// pi-ai `validateToolArguments(tool, { type: "toolCall", id: "call", name: tool.name, arguments })`.
fn validate_tool_arguments(
    tool: &ToolDefinition,
    arguments: &Value,
) -> Result<Value, maho_ai::utils::validation::ValidationError> {
    let model_tool: maho_ai::types::Tool = serde_json::from_value(json!({
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.parameters,
    }))
    .expect("model tool");
    maho_ai::utils::validation::validate_tool_arguments(
        &model_tool,
        &maho_ai::types::ToolCall {
            id: "call".into(),
            name: tool.name.clone(),
            arguments: arguments.as_object().cloned().expect("object arguments"),
            ..Default::default()
        },
    )
}

async fn execute(tool: &ToolDefinition, args: Value) -> maho_ext_api::ToolResult {
    (tool.execute)(ToolCall {
        id: "call",
        params: args,
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    })
    .await
    .expect("execute")
}

fn no_op_ensure_connected() -> McpCatalogCallback {
    Arc::new(
        || -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), McpError>> + Send>> {
            Box::pin(async { Ok::<(), McpError>(()) })
        },
    )
}

/// The Rust counterpart of `cachedToolsToCatalogEntries(server, catalog, connection, 10_000, async () => {})`.
fn cached_entries(peer: &Peer, catalog: &McpCachedServerCatalog) -> Vec<McpToolCatalogEntry> {
    let runtime = Arc::new(McpCatalogRuntime {
        connection: peer.connection.clone(),
        lifecycle: McpConnectionLifecycle::configure(
            peer.connection.clone(),
            peer.connection.config().clone(),
        ),
        health: Default::default(),
    });
    cached_mcp_catalog_entries(
        "fixture",
        catalog,
        runtime,
        TIMEOUT,
        no_op_ensure_connected(),
    )
}

async fn call_fixture(
    peer: &Peer,
    schema: &Value,
) -> Result<Map<String, Value>, maho_ext_mcp::errors::McpError> {
    call_mcp_tool_request(
        &peer.connection.client().expect("client"),
        json!({"name": "fixture"}),
        Some(schema),
        TIMEOUT,
        &McpRequestOptions::default(),
    )
    .await
}

async fn accepts_structured_content(schema: Value, value: Value) {
    let peer = connect(
        vec![json!({"content": [], "structuredContent": value})],
        None,
    )
    .await;
    let result = call_fixture(&peer, &schema)
        .await
        .expect("callMcpToolRequest");
    assert_eq!(result.get("structuredContent"), Some(&value), "{schema}");
    peer.close().await;
}

/// 4191-modern-schema-validation.test.ts: "accepts structuredContent $value against $schema" (null / {type: null})
#[tokio::test]
async fn accepts_structured_content_null_against_type_null() {
    accepts_structured_content(json!({"type": "null"}), Value::Null).await;
}

/// 4191-modern-schema-validation.test.ts: "accepts structuredContent $value against $schema" (false / boolean)
#[tokio::test]
async fn accepts_structured_content_false_against_type_boolean() {
    accepts_structured_content(json!({"type": "boolean"}), json!(false)).await;
}

/// 4191-modern-schema-validation.test.ts: "accepts structuredContent $value against $schema" (0 / number)
#[tokio::test]
async fn accepts_structured_content_zero_against_type_number() {
    accepts_structured_content(json!({"type": "number"}), json!(0)).await;
}

/// 4191-modern-schema-validation.test.ts: "accepts structuredContent $value against $schema" ("" / string)
#[tokio::test]
async fn accepts_structured_content_empty_string_against_type_string() {
    accepts_structured_content(json!({"type": "string"}), json!("")).await;
}

/// 4191-modern-schema-validation.test.ts: "accepts structuredContent $value against $schema" ([1] / prefixItems
/// with items: false)
#[tokio::test]
async fn accepts_structured_content_tuple_against_prefix_items() {
    accepts_structured_content(
        json!({"type": "array", "prefixItems": [{"type": "integer"}], "items": false}),
        json!([1]),
    )
    .await;
}

/// 4191-modern-schema-validation.test.ts: "accepts structuredContent $value against $schema" (object / true)
#[tokio::test]
async fn accepts_structured_content_object_against_true_schema() {
    accepts_structured_content(json!(true), json!({"any": [false, null]})).await;
}

async fn rejects_structured_output(schema: Value, result: Value) {
    let mut reply = json!({"content": []});
    for (key, value) in result.as_object().expect("result row") {
        reply[key] = value.clone();
    }
    let peer = connect(vec![reply], None).await;
    let error = call_fixture(&peer, &schema)
        .await
        .expect_err("output schema mismatch");
    assert_eq!(
        error
            .cause
            .as_deref()
            .and_then(|cause| cause.get("code"))
            .and_then(Value::as_str),
        Some("MCP_OUTPUT_SCHEMA_MISMATCH"),
        "{schema}"
    );
    peer.close().await;
}

/// 4191-modern-schema-validation.test.ts: "rejects missing or invalid structured output without coercion"
/// ({type: number} / "0")
#[tokio::test]
async fn rejects_missing_or_invalid_structured_output_without_coercion_string_for_number() {
    rejects_structured_output(json!({"type": "number"}), json!({"structuredContent": "0"})).await;
}

/// 4191-modern-schema-validation.test.ts: "rejects missing or invalid structured output without coercion"
/// ({type: null} / missing)
#[tokio::test]
async fn rejects_missing_or_invalid_structured_output_without_coercion_missing_for_null() {
    rejects_structured_output(json!({"type": "null"}), json!({})).await;
}

/// 4191-modern-schema-validation.test.ts: "rejects missing or invalid structured output without coercion"
/// (false / null)
#[tokio::test]
async fn rejects_missing_or_invalid_structured_output_without_coercion_false_schema() {
    rejects_structured_output(json!(false), json!({"structuredContent": null})).await;
}

/// 4191-modern-schema-validation.test.ts: "rejects missing or invalid structured output without coercion"
/// (prefixItems with items: false / [1, 2])
#[tokio::test]
async fn rejects_missing_or_invalid_structured_output_without_coercion_extra_tuple_item() {
    rejects_structured_output(
        json!({"type": "array", "prefixItems": [{"type": "integer"}], "items": false}),
        json!({"structuredContent": [1, 2]}),
    )
    .await;
}

/// 4191-modern-schema-validation.test.ts: "does not require structured output on an execution error"
#[tokio::test]
async fn does_not_require_structured_output_on_an_execution_error() {
    let peer = connect(vec![json!({"content": [], "isError": true})], None).await;
    let result = call_fixture(&peer, &json!(false))
        .await
        .expect("an execution error resolves");
    assert_eq!(result.get("isError"), Some(&json!(true)));
    peer.close().await;
}

async fn drops_only_the_unsupported_definition(schema: Value, code: SchemaErrorCode) {
    let warnings = Warnings::capture().await;
    let peer = connect(
        vec![],
        Some(vec![
            json!({"name": "before", "inputSchema": true}),
            json!({"name": "unsupported", "inputSchema": schema}),
            json!({"name": "after", "inputSchema": {"type": "object"}}),
        ]),
    )
    .await;
    let result = list_mcp_tools(
        &peer.connection.client().expect("client"),
        json!({}),
        TIMEOUT,
        &McpRequestOptions::default(),
    )
    .await
    .expect("listMcpTools");
    let names: Vec<&str> = result["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(names, ["before", "after"]);
    warnings.assert_any_contains("unsupported");
    warnings.assert_any_contains(code.as_str());
    peer.close().await;
}

/// 4191-modern-schema-validation.test.ts: "drops only the unsupported definition at discovery: $code"
/// (draft-07 $schema, MCP_SCHEMA_UNSUPPORTED_DIALECT)
#[tokio::test]
async fn drops_only_the_unsupported_definition_at_discovery_draft_07_dialect() {
    drops_only_the_unsupported_definition(
        json!({"$schema": "http://json-schema.org/draft-07/schema#"}),
        SchemaErrorCode::UnsupportedDialect,
    )
    .await;
}

/// 4191-modern-schema-validation.test.ts: "drops only the unsupported definition at discovery: $code"
/// (unknown $schema, MCP_SCHEMA_UNSUPPORTED_DIALECT)
#[tokio::test]
async fn drops_only_the_unsupported_definition_at_discovery_unknown_dialect() {
    drops_only_the_unsupported_definition(
        json!({"$schema": "https://example.invalid/dialect"}),
        SchemaErrorCode::UnsupportedDialect,
    )
    .await;
}

/// 4191-modern-schema-validation.test.ts: "drops only the unsupported definition at discovery: $code"
/// (remote $ref, MCP_SCHEMA_INVALID)
#[tokio::test]
async fn drops_only_the_unsupported_definition_at_discovery_remote_ref() {
    drops_only_the_unsupported_definition(
        json!({"$ref": "https://example.invalid/schema"}),
        SchemaErrorCode::Invalid,
    )
    .await;
}

/// 4191-modern-schema-validation.test.ts: "drops only the unsupported definition at discovery: $code"
/// (type: bogus, MCP_SCHEMA_INVALID)
#[tokio::test]
async fn drops_only_the_unsupported_definition_at_discovery_bogus_type() {
    drops_only_the_unsupported_definition(json!({"type": "bogus"}), SchemaErrorCode::Invalid).await;
}

/// 4191-modern-schema-validation.test.ts: "reports the argument path and keyword so the model can correct a
/// modern call"
#[tokio::test]
async fn reports_the_argument_path_and_keyword_so_the_model_can_correct_a_modern_call() {
    let peer = connect(
        vec![],
        Some(vec![json!({
            "name": "fixture",
            "inputSchema": {"type": "object", "properties": {"count": {"type": "integer"}}, "required": ["count"]},
        })]),
    )
    .await;
    let entries = collect_tool_catalog(
        "fixture",
        peer.connection.client().expect("client"),
        TIMEOUT,
    )
    .await
    .expect("collectToolCatalog");
    let tool = definitions(&entries, &peer.root)
        .into_iter()
        .next()
        .expect("Expected fixture");
    for (args, keyword) in [
        (json!({"count": "private-value"}), "type"),
        (json!({}), "required"),
    ] {
        let message = validate_tool_arguments(&tool, &args)
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(message.contains("count:"), "{message}");
        assert!(message.contains(keyword), "{message}");
        assert!(!message.contains("private-value"), "{message}");
    }
    assert!(peer.requests().is_empty());
    peer.close().await;
}

/// 4191-modern-schema-validation.test.ts: "uses the live modern era for a pre-upgrade cache without the modern
/// flag"
#[tokio::test]
async fn uses_the_live_modern_era_for_a_pre_upgrade_cache_without_the_modern_flag() {
    let peer = connect(
        vec![],
        Some(vec![json!({
            "name": "fixture",
            "inputSchema": {"type": "object", "properties": {"count": {"type": "integer"}}, "additionalProperties": false},
        })]),
    )
    .await;
    let mut old_cache =
        collect_server_catalog_for_cache(&peer.connection, TIMEOUT, "schema-config")
            .await
            .expect("collectServerCatalogForCache");
    // `const { modern: _modern, ...oldCache }`: a cache written before the flag existed.
    old_cache.modern = false;
    let entries = cached_entries(&peer, &old_cache);
    let tool = definitions(&entries, &peer.root)
        .into_iter()
        .next()
        .expect("Expected fixture");
    assert!(validate_tool_arguments(&tool, &json!({"count": "3"})).is_err());
    assert!(validate_tool_arguments(&tool, &json!({"count": 3, "extra": true})).is_err());
    peer.close().await;
}

/// 4191-modern-schema-validation.test.ts: "drops invalid cached input and output schemas before tool
/// registration"
#[tokio::test]
async fn drops_invalid_cached_input_and_output_schemas_before_tool_registration() {
    let warnings = Warnings::capture().await;
    let agent_dir = tempfile::tempdir().expect("agent dir");
    let peer = connect(
        vec![],
        Some(vec![json!({"name": "valid", "inputSchema": true})]),
    )
    .await;
    let mut catalog = collect_server_catalog_for_cache(&peer.connection, TIMEOUT, "schema-config")
        .await
        .expect("collectServerCatalogForCache");
    catalog.tools.extend([
        json!({"name": "invalid-input", "inputSchema": {"type": "bogus"}}),
        json!({"name": "invalid-output", "inputSchema": true, "outputSchema": {"$ref": "https://example.invalid/schema"}}),
    ]);
    write_mcp_cached_server(agent_dir.path(), "fixture", catalog).expect("writeMcpCachedServer");
    let cached = read_mcp_catalog_cache(agent_dir.path())
        .servers
        .remove("fixture")
        .expect("Missing cached catalog");
    let entries = cached_entries(&peer, &cached);
    let names: Vec<String> = definitions(&entries, &peer.root)
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert_eq!(names, ["mcp_fixture_valid"]);
    for name in ["invalid-input", "invalid-output"] {
        warnings.assert_any_contains(name);
    }
    warnings.assert_any_contains("MCP_SCHEMA_INVALID");
    peer.close().await;
}

/// 4191-modern-schema-validation.test.ts: "refuses a newly invalid current schema without dispatching or
/// throwing"
///
/// The port has no `McpInvocationResolver` (expose/call.rs DIVERGENCE note): the entry the tool executes is the
/// current definition, so the newly invalid schema is installed on that entry.
#[tokio::test]
async fn refuses_a_newly_invalid_current_schema_without_dispatching_or_throwing() {
    let peer = connect(
        vec![],
        Some(vec![json!({"name": "fixture", "inputSchema": true})]),
    )
    .await;
    let mut entries = collect_tool_catalog(
        "fixture",
        peer.connection.client().expect("client"),
        TIMEOUT,
    )
    .await
    .expect("collectToolCatalog");
    assert!(!entries.is_empty(), "Expected fixture");
    entries[0].schema = json!({"type": "bogus"});
    let tool = definitions(&entries, &peer.root)
        .into_iter()
        .next()
        .expect("Expected fixture");
    let result = (tool.execute)(ToolCall {
        id: "call",
        params: json!({}),
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    })
    .await
    .expect("a refusal is a result, not a thrown error");
    let details = result.details.expect("details");
    assert_eq!(details["error"]["kind"], "schema_changed");
    assert_eq!(details["error"]["tool"], "fixture");
    assert!(peer.requests().is_empty());
    peer.close().await;
}

/// 4191-modern-schema-validation.test.ts: "enforces ref siblings and unevaluatedProperties without rewriting
/// arguments"
#[tokio::test]
async fn enforces_ref_siblings_and_unevaluated_properties_without_rewriting_arguments() {
    let peer = connect(
        vec![],
        Some(vec![json!({
            "name": "fixture",
            "inputSchema": {
                "$defs": {"count": {"type": "integer", "minimum": 0}},
                "type": "object",
                "properties": {"count": {"$ref": "#/$defs/count", "maximum": 5}},
                "unevaluatedProperties": false,
            },
        })]),
    )
    .await;
    let entries = collect_tool_catalog(
        "fixture",
        peer.connection.client().expect("client"),
        TIMEOUT,
    )
    .await
    .expect("collectToolCatalog");
    let tool = definitions(&entries, &peer.root)
        .into_iter()
        .next()
        .expect("Expected fixture");
    for args in [
        json!({"count": 6}),
        json!({"count": "3"}),
        json!({"count": 3, "extra": 1}),
    ] {
        assert!(validate_tool_arguments(&tool, &args).is_err(), "{args}");
    }
    assert!(peer.requests().is_empty());
    peer.close().await;
}

/// 4191-modern-schema-validation.test.ts: "round trips raw schemas and protocol era through the persistent
/// cache"
#[tokio::test]
async fn round_trips_raw_schemas_and_protocol_era_through_the_persistent_cache() {
    let agent_dir = tempfile::tempdir().expect("agent dir");
    let peer = connect(
        vec![json!({"content": [], "structuredContent": false})],
        Some(vec![
            json!({"name": "fixture", "inputSchema": true, "outputSchema": {"type": "boolean"}}),
        ]),
    )
    .await;
    let catalog = collect_server_catalog_for_cache(&peer.connection, TIMEOUT, "schema-config")
        .await
        .expect("collectServerCatalogForCache");
    write_mcp_cached_server(agent_dir.path(), "fixture", catalog.clone())
        .expect("writeMcpCachedServer");
    let cached = read_mcp_catalog_cache(agent_dir.path())
        .servers
        .remove("fixture")
        .expect("Missing cached catalog");
    assert_eq!(cached.tools, catalog.tools);
    let entries = cached_entries(&peer, &cached);
    let tool = definitions(&entries, &peer.root)
        .into_iter()
        .next()
        .expect("Expected cached fixture");
    let result = execute(&tool, json!({})).await;
    assert_eq!(result.content, [ToolContent::text("false")]);
    peer.close().await;
}

/// 4191-modern-schema-validation.test.ts: "bounds schema depth and isolates identical schema identifiers"
#[test]
fn bounds_schema_depth_and_isolates_identical_schema_identifiers() {
    let mut deep = json!({"type": "object"});
    for _ in 0..70 {
        deep = json!({"allOf": [deep]});
    }
    assert_eq!(
        compile_modern_schema(&deep)
            .expect_err("depth limit")
            .code
            .as_str(),
        "MCP_SCHEMA_LIMIT"
    );
    let first =
        compile_modern_schema(&json!({"$id": "urn:test:schema", "const": 1})).expect("first");
    let second =
        compile_modern_schema(&json!({"$id": "urn:test:schema", "const": 2})).expect("second");
    assert_eq!(
        [
            first.validate(&json!(1)),
            first.validate(&json!(2)),
            second.validate(&json!(1)),
            second.validate(&json!(2)),
        ],
        [true, false, false, true]
    );
}
