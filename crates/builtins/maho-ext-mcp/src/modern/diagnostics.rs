//! Port of senpi mcp/modern/diagnostics.ts.
use super::errors::{McpMrtrError, McpMrtrErrorCode};
use serde_json::{Map, Value, json};

/// Control/diagnostic payloads are private from admission, not merely after consent starts.
pub fn sanitize_modern_message(message: &Value) -> Result<Option<Value>, McpMrtrError> {
    if let Some(error) = message.get("error") {
        return Ok(Some(json!({
            "jsonrpc": "2.0",
            "id": message.get("id").cloned().unwrap_or(Value::Null),
            "error": {"code": error.get("code").cloned().unwrap_or(Value::Null), "message": "MCP server request failed."},
        })));
    }
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return Ok(Some(message.clone()));
    };
    if message.get("id").is_some() {
        return Err(McpMrtrError::new(McpMrtrErrorCode::InvalidResult));
    }
    if method == "notifications/progress" {
        let Some(params) = message.get("params").and_then(Value::as_object) else {
            return Ok(None);
        };
        let finite = |value: &Value| value.as_f64().is_some_and(f64::is_finite);
        if !params.get("progress").is_some_and(finite)
            || params.get("total").is_some_and(|total| !finite(total))
        {
            return Ok(None);
        }
        let mut safe = Map::new();
        // An absent token stays absent, as JSON.stringify drops upstream's undefined `progressToken`.
        if let Some(token) = params.get("progressToken") {
            safe.insert("progressToken".into(), token.clone());
        }
        safe.insert("progress".into(), params["progress"].clone());
        if let Some(total) = params.get("total") {
            safe.insert("total".into(), total.clone());
        }
        return Ok(Some(
            json!({"jsonrpc": "2.0", "method": method, "params": safe}),
        ));
    }
    if method == "notifications/message" {
        return Ok(Some(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": {"level": "info", "data": "MCP server diagnostic suppressed."},
        })));
    }
    Ok(Some(message.clone()))
}
