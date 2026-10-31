//! Port of senpi mcp/modern/tools.ts: modern `tools/list` and `tools/call` result contracts.
use super::{
    errors::{McpMrtrError, McpMrtrErrorCode},
    tool_schema::{
        McpToolSchemaError, SchemaErrorCode, compile_modern_schema,
        has_supported_modern_tool_schemas,
    },
};
use crate::{errors::McpError, request_origin::McpRequestOptions, transport_sdk::McpClient};
use serde_json::{Map, Value};
use std::{sync::Arc, time::Duration};

fn invalid() -> McpError {
    McpMrtrError::new(McpMrtrErrorCode::InvalidResult).into()
}
fn schema(value: &Value) -> bool {
    value.is_boolean() || value.is_object()
}

pub async fn list_modern_tools(
    client: &Arc<McpClient>,
    params: Value,
    timeout: Duration,
    options: &McpRequestOptions,
) -> Result<Map<String, Value>, McpError> {
    let raw = client
        .request_with_options("tools/list", params, timeout, options)
        .await?;
    let Some(mut result) = raw.as_object().cloned() else {
        return Err(invalid());
    };
    let Some(tools) = result.get("tools").and_then(Value::as_array) else {
        return Err(invalid());
    };
    let well_formed = tools.iter().all(|tool| {
        tool.get("name").is_some_and(Value::is_string)
            && tool.get("inputSchema").is_some_and(schema)
            && tool.get("outputSchema").is_none_or(schema)
    }) && result.get("nextCursor").is_none_or(Value::is_string);
    if !well_formed {
        return Err(invalid());
    }
    // SDK `ToolSchema` metadata (without the schemas) must still parse.
    let metadata_ok = |tool: &Value| {
        ["title", "description"]
            .iter()
            .all(|key| tool.get(*key).is_none_or(Value::is_string))
            && tool.get("annotations").is_none_or(Value::is_object)
            && tool.get("_meta").is_none_or(Value::is_object)
    };
    let mut kept = Vec::new();
    for tool in tools
        .iter()
        .filter(|tool| has_supported_modern_tool_schemas(tool))
    {
        if !metadata_ok(tool) {
            return Err(invalid());
        }
        kept.push(tool.clone());
    }
    result.insert("tools".into(), Value::Array(kept));
    Ok(result)
}

pub async fn call_modern_tool(
    client: &Arc<McpClient>,
    params: Value,
    output_schema: Option<&Value>,
    timeout: Duration,
    options: &McpRequestOptions,
) -> Result<Map<String, Value>, McpError> {
    let validate = output_schema
        .map(compile_modern_schema)
        .transpose()
        .map_err(schema_error)?;
    let raw = client
        .request_with_options("tools/call", params, timeout, options)
        .await?;
    let Some(result) = raw.as_object().cloned() else {
        return Err(invalid());
    };
    if result.get("isError").is_some_and(|flag| !flag.is_boolean()) {
        return Err(invalid());
    }
    if let Some(content) = result.get("content")
        && !content.as_array().is_some_and(|blocks| {
            blocks
                .iter()
                .all(|block| block.get("type").is_some_and(Value::is_string))
        })
    {
        return Err(invalid());
    }
    if let Some(validate) = validate
        && result.get("isError") != Some(&Value::Bool(true))
        && !result
            .get("structuredContent")
            .is_some_and(|structured| validate.validate(structured))
    {
        return Err(schema_error(McpToolSchemaError::new(
            SchemaErrorCode::OutputSchemaMismatch,
            "Tool result does not match outputSchema.",
        )));
    }
    Ok(result)
}

/// `McpToolSchemaError` crossing an `McpError` boundary keeps its code in `cause`.
pub fn schema_error(error: McpToolSchemaError) -> McpError {
    let mut mapped = McpError::new(crate::errors::McpErrorKind::ToolExec, error.message.clone());
    mapped.cause = Some(Box::new(
        serde_json::json!({"name": "McpToolSchemaError", "code": error.code.as_str()}),
    ));
    mapped
}
