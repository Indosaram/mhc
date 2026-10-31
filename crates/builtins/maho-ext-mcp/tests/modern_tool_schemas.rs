//! Port of senpi test/mcp/4191-modern-tool-schemas.test.ts (senpi#4191): modern (2026-07-28) tool schemas
//! through discovery, exposure and execution. The peer is a loopback stub that mirrors
//! fixtures/mrtr-2026-server.ts (`MrtrScript`); `connect` mirrors fixtures/mrtr-connection.ts over HTTP.

use axum::{Json, Router, routing::post};
use maho_ext_api::{AbortSignal, ToolCall, ToolContent, ToolDefinition};
use maho_ext_mcp::{
    catalog::{McpToolCatalogEntry, collect_tool_catalog},
    config_schema::{Auth, McpServerConfig, Transport},
    connection::ServerConnection,
    expose::register::build_mcp_tool_definitions,
    guard::output_guard::McpOutputArtifacts,
    log::McpLogger,
};
use serde_json::{Value, json};
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

/// fixtures/mrtr-connection.ts `connect(results, false, false)` with `script.tools = tools`.
async fn connect(results: Vec<Value>, tools: Vec<Value>) -> Peer {
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

struct Fixture {
    peer: Peer,
    entries: Vec<McpToolCatalogEntry>,
    tool: ToolDefinition,
}

impl Fixture {
    async fn execute(&self, args: Value) -> maho_ext_api::ToolResult {
        (self.tool.execute)(ToolCall {
            id: "call",
            params: args,
            signal: AbortSignal::default(),
            on_update: None,
            context: None,
        })
        .await
        .expect("execute")
    }
}

/// The test file's `fixture(inputSchema, outputSchema?, structuredContent?)`.
async fn fixture(
    input_schema: Value,
    output_schema: Option<Value>,
    structured_content: Option<Value>,
) -> Fixture {
    let mut result = json!({"resultType": "complete", "content": []});
    if let Some(structured) = structured_content {
        result["structuredContent"] = structured;
    }
    let mut tool = json!({"name": "echo", "inputSchema": input_schema});
    if let Some(output) = output_schema {
        tool["outputSchema"] = output;
    }
    let peer = connect(vec![result], vec![tool]).await;
    let client = peer.connection.client().expect("client");
    let entries = collect_tool_catalog("fixture", client, TIMEOUT)
        .await
        .expect("collectToolCatalog");
    let tool = build_mcp_tool_definitions(
        &entries,
        peer.root.path().into(),
        Arc::new(McpOutputArtifacts::default()),
        None,
    )
    .into_iter()
    .next()
    .expect("Expected echo tool");
    assert!(!entries.is_empty(), "Expected echo tool");
    Fixture {
        peer,
        entries,
        tool,
    }
}

/// pi-ai `validateToolArguments(tool, toolCall)` against the exposed definition.
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

/// 4191-modern-tool-schemas.test.ts: "accepts $label through discovery and exposure" (boolean inputSchema)
#[tokio::test]
async fn accepts_boolean_input_schema_through_discovery_and_exposure() {
    let input_schema = json!(true);
    let fixture = fixture(input_schema.clone(), None, None).await;
    assert_eq!(fixture.entries[0].schema, input_schema);
    fixture.peer.close().await;
}

/// 4191-modern-tool-schemas.test.ts: "accepts $label through discovery and exposure" (array outputSchema)
#[tokio::test]
async fn accepts_array_output_schema_through_discovery_and_exposure() {
    let input_schema = json!({"type": "object"});
    let fixture = fixture(
        input_schema.clone(),
        Some(json!({"type": "array", "items": {"type": "string"}})),
        None,
    )
    .await;
    assert_eq!(fixture.entries[0].schema, input_schema);
    fixture.peer.close().await;
}

async fn delivers_structured_content(value: Value) {
    let fixture = fixture(json!({"type": "object"}), None, Some(value.clone())).await;
    let result = fixture.execute(json!({})).await;
    assert_eq!(result.content, [ToolContent::text(value.to_string())]);
    fixture.peer.close().await;
}

/// 4191-modern-tool-schemas.test.ts: "delivers non-object structuredContent $value to the model" (null)
#[tokio::test]
async fn delivers_non_object_structured_content_null_to_the_model() {
    delivers_structured_content(Value::Null).await;
}

/// 4191-modern-tool-schemas.test.ts: "delivers non-object structuredContent $value to the model" (false)
#[tokio::test]
async fn delivers_non_object_structured_content_false_to_the_model() {
    delivers_structured_content(json!(false)).await;
}

/// 4191-modern-tool-schemas.test.ts: "delivers non-object structuredContent $value to the model" (0)
#[tokio::test]
async fn delivers_non_object_structured_content_zero_to_the_model() {
    delivers_structured_content(json!(0)).await;
}

/// 4191-modern-tool-schemas.test.ts: "delivers non-object structuredContent $value to the model" ("text")
#[tokio::test]
async fn delivers_non_object_structured_content_text_to_the_model() {
    delivers_structured_content(json!("text")).await;
}

/// 4191-modern-tool-schemas.test.ts: "delivers non-object structuredContent $value to the model" ([1, 2])
#[tokio::test]
async fn delivers_non_object_structured_content_array_to_the_model() {
    delivers_structured_content(json!([1, 2])).await;
}

/// 4191-modern-tool-schemas.test.ts: "retains additionalProperties and ref sibling constraints in the exposed
/// schema"
#[tokio::test]
async fn retains_additional_properties_and_ref_sibling_constraints_in_the_exposed_schema() {
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "$defs": {"count": {"type": "integer", "minimum": 0}},
        "properties": {"count": {"$ref": "#/$defs/count", "maximum": 5}},
    });
    let fixture = fixture(schema.clone(), None, None).await;
    assert_eq!(fixture.tool.parameters, schema);
    fixture.peer.close().await;
}

/// 4191-modern-tool-schemas.test.ts: "sends exactly the model arguments after validation and exposure"
#[tokio::test]
async fn sends_exactly_the_model_arguments_after_validation_and_exposure() {
    let fixture = fixture(
        json!({
            "type": "object",
            "properties": {
                "value": {"anyOf": [{"type": "integer"}, {"type": "string"}]},
                "optionalCount": {"type": "integer"},
            },
            "required": ["value"],
        }),
        None,
        None,
    )
    .await;
    let arguments_from_model = json!({"value": "007", "extra": {"nested": [false, null, 0]}});
    let args = validate_tool_arguments(&fixture.tool, &arguments_from_model).expect("valid");
    fixture.execute(args).await;
    assert_eq!(
        fixture.peer.requests.lock().unwrap()[0]["params"]["arguments"],
        arguments_from_model
    );
    assert_eq!(arguments_from_model["value"], "007");
    // The legacy path silently removes optional nulls and dispatches the repaired call.
    let mut invalid = arguments_from_model.clone();
    invalid["optionalCount"] = Value::Null;
    assert!(validate_tool_arguments(&fixture.tool, &invalid).is_err());
    assert_eq!(fixture.peer.requests.lock().unwrap().len(), 1);
    fixture.peer.close().await;
}
