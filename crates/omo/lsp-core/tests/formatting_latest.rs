//! Tests ported from TypeScript `packages/lsp-core/src/lsp/format-document.test.ts`
//! and `packages/lsp-core/src/tools/format.test.ts`.

use lsp_core::lsp::client::{LspClient, LspClientOptions};
use lsp_core::lsp::format_document::{
    FormatDocumentOptions, FormatDocumentResult, format_document_with_client, line_delta,
};
use lsp_core::lsp::types::ResolvedServer;
use lsp_core::lsp::workspace_edit::types::ParsedTextEdit;
use lsp_core::request_context::{LspRequestContext, parse_lsp_request_context, scope_request_context};
use lsp_core::tools::execute_lsp_format;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

struct TestWorkspace {
    dir: tempfile::TempDir,
}

impl TestWorkspace {
    fn new(file_name: &str, content: &str) -> (Self, PathBuf) {
        let dir = tempfile::Builder::new()
            .prefix("lsp-format-")
            .tempdir()
            .expect("tempdir");
        let file_path = dir.path().join(file_name);
        fs::write(&file_path, content.as_bytes()).expect("write");
        (Self { dir }, file_path)
    }

    fn root(&self) -> String {
        self.dir.path().to_string_lossy().into_owned()
    }
}

fn python_cmd() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

/// Helper that creates a Python-based mock LSP server responding to initialize and formatting requests.
fn create_mock_server_script(
    dir: &Path,
    edits: Option<Vec<Value>>,
    advertise_capability: bool,
    mutate_on_format: Option<(&Path, &str)>,
) -> PathBuf {
    let script_path = dir.join("mock_lsp_server.py");
    let capability_val = if advertise_capability { "True" } else { "False" };
    let edits_val = match edits {
        Some(e) => serde_json::to_string(&e).expect("serialize formatting edits"),
        None => "None".to_string(),
    };
    let (mutate_path_val, mutate_content_val) = match mutate_on_format {
        Some((path, content)) => (
            serde_json::to_string(&path.to_string_lossy()).expect("serialize mutate path"),
            serde_json::to_string(content).expect("serialize mutate content"),
        ),
        None => ("None".to_string(), "None".to_string()),
    };

    let template = r#"import sys
import json

def send_msg(msg):
    body = json.dumps(msg).encode("utf-8")
    header = ("Content-Length: " + str(len(body)) + chr(13) + chr(10) + chr(13) + chr(10)).encode("ascii")
    sys.stdout.buffer.write(header + body)
    sys.stdout.buffer.flush()

def read_msg():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line_str = line.decode("ascii", errors="ignore").strip()
        if not line_str:
            break
        if line_str.lower().startswith("content-length:"):
            length = int(line_str.split(":", 1)[1].strip())
    if length is None:
        return None
    data = sys.stdin.buffer.read(length)
    return json.loads(data.decode("utf-8"))

advertise = __CAPABILITY__
edits = __EDITS__
mutate_path = __MUTATE_PATH__
mutate_content = __MUTATE_CONTENT__

while True:
    msg = read_msg()
    if msg is None:
        break
    msg_id = msg.get("id")
    method = msg.get("method")

    if method == "initialize":
        caps = {}
        if advertise:
            caps["documentFormattingProvider"] = True
        send_msg({"jsonrpc": "2.0", "id": msg_id, "result": {"capabilities": caps}})
    elif method == "initialized":
        pass
    elif method == "textDocument/didOpen":
        pass
    elif method == "textDocument/formatting":
        if mutate_path is not None:
            with open(mutate_path, "w", newline="", encoding="utf-8") as f:
                f.write(mutate_content)
        send_msg({"jsonrpc": "2.0", "id": msg_id, "result": edits})
    elif method == "shutdown":
        send_msg({"jsonrpc": "2.0", "id": msg_id, "result": None})
    elif method == "exit":
        break
    elif msg_id is not None:
        send_msg({"jsonrpc": "2.0", "id": msg_id, "result": None})
"#;

    let script = template
        .replace("__CAPABILITY__", capability_val)
        .replace("__EDITS__", &edits_val)
        .replace("__MUTATE_PATH__", &mutate_path_val)
        .replace("__MUTATE_CONTENT__", &mutate_content_val);

    fs::write(&script_path, script).expect("write mock server script");
    script_path
}

// ---------------------------------------------------------------------------
// Unit & deterministic workflow tests (lsp/format-document.test.ts)
// ---------------------------------------------------------------------------

#[test]
fn given_edits_when_line_delta_is_computed_then_reports_added_and_removed_lines() {
    let edits = vec![
        ParsedTextEdit {
            start_line: 0.0,
            start_character: 0.0,
            end_line: 1.0,
            end_character: 12.0,
            new_text: "const a = 1;\nconst b = 2;".to_string(),
        },
    ];
    let (added, removed) = line_delta(&edits);
    assert_eq!(added, 2);
    assert_eq!(removed, 2);

    let edits_add = vec![
        ParsedTextEdit {
            start_line: 0.0,
            start_character: 0.0,
            end_line: 0.0,
            end_character: 19.0,
            new_text: "const a = 1;\nconst b = 2;".to_string(),
        },
    ];
    let (added2, removed2) = line_delta(&edits_add);
    assert_eq!(added2, 2);
    assert_eq!(removed2, 1);
}

#[tokio::test]
async fn given_a_server_returning_text_edits_when_the_document_is_formatted_then_the_file_is_rewritten_and_line_deltas_are_reported() {
    let (_workspace, file_path) = TestWorkspace::new("sample.ts", "const a=1\nconst b   =2\nconst c=3\n");
    let script_path = create_mock_server_script(
        file_path.parent().unwrap(),
        Some(vec![json!({
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 1, "character": 12 }
            },
            "newText": "const a = 1;\nconst b = 2;"
        })]),
        true,
        None,
    );

    let client = LspClient::new(
        file_path.parent().unwrap().to_string_lossy().to_string(),
        ResolvedServer {
            id: "mock-fmt".to_string(),
            command: vec![python_cmd().to_string(), script_path.to_string_lossy().to_string()],
            extensions: vec![".ts".to_string()],
            priority: 1.0,
            env: None,
            initialization: None,
        },
        LspClientOptions::default(),
    );

    client.start().expect("start client");
    client.initialize().await.expect("initialize client");

    let result = format_document_with_client(
        &client,
        file_path.to_str().unwrap(),
        FormatDocumentOptions::default(),
    )
    .await
    .expect("format_document_with_client");

    assert_eq!(
        result,
        FormatDocumentResult::Formatted {
            lines_added: 2,
            lines_removed: 2,
        }
    );
    assert_eq!(
        fs::read_to_string(&file_path).unwrap(),
        "const a = 1;\nconst b = 2;\nconst c=3\n"
    );
    client.stop().await;
}

#[tokio::test]
async fn given_a_server_returning_an_edit_that_adds_lines_when_the_document_is_formatted_then_the_added_and_removed_line_counts_differ() {
    let (_workspace, file_path) = TestWorkspace::new("sample.ts", "const a=1;const b=2\n");
    let script_path = create_mock_server_script(
        file_path.parent().unwrap(),
        Some(vec![json!({
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 0, "character": 19 }
            },
            "newText": "const a = 1;\nconst b = 2;"
        })]),
        true,
        None,
    );

    let client = LspClient::new(
        file_path.parent().unwrap().to_string_lossy().to_string(),
        ResolvedServer {
            id: "mock-fmt".to_string(),
            command: vec![python_cmd().to_string(), script_path.to_string_lossy().to_string()],
            extensions: vec![".ts".to_string()],
            priority: 1.0,
            env: None,
            initialization: None,
        },
        LspClientOptions::default(),
    );

    client.start().expect("start client");
    client.initialize().await.expect("initialize client");

    let result = format_document_with_client(
        &client,
        file_path.to_str().unwrap(),
        FormatDocumentOptions::default(),
    )
    .await
    .expect("format_document_with_client");

    assert_eq!(
        result,
        FormatDocumentResult::Formatted {
            lines_added: 2,
            lines_removed: 1,
        }
    );
    assert_eq!(
        fs::read_to_string(&file_path).unwrap(),
        "const a = 1;\nconst b = 2;\n"
    );
    client.stop().await;
}

#[tokio::test]
async fn given_a_server_that_does_not_advertise_document_formatting_when_the_document_is_formatted_then_the_result_is_unavailable_and_the_file_bytes_are_untouched() {
    let original = "const a=1\n";
    let (_workspace, file_path) = TestWorkspace::new("sample.ts", original);
    let before_metadata = fs::metadata(&file_path).unwrap();
    let before_mtime = before_metadata.modified().unwrap();

    let script_path = create_mock_server_script(
        file_path.parent().unwrap(),
        None,
        false,
        None,
    );

    let client = LspClient::new(
        file_path.parent().unwrap().to_string_lossy().to_string(),
        ResolvedServer {
            id: "mock-fmt".to_string(),
            command: vec![python_cmd().to_string(), script_path.to_string_lossy().to_string()],
            extensions: vec![".ts".to_string()],
            priority: 1.0,
            env: None,
            initialization: None,
        },
        LspClientOptions::default(),
    );

    client.start().expect("start client");
    client.initialize().await.expect("initialize client");

    let result = format_document_with_client(
        &client,
        file_path.to_str().unwrap(),
        FormatDocumentOptions::default(),
    )
    .await
    .expect("format_document_with_client");

    assert_eq!(result, FormatDocumentResult::CAPABILITY_NOT_ADVERTISED);
    assert_eq!(fs::read_to_string(&file_path).unwrap(), original);
    let after_mtime = fs::metadata(&file_path).unwrap().modified().unwrap();
    assert_eq!(before_mtime, after_mtime);
    client.stop().await;
}

#[tokio::test]
async fn given_a_server_returning_no_edits_when_the_document_is_formatted_then_the_result_is_unchanged_and_the_file_bytes_are_untouched() {
    let original = "const a = 1;\n";
    let (_workspace, file_path) = TestWorkspace::new("sample.ts", original);
    let before_metadata = fs::metadata(&file_path).unwrap();
    let before_mtime = before_metadata.modified().unwrap();

    let script_path = create_mock_server_script(
        file_path.parent().unwrap(),
        Some(vec![]),
        true,
        None,
    );

    let client = LspClient::new(
        file_path.parent().unwrap().to_string_lossy().to_string(),
        ResolvedServer {
            id: "mock-fmt".to_string(),
            command: vec![python_cmd().to_string(), script_path.to_string_lossy().to_string()],
            extensions: vec![".ts".to_string()],
            priority: 1.0,
            env: None,
            initialization: None,
        },
        LspClientOptions::default(),
    );

    client.start().expect("start client");
    client.initialize().await.expect("initialize client");

    let result = format_document_with_client(
        &client,
        file_path.to_str().unwrap(),
        FormatDocumentOptions::default(),
    )
    .await
    .expect("format_document_with_client");

    assert_eq!(result, FormatDocumentResult::UNCHANGED);
    assert_eq!(fs::read_to_string(&file_path).unwrap(), original);
    let after_mtime = fs::metadata(&file_path).unwrap().modified().unwrap();
    assert_eq!(before_mtime, after_mtime);
    client.stop().await;
}

#[tokio::test]
async fn given_edits_that_reproduce_the_current_bytes_when_the_document_is_formatted_then_the_result_is_unchanged() {
    let original = "const a = 1;\n";
    let (_workspace, file_path) = TestWorkspace::new("sample.ts", original);
    let before_metadata = fs::metadata(&file_path).unwrap();
    let before_mtime = before_metadata.modified().unwrap();

    let script_path = create_mock_server_script(
        file_path.parent().unwrap(),
        Some(vec![json!({
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 0, "character": 12 }
            },
            "newText": "const a = 1;"
        })]),
        true,
        None,
    );

    let client = LspClient::new(
        file_path.parent().unwrap().to_string_lossy().to_string(),
        ResolvedServer {
            id: "mock-fmt".to_string(),
            command: vec![python_cmd().to_string(), script_path.to_string_lossy().to_string()],
            extensions: vec![".ts".to_string()],
            priority: 1.0,
            env: None,
            initialization: None,
        },
        LspClientOptions::default(),
    );

    client.start().expect("start client");
    client.initialize().await.expect("initialize client");

    let result = format_document_with_client(
        &client,
        file_path.to_str().unwrap(),
        FormatDocumentOptions::default(),
    )
    .await
    .expect("format_document_with_client");

    assert_eq!(result, FormatDocumentResult::UNCHANGED);
    assert_eq!(fs::read_to_string(&file_path).unwrap(), original);
    let after_mtime = fs::metadata(&file_path).unwrap().modified().unwrap();
    assert_eq!(before_mtime, after_mtime);
    client.stop().await;
}

#[tokio::test]
async fn given_the_file_shrinks_after_the_server_computed_its_edits_when_the_edits_are_applied_then_the_stale_edit_is_rejected_instead_of_corrupting_the_file() {
    let (_workspace, file_path) = TestWorkspace::new("sample.ts", "const a=1\nconst b=2\n");

    let script_path = create_mock_server_script(
        file_path.parent().unwrap(),
        Some(vec![json!({
            "range": {
                "start": { "line": 1, "character": 0 },
                "end": { "line": 1, "character": 9 }
            },
            "newText": "const b = 2;"
        })]),
        true,
        Some((&file_path, "short\n")),
    );

    let client = LspClient::new(
        file_path.parent().unwrap().to_string_lossy().to_string(),
        ResolvedServer {
            id: "mock-fmt".to_string(),
            command: vec![python_cmd().to_string(), script_path.to_string_lossy().to_string()],
            extensions: vec![".ts".to_string()],
            priority: 1.0,
            env: None,
            initialization: None,
        },
        LspClientOptions::default(),
    );

    client.start().expect("start client");
    client.initialize().await.expect("initialize client");

    let err = format_document_with_client(
        &client,
        file_path.to_str().unwrap(),
        FormatDocumentOptions::default(),
    )
    .await
    .expect_err("should reject stale edit");

    assert!(err.to_string().contains("outside line"), "{}", err);
    assert_eq!(fs::read_to_string(&file_path).unwrap().replace("\r\n", "\n"), "short\n");
    client.stop().await;
}

#[tokio::test]
async fn given_a_formatted_document_when_the_write_completes_then_no_temporary_file_is_left_behind() {
    let (_workspace, file_path) = TestWorkspace::new("sample.ts", "const a=1\n");
    let temp_candidate = format!("{}.omo-format.tmp", file_path.to_string_lossy());
    assert!(!Path::new(&temp_candidate).exists());

    let script_path = create_mock_server_script(
        file_path.parent().unwrap(),
        Some(vec![json!({
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 0, "character": 9 }
            },
            "newText": "const a = 1;"
        })]),
        true,
        None,
    );

    let client = LspClient::new(
        file_path.parent().unwrap().to_string_lossy().to_string(),
        ResolvedServer {
            id: "mock-fmt".to_string(),
            command: vec![python_cmd().to_string(), script_path.to_string_lossy().to_string()],
            extensions: vec![".ts".to_string()],
            priority: 1.0,
            env: None,
            initialization: None,
        },
        LspClientOptions::default(),
    );

    client.start().expect("start client");
    client.initialize().await.expect("initialize client");

    let result = format_document_with_client(
        &client,
        file_path.to_str().unwrap(),
        FormatDocumentOptions::default(),
    )
    .await
    .expect("format_document_with_client");

    assert_eq!(
        result,
        FormatDocumentResult::Formatted {
            lines_added: 1,
            lines_removed: 1,
        }
    );
    assert_eq!(fs::read_to_string(&file_path).unwrap(), "const a = 1;\n");
    assert!(!Path::new(&temp_candidate).exists());
    client.stop().await;
}

// ---------------------------------------------------------------------------
// Tool execution tests (tools/format.test.ts)
// ---------------------------------------------------------------------------

fn context_for(root: &str) -> LspRequestContext {
    let root_buf = PathBuf::from(root);
    let val = json!({
        "cwd": root,
        "projectConfigPaths": [root_buf.join("lsp.json").to_string_lossy()],
        "userConfigPath": root_buf.join("user-lsp.json").to_string_lossy(),
        "installDecisionsPath": root_buf.join("install-decisions.json").to_string_lossy(),
        "capabilities": { "installDecisionTool": false },
    });
    parse_lsp_request_context(&val).expect("context")
}

#[tokio::test]
async fn given_an_extension_with_no_configured_server_when_format_runs_then_the_missing_dependency_result_is_returned_and_the_file_is_untouched() {
    let original = "value\n";
    let (workspace, file_path) = TestWorkspace::new("module.wat", original);
    let path_str = file_path.to_string_lossy().to_string();

    let context = context_for(&workspace.root());

    let args = serde_json::Map::from_iter([("filePath".to_string(), json!(path_str))]);
    let result = scope_request_context(
        context,
        execute_lsp_format(&args, None),
    )
    .await
    .expect("format executed");

    assert_eq!(result.details.get("errorKind").and_then(Value::as_str), Some("missing_dependency"));
    assert_eq!(result.details.get("status").and_then(Value::as_str), Some("unavailable"));
    assert!(result.first_text().contains("No LSP server configured for extension: .wat"), "{}", result.first_text());
    assert_eq!(fs::read_to_string(&file_path).unwrap(), original);
}

#[tokio::test]
async fn given_a_missing_file_path_argument_when_format_runs_then_the_parameter_contract_is_enforced() {
    let empty_args = serde_json::Map::new();
    let err = execute_lsp_format(&empty_args, None)
        .await
        .expect_err("should reject missing filePath");
    assert!(err.to_string().contains("filePath"), "{}", err);
}
