mod definitions;
mod handlers;
mod parameters;

pub use definitions::LspMcpTool;
pub use definitions::LspToolKind;
pub use definitions::lsp_mcp_tools;
pub use handlers::execute_lsp_diagnostics;
pub use handlers::execute_lsp_find_references;
pub use handlers::execute_lsp_goto_definition;
pub use handlers::execute_lsp_install_decision;
pub use handlers::execute_lsp_prepare_rename;
pub use handlers::execute_lsp_rename;
pub use handlers::execute_lsp_status;
pub use handlers::execute_lsp_symbols;
pub use parameters::is_record;

use crate::abort::AbortSignal;
use crate::lsp::errors::LspError;
use crate::missing_dependency_result::ToolExecutionResult;
use serde_json::Map;
use serde_json::Value;

/// TS `executeLspTool`: dispatches by listed name or legacy alias.
pub async fn execute_lsp_tool(
    name: &str,
    params: &Map<String, Value>,
    signal: Option<&AbortSignal>,
) -> Result<ToolExecutionResult, LspError> {
    let tool = lsp_mcp_tools()
        .into_iter()
        .find(|tool| tool.name == name || tool.aliases.contains(&name))
        .ok_or_else(|| LspError::other(format!("Unknown LSP tool: {name}")))?;
    match tool.kind {
        LspToolKind::Status => execute_lsp_status(),
        LspToolKind::Diagnostics => execute_lsp_diagnostics(params, signal).await,
        LspToolKind::GotoDefinition => execute_lsp_goto_definition(params, signal).await,
        LspToolKind::FindReferences => execute_lsp_find_references(params, signal).await,
        LspToolKind::Symbols => execute_lsp_symbols(params, signal).await,
        LspToolKind::PrepareRename => execute_lsp_prepare_rename(params, signal).await,
        LspToolKind::Rename => execute_lsp_rename(params, signal).await,
        LspToolKind::InstallDecision => execute_lsp_install_decision(params),
    }
}

/// TS `coerceToolArguments`: non-object arguments become an empty record.
pub fn coerce_tool_arguments(value: &Value) -> Map<String, Value> {
    match value {
        Value::Object(record) => record.clone(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            Map::new()
        }
    }
}

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tests;
