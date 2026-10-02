//! Proxy operations independent of the shared tool-search BM25 index.
//! Gateway construction, search and nearest matches require the owner crate's
//! `buildBm25Index` and `deriveMcpRegistrationId`; no replacement index is used.
use maho_ext_api::{ToolCall, ToolContent, ToolDefinition, ToolError, ToolResult};
use serde_json::{Value, json};

use crate::catalog::McpToolCatalogEntry;

fn text_result(server: &str, text: String) -> ToolResult {
    let preview = String::from_utf16_lossy(
        &text.split('\n').next().unwrap_or("").encode_utf16().take(80).collect::<Vec<_>>(),
    );
    ToolResult {
        content: vec![ToolContent::text(text)],
        details: Some(json!({"preview": preview, "server": server, "tool": "proxy"})),
    }
}

/// Describe a selected catalog entry without connecting to the server.
pub fn describe_mcp_proxy_entry(entry: &McpToolCatalogEntry) -> Result<ToolResult, serde_json::Error> {
    let empty_schema = json!({});
    Ok(text_result(&entry.server, format!(
        "{}: {}\nInput schema (JSON Schema):\n{}",
        entry.tool, entry.description.as_deref().unwrap_or("(no description)"),
        serde_json::to_string_pretty(if entry.schema.is_null() { &empty_schema } else { &entry.schema })?,
    )))
}

/// Parse proxy JSON-string arguments and invoke the existing native definition.
/// Signal, update callback and context are forwarded without schema validation,
/// matching the explicitly opted-in proxy contract.
pub async fn call_mcp_proxy_entry(
    entry: &McpToolCatalogEntry,
    full: &ToolDefinition,
    args: Option<&str>,
    mut call: ToolCall<'_>,
) -> Result<ToolResult, ToolError> {
    let parsed = serde_json::from_str::<Value>(args.unwrap_or("{}"));
    let params = match parsed {
        Ok(params) if params.is_object() => params,
        result => {
            let error = match result {
                Ok(_) => "args must encode a JSON object".to_owned(),
                Err(error) => error.to_string(),
            };
            return Ok(text_result(&entry.server, format!(
                "Invalid args for '{}': {error}. Pass args as a JSON object STRING matching the schema from op:\"describe\", e.g. {{\"op\":\"call\",\"tool\":\"{}\",\"args\":\"{{\\\"key\\\": \\\"value\\\"}}\"}}.",
                entry.tool, entry.tool,
            )));
        }
    };
    call.params = params;
    (full.execute)(call).await
}
