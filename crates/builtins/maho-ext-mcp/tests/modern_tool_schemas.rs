//! Port of senpi test/mcp/4191-modern-tool-schemas.test.ts (#4191): modern MCP tool schemas
//! (JSON Schema 2020-12), boolean inputSchema, outputSchema, non-object structuredContent delivery,
//! and exact model argument validation without coercion.

use axum::{
    Json, Router,
    http::{StatusCode, header::CONTENT_TYPE},
    response::IntoResponse,
    routing::post,
};
use maho_ext_api::{AbortSignal, ToolCall, ToolContent};
use maho_ext_mcp::{
    catalog::collect_tool_catalog,
    config_schema::{Auth, McpServerConfig, Transport},
    connection::ServerConnection,
    expose::register::build_mcp_tool_definitions,
    guard::output_guard::McpOutputArtifacts,
    log::McpLogger,
    modern::tool_schema::compile_modern_schema,
    transport_sdk::McpClient,
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

struct ModernToolFixture {
    client: Arc<McpClient>,
    connection: Arc<ServerConnection>,
    requests: Arc<Mutex<Vec<Value>>>,
    tools: Arc<Mutex<Vec<Value>>>,
    results: Arc<Mutex<Vec<Value>>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
    root: tempfile::TempDir,
}

impl ModernToolFixture {
    async fn close(mut self) {
        let _ = self.connection.dispose().await;
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), self.task).await;
    }
}

async fn open_modern_tool_fixture(
    input_schema: Value,
    output_schema: Option<Value>,
    structured_content: Option<Value>,
) -> ModernToolFixture {
    let mut initial_tool = json!({
        "name": "echo",
        "inputSchema": input_schema,
    });
    if let Some(output) = output_schema {
        initial_tool["outputSchema"] = output;
    }

    let initial_result = if let Some(content) = structured_content {
        json!({
            "resultType": "complete",
            "content": [],
            "structuredContent": content,
        })
    } else {
        json!({
            "resultType": "complete",
            "content": [],
        })
    };

    let requests = Arc::new(Mutex::new(Vec::new()));
    let tools = Arc::new(Mutex::new(vec![initial_tool]));
    let results = Arc::new(Mutex::new(vec![initial_result]));

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
        McpLogger::new("modern-tool-schemas", root.path(), None).expect("logger"),
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

    ModernToolFixture {
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

/// Port of senpi test/mcp/4191-modern-tool-schemas.test.ts:
/// "accepts boolean inputSchema through discovery and exposure"
#[tokio::test]
async fn accepts_boolean_input_schema_through_discovery_and_exposure() {
    let fixture = open_modern_tool_fixture(json!(true), None, None).await;
    let entries = collect_tool_catalog("fixture", fixture.client.clone(), Duration::from_secs(5))
        .await
        .expect("collect_tool_catalog");

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].schema, json!(true));
    assert!(entries[0].modern);

    let artifacts = Arc::new(McpOutputArtifacts::default());
    let tools = build_mcp_tool_definitions(&entries, fixture.root.path().into(), artifacts, None);
    assert_eq!(tools.len(), 1);
    // Boolean schemas are exposed wrapped in allOf for TypeBox/tool API compatibility
    assert_eq!(tools[0].parameters, json!({ "allOf": [true] }));

    fixture.close().await;
}

/// Port of senpi test/mcp/4191-modern-tool-schemas.test.ts:
/// "accepts array outputSchema through discovery and exposure"
#[tokio::test]
async fn accepts_array_output_schema_through_discovery_and_exposure() {
    let output_schema = json!({ "type": "array", "items": { "type": "string" } });
    let fixture = open_modern_tool_fixture(
        json!({ "type": "object" }),
        Some(output_schema.clone()),
        None,
    )
    .await;
    let entries = collect_tool_catalog("fixture", fixture.client.clone(), Duration::from_secs(5))
        .await
        .expect("collect_tool_catalog");

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].schema, json!({ "type": "object" }));
    assert_eq!(entries[0].output_schema, Some(output_schema));
    assert!(entries[0].modern);

    fixture.close().await;
}

/// Port of senpi test/mcp/4191-modern-tool-schemas.test.ts:
/// "delivers non-object structuredContent $value to the model"
#[tokio::test]
async fn delivers_non_object_structured_content_to_the_model() {
    let test_values = vec![
        Value::Null,
        json!(false),
        json!(0),
        json!("text"),
        json!([1, 2]),
    ];

    for value in test_values {
        let fixture =
            open_modern_tool_fixture(json!({ "type": "object" }), None, Some(value.clone())).await;

        let entries =
            collect_tool_catalog("fixture", fixture.client.clone(), Duration::from_secs(5))
                .await
                .expect("collect_tool_catalog");

        let artifacts = Arc::new(McpOutputArtifacts::default());
        let tools =
            build_mcp_tool_definitions(&entries, fixture.root.path().into(), artifacts, None);

        let result = (tools[0].execute)(ToolCall {
            id: "call".into(),
            params: json!({}),
            signal: AbortSignal::default(),
            on_update: None,
            context: None,
        })
        .await
        .expect("execute echo tool");

        let expected_text = serde_json::to_string(&value).unwrap();
        assert_eq!(result.content, vec![ToolContent::text(expected_text)]);

        fixture.close().await;
    }
}

/// Port of senpi test/mcp/4191-modern-tool-schemas.test.ts:
/// "retains additionalProperties and ref sibling constraints in the exposed schema"
#[tokio::test]
async fn retains_additional_properties_and_ref_sibling_constraints_in_the_exposed_schema() {
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "$defs": { "count": { "type": "integer", "minimum": 0 } },
        "properties": { "count": { "$ref": "#/$defs/count", "maximum": 5 } }
    });

    let fixture = open_modern_tool_fixture(schema.clone(), None, None).await;
    let entries = collect_tool_catalog("fixture", fixture.client.clone(), Duration::from_secs(5))
        .await
        .expect("collect_tool_catalog");

    let artifacts = Arc::new(McpOutputArtifacts::default());
    let tools = build_mcp_tool_definitions(&entries, fixture.root.path().into(), artifacts, None);

    assert_eq!(tools[0].parameters, schema);

    fixture.close().await;
}

/// Port of senpi test/mcp/4191-modern-tool-schemas.test.ts:
/// "sends exactly the model arguments after validation and exposure"
#[tokio::test]
async fn sends_exactly_the_model_arguments_after_validation_and_exposure() {
    let schema = json!({
        "type": "object",
        "properties": {
            "value": { "anyOf": [{ "type": "integer" }, { "type": "string" }] },
            "optionalCount": { "type": "integer" }
        },
        "required": ["value"]
    });

    let fixture = open_modern_tool_fixture(schema.clone(), None, None).await;
    let entries = collect_tool_catalog("fixture", fixture.client.clone(), Duration::from_secs(5))
        .await
        .expect("collect_tool_catalog");

    let artifacts = Arc::new(McpOutputArtifacts::default());
    let tools = build_mcp_tool_definitions(&entries, fixture.root.path().into(), artifacts, None);

    let arguments_from_model = json!({
        "value": "007",
        "extra": { "nested": [false, Value::Null, 0] }
    });

    // Valid arguments validate cleanly and preserve exact string value ("007" not coerced)
    let validator = compile_modern_schema(&schema).expect("compile modern schema");
    assert!(validator.validate(&arguments_from_model));

    let result = (tools[0].execute)(ToolCall {
        id: "call".into(),
        params: arguments_from_model.clone(),
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    })
    .await
    .expect("tool execution succeeds");

    assert_eq!(result.details.as_ref().unwrap()["tool"], "echo");

    let requests = fixture.requests.lock().unwrap().clone();
    let call_req = requests
        .iter()
        .find(|r| r.get("method").and_then(Value::as_str) == Some("tools/call"))
        .expect("tools/call request dispatched");

    assert_eq!(call_req["params"]["arguments"], arguments_from_model);
    assert_eq!(arguments_from_model["value"], "007");

    // The modern path rejects optional nulls without silently stripping or coercing them
    let mut invalid_arguments = arguments_from_model.clone();
    invalid_arguments["optionalCount"] = Value::Null;

    assert!(!validator.validate(&invalid_arguments));

    let invalid_result = (tools[0].execute)(ToolCall {
        id: "invalid".into(),
        params: invalid_arguments,
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    })
    .await;

    assert!(invalid_result.is_err());

    // Only 1 request reached the server
    let call_count = fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.get("method").and_then(Value::as_str) == Some("tools/call"))
        .count();
    assert_eq!(call_count, 1);

    fixture.close().await;
}
