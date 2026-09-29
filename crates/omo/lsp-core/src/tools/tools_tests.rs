use super::*;
use crate::request_context::StandaloneMcpRequestContextInput;
use crate::request_context::create_standalone_mcp_request_context;
use crate::request_context::scope_request_context;
use pretty_assertions::assert_eq;
use serde_json::json;

fn expected_tool_surface() -> Value {
    json!([
        {
            "name": "status",
            "title": "LSP Status",
            "description": "List configured and active LSP servers without starting a new language server.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] },
        },
        {
            "name": "diagnostics",
            "title": "LSP Diagnostics",
            "description": "Get errors, warnings, and hints for a source file or directory.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filePath": { "type": "string", "description": "File or directory path to check." },
                    "severity": {
                        "type": "string",
                        "enum": ["error", "warning", "information", "hint", "all"],
                        "description": "Severity filter. Defaults to all.",
                    },
                },
                "required": ["filePath"],
            },
        },
        {
            "name": "goto_definition",
            "title": "LSP Goto Definition",
            "description": "Find where a symbol is defined.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filePath": { "type": "string", "description": "Source file containing the symbol." },
                    "line": { "type": "number", "description": "1-based line number." },
                    "character": { "type": "number", "description": "0-based column." },
                },
                "required": ["filePath", "line", "character"],
            },
        },
        {
            "name": "find_references",
            "title": "LSP Find References",
            "description": "Find references of a symbol across the workspace.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filePath": { "type": "string", "description": "Source file containing the symbol." },
                    "line": { "type": "number", "description": "1-based line number." },
                    "character": { "type": "number", "description": "0-based column." },
                    "includeDeclaration": { "type": "boolean", "description": "Include the declaration. Defaults to true." },
                },
                "required": ["filePath", "line", "character"],
            },
        },
        {
            "name": "symbols",
            "title": "LSP Symbols",
            "description": "List document symbols or search workspace symbols.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filePath": { "type": "string", "description": "File path used as LSP context." },
                    "scope": {
                        "type": "string",
                        "enum": ["document", "workspace"],
                        "description": "Use document for file outline or workspace for project-wide search.",
                    },
                    "query": { "type": "string", "description": "Workspace symbol query." },
                    "limit": { "type": "number", "description": "Maximum number of symbols to return." },
                },
                "required": ["filePath", "scope"],
            },
        },
        {
            "name": "prepare_rename",
            "title": "LSP Prepare Rename",
            "description": "Check whether a symbol can be renamed at a position.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filePath": { "type": "string", "description": "Source file path." },
                    "line": { "type": "number", "description": "1-based line number." },
                    "character": { "type": "number", "description": "0-based column." },
                },
                "required": ["filePath", "line", "character"],
            },
        },
        {
            "name": "rename",
            "title": "LSP Rename",
            "description": "Rename a symbol across the workspace and apply the returned workspace edit.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filePath": { "type": "string", "description": "Source file path." },
                    "line": { "type": "number", "description": "1-based line number." },
                    "character": { "type": "number", "description": "0-based column." },
                    "newName": { "type": "string", "description": "New symbol name." },
                },
                "required": ["filePath", "line", "character", "newName"],
            },
        },
        {
            "name": "install_decision",
            "title": "LSP Install Decision",
            "description": "Record whether the user allowed or declined installing a missing LSP server. Record 'declined' when the user declines, or has not explicitly asked for LSP installation, to silence future prompts.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "server_id": {
                        "type": "string",
                        "description": "The LSP server id from the not-installed message (e.g. 'rust').",
                    },
                    "decision": {
                        "type": "string",
                        "enum": ["declined", "allowed"],
                        "description": "'declined' silences future prompts; 'allowed' pre-authorizes installation.",
                    },
                },
                "required": ["server_id", "decision"],
            },
        },
    ])
}

fn args(value: Value) -> Map<String, Value> {
    coerce_tool_arguments(&value)
}

fn standalone_context() -> crate::request_context::LspRequestContext {
    create_standalone_mcp_request_context(StandaloneMcpRequestContextInput::default())
        .expect("context")
}

#[test]
fn public_eight_tool_schemas_are_pinned_in_order() {
    let surface: Vec<Value> = lsp_mcp_tools()
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.name, "title": tool.title,
                "description": tool.description, "inputSchema": tool.input_schema,
            })
        })
        .collect();

    assert_eq!(
        serde_json::to_string(&Value::Array(surface)).expect("json"),
        serde_json::to_string(&expected_tool_surface()).expect("json")
    );
}

#[tokio::test]
async fn legacy_aliases_are_callable_but_not_listed() {
    let result = scope_request_context(
        standalone_context(),
        execute_lsp_tool(
            "lsp_diagnostics",
            &args(json!({ "filePath": "module.wat" })),
            None,
        ),
    )
    .await
    .expect("alias executes");

    assert!(
        result
            .first_text()
            .contains("No LSP server configured for extension: .wat"),
        "{}",
        result.first_text()
    );
    assert!(
        !lsp_mcp_tools()
            .iter()
            .any(|tool| tool.name == "lsp_diagnostics")
    );
}

#[test]
fn non_object_tool_arguments_coerce_to_empty_record() {
    assert_eq!(coerce_tool_arguments(&Value::Null), Map::new());
    assert_eq!(coerce_tool_arguments(&json!(["filePath"])), Map::new());
}

#[tokio::test]
async fn unknown_tool_and_missing_parameters_reject_with_exact_messages() {
    let unknown = execute_lsp_tool("nope", &Map::new(), None)
        .await
        .expect_err("unknown");
    let missing_path = execute_lsp_tool("diagnostics", &Map::new(), None)
        .await
        .expect_err("path");
    let missing_line = execute_lsp_tool(
        "goto_definition",
        &args(json!({ "filePath": "a.ts", "line": "1" })),
        None,
    )
    .await
    .expect_err("line");
    let empty_path = execute_lsp_tool("symbols", &args(json!({ "filePath": "" })), None)
        .await
        .expect_err("empty");

    assert_eq!(
        vec![
            unknown.to_string(),
            missing_path.to_string(),
            missing_line.to_string(),
            empty_path.to_string()
        ],
        vec![
            "Unknown LSP tool: nope".to_string(),
            "Missing required string parameter 'filePath'".to_string(),
            "Missing required number parameter 'line'".to_string(),
            "Missing required string parameter 'filePath'".to_string(),
        ]
    );
}

#[tokio::test]
async fn workspace_symbols_without_query_reports_missing_query_without_error_flag() {
    let result = execute_lsp_tool(
        "lsp_symbols",
        &args(json!({ "filePath": "a.ts", "scope": "workspace" })),
        None,
    )
    .await
    .expect("result");

    assert_eq!(
        result.first_text(),
        "Error: 'query' is required for workspace scope"
    );
    assert!(!result.is_error);
    assert_eq!(
        Value::Object(result.details),
        json!({
            "filePath": "a.ts", "scope": "workspace", "symbols": [], "totalSymbols": 0,
            "truncated": false, "error": "Error: 'query' is required for workspace scope",
            "errorKind": "missing_query",
        })
    );
}

#[test]
fn install_decision_rejects_invalid_decision_before_touching_state() {
    let result =
        execute_lsp_install_decision(&args(json!({ "server_id": "rust", "decision": "maybe" })))
            .expect("result");

    assert!(result.is_error);
    assert_eq!(
        result.first_text(),
        "Invalid decision 'maybe'. Expected \"declined\" or \"allowed\"."
    );
    assert_eq!(
        Value::Object(result.details),
        json!({ "serverId": "rust", "errorKind": "invalid_decision" })
    );
}

#[tokio::test]
async fn directory_without_source_files_reports_no_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dir_path = dir.path().to_string_lossy().into_owned();
    let context = create_standalone_mcp_request_context(StandaloneMcpRequestContextInput {
        cwd: Some(dir_path.clone()),
        ..StandaloneMcpRequestContextInput::default()
    })
    .expect("context");

    let result = scope_request_context(
        context,
        execute_lsp_tool(
            "diagnostics",
            &args(json!({ "filePath": ".", "severity": "error" })),
            None,
        ),
    )
    .await
    .expect("result");

    let abs = crate::request_context::canonicalize_existing_or_nearest_ancestor(&dir_path)
        .expect("canonical");
    let message = format!("No supported source files found in directory: {abs}");
    assert_eq!(result.first_text(), message);
    assert_eq!(
        Value::Object(result.details),
        json!({
            "filePath": ".", "severity": "error", "mode": "directory", "diagnostics": [],
            "totalDiagnostics": 0, "truncated": false, "error": message, "errorKind": "no_files",
        })
    );
}
