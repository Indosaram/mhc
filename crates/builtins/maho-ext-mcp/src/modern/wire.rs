//! Port of senpi mcp/modern/wire.ts: the stateless per-request modern wire shape.
use super::parameter_headers::encode_mcp_header_value;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

pub const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";
pub const HEADER_MISMATCH: i64 = -32020;
pub const MISSING_REQUIRED_CLIENT_CAPABILITY: i64 = -32021;
pub const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;
pub const PROTOCOL_VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
pub const CLIENT_INFO_KEY: &str = "io.modelcontextprotocol/clientInfo";
pub const CLIENT_CAPABILITIES_KEY: &str = "io.modelcontextprotocol/clientCapabilities";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ModernClientInfo {
    pub name: String,
    pub version: String,
}

fn object(value: Option<&Value>) -> Map<String, Value> {
    value
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// A JSON-RPC request object: `{jsonrpc, id, method, params: {..., _meta}}`.
pub fn build_modern_request(
    id: Value,
    method: &str,
    client_info: &ModernClientInfo,
    client_capabilities: &Value,
    params: Option<&Value>,
) -> Value {
    let mut params = object(params);
    let mut meta = object(params.get("_meta"));
    meta.insert(PROTOCOL_VERSION_KEY.into(), MODERN_PROTOCOL_VERSION.into());
    meta.insert(CLIENT_INFO_KEY.into(), json!(client_info));
    meta.insert(CLIENT_CAPABILITIES_KEY.into(), client_capabilities.clone());
    params.insert("_meta".into(), Value::Object(meta));
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

/// Rewrite an outgoing request or notification into the modern wire shape at the negotiated version.
pub fn modernize_request(
    id: Value,
    method: &str,
    client_info: &ModernClientInfo,
    client_capabilities: &Value,
    version: &str,
    params: Option<&Value>,
) -> Value {
    let mut built = build_modern_request(id, method, client_info, client_capabilities, params);
    let supplied = params
        .and_then(|params| params.get("_meta"))
        .and_then(|meta| meta.get(CLIENT_CAPABILITIES_KEY))
        .filter(|capabilities| capabilities.is_object())
        .cloned();
    let meta = &mut built["params"]["_meta"];
    meta[CLIENT_CAPABILITIES_KEY] = supplied.unwrap_or_else(|| client_capabilities.clone());
    meta[PROTOCOL_VERSION_KEY] = version.into();
    built
}

/// Derive protocol headers from the same request that will be serialized.
pub fn derive_modern_headers(request: &Value) -> BTreeMap<String, String> {
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = request.get("params").unwrap_or(&Value::Null);
    let mut headers = BTreeMap::new();
    headers.insert(
        "MCP-Protocol-Version".into(),
        params["_meta"][PROTOCOL_VERSION_KEY]
            .as_str()
            .unwrap_or_default()
            .into(),
    );
    headers.insert("Mcp-Method".into(), method.into());
    let name = match method {
        "resources/read" => params.get("uri"),
        "tools/call" | "prompts/get" => params.get("name"),
        "tasks/get" | "tasks/update" | "tasks/cancel" => params.get("taskId"),
        _ => None,
    };
    if let Some(name) = name.and_then(Value::as_str) {
        headers.insert("Mcp-Name".into(), encode_mcp_header_value(name));
    }
    headers
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ModernWireError(pub String);

/// Parse the common envelope only; method-specific payloads remain available to their own parsers.
pub fn parse_modern_result(response: &Value) -> Result<Map<String, Value>, ModernWireError> {
    let invalid = || ModernWireError("Invalid MCP result envelope".into());
    let response = response.as_object().ok_or_else(invalid)?;
    if response.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || response.contains_key("error")
    {
        return Err(invalid());
    }
    let mut result = response
        .get("result")
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(invalid)?;
    match result.get("resultType") {
        None => {
            result.insert("resultType".into(), "complete".into());
        }
        Some(Value::String(_)) => {}
        Some(_) => return Err(ModernWireError("Invalid MCP resultType".into())),
    }
    Ok(result)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModernErrorKind {
    HeaderMismatch,
    MissingRequiredClientCapabilityError,
    UnsupportedProtocolVersionError,
}
impl ModernErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HeaderMismatch => "HeaderMismatch",
            Self::MissingRequiredClientCapabilityError => "MissingRequiredClientCapabilityError",
            Self::UnsupportedProtocolVersionError => "UnsupportedProtocolVersionError",
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct ModernError {
    pub kind: ModernErrorKind,
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
    pub supported_versions: Option<Vec<String>>,
}

fn parse_body(body: &Value) -> Option<Value> {
    match body {
        Value::String(text) => serde_json::from_str(text).ok(),
        other => Some(other.clone()),
    }
}

/// Only the modern-specific codes establish the era, never generic JSON-RPC errors or message text.
pub fn classify_modern_error(body: &Value) -> Option<ModernError> {
    let response = parse_body(body)?;
    if response.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return None;
    }
    let error = response.get("error")?.as_object()?;
    let message = error.get("message")?.as_str()?.to_owned();
    let data = error.get("data").cloned();
    let code = error.get("code").and_then(Value::as_f64)?;
    let kind = match code {
        c if c == HEADER_MISMATCH as f64 => ModernErrorKind::HeaderMismatch,
        c if c == MISSING_REQUIRED_CLIENT_CAPABILITY as f64 => {
            ModernErrorKind::MissingRequiredClientCapabilityError
        }
        c if c == UNSUPPORTED_PROTOCOL_VERSION as f64 => {
            ModernErrorKind::UnsupportedProtocolVersionError
        }
        _ => return None,
    };
    let supported_versions = (kind == ModernErrorKind::UnsupportedProtocolVersionError)
        .then(|| data.as_ref()?.get("supported")?.as_array().cloned())
        .flatten()
        .and_then(|versions| {
            versions
                .iter()
                .map(|version| version.as_str().map(str::to_owned))
                .collect()
        });
    Some(ModernError {
        kind,
        code: code as i64,
        message,
        data,
        supported_versions,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeErrorEra {
    Modern,
    Legacy,
    Indeterminate,
}
pub fn classify_http_probe_error(status: u16, body: &Value) -> ProbeErrorEra {
    if status != 400 {
        return ProbeErrorEra::Indeterminate;
    }
    classify_stdio_probe_error(body)
}
pub fn classify_stdio_probe_error(body: &Value) -> ProbeErrorEra {
    if classify_modern_error(body).is_some() {
        ProbeErrorEra::Modern
    } else {
        ProbeErrorEra::Legacy
    }
}
