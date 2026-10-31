//! Port of senpi test/mcp/4191-modern-schema-validation.test.ts (#4191): modern MCP schema validation
//! (JSON Schema 2020-12), outputSchema validation without coercion, error handling, dialect and
//! keyword bounds, discovery filtering, persistent cache round trip, and isolated schema identifiers.

use axum::{
    Json, Router,
    http::{StatusCode, header::CONTENT_TYPE},
    response::IntoResponse,
    routing::post,
};
use maho_ext_api::{AbortSignal, ToolCall, ToolContent};
use maho_ext_mcp::{
    catalog::{McpCatalogRuntime, cached_mcp_catalog_entries, collect_tool_catalog},
    catalog_cache::{
        collect_server_catalog_for_cache, read_mcp_catalog_cache, write_mcp_cached_server,
    },
    config_schema::{Auth, McpServerConfig, Transport},
    connection::ServerConnection,
    expose::register::build_mcp_tool_definitions,
    guard::output_guard::McpOutputArtifacts,
    log::McpLogger,
    modern::tool_schema::{
        SchemaErrorCode, compile_modern_schema, has_supported_modern_tool_schemas,
    },
    request_origin::McpRequestOptions,
    tool_client::{call_mcp_tool_request, list_mcp_tools},
    transport_sdk::McpClient,
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

struct ModernValidationFixture {
    client: Arc<McpClient>,
    connection: Arc<ServerConnection>,
    requests: Arc<Mutex<Vec<Value>>>,
    tools: Arc<Mutex<Vec<Value>>>,
    results: Arc<Mutex<Vec<Value>>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
    root: tempfile::TempDir,
}

impl ModernValidationFixture {
    async fn close(mut self) {
        let _ = self.connection.dispose().await;
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), self.task).await;
    }
}

async fn open_modern_validation_fixture(
    initial_tools: Vec<Value>,
    initial_results: Vec<Value>,
) -> ModernValidationFixture {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let tools = Arc::new(Mutex::new(initial_tools));
    let results = Arc::new(Mutex::new(initial_results));

    let requests_capture = requests.clone();
    let tools_capture = tools.clone();
    let results_capture = results.clone();

    let app = Router::new().route(
        "/mcp",
        post(move |Json(message): Json<Value>| {
            let requests = requests_capture.clone();
            let tools = tools_capture.clone();
            let results = results_capture.clone();
            async move {
                requests.lock().unwrap().push(message.clone());

                let method = message.get("method").and_then(Value::as_str);
                let id = message.get("id").cloned().unwrap_or(Value::Null);

                let result = match method {
                    Some("server/discover") => json!({
                        "resultType": "complete",
                        "supportedVersions": ["2026-07-28"],
                        "capabilities": {"tools": {}, "resources": {}, "prompts": {}},
                        "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "synthetic", "version": "1"}},
                    }),
                    Some("tools/list") => json!({
                        "tools": *tools.lock().unwrap()
                    }),
                    Some("tools/call") => {
                        let mut queue = results.lock().unwrap();
                        if queue.is_empty() {
                            json!({"resultType": "complete", "content": []})
                        } else {
                            queue.remove(0)
                        }
                    }
                    _ => json!({"content": []}),
                };

                let reply = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result
                });

                (
                    StatusCode::OK,
                    [(CONTENT_TYPE, "application/json")],
                    reply.to_string(),
                ).into_response()
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let address = listener.local_addr().expect("local addr");
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .expect("serve axum");
    });

    let root = tempfile::tempdir().expect("tempdir");
    let logger = Arc::new(Mutex::new(
        McpLogger::new("modern-schema-validation", root.path(), None).expect("logger"),
    ));

    let config = McpServerConfig {
        enabled: Some(true),
        transport: Some(Transport::Http),
        url: Some(format!("http://{address}/mcp")),
        auth: Some(Auth::Disabled(false)),
        connect_timeout_ms: Some(5000.0),
        ..Default::default()
    };
    let connection = Arc::new(ServerConnection::new(
        "fixture",
        config,
        None,
        logger.clone(),
    ));

    let client = connection.connect().await.expect("connect client");

    ModernValidationFixture {
        client,
        connection,
        requests,
        tools,
        results,
        stop: Some(stop),
        task,
        root,
    }
}

/// Port of senpi test/mcp/4191-modern-schema-validation.test.ts:
/// "accepts structuredContent $value against $schema"
#[tokio::test]
async fn accepts_structured_content_against_schema() {
    let test_cases = vec![
        (json!({ "type": "null" }), Value::Null),
        (json!({ "type": "boolean" }), json!(false)),
        (json!({ "type": "number" }), json!(0)),
        (json!({ "type": "string" }), json!("")),
        (
            json!({ "type": "array", "prefixItems": [{ "type": "integer" }], "items": false }),
            json!([1]),
        ),
        (json!(true), json!({ "any": [false, Value::Null] })),
    ];

    for (schema, value) in test_cases {
        let fixture = open_modern_validation_fixture(
            vec![json!({ "name": "fixture", "inputSchema": { "type": "object" } })],
            vec![json!({ "resultType": "complete", "content": [], "structuredContent": value })],
        )
        .await;

        let result = call_mcp_tool_request(
            &fixture.client,
            json!({ "name": "fixture" }),
            Some(&schema),
            Duration::from_secs(5),
            &McpRequestOptions::default(),
        )
        .await
        .expect("call_mcp_tool_request succeeds");

        assert_eq!(result.get("structuredContent"), Some(&value));
        fixture.close().await;
    }
}

/// Port of senpi test/mcp/4191-modern-schema-validation.test.ts:
/// "rejects missing or invalid structured output without coercion"
#[tokio::test]
async fn rejects_missing_or_invalid_structured_output_without_coercion() {
    let test_cases = vec![
        (
            json!({ "type": "number" }),
            json!({ "content": [], "structuredContent": "0" }),
        ),
        (json!({ "type": "null" }), json!({ "content": [] })),
        (
            json!(false),
            json!({ "content": [], "structuredContent": Value::Null }),
        ),
        (
            json!({ "type": "array", "prefixItems": [{ "type": "integer" }], "items": false }),
            json!({ "content": [], "structuredContent": [1, 2] }),
        ),
    ];

    for (schema, reply) in test_cases {
        let fixture = open_modern_validation_fixture(
            vec![json!({ "name": "fixture", "inputSchema": { "type": "object" } })],
            vec![reply],
        )
        .await;

        let err = call_mcp_tool_request(
            &fixture.client,
            json!({ "name": "fixture" }),
            Some(&schema),
            Duration::from_secs(5),
            &McpRequestOptions::default(),
        )
        .await
        .expect_err("call_mcp_tool_request must reject invalid structured output");

        let code = err
            .cause
            .as_deref()
            .and_then(|c| c.get("code"))
            .and_then(Value::as_str);
        assert_eq!(
            code,
            Some("MCP_OUTPUT_SCHEMA_MISMATCH"),
            "expected MCP_OUTPUT_SCHEMA_MISMATCH cause, got {err:?}"
        );

        fixture.close().await;
    }
}

/// Port of senpi test/mcp/4191-modern-schema-validation.test.ts:
/// "does not require structured output on an execution error"
#[tokio::test]
async fn does_not_require_structured_output_on_an_execution_error() {
    let fixture = open_modern_validation_fixture(
        vec![json!({ "name": "fixture", "inputSchema": { "type": "object" } })],
        vec![json!({ "content": [], "isError": true })],
    )
    .await;

    let result = call_mcp_tool_request(
        &fixture.client,
        json!({ "name": "fixture" }),
        Some(&json!(false)),
        Duration::from_secs(5),
        &McpRequestOptions::default(),
    )
    .await
    .expect("execution error does not require structuredContent");

    assert_eq!(result.get("isError"), Some(&Value::Bool(true)));
    fixture.close().await;
}

/// Port of senpi test/mcp/4191-modern-schema-validation.test.ts:
/// "drops only the unsupported definition at discovery: $code"
#[tokio::test]
async fn drops_only_the_unsupported_definition_at_discovery() {
    let test_cases = vec![
        (
            json!({ "$schema": "http://json-schema.org/draft-07/schema#" }),
            "MCP_SCHEMA_UNSUPPORTED_DIALECT",
        ),
        (
            json!({ "$schema": "https://example.invalid/dialect" }),
            "MCP_SCHEMA_UNSUPPORTED_DIALECT",
        ),
        (
            json!({ "$ref": "https://example.invalid/schema" }),
            "MCP_SCHEMA_INVALID",
        ),
        (json!({ "type": "bogus" }), "MCP_SCHEMA_INVALID"),
    ];

    for (unsupported_schema, expected_code) in test_cases {
        let tools = vec![
            json!({ "name": "before", "inputSchema": true }),
            json!({ "name": "unsupported", "inputSchema": unsupported_schema.clone() }),
            json!({ "name": "after", "inputSchema": { "type": "object" } }),
        ];

        let fixture = open_modern_validation_fixture(tools, vec![]).await;

        let result = list_mcp_tools(
            &fixture.client,
            json!({}),
            Duration::from_secs(5),
            &McpRequestOptions::default(),
        )
        .await
        .expect("list_mcp_tools");

        let listed_names: Vec<&str> = result
            .get("tools")
            .and_then(Value::as_array)
            .expect("tools array")
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        assert_eq!(listed_names, vec!["before", "after"]);

        let err = compile_modern_schema(&unsupported_schema).expect_err("compile must fail");
        assert_eq!(err.code.as_str(), expected_code);

        assert!(!has_supported_modern_tool_schemas(&json!({
            "name": "unsupported",
            "inputSchema": unsupported_schema
        })));
        assert!(has_supported_modern_tool_schemas(&json!({
            "name": "before",
            "inputSchema": true
        })));
        assert!(has_supported_modern_tool_schemas(&json!({
            "name": "after",
            "inputSchema": { "type": "object" }
        })));

        fixture.close().await;
    }
}

/// Port of senpi test/mcp/4191-modern-schema-validation.test.ts:
/// "reports the argument path and keyword so the model can correct a modern call"
#[tokio::test]
async fn reports_the_argument_path_and_keyword_so_the_model_can_correct_a_modern_call() {
    let schema = json!({
        "type": "object",
        "properties": { "count": { "type": "integer" } },
        "required": ["count"]
    });

    let fixture = open_modern_validation_fixture(
        vec![json!({ "name": "fixture", "inputSchema": schema.clone() })],
        vec![],
    )
    .await;

    let entries = collect_tool_catalog("fixture", fixture.client.clone(), Duration::from_secs(5))
        .await
        .expect("collect_tool_catalog");

    let artifacts = Arc::new(McpOutputArtifacts::default());
    let tools = build_mcp_tool_definitions(&entries, fixture.root.path().into(), artifacts, None);
    assert_eq!(tools.len(), 1);

    for (args, keyword) in [
        (json!({ "count": "private-value" }), "type"),
        (json!({}), "required"),
    ] {
        let validator = compile_modern_schema(&schema).expect("compile_modern_schema");
        assert!(!validator.validate(&args));
        let issues = maho_ai::utils::validation::strict_errors(&schema, &args);
        assert!(!issues.is_empty());
        assert!(
            issues
                .iter()
                .any(|(path, msg)| path.contains("count") || msg.contains(keyword))
        );

        let call_err = (tools[0].execute)(ToolCall {
            id: "call".into(),
            params: args.clone(),
            signal: AbortSignal::default(),
            on_update: None,
            context: None,
        })
        .await;
        assert!(call_err.is_err());

        // Upstream assertion against validate_tool_arguments (or tool error)
        let tool_def = maho_ai::types::Tool {
            name: tools[0].name.clone(),
            description: tools[0].description.clone(),
            parameters: tools[0].parameters.clone(),
            output_schema: None,
            freeform: None,
            constrained_sampling: None,
        };
        let tool_call = maho_ai::types::ToolCall {
            id: "call".into(),
            name: tools[0].name.clone(),
            arguments: args.as_object().cloned().unwrap_or_default(),
            ..Default::default()
        };
        let err = maho_ai::utils::validation::validate_tool_arguments(&tool_def, &tool_call)
            .expect_err("validation must fail");
        let message = err.to_string();
        assert!(
            message.contains("count:"),
            "expected message to contain 'count:', got '{message}'"
        );
        assert!(
            message.contains(keyword),
            "expected message to contain '{keyword}', got '{message}'"
        );
        assert!(
            !message.contains("private-value"),
            "expected message not to contain 'private-value', got '{message}'"
        );
    }

    let calls = fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.get("method").and_then(Value::as_str) == Some("tools/call"))
        .count();
    assert_eq!(calls, 0);

    fixture.close().await;
}

/// Port of senpi test/mcp/4191-modern-schema-validation.test.ts:
/// "uses the live modern era for a pre-upgrade cache without the modern flag"
#[tokio::test]
async fn uses_the_live_modern_era_for_a_pre_upgrade_cache_without_the_modern_flag() {
    let tool_schema = json!({
        "type": "object",
        "properties": { "count": { "type": "integer" } },
        "additionalProperties": false
    });

    let fixture = open_modern_validation_fixture(
        vec![json!({ "name": "fixture", "inputSchema": tool_schema })],
        vec![],
    )
    .await;

    let mut old_cache = collect_server_catalog_for_cache(
        &fixture.connection,
        Duration::from_secs(10),
        "schema-config",
    )
    .await
    .expect("collect_server_catalog_for_cache");
    // Pre-upgrade cache files lacked the `modern` flag
    old_cache.modern = false;

    let runtime = Arc::new(McpCatalogRuntime {
        connection: fixture.connection.clone(),
        lifecycle: maho_ext_mcp::idle::McpConnectionLifecycle::configure(
            fixture.connection.clone(),
            fixture.connection.config().clone(),
        ),
        health: Default::default(),
    });

    let entries = cached_mcp_catalog_entries(
        "fixture",
        &old_cache,
        runtime,
        Duration::from_secs(10),
        Arc::new(|| Box::pin(async { Ok(()) })),
    );

    assert_eq!(entries.len(), 1);
    assert!(
        entries[0].modern,
        "pre-upgrade cache adopts live modern era from connected connection"
    );

    let artifacts = Arc::new(McpOutputArtifacts::default());
    let tools = build_mcp_tool_definitions(&entries, fixture.root.path().into(), artifacts, None);
    assert_eq!(tools.len(), 1);

    let validator = compile_modern_schema(&entries[0].schema).expect("compile schema");
    assert!(!validator.validate(&json!({ "count": "3" })));
    assert!(!validator.validate(&json!({ "count": 3, "extra": true })));

    let call_err1 = (tools[0].execute)(ToolCall {
        id: "call1".into(),
        params: json!({ "count": "3" }),
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    })
    .await;
    assert!(call_err1.is_err());

    let call_err2 = (tools[0].execute)(ToolCall {
        id: "call2".into(),
        params: json!({ "count": 3, "extra": true }),
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    })
    .await;
    assert!(call_err2.is_err());

    fixture.close().await;
}

/// Port of senpi test/mcp/4191-modern-schema-validation.test.ts:
/// "drops invalid cached input and output schemas before tool registration"
#[tokio::test]
async fn drops_invalid_cached_input_and_output_schemas_before_tool_registration() {
    let fixture = open_modern_validation_fixture(
        vec![json!({ "name": "valid", "inputSchema": true })],
        vec![],
    )
    .await;

    let mut catalog = collect_server_catalog_for_cache(
        &fixture.connection,
        Duration::from_secs(10),
        "schema-config",
    )
    .await
    .expect("collect_server_catalog_for_cache");

    catalog.tools.push(json!({
        "name": "invalid-input",
        "inputSchema": { "type": "bogus" }
    }));
    catalog.tools.push(json!({
        "name": "invalid-output",
        "inputSchema": true,
        "outputSchema": { "$ref": "https://example.invalid/schema" }
    }));

    write_mcp_cached_server(fixture.root.path(), "fixture", catalog)
        .expect("write_mcp_cached_server");

    let mut cache_file = read_mcp_catalog_cache(fixture.root.path());
    let cached = cache_file
        .servers
        .remove("fixture")
        .expect("cached catalog exists");

    let runtime = Arc::new(McpCatalogRuntime {
        connection: fixture.connection.clone(),
        lifecycle: maho_ext_mcp::idle::McpConnectionLifecycle::configure(
            fixture.connection.clone(),
            fixture.connection.config().clone(),
        ),
        health: Default::default(),
    });

    let entries = cached_mcp_catalog_entries(
        "fixture",
        &cached,
        runtime,
        Duration::from_secs(10),
        Arc::new(|| Box::pin(async { Ok(()) })),
    );

    let artifacts = Arc::new(McpOutputArtifacts::default());
    let tools = build_mcp_tool_definitions(&entries, fixture.root.path().into(), artifacts, None);
    let tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
    assert_eq!(tool_names, vec!["mcp_fixture_valid"]);

    // Verify invalid definitions fail validation
    assert!(!has_supported_modern_tool_schemas(&json!({
        "name": "invalid-input",
        "inputSchema": { "type": "bogus" }
    })));
    assert!(!has_supported_modern_tool_schemas(&json!({
        "name": "invalid-output",
        "inputSchema": true,
        "outputSchema": { "$ref": "https://example.invalid/schema" }
    })));

    fixture.close().await;
}

/// Port of senpi test/mcp/4191-modern-schema-validation.test.ts:
/// "refuses a newly invalid current schema without dispatching or throwing"
#[tokio::test]
async fn refuses_a_newly_invalid_current_schema_without_dispatching_or_throwing() {
    let fixture = open_modern_validation_fixture(
        vec![json!({ "name": "fixture", "inputSchema": true })],
        vec![],
    )
    .await;

    let mut entries =
        collect_tool_catalog("fixture", fixture.client.clone(), Duration::from_secs(5))
            .await
            .expect("collect_tool_catalog");

    // Offiered definition is current, but its current schema was replaced with an invalid one
    entries[0].schema = json!({ "type": "bogus" });

    let artifacts = Arc::new(McpOutputArtifacts::default());
    let tools = build_mcp_tool_definitions(&entries, fixture.root.path().into(), artifacts, None);

    let result = (tools[0].execute)(ToolCall {
        id: "call".into(),
        params: json!({}),
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    })
    .await;

    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("schema_changed"),
        "expected schema_changed in error, got '{err_msg}'"
    );
    assert!(
        err_msg.contains("MCP_SCHEMA_INVALID"),
        "expected MCP_SCHEMA_INVALID in error, got '{err_msg}'"
    );

    let calls = fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.get("method").and_then(Value::as_str) == Some("tools/call"))
        .count();
    assert_eq!(calls, 0);

    fixture.close().await;
}

/// Port of senpi test/mcp/4191-modern-schema-validation.test.ts:
/// "enforces ref siblings and unevaluatedProperties without rewriting arguments"
#[tokio::test]
async fn enforces_ref_siblings_and_unevaluated_properties_without_rewriting_arguments() {
    let schema = json!({
        "$defs": { "count": { "type": "integer", "minimum": 0 } },
        "type": "object",
        "properties": { "count": { "$ref": "#/$defs/count", "maximum": 5 } },
        "unevaluatedProperties": false
    });

    let fixture = open_modern_validation_fixture(
        vec![json!({ "name": "fixture", "inputSchema": schema.clone() })],
        vec![],
    )
    .await;

    let entries = collect_tool_catalog("fixture", fixture.client.clone(), Duration::from_secs(5))
        .await
        .expect("collect_tool_catalog");

    let artifacts = Arc::new(McpOutputArtifacts::default());
    let tools = build_mcp_tool_definitions(&entries, fixture.root.path().into(), artifacts, None);

    let invalid_arguments = vec![
        json!({ "count": 6 }),
        json!({ "count": "3" }),
        json!({ "count": 3, "extra": 1 }),
    ];

    for args in invalid_arguments {
        let validator = compile_modern_schema(&schema).expect("compile modern schema");
        assert!(!validator.validate(&args));

        let result = (tools[0].execute)(ToolCall {
            id: "call".into(),
            params: args,
            signal: AbortSignal::default(),
            on_update: None,
            context: None,
        })
        .await;

        assert!(result.is_err());
    }

    let calls = fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.get("method").and_then(Value::as_str) == Some("tools/call"))
        .count();
    assert_eq!(calls, 0);

    fixture.close().await;
}

/// Port of senpi test/mcp/4191-modern-schema-validation.test.ts:
/// "round trips raw schemas and protocol era through the persistent cache"
#[tokio::test]
async fn round_trips_raw_schemas_and_protocol_era_through_the_persistent_cache() {
    let tool_schema = json!({
        "name": "fixture",
        "inputSchema": true,
        "outputSchema": { "type": "boolean" }
    });
    let call_result = json!({
        "resultType": "complete",
        "content": [],
        "structuredContent": false
    });

    let fixture = open_modern_validation_fixture(vec![tool_schema], vec![call_result]).await;

    let catalog = collect_server_catalog_for_cache(
        &fixture.connection,
        Duration::from_secs(10),
        "schema-config",
    )
    .await
    .expect("collect_server_catalog_for_cache");

    assert!(catalog.modern);
    assert_eq!(catalog.tools[0]["inputSchema"], json!(true));
    assert_eq!(
        catalog.tools[0]["outputSchema"],
        json!({ "type": "boolean" })
    );

    write_mcp_cached_server(fixture.root.path(), "fixture", catalog.clone())
        .expect("write_mcp_cached_server");

    let mut cache_file = read_mcp_catalog_cache(fixture.root.path());
    let cached = cache_file.servers.remove("fixture").expect("cached server");

    assert_eq!(cached.tools, catalog.tools);
    assert!(cached.modern);

    let runtime = Arc::new(McpCatalogRuntime {
        connection: fixture.connection.clone(),
        lifecycle: maho_ext_mcp::idle::McpConnectionLifecycle::configure(
            fixture.connection.clone(),
            fixture.connection.config().clone(),
        ),
        health: Default::default(),
    });

    let entries = cached_mcp_catalog_entries(
        "fixture",
        &cached,
        runtime,
        Duration::from_secs(10),
        Arc::new(|| Box::pin(async { Ok(()) })),
    );

    assert_eq!(entries.len(), 1);
    assert!(entries[0].modern);

    let artifacts = Arc::new(McpOutputArtifacts::default());
    let tools = build_mcp_tool_definitions(&entries, fixture.root.path().into(), artifacts, None);

    let result = (tools[0].execute)(ToolCall {
        id: "call".into(),
        params: json!({}),
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    })
    .await
    .expect("tool execution succeeds");

    assert_eq!(result.content, vec![ToolContent::text("false")]);

    fixture.close().await;
}

/// Port of senpi test/mcp/4191-modern-schema-validation.test.ts:
/// "bounds schema depth and isolates identical schema identifiers"
#[test]
fn bounds_schema_depth_and_isolates_identical_schema_identifiers() {
    let mut deep = json!({ "type": "object" });
    for _ in 0..70 {
        deep = json!({ "allOf": [deep] });
    }
    let err = compile_modern_schema(&deep).expect_err("exceeds depth limit");
    assert_eq!(err.code, SchemaErrorCode::Limit);
    assert_eq!(err.code.as_str(), "MCP_SCHEMA_LIMIT");

    let first = compile_modern_schema(&json!({ "$id": "urn:test:schema", "const": 1 }))
        .expect("compile first");
    let second = compile_modern_schema(&json!({ "$id": "urn:test:schema", "const": 2 }))
        .expect("compile second");

    assert_eq!(
        vec![
            first.validate(&json!(1)),
            first.validate(&json!(2)),
            second.validate(&json!(1)),
            second.validate(&json!(2)),
        ],
        vec![true, false, false, true]
    );
}
