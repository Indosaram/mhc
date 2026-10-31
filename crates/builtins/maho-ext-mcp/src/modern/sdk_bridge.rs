//! Port of senpi mcp/modern/sdk-bridge.ts.
//!
//! senpi bridges the SDK `Client` lifecycle onto the modern wire with a wrapping `Transport`. The port's
//! `McpClient` owns both ends, so the bridge reduces to its two data transforms; the subscription routing
//! the bridge performs lives in `McpClient` (`modern_incoming` / `modern_outgoing`).
use super::{era::DiscoveryResult, era::LATEST_PROTOCOL_VERSION, transport::ModernTransportError};
use serde_json::{Value, json};

/// The local `initialize` answer, built from validated discovery; discovery must support the real wire version.
pub fn modern_initialize_result(
    discovery: &DiscoveryResult,
    wire_version: &str,
) -> Result<Value, ModernTransportError> {
    let supported = discovery.get("supportedVersions").and_then(Value::as_array);
    if !supported.is_some_and(|versions| {
        versions
            .iter()
            .any(|version| version.as_str() == Some(wire_version))
    }) {
        return Err(ModernTransportError(format!(
            "Discovery does not support negotiated MCP version {wire_version}"
        )));
    }
    // SDK `InitializeResultSchema`: every declared capability must be an object.
    let capabilities = discovery
        .get("capabilities")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let well_formed = capabilities
        .as_object()
        .is_some_and(|capabilities| capabilities.values().all(Value::is_object));
    if !well_formed {
        return Err(ModernTransportError(
            "Discovery capabilities are malformed".into(),
        ));
    }
    let mut result = json!({
        "protocolVersion": LATEST_PROTOCOL_VERSION,
        "capabilities": capabilities,
        "serverInfo": discovery.get("_meta").and_then(|meta| meta.get("io.modelcontextprotocol/serverInfo")).cloned().unwrap_or(Value::Null),
    });
    if let Some(instructions) = discovery.get("instructions") {
        result["instructions"] = instructions.clone();
    }
    Ok(result)
}

/// Modern results may carry `nextCursor: null`, which the legacy list parsers reject.
pub fn normalize_modern_result(mut message: Value) -> Value {
    if let Some(result) = message.get_mut("result").and_then(Value::as_object_mut)
        && result.get("nextCursor").is_some_and(Value::is_null)
    {
        result.remove("nextCursor");
    }
    message
}
