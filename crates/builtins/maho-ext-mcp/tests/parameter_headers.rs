//! Port of senpi test/mcp/4192-parameter-headers.test.ts (#4192, #4246): declared `x-mcp-header` tool
//! parameters on the modern HTTP transport, driven through the real `ServerConnection` against a loopback
//! stub that mirrors fixtures/header-http-server.ts.

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
    errors::McpError,
    log::McpLogger,
    request_origin::McpRequestOptions,
    tool_client::list_mcp_tools,
    transport_sdk::McpClient,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct Wire {
    message: Value,
    headers: BTreeMap<String, String>,
}

#[derive(Default)]
struct Options {
    legacy: bool,
    sse: bool,
    headers: Option<BTreeMap<String, String>>,
}

/// fixtures/header-http-server.ts `openHeaderFixture`.
struct Fixture {
    client: Arc<McpClient>,
    connection: Arc<ServerConnection>,
    requests: Arc<Mutex<Vec<Wire>>>,
    tools: Arc<Mutex<Vec<Value>>>,
    logger: Arc<Mutex<McpLogger>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
    _root: tempfile::TempDir,
}

impl Fixture {
    async fn list_tools(&self) -> Vec<String> {
        let result = list_mcp_tools(
            &self.client,
            json!({}),
            TIMEOUT,
            &McpRequestOptions::default(),
        )
        .await
        .expect("listTools");
        result["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|tool| tool["name"].as_str().expect("name").to_owned())
            .collect()
    }
    async fn call_tool(&self, name: &str, arguments: Option<Value>) -> Result<Value, McpError> {
        let mut params = json!({"name": name});
        if let Some(arguments) = arguments {
            params["arguments"] = arguments;
        }
        self.client.request("tools/call", params, TIMEOUT).await
    }
    fn requests(&self) -> Vec<Wire> {
        self.requests.lock().unwrap().clone()
    }
    fn last(&self) -> Wire {
        self.requests().last().expect("a request").clone()
    }
    fn ring(&self) -> Vec<String> {
        self.logger.lock().unwrap().get_ring_buffer()
    }
    async fn close(mut self) {
        self.connection.dispose().await.expect("dispose");
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        tokio::time::timeout(TIMEOUT, self.task)
            .await
            .expect("stub stops in time")
            .expect("stub task");
    }
}

async fn open_header_fixture(tools: Vec<Value>, options: Options) -> Fixture {
    let requests = Arc::new(Mutex::new(Vec::<Wire>::new()));
    let tools = Arc::new(Mutex::new(tools));
    let (seen, listed, legacy, sse) =
        (requests.clone(), tools.clone(), options.legacy, options.sse);
    let app = Router::new().route(
        "/mcp",
        post(move |headers: HeaderMap, Json(message): Json<Value>| {
            let (seen, listed) = (seen.clone(), listed.clone());
            async move {
                let headers = headers
                    .iter()
                    .filter_map(|(name, value)| {
                        Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned()))
                    })
                    .collect();
                seen.lock().unwrap().push(Wire {
                    message: message.clone(),
                    headers,
                });
                let method = message["method"].as_str().unwrap_or_default();
                if method == "server/discover" && legacy {
                    let body = json!({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32601,"message":"Unknown"}});
                    return (StatusCode::BAD_REQUEST, [(CONTENT_TYPE, "application/json")], body.to_string()).into_response();
                }
                if method == "notifications/initialized" {
                    return StatusCode::ACCEPTED.into_response();
                }
                let result = match method {
                    "server/discover" => json!({
                        "resultType": "complete",
                        "supportedVersions": ["2026-07-28"],
                        "capabilities": {"tools": {}},
                        "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "synthetic-peer", "version": "1"}},
                    }),
                    "initialize" => json!({
                        "protocolVersion": "2025-11-25",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "legacy", "version": "1"},
                    }),
                    "tools/list" => json!({"tools": *listed.lock().unwrap()}),
                    _ => json!({"content": []}),
                };
                let reply = json!({"jsonrpc": "2.0", "id": message["id"], "result": result});
                if sse {
                    (StatusCode::OK, [(CONTENT_TYPE, "text/event-stream")], format!("data: {reply}\n\n")).into_response()
                } else {
                    (StatusCode::OK, [(CONTENT_TYPE, "application/json")], reply.to_string()).into_response()
                }
            }
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
    let root = tempfile::tempdir().expect("log dir");
    let logger = Arc::new(Mutex::new(
        McpLogger::new("headers", root.path(), None).expect("logger"),
    ));
    let connection = ServerConnection::new(
        "configured-fixture",
        McpServerConfig {
            enabled: Some(true),
            transport: Some(Transport::Http),
            url: Some(format!("http://{address}/mcp")),
            auth: Some(Auth::Disabled(false)),
            headers: options.headers,
            connect_timeout_ms: Some(5000.0),
            ..Default::default()
        },
        None,
        logger.clone(),
    );
    let client = connection.connect().await.expect("connect");
    Fixture {
        client,
        connection,
        requests,
        tools,
        logger,
        stop: Some(stop),
        task,
        _root: root,
    }
}

fn base64_sentinel(value: &str) -> String {
    format!(
        "=?base64?{}?=",
        base64::engine::general_purpose::STANDARD.encode(value.as_bytes())
    )
}

fn header<'a>(wire: &'a Wire, name: &str) -> Option<&'a str> {
    wire.headers.get(name).map(String::as_str)
}

/// 4192-parameter-headers.test.ts: "mirrors annotated arguments at their exact nested paths"
#[tokio::test]
async fn mirrors_annotated_arguments_at_their_exact_nested_paths() {
    let fixture = open_header_fixture(
        vec![json!({
            "name": "echo",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "region": {"type": "string", "x-mcp-header": "Region"},
                    "nested": {"type": "object", "properties": {"flag": {"type": "boolean", "x-mcp-header": "Flag"}}},
                },
            },
        })],
        Options::default(),
    )
    .await;
    fixture.list_tools().await;
    fixture
        .call_tool(
            "echo",
            Some(json!({"region": "us-west1", "nested": {"flag": false}})),
        )
        .await
        .expect("callTool");
    let requests = fixture.requests();
    let wire = requests
        .iter()
        .find(|wire| wire.message["method"] == "tools/call")
        .expect("tools/call on the wire");
    assert_eq!(header(wire, "mcp-name"), Some("echo"));
    assert_eq!(header(wire, "mcp-param-region"), Some("us-west1"));
    assert_eq!(header(wire, "mcp-param-flag"), Some("false"));
    fixture.close().await;
}

async fn encodes_mcp_name(name: &str) {
    let fixture = open_header_fixture(
        vec![json!({"name": name, "inputSchema": {"type": "object"}})],
        Options::default(),
    )
    .await;
    fixture.list_tools().await;
    fixture.call_tool(name, None).await.expect("callTool");
    let requests = fixture.requests();
    let wire = requests
        .iter()
        .find(|wire| wire.message["method"] == "tools/call")
        .expect("tools/call on the wire");
    assert_eq!(
        header(wire, "mcp-name"),
        Some(base64_sentinel(name).as_str()),
        "{name:?}"
    );
    fixture.close().await;
}

/// 4192-parameter-headers.test.ts: "encodes Mcp-Name with standard Base64 and the spec sentinel for %j"
/// ("snowman-\u2603")
#[tokio::test]
async fn encodes_mcp_name_with_standard_base64_and_the_spec_sentinel_for_snowman() {
    encodes_mcp_name("snowman-\u{2603}").await;
}

/// 4192-parameter-headers.test.ts: "encodes Mcp-Name with standard Base64 and the spec sentinel for %j"
/// (" padded ")
#[tokio::test]
async fn encodes_mcp_name_with_standard_base64_and_the_spec_sentinel_for_padded() {
    encodes_mcp_name(" padded ").await;
}

/// 4192-parameter-headers.test.ts: "encodes Mcp-Name with standard Base64 and the spec sentinel for %j"
/// ("=?base64?literal?=")
#[tokio::test]
async fn encodes_mcp_name_with_standard_base64_and_the_spec_sentinel_for_literal_sentinel() {
    encodes_mcp_name("=?base64?literal?=").await;
}

/// 4192-parameter-headers.test.ts: "excludes an invalid number annotation without losing valid tools"
#[tokio::test]
async fn excludes_an_invalid_number_annotation_without_losing_valid_tools() {
    let fixture = open_header_fixture(
        vec![
            json!({"name": "good", "inputSchema": {"type": "object"}}),
            json!({
                "name": "bad",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "value": {"type": "number", "x-mcp-header": "Value", "default": "private-schema-value"},
                        "other": {"type": "number", "x-mcp-header": "Other"},
                    },
                },
            }),
        ],
        Options::default(),
    )
    .await;
    assert_eq!(fixture.list_tools().await, ["good"]);
    // #4246: one warning per rejected tool, with a fixed reason, never schema values.
    let warnings: Vec<Value> = fixture
        .ring()
        .iter()
        .map(|line| serde_json::from_str::<Value>(line).expect("json log line"))
        .filter(|entry| entry["level"] == "warning")
        .collect();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let message = warnings[0]["message"].as_str().expect("message");
    assert!(message.contains("bad"), "{message}");
    assert!(
        message.contains("type must be string, integer, or boolean"),
        "{message}"
    );
    assert!(
        !Value::from(warnings)
            .to_string()
            .contains("private-schema-value")
    );
    fixture.close().await;
}

/// 4192-parameter-headers.test.ts: "rejects invalid header definitions in schema %j" (all 15 `it.each` rows,
/// each against its own fixture).
#[tokio::test]
async fn rejects_invalid_header_definitions_in_schema() {
    let cases = [
        json!({"properties": {"value": {"type": "string", "x-mcp-header": ""}}}),
        json!({"properties": {"value": {"type": "string", "x-mcp-header": "bad name"}}}),
        json!({"properties": {"value": {"type": "string", "x-mcp-header": "bad\r\nname"}}}),
        json!({"properties": {"value": {"type": "string", "x-mcp-header": 1}}}),
        json!({"properties": {"value": {"type": ["string", "null"], "x-mcp-header": "Value"}}}),
        json!({"properties": {"first": {"type": "string", "x-mcp-header": "A"}, "second": {"type": "boolean", "x-mcp-header": "a"}}}),
        json!({"type": "string", "x-mcp-header": "Root"}),
        json!({"properties": {"values": {"type": "array", "items": {"type": "string", "x-mcp-header": "Value"}}}}),
        json!({"allOf": [{"properties": {"value": {"type": "string", "x-mcp-header": "Value"}}}]}),
        json!({"if": {"properties": {"value": {"type": "string", "x-mcp-header": "Value"}}}}),
        json!({"$defs": {"value": {"type": "string", "x-mcp-header": "Value"}}, "properties": {"value": {"$ref": "#/$defs/value"}}}),
        json!({"additionalProperties": {"type": "string", "x-mcp-header": "Value"}}),
        // #4246: "anywhere else" includes unknown/vendor keyword objects and arrays.
        json!({"x-vendor": {"type": "string", "x-mcp-header": "Value"}}),
        json!({"x-vendor": [{"properties": {"value": {"type": "string", "x-mcp-header": "Value"}}}]}),
        json!({"unknown-keyword": {"nested": {"type": "string", "x-mcp-header": "Value"}}}),
    ];
    for case in cases {
        // `{ type: "object", ...inputSchema }`: the row's keys override the object type.
        let mut schema = json!({"type": "object"});
        for (key, value) in case.as_object().expect("row") {
            schema[key] = value.clone();
        }
        let fixture = open_header_fixture(
            vec![
                json!({"name": "bad", "inputSchema": schema}),
                json!({"name": "good", "inputSchema": {"type": "object"}}),
            ],
            Options::default(),
        )
        .await;
        assert_eq!(fixture.list_tools().await, ["good"], "{case}");
        fixture.close().await;
    }
}

async fn encodes_primitive_values(sse: bool) {
    let fixture = open_header_fixture(
        vec![json!({
            "name": "echo",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": {"type": "string", "x-mcp-header": "Text"},
                    "count": {"type": "integer", "x-mcp-header": "Count"},
                    "flag": {"type": "boolean", "x-mcp-header": "Flag"},
                    "missing": {"type": "string", "x-mcp-header": "Missing"},
                    "nested": {"type": "object", "properties": {"private": {"type": "string"}}},
                },
                "examples": [{"x-mcp-header": "this is instance data"}],
                "default": {"x-mcp-header": "this is instance data"},
                "const": {"x-mcp-header": "this is instance data"},
                "enum": [{"x-mcp-header": "this is instance data"}],
            },
        })],
        Options {
            sse,
            headers: Some(BTreeMap::from([
                (
                    "Mcp-Param-Missing".to_owned(),
                    "must-not-project".to_owned(),
                ),
                ("Mcp-Param-Extra".to_owned(), "must-not-project".to_owned()),
            ])),
            ..Default::default()
        },
    )
    .await;
    fixture.list_tools().await;
    for text in [
        "Hello, \u{4e16}\u{754c}",
        " padded ",
        "line\nbreak",
        "=?base64?literal?=",
        "a\tb",
        "",
    ] {
        fixture
            .call_tool(
                "echo",
                Some(json!({
                    "text": text,
                    "count": -42,
                    "flag": true,
                    "private": "not-declared",
                    "nested": {"private": "not-declared"},
                })),
            )
            .await
            .expect("callTool");
        let wire = fixture.last();
        let expected = if ["a\tb", ""].contains(&text) {
            text.to_owned()
        } else {
            base64_sentinel(text)
        };
        assert_eq!(
            header(&wire, "mcp-param-text"),
            Some(expected.as_str()),
            "{text:?}"
        );
        assert_eq!(header(&wire, "mcp-param-count"), Some("-42"));
        assert_eq!(header(&wire, "mcp-param-flag"), Some("true"));
        let projected: Vec<&str> = wire
            .headers
            .keys()
            .map(String::as_str)
            .filter(|key| key.starts_with("mcp-param-"))
            .collect();
        assert_eq!(
            projected,
            ["mcp-param-count", "mcp-param-flag", "mcp-param-text"]
        );
    }
    fixture.close().await;
}

/// 4192-parameter-headers.test.ts: "encodes primitive parameter-header values and omits undeclared and
/// absent arguments (SSE %j)" (false)
#[tokio::test]
async fn encodes_primitive_parameter_header_values_and_omits_undeclared_and_absent_arguments_sse_false()
 {
    encodes_primitive_values(false).await;
}

/// 4192-parameter-headers.test.ts: "encodes primitive parameter-header values and omits undeclared and
/// absent arguments (SSE %j)" (true)
#[tokio::test]
async fn encodes_primitive_parameter_header_values_and_omits_undeclared_and_absent_arguments_sse_true()
 {
    encodes_primitive_values(true).await;
}

/// 4192-parameter-headers.test.ts: "projects current annotations before clearing them on a catalog refresh"
#[tokio::test]
async fn projects_current_annotations_before_clearing_them_on_a_catalog_refresh() {
    let fixture = open_header_fixture(
        vec![json!({
            "name": "echo",
            "inputSchema": {"type": "object", "properties": {"text": {"type": "string", "x-mcp-header": "Text"}}},
        })],
        Options::default(),
    )
    .await;
    fixture.list_tools().await;
    fixture
        .call_tool("echo", Some(json!({"text": "current"})))
        .await
        .expect("callTool");
    assert_eq!(header(&fixture.last(), "mcp-param-text"), Some("current"));
    fixture.tools.lock().unwrap().clear();
    fixture.list_tools().await;
    fixture
        .call_tool("echo", Some(json!({"text": "not-current"})))
        .await
        .expect("callTool");
    assert_eq!(header(&fixture.last(), "mcp-param-text"), None);
    fixture.close().await;
}

async fn rejects_invalid_argument(kind: &str, value: Value) {
    let fixture = open_header_fixture(
        vec![json!({
            "name": "echo",
            "inputSchema": {
                "type": "object",
                "properties": {"nested": {"type": "object", "properties": {"value": {"type": kind, "x-mcp-header": "Value"}}}},
            },
        })],
        Options::default(),
    )
    .await;
    fixture.list_tools().await;
    let before = fixture.requests().len();
    let logs_before = fixture.ring();
    let error = fixture
        .call_tool("echo", Some(json!({"nested": {"value": value}})))
        .await
        .expect_err("local validation failure");
    let expected = if kind == "integer" {
        "a safe integer".to_owned()
    } else {
        format!("a {kind}")
    };
    assert_eq!(
        error.message,
        format!("MCP tool \"echo\" argument [\"nested\",\"value\"] must be {expected}")
    );
    assert_eq!(fixture.requests().len(), before);
    // A local validation failure must not break the connection or log argument values.
    let (valid, projected) = match kind {
        "string" => (json!("ok"), "ok"),
        "boolean" => (json!(false), "false"),
        _ => (json!(5), "5"),
    };
    fixture
        .call_tool("echo", Some(json!({"nested": {"value": valid}})))
        .await
        .expect("connection still usable");
    assert_eq!(header(&fixture.last(), "mcp-param-value"), Some(projected));
    assert_eq!(fixture.ring(), logs_before);
    fixture.close().await;
}

/// 4192-parameter-headers.test.ts: "rejects a present argument that is not a valid %s" (["integer", "5"])
#[tokio::test]
async fn rejects_a_present_argument_that_is_not_a_valid_integer_string_5() {
    rejects_invalid_argument("integer", json!("5")).await;
}

/// 4192-parameter-headers.test.ts: "rejects a present argument that is not a valid %s" (["integer", 2 ** 53])
#[tokio::test]
async fn rejects_a_present_argument_that_is_not_a_valid_integer_2_pow_53() {
    rejects_invalid_argument("integer", json!(9_007_199_254_740_992u64)).await;
}

/// 4192-parameter-headers.test.ts: "rejects a present argument that is not a valid %s" (["integer", 1.5])
#[tokio::test]
async fn rejects_a_present_argument_that_is_not_a_valid_integer_1_5() {
    rejects_invalid_argument("integer", json!(1.5)).await;
}

/// 4192-parameter-headers.test.ts: "rejects a present argument that is not a valid %s" (["boolean", "false"])
#[tokio::test]
async fn rejects_a_present_argument_that_is_not_a_valid_boolean_string_false() {
    rejects_invalid_argument("boolean", json!("false")).await;
}

/// 4192-parameter-headers.test.ts: "rejects a present argument that is not a valid %s" (["string", null])
#[tokio::test]
async fn rejects_a_present_argument_that_is_not_a_valid_string_null() {
    rejects_invalid_argument("string", Value::Null).await;
}

/// 4192-parameter-headers.test.ts: "leaves legacy HTTP catalogs and parameter-header behavior unchanged"
#[tokio::test]
async fn leaves_legacy_http_catalogs_and_parameter_header_behavior_unchanged() {
    let fixture = open_header_fixture(
        vec![
            json!({"name": "echo", "inputSchema": {"type": "object", "properties": {"text": {"type": "string", "x-mcp-header": "Text"}}}}),
            json!({"name": "bad", "inputSchema": {"type": "object", "properties": {"text": {"type": "number", "x-mcp-header": "Text"}}}}),
        ],
        Options {
            legacy: true,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(fixture.list_tools().await, ["echo", "bad"]);
    fixture
        .call_tool("echo", Some(json!({"text": "not-projected"})))
        .await
        .expect("callTool");
    assert_eq!(header(&fixture.last(), "mcp-param-text"), None);
    fixture.close().await;
}
