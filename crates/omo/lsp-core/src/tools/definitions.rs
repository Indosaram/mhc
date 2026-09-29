use serde_json::Value;
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LspToolKind {
    Status,
    Diagnostics,
    GotoDefinition,
    FindReferences,
    Symbols,
    PrepareRename,
    Rename,
    InstallDecision,
}

/// TS `LspMcpTool` without `execute`; dispatch goes through [`LspToolKind`].
#[derive(Debug, Clone, PartialEq)]
pub struct LspMcpTool {
    pub name: &'static str,
    pub aliases: Vec<&'static str>,
    pub title: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub kind: LspToolKind,
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required })
}

fn position_properties(file_description: &str) -> Value {
    json!({
        "filePath": { "type": "string", "description": file_description },
        "line": { "type": "number", "description": "1-based line number." },
        "character": { "type": "number", "description": "0-based column." },
    })
}

/// TS `LSP_MCP_TOOLS`, in listing order.
pub fn lsp_mcp_tools() -> Vec<LspMcpTool> {
    let mut references = position_properties("Source file containing the symbol.");
    references["includeDeclaration"] =
        json!({ "type": "boolean", "description": "Include the declaration. Defaults to true." });
    let mut rename = position_properties("Source file path.");
    rename["newName"] = json!({ "type": "string", "description": "New symbol name." });
    vec![
        LspMcpTool {
            name: "status",
            aliases: vec!["lsp_status"],
            title: "LSP Status",
            description: "List configured and active LSP servers without starting a new language server.",
            input_schema: object_schema(json!({}), &[]),
            kind: LspToolKind::Status,
        },
        LspMcpTool {
            name: "diagnostics",
            aliases: vec!["lsp_diagnostics"],
            title: "LSP Diagnostics",
            description: "Get errors, warnings, and hints for a source file or directory.",
            input_schema: object_schema(
                json!({
                    "filePath": { "type": "string", "description": "File or directory path to check." },
                    "severity": {
                        "type": "string",
                        "enum": ["error", "warning", "information", "hint", "all"],
                        "description": "Severity filter. Defaults to all.",
                    },
                }),
                &["filePath"],
            ),
            kind: LspToolKind::Diagnostics,
        },
        LspMcpTool {
            name: "goto_definition",
            aliases: vec!["lsp_goto_definition"],
            title: "LSP Goto Definition",
            description: "Find where a symbol is defined.",
            input_schema: object_schema(
                position_properties("Source file containing the symbol."),
                &["filePath", "line", "character"],
            ),
            kind: LspToolKind::GotoDefinition,
        },
        LspMcpTool {
            name: "find_references",
            aliases: vec!["lsp_find_references"],
            title: "LSP Find References",
            description: "Find references of a symbol across the workspace.",
            input_schema: object_schema(references, &["filePath", "line", "character"]),
            kind: LspToolKind::FindReferences,
        },
        LspMcpTool {
            name: "symbols",
            aliases: vec!["lsp_symbols"],
            title: "LSP Symbols",
            description: "List document symbols or search workspace symbols.",
            input_schema: object_schema(
                json!({
                    "filePath": { "type": "string", "description": "File path used as LSP context." },
                    "scope": {
                        "type": "string",
                        "enum": ["document", "workspace"],
                        "description": "Use document for file outline or workspace for project-wide search.",
                    },
                    "query": { "type": "string", "description": "Workspace symbol query." },
                    "limit": { "type": "number", "description": "Maximum number of symbols to return." },
                }),
                &["filePath", "scope"],
            ),
            kind: LspToolKind::Symbols,
        },
        LspMcpTool {
            name: "prepare_rename",
            aliases: vec!["lsp_prepare_rename"],
            title: "LSP Prepare Rename",
            description: "Check whether a symbol can be renamed at a position.",
            input_schema: object_schema(
                position_properties("Source file path."),
                &["filePath", "line", "character"],
            ),
            kind: LspToolKind::PrepareRename,
        },
        LspMcpTool {
            name: "rename",
            aliases: vec!["lsp_rename"],
            title: "LSP Rename",
            description: "Rename a symbol across the workspace and apply the returned workspace edit.",
            input_schema: object_schema(rename, &["filePath", "line", "character", "newName"]),
            kind: LspToolKind::Rename,
        },
        LspMcpTool {
            name: "install_decision",
            aliases: vec!["lsp_install_decision"],
            title: "LSP Install Decision",
            description: "Record whether the user allowed or declined installing a missing LSP server. Record 'declined' when the user declines, or has not explicitly asked for LSP installation, to silence future prompts.",
            input_schema: object_schema(
                json!({
                    "server_id": {
                        "type": "string",
                        "description": "The LSP server id from the not-installed message (e.g. 'rust').",
                    },
                    "decision": {
                        "type": "string",
                        "enum": ["declined", "allowed"],
                        "description": "'declined' silences future prompts; 'allowed' pre-authorizes installation.",
                    },
                }),
                &["server_id", "decision"],
            ),
            kind: LspToolKind::InstallDecision,
        },
    ]
}
