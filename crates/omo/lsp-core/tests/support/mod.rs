#![allow(dead_code)]

use lsp_core::Diagnostic;
use lsp_core::LspClient;
use lsp_core::LspClientOptions;
use lsp_core::LspClientTimeoutOptions;
use lsp_core::LspClientTransport;
use lsp_core::ResolvedServer;
use lsp_core::file_uri;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;
use tempfile::TempDir;

pub struct WorkspaceEditTestContext {
    pub client: LspClient,
    pub workspace: String,
    pub source: String,
    pub destination: String,
    pub closed: String,
    pub created: String,
    pub events: String,
    pub server: ResolvedServer,
    _dir: TempDir,
}

impl WorkspaceEditTestContext {
    pub async fn stop(&self) {
        self.client.stop().await;
    }
}

pub fn read(path: &str) -> String {
    std::fs::read_to_string(path).expect("read file")
}

pub fn canonical_uri(path: &str) -> String {
    let real = std::fs::canonicalize(path).expect("canonicalize");
    file_uri(&real.to_string_lossy())
}

pub fn default_options() -> LspClientOptions {
    LspClientOptions {
        timeouts: LspClientTimeoutOptions {
            request_timeout_ms: Some(5_000),
            initialize_timeout_ms: Some(5_000),
        },
        ..LspClientOptions::default()
    }
}

pub async fn make_client(scenario: Value) -> WorkspaceEditTestContext {
    make_client_with(scenario, default_options()).await
}

pub async fn make_client_with(
    scenario: Value,
    options: LspClientOptions,
) -> WorkspaceEditTestContext {
    let dir = tempfile::Builder::new()
        .prefix("lsp-apply-edit-")
        .tempdir()
        .expect("tempdir");
    let workspace = dir.path().to_string_lossy().into_owned();
    let join = |name: &str| dir.path().join(name).to_string_lossy().into_owned();
    let (source, destination, closed, created) = (
        join("source.ts"),
        join("destination.ts"),
        join("closed.ts"),
        join("created.ts"),
    );
    let scenario_path = join("scenario.json");
    let events = join("events.jsonl");
    std::fs::write(&source, "const before = 1;\n").expect("write source");
    std::fs::write(&closed, "const closed = 1;\n").expect("write closed");
    let placeholders = HashMap::from([
        ("file:///placeholder", file_uri(&source)),
        ("file:///destination", file_uri(&destination)),
        ("file:///closed", file_uri(&closed)),
        ("file:///created", file_uri(&created)),
    ]);
    let scenario = replace_placeholder_uris(scenario, &placeholders);
    std::fs::write(&scenario_path, scenario.to_string()).expect("write scenario");
    std::fs::write(&events, "").expect("write events");
    let server = ResolvedServer {
        id: "workspace-edit-fixture".to_string(),
        command: vec![
            env!("CARGO_BIN_EXE_lsp_core_workspace_edit_fixture").to_string(),
            scenario_path,
            events.clone(),
        ],
        extensions: vec![".ts".to_string()],
        priority: 0.0,
        env: None,
        initialization: None,
    };
    let client = LspClient::new(workspace.clone(), server.clone(), options);
    client.start().expect("start fixture");
    client.initialize().await.expect("initialize fixture");
    WorkspaceEditTestContext {
        client,
        workspace,
        source,
        destination,
        closed,
        created,
        events,
        server,
        _dir: dir,
    }
}

/// TS `makeBareConnection`: a transport with no workspace/applyEdit handler installed.
pub async fn make_bare_connection(
    scenario: Value,
) -> (LspClientTransport, WorkspaceEditTestContext) {
    let context = make_client(scenario).await;
    context.stop().await;
    std::fs::write(&context.events, "").expect("reset events");
    let transport = LspClientTransport::new(
        context.workspace.clone(),
        context.server.clone(),
        default_options().timeouts,
    );
    transport.start().expect("start bare");
    transport.initialize().await.expect("initialize bare");
    (transport, context)
}

fn replace_placeholder_uris(value: Value, placeholders: &HashMap<&str, String>) -> Value {
    match value {
        Value::String(text) => {
            Value::String(placeholders.get(text.as_str()).cloned().unwrap_or(text))
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| replace_placeholder_uris(item, placeholders))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, item)| (key, replace_placeholder_uris(item, placeholders)))
                .collect(),
        ),
        other @ (Value::Null | Value::Bool(_) | Value::Number(_)) => other,
    }
}

pub fn rename_text_edit(
    before: &str,
    after: &str,
    version: Option<i64>,
    uri: Option<&str>,
) -> Value {
    json!({
        "documentChanges": [{
            "textDocument": { "uri": uri.unwrap_or("file:///placeholder"), "version": version },
            "edits": [{
                "range": {
                    "start": { "line": 0, "character": 6 },
                    "end": { "line": 0, "character": 6 + before.len() },
                },
                "newText": after,
            }],
        }],
    })
}

pub fn diagnostic_json(message: &str) -> Value {
    json!({ "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } }, "message": message })
}

pub fn diagnostic(message: &str) -> Diagnostic {
    serde_json::from_value(diagnostic_json(message)).expect("diagnostic")
}

pub fn read_events(path: &str) -> Vec<Value> {
    read(path)
        .lines()
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event.get("type").is_some_and(Value::is_string))
        .collect()
}

pub fn is_event(event: &Value, kind: &str, method: &str) -> bool {
    event["type"] == kind && event["method"] == method
}

/// The fixture is a separate process that only reports through its events file, so there is
/// no in-process signal to await; this waits on that file's state with the TS 2s deadline.
pub async fn wait_for_event_count(
    path: &str,
    predicate: impl Fn(&Value) -> bool,
    count: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let matches: Vec<Value> = read_events(path)
            .into_iter()
            .filter(|event| predicate(event))
            .collect();
        if matches.len() >= count || Instant::now() >= deadline {
            return matches;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
