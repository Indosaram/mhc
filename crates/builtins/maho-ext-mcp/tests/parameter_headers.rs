//! Port of senpi test/mcp/4192-parameter-headers.test.ts (#4192, #4246): declared `x-mcp-header`
//! tool parameters on modern HTTP transport, argument projection, encoding rules, and catalog invalidation.

use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode, header::CONTENT_TYPE},
    response::IntoResponse,
    routing::post,
};
use base64::Engine;
use maho_ext_mcp::{
    config_schema::{Auth, McpServerConfig, Transport},
    connection::ServerConnection,
    errors::{McpError, McpErrorKind},
    log::McpLogger,
    modern::parameter_headers::{
        ModernParameterHeaders, clear_mcp_parameter_headers, encode_mcp_header_value,
    },
    transport_sdk::McpClient,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone, Debug)]
struct RecordedRequest {
    message: Value,
    headers: BTreeMap<String, String>,
}

#[derive(Default)]
struct HeaderFixtureOptions {
    legacy: bool,
    sse: bool,
    headers: Option<BTreeMap<String, String>>,
}

struct HeaderFixture {
    client: Arc<McpClient>,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    tools: Arc<Mutex<Vec<Value>>>,
    logger: Arc<Mutex<McpLogger>>,
    connection: Arc<ServerConnection>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
    _root: tempfile::TempDir,
}

impl HeaderFixture {
    async fn list_tools(&self) -> Result<Value, McpError> {
        self.client
            .request("tools/list", json!({}), Duration::from_secs(5))
            .await
    }

    async fn call_tool(&self, name: &str, arguments: Option<Value>) -> Result<Value, McpError> {
        let mut params = json!({"name": name});
        if let Some(args) = arguments {
            params["arguments"] = args;
        }
        self.client
            .request("tools/call", params, Duration::from_secs(5))
            .await
    }

    async fn close(mut self) {
        let _ = self.connection.dispose().await;
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), self.task).await;
    }
}

async fn open_header_fixture(tools: Vec<Value>, options: HeaderFixtureOptions) -> HeaderFixture {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let tools = Arc::new(Mutex::new(tools));

    let requests_capture = requests.clone();
    let tools_capture = tools.clone();
    let legacy = options.legacy;
    let sse = options.sse;

    let app = Router::new().route(
        "/mcp",
        post(move |headers: HeaderMap, Json(message): Json<Value>| {
            let requests = requests_capture.clone();
            let tools = tools_capture.clone();
            async move {
                let mut recorded_headers = BTreeMap::new();
                for (name, val) in &headers {
                    if let Ok(v) = val.to_str() {
                        recorded_headers.insert(name.as_str().to_ascii_lowercase(), v.to_string());
                    }
                }
                requests.lock().unwrap().push(RecordedRequest {
                    message: message.clone(),
                    headers: recorded_headers,
                });

                let method = message.get("method").and_then(Value::as_str);
                let id = message.get("id").cloned().unwrap_or(Value::Null);

                if method == Some("server/discover") && legacy {
                    return (
                        StatusCode::BAD_REQUEST,
                        [(CONTENT_TYPE, "application/json")],
                        json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "Unknown"}}).to_string(),
                    ).into_response();
                }

                if method == Some("notifications/initialized") {
                    return StatusCode::ACCEPTED.into_response();
                }

                let result = match method {
                    Some("server/discover") => json!({
                        "resultType": "complete",
                        "supportedVersions": ["2026-07-28"],
                        "capabilities": {"tools": {}},
                        "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "synthetic-peer", "version": "1"}},
                    }),
                    Some("initialize") => json!({
                        "protocolVersion": "2025-11-25",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "legacy", "version": "1"},
                    }),
                    Some("tools/list") => json!({
                        "tools": *tools.lock().unwrap()
                    }),
                    _ => json!({"content": []}),
                };

                let reply = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result
                });

                if sse {
                    (
                        StatusCode::OK,
                        [(CONTENT_TYPE, "text/event-stream")],
                        format!("data: {reply}\n\n"),
                    ).into_response()
                } else {
                    (
                        StatusCode::OK,
                        [(CONTENT_TYPE, "application/json")],
                        reply.to_string(),
                    ).into_response()
                }
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
        McpLogger::new("headers", root.path(), None).expect("logger"),
    ));

    let config = McpServerConfig {
        enabled: Some(true),
        transport: Some(Transport::Http),
        url: Some(format!("http://{address}/mcp")),
        auth: Some(Auth::Disabled(false)),
        headers: options.headers,
        connect_timeout_ms: Some(5000.0),
        ..Default::default()
    };
    let connection = Arc::new(ServerConnection::new(
        "configured-fixture",
        config,
        None,
        logger.clone(),
    ));

    let client = connection.connect().await.expect("connect client");

    HeaderFixture {
        client,
        requests,
        tools,
        logger,
        connection,
        stop: Some(stop),
        task,
        _root: root,
    }
}

/// Port of senpi test/mcp/4192-parameter-headers.test.ts:
/// "mirrors annotated arguments at their exact nested paths"
#[tokio::test]
async fn mirrors_annotated_arguments_at_their_exact_nested_paths() {
    let tools = vec![json!({
        "name": "echo",
        "inputSchema": {
            "type": "object",
            "properties": {
                "region": { "type": "string", "x-mcp-header": "Region" },
                "nested": {
                    "type": "object",
                    "properties": {
                        "flag": { "type": "boolean", "x-mcp-header": "Flag" }
                    }
                }
            }
        }
    })];

    let fixture = open_header_fixture(tools, HeaderFixtureOptions::default()).await;
    fixture.list_tools().await.expect("listTools");
    fixture
        .call_tool(
            "echo",
            Some(json!({
                "region": "us-west1",
                "nested": { "flag": false }
            })),
        )
        .await
        .expect("callTool");

    let requests = fixture.requests.lock().unwrap().clone();
    let wire = requests
        .iter()
        .find(|r| r.message.get("method").and_then(Value::as_str) == Some("tools/call"))
        .expect("wire tools/call request");

    assert_eq!(
        wire.headers.get("mcp-name").map(String::as_str),
        Some("echo")
    );
    assert_eq!(
        wire.headers.get("mcp-param-region").map(String::as_str),
        Some("us-west1")
    );
    assert_eq!(
        wire.headers.get("mcp-param-flag").map(String::as_str),
        Some("false")
    );

    fixture.close().await;
}

/// Port of senpi test/mcp/4192-parameter-headers.test.ts:
/// "encodes Mcp-Name with standard Base64 and the spec sentinel for %j" (snowman)
#[tokio::test]
async fn encodes_mcp_name_with_standard_base64_and_the_spec_sentinel_snowman() {
    let name = "snowman-\u{2603}";
    let tools = vec![json!({ "name": name, "inputSchema": { "type": "object" } })];
    let fixture = open_header_fixture(tools, HeaderFixtureOptions::default()).await;
    fixture.list_tools().await.expect("listTools");
    fixture.call_tool(name, None).await.expect("callTool");

    let requests = fixture.requests.lock().unwrap().clone();
    let wire = requests
        .iter()
        .find(|r| r.message.get("method").and_then(Value::as_str) == Some("tools/call"))
        .expect("wire tools/call request");

    let expected = format!(
        "=?base64?{}?=",
        base64::engine::general_purpose::STANDARD.encode(name.as_bytes())
    );
    assert_eq!(
        wire.headers.get("mcp-name").map(String::as_str),
        Some(expected.as_str())
    );

    fixture.close().await;
}

/// Port of senpi test/mcp/4192-parameter-headers.test.ts:
/// "encodes Mcp-Name with standard Base64 and the spec sentinel for %j" (padded)
#[tokio::test]
async fn encodes_mcp_name_with_standard_base64_and_the_spec_sentinel_padded() {
    let name = " padded ";
    let tools = vec![json!({ "name": name, "inputSchema": { "type": "object" } })];
    let fixture = open_header_fixture(tools, HeaderFixtureOptions::default()).await;
    fixture.list_tools().await.expect("listTools");
    fixture.call_tool(name, None).await.expect("callTool");

    let requests = fixture.requests.lock().unwrap().clone();
    let wire = requests
        .iter()
        .find(|r| r.message.get("method").and_then(Value::as_str) == Some("tools/call"))
        .expect("wire tools/call request");

    let expected = format!(
        "=?base64?{}?=",
        base64::engine::general_purpose::STANDARD.encode(name.as_bytes())
    );
    assert_eq!(
        wire.headers.get("mcp-name").map(String::as_str),
        Some(expected.as_str())
    );

    fixture.close().await;
}

/// Port of senpi test/mcp/4192-parameter-headers.test.ts:
/// "encodes Mcp-Name with standard Base64 and the spec sentinel for %j" (literal sentinel)
#[tokio::test]
async fn encodes_mcp_name_with_standard_base64_and_the_spec_sentinel_literal() {
    let name = "=?base64?literal?=";
    let tools = vec![json!({ "name": name, "inputSchema": { "type": "object" } })];
    let fixture = open_header_fixture(tools, HeaderFixtureOptions::default()).await;
    fixture.list_tools().await.expect("listTools");
    fixture.call_tool(name, None).await.expect("callTool");

    let requests = fixture.requests.lock().unwrap().clone();
    let wire = requests
        .iter()
        .find(|r| r.message.get("method").and_then(Value::as_str) == Some("tools/call"))
        .expect("wire tools/call request");

    let expected = format!(
        "=?base64?{}?=",
        base64::engine::general_purpose::STANDARD.encode(name.as_bytes())
    );
    assert_eq!(
        wire.headers.get("mcp-name").map(String::as_str),
        Some(expected.as_str())
    );

    fixture.close().await;
}

/// Port of senpi test/mcp/4192-parameter-headers.test.ts:
/// "excludes an invalid number annotation without losing valid tools"
#[tokio::test]
async fn excludes_an_invalid_number_annotation_without_losing_valid_tools() {
    let tools = vec![
        json!({ "name": "good", "inputSchema": { "type": "object" } }),
        json!({
            "name": "bad",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "value": { "type": "number", "x-mcp-header": "Value", "default": "private-schema-value" },
                    "other": { "type": "number", "x-mcp-header": "Other" }
                }
            }
        }),
    ];

    let fixture = open_header_fixture(tools, HeaderFixtureOptions::default()).await;
    let result = fixture.list_tools().await.expect("listTools");
    let names: Vec<&str> = result["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();
    assert_eq!(names, vec!["good"]);

    // #4246: one warning per rejected tool, with a fixed reason, never schema values.
    let ring = fixture.logger.lock().unwrap().get_ring_buffer();
    let warnings: Vec<Value> = ring
        .iter()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|entry| entry.get("level").and_then(Value::as_str) == Some("warning"))
        .collect();

    assert_eq!(warnings.len(), 1);
    let message = warnings[0]["message"].as_str().unwrap_or_default();
    assert!(message.contains("bad"));
    assert!(message.contains("type must be string, integer, or boolean"));
    let warnings_json = serde_json::to_string(&warnings).unwrap();
    assert!(!warnings_json.contains("private-schema-value"));

    fixture.close().await;
}

/// Port of senpi test/mcp/4192-parameter-headers.test.ts:
/// "rejects invalid header definitions in schema %j"
#[tokio::test]
async fn rejects_invalid_header_definitions_in_schema() {
    let test_cases = vec![
        json!({ "properties": { "value": { "type": "string", "x-mcp-header": "" } } }),
        json!({ "properties": { "value": { "type": "string", "x-mcp-header": "bad name" } } }),
        json!({ "properties": { "value": { "type": "string", "x-mcp-header": "bad\r\nname" } } }),
        json!({ "properties": { "value": { "type": "string", "x-mcp-header": 1 } } }),
        json!({ "properties": { "value": { "type": ["string", "null"], "x-mcp-header": "Value" } } }),
        json!({ "properties": { "first": { "type": "string", "x-mcp-header": "A" }, "second": { "type": "boolean", "x-mcp-header": "a" } } }),
        json!({ "type": "string", "x-mcp-header": "Root" }),
        json!({ "properties": { "values": { "type": "array", "items": { "type": "string", "x-mcp-header": "Value" } } } }),
        json!({ "allOf": [{ "properties": { "value": { "type": "string", "x-mcp-header": "Value" } } }] }),
        json!({ "if": { "properties": { "value": { "type": "string", "x-mcp-header": "Value" } } } }),
        json!({ "$defs": { "value": { "type": "string", "x-mcp-header": "Value" } }, "properties": { "value": { "$ref": "#/$defs/value" } } }),
        json!({ "additionalProperties": { "type": "string", "x-mcp-header": "Value" } }),
        json!({ "x-vendor": { "type": "string", "x-mcp-header": "Value" } }),
        json!({ "x-vendor": [{ "properties": { "value": { "type": "string", "x-mcp-header": "Value" } } }] }),
        json!({ "unknown-keyword": { "nested": { "type": "string", "x-mcp-header": "Value" } } }),
    ];

    for invalid_schema in test_cases {
        let mut bad_schema = json!({ "type": "object" });
        if let Some(obj) = invalid_schema.as_object() {
            for (k, v) in obj {
                bad_schema[k] = v.clone();
            }
        }
        let tools = vec![
            json!({ "name": "bad", "inputSchema": bad_schema }),
            json!({ "name": "good", "inputSchema": { "type": "object" } }),
        ];

        let fixture = open_header_fixture(tools, HeaderFixtureOptions::default()).await;
        let result = fixture.list_tools().await.expect("listTools");
        let names: Vec<&str> = result["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        assert_eq!(names, vec!["good"]);
        fixture.close().await;
    }
}

/// Port of senpi test/mcp/4192-parameter-headers.test.ts:
/// "encodes primitive parameter-header values and omits undeclared and absent arguments (SSE false)"
#[tokio::test]
async fn encodes_primitive_parameter_header_values_and_omits_undeclared_and_absent_arguments_json()
{
    test_primitive_parameter_headers_matrix(false).await;
}

/// Port of senpi test/mcp/4192-parameter-headers.test.ts:
/// "encodes primitive parameter-header values and omits undeclared and absent arguments (SSE true)"
#[tokio::test]
async fn encodes_primitive_parameter_header_values_and_omits_undeclared_and_absent_arguments_sse() {
    test_primitive_parameter_headers_matrix(true).await;
}

async fn test_primitive_parameter_headers_matrix(sse: bool) {
    let tools = vec![json!({
        "name": "echo",
        "inputSchema": {
            "type": "object",
            "properties": {
                "text": { "type": "string", "x-mcp-header": "Text" },
                "count": { "type": "integer", "x-mcp-header": "Count" },
                "flag": { "type": "boolean", "x-mcp-header": "Flag" },
                "missing": { "type": "string", "x-mcp-header": "Missing" },
                "nested": { "type": "object", "properties": { "private": { "type": "string" } } }
            },
            "examples": [{ "x-mcp-header": "this is instance data" }],
            "default": { "x-mcp-header": "this is instance data" },
            "const": { "x-mcp-header": "this is instance data" },
            "enum": [{ "x-mcp-header": "this is instance data" }]
        }
    })];

    let mut configured_headers = BTreeMap::new();
    configured_headers.insert("Mcp-Param-Missing".into(), "must-not-project".into());
    configured_headers.insert("Mcp-Param-Extra".into(), "must-not-project".into());

    let fixture = open_header_fixture(
        tools,
        HeaderFixtureOptions {
            sse,
            headers: Some(configured_headers),
            ..Default::default()
        },
    )
    .await;

    fixture.list_tools().await.expect("listTools");

    let test_texts = [
        "Hello, \u{4e16}\u{754c}",
        " padded ",
        "line\nbreak",
        "=?base64?literal?=",
        "a\tb",
        "",
    ];

    for text in test_texts {
        fixture
            .call_tool(
                "echo",
                Some(json!({
                    "text": text,
                    "count": -42,
                    "flag": true,
                    "private": "not-declared",
                    "nested": { "private": "not-declared" }
                })),
            )
            .await
            .expect("callTool");

        let requests = fixture.requests.lock().unwrap().clone();
        let wire = requests.last().expect("last request");

        let expected = if text == "a\tb" || text.is_empty() {
            text.to_string()
        } else {
            format!(
                "=?base64?{}?=",
                base64::engine::general_purpose::STANDARD.encode(text.as_bytes())
            )
        };

        assert_eq!(
            wire.headers.get("mcp-param-text").map(String::as_str),
            Some(expected.as_str())
        );
        assert_eq!(
            wire.headers.get("mcp-param-count").map(String::as_str),
            Some("-42")
        );
        assert_eq!(
            wire.headers.get("mcp-param-flag").map(String::as_str),
            Some("true")
        );

        let mut param_keys: Vec<String> = wire
            .headers
            .keys()
            .filter(|key| key.starts_with("mcp-param-"))
            .cloned()
            .collect();
        param_keys.sort();

        assert_eq!(
            param_keys,
            vec!["mcp-param-count", "mcp-param-flag", "mcp-param-text"]
        );
    }

    fixture.close().await;
}

/// Port of senpi test/mcp/4192-parameter-headers.test.ts:
/// "projects current annotations before clearing them on a catalog refresh"
#[tokio::test]
async fn projects_current_annotations_before_clearing_them_on_a_catalog_refresh() {
    let tools = vec![json!({
        "name": "echo",
        "inputSchema": {
            "type": "object",
            "properties": {
                "text": { "type": "string", "x-mcp-header": "Text" }
            }
        }
    })];

    let fixture = open_header_fixture(tools, HeaderFixtureOptions::default()).await;
    fixture.list_tools().await.expect("listTools");
    fixture
        .call_tool("echo", Some(json!({ "text": "current" })))
        .await
        .expect("callTool");

    let last_wire = fixture.requests.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        last_wire.headers.get("mcp-param-text").map(String::as_str),
        Some("current")
    );

    // Refresh catalog with empty tools
    fixture.tools.lock().unwrap().clear();
    fixture.list_tools().await.expect("listTools refresh");
    fixture
        .call_tool("echo", Some(json!({ "text": "not-current" })))
        .await
        .expect("callTool after clear");

    let after_wire = fixture.requests.lock().unwrap().last().unwrap().clone();
    assert!(after_wire.headers.get("mcp-param-text").is_none());

    fixture.close().await;
}

/// Port of senpi test/mcp/4192-parameter-headers.test.ts:
/// "rejects a present argument that is not a valid integer" ("5")
#[tokio::test]
async fn rejects_a_present_argument_that_is_not_a_valid_integer_string() {
    assert_rejects_invalid_parameter_argument(
        "integer",
        json!("5"),
        "a safe integer",
        json!(5),
        "5",
    )
    .await;
}

/// Port of senpi test/mcp/4192-parameter-headers.test.ts:
/// "rejects a present argument that is not a valid integer" (2 ** 53)
#[tokio::test]
async fn rejects_a_present_argument_that_is_not_a_valid_integer_large() {
    assert_rejects_invalid_parameter_argument(
        "integer",
        json!(9_007_199_254_740_992.0),
        "a safe integer",
        json!(5),
        "5",
    )
    .await;
}

/// Port of senpi test/mcp/4192-parameter-headers.test.ts:
/// "rejects a present argument that is not a valid integer" (1.5)
#[tokio::test]
async fn rejects_a_present_argument_that_is_not_a_valid_integer_float() {
    assert_rejects_invalid_parameter_argument(
        "integer",
        json!(1.5),
        "a safe integer",
        json!(5),
        "5",
    )
    .await;
}

/// Port of senpi test/mcp/4192-parameter-headers.test.ts:
/// "rejects a present argument that is not a valid boolean" ("false")
#[tokio::test]
async fn rejects_a_present_argument_that_is_not_a_valid_boolean() {
    assert_rejects_invalid_parameter_argument(
        "boolean",
        json!("false"),
        "a boolean",
        json!(false),
        "false",
    )
    .await;
}

/// Port of senpi test/mcp/4192-parameter-headers.test.ts:
/// "rejects a present argument that is not a valid string" (null)
#[tokio::test]
async fn rejects_a_present_argument_that_is_not_a_valid_string() {
    assert_rejects_invalid_parameter_argument("string", Value::Null, "a string", json!("ok"), "ok")
        .await;
}

async fn assert_rejects_invalid_parameter_argument(
    param_type: &str,
    invalid_val: Value,
    expected_type_label: &str,
    valid_val: Value,
    expected_valid_header: &str,
) {
    let tools = vec![json!({
        "name": "echo",
        "inputSchema": {
            "type": "object",
            "properties": {
                "nested": {
                    "type": "object",
                    "properties": {
                        "value": { "type": param_type, "x-mcp-header": "Value" }
                    }
                }
            }
        }
    })];

    let fixture = open_header_fixture(tools, HeaderFixtureOptions::default()).await;
    fixture.list_tools().await.expect("listTools");

    let before = fixture.requests.lock().unwrap().len();
    let logs_before = fixture.logger.lock().unwrap().get_ring_buffer();

    let err = fixture
        .call_tool("echo", Some(json!({ "nested": { "value": invalid_val } })))
        .await
        .expect_err("callTool must fail locally");

    let expected_message =
        format!("MCP tool \"echo\" argument [\"nested\",\"value\"] must be {expected_type_label}");
    assert_eq!(err.kind, McpErrorKind::ToolExec);
    assert!(
        err.message.contains(&expected_message),
        "expected error message to contain '{expected_message}', got '{}'",
        err.message
    );
    assert_eq!(fixture.requests.lock().unwrap().len(), before);

    // A local validation failure must not break the connection or log argument values.
    fixture
        .call_tool("echo", Some(json!({ "nested": { "value": valid_val } })))
        .await
        .expect("subsequent valid call succeeds");

    let last_wire = fixture.requests.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        last_wire.headers.get("mcp-param-value").map(String::as_str),
        Some(expected_valid_header)
    );
    assert_eq!(
        fixture.logger.lock().unwrap().get_ring_buffer(),
        logs_before
    );

    fixture.close().await;
}

/// Port of senpi test/mcp/4192-parameter-headers.test.ts:
/// "leaves legacy HTTP catalogs and parameter-header behavior unchanged"
#[tokio::test]
async fn leaves_legacy_http_catalogs_and_parameter_header_behavior_unchanged() {
    let tools = vec![
        json!({
            "name": "echo",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string", "x-mcp-header": "Text" }
                }
            }
        }),
        json!({
            "name": "bad",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "number", "x-mcp-header": "Text" }
                }
            }
        }),
    ];

    let fixture = open_header_fixture(
        tools,
        HeaderFixtureOptions {
            legacy: true,
            ..Default::default()
        },
    )
    .await;

    let result = fixture.list_tools().await.expect("listTools");
    let names: Vec<&str> = result["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();
    assert_eq!(names, vec!["echo", "bad"]);

    fixture
        .call_tool("echo", Some(json!({ "text": "not-projected" })))
        .await
        .expect("callTool");

    let last_wire = fixture.requests.lock().unwrap().last().unwrap().clone();
    assert!(last_wire.headers.get("mcp-param-text").is_none());

    fixture.close().await;
}

#[test]
fn unit_clear_mcp_parameter_headers_strips_parameter_headers() {
    let mut headers = BTreeMap::new();
    headers.insert("Content-Type".into(), "application/json".into());
    headers.insert("Mcp-Param-Text".into(), "projected".into());
    headers.insert("mcp-param-flag".into(), "true".into());
    headers.insert("Authorization".into(), "Bearer token".into());

    clear_mcp_parameter_headers(&mut headers);

    assert_eq!(headers.len(), 2);
    assert_eq!(
        headers.get("Content-Type").map(String::as_str),
        Some("application/json")
    );
    assert_eq!(
        headers.get("Authorization").map(String::as_str),
        Some("Bearer token")
    );
    assert!(!headers.contains_key("Mcp-Param-Text"));
    assert!(!headers.contains_key("mcp-param-flag"));
}

#[test]
fn unit_filter_tools_clears_catalog_on_first_page() {
    let filter = ModernParameterHeaders::default();
    let warnings = Arc::new(Mutex::new(Vec::new()));
    let warn_fn = {
        let warnings = warnings.clone();
        move |msg: &str| warnings.lock().unwrap().push(msg.to_string())
    };

    let tools1 = vec![json!({
        "name": "tool1",
        "inputSchema": {
            "type": "object",
            "properties": {
                "val": { "type": "string", "x-mcp-header": "Val" }
            }
        }
    })];
    let kept1 = filter.filter_tools(&tools1, true, &warn_fn);
    assert_eq!(kept1.len(), 1);

    let mut headers = BTreeMap::new();
    filter
        .append(
            &mut headers,
            Some(&json!("tool1")),
            Some(&json!({"val": "first"})),
        )
        .unwrap();
    assert_eq!(
        headers.get("Mcp-Param-Val").map(String::as_str),
        Some("first")
    );

    // Second page with first_page = false retains tool1 and adds tool2
    let tools2 = vec![json!({
        "name": "tool2",
        "inputSchema": {
            "type": "object",
            "properties": {
                "other": { "type": "boolean", "x-mcp-header": "Other" }
            }
        }
    })];
    let kept2 = filter.filter_tools(&tools2, false, &warn_fn);
    assert_eq!(kept2.len(), 1);

    let mut headers2 = BTreeMap::new();
    filter
        .append(
            &mut headers2,
            Some(&json!("tool1")),
            Some(&json!({"val": "first"})),
        )
        .unwrap();
    assert_eq!(
        headers2.get("Mcp-Param-Val").map(String::as_str),
        Some("first")
    );

    // Fresh catalog fetch with first_page = true clears previous tools
    let empty_tools: Vec<Value> = vec![];
    let kept3 = filter.filter_tools(&empty_tools, true, &warn_fn);
    assert!(kept3.is_empty());

    let mut headers3 = BTreeMap::new();
    filter
        .append(
            &mut headers3,
            Some(&json!("tool1")),
            Some(&json!({"val": "first"})),
        )
        .unwrap();
    assert!(headers3.is_empty());
}

#[test]
fn unit_encode_mcp_header_value_rules() {
    // Visible ASCII without boundary whitespace is preserved verbatim
    assert_eq!(encode_mcp_header_value("hello-world"), "hello-world");
    assert_eq!(encode_mcp_header_value("a\tb"), "a\tb");
    assert_eq!(encode_mcp_header_value(""), "");

    // Padded whitespace triggers base64 encoding
    assert_eq!(
        encode_mcp_header_value(" padded "),
        "=?base64?IHBhZGRlZCA=?="
    );

    // Non-ASCII triggers base64 encoding
    assert_eq!(
        encode_mcp_header_value("snowman-\u{2603}"),
        format!(
            "=?base64?{}?=",
            base64::engine::general_purpose::STANDARD.encode("snowman-\u{2603}".as_bytes())
        )
    );

    // Literal sentinel strings are base64 encoded
    assert_eq!(
        encode_mcp_header_value("=?base64?literal?="),
        format!(
            "=?base64?{}?=",
            base64::engine::general_purpose::STANDARD.encode("=?base64?literal?=".as_bytes())
        )
    );
}
