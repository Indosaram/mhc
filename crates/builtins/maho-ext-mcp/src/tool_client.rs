//! Port of senpi mcp/tool-client.ts: tool operations have an era-specific contract.
use crate::{
    errors::McpError,
    modern::{
        request::is_modern_mcp_client,
        tools::{call_modern_tool, list_modern_tools},
    },
    request_origin::McpRequestOptions,
    transport_sdk::McpClient,
};
use serde_json::{Map, Value};
use std::{sync::Arc, time::Duration};

/// The client remains the lifecycle/request owner, but legacy tool helpers never parse modern data.
pub async fn list_mcp_tools(
    client: &Arc<McpClient>,
    params: Value,
    timeout: Duration,
    options: &McpRequestOptions,
) -> Result<Map<String, Value>, McpError> {
    if !is_modern_mcp_client(client) {
        let raw = client
            .request_with_options("tools/list", params, timeout, options)
            .await?;
        return Ok(raw.as_object().cloned().unwrap_or_default());
    }
    list_modern_tools(client, params, timeout, options).await
}

pub async fn call_mcp_tool_request(
    client: &Arc<McpClient>,
    params: Value,
    output_schema: Option<&Value>,
    timeout: Duration,
    options: &McpRequestOptions,
) -> Result<Map<String, Value>, McpError> {
    if !is_modern_mcp_client(client) {
        let raw = client
            .request_with_options("tools/call", params, timeout, options)
            .await?;
        return Ok(raw.as_object().cloned().unwrap_or_default());
    }
    call_modern_tool(client, params, output_schema, timeout, options).await
}
