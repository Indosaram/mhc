use serde_json::Value;
use base64::Engine;
use sha2::{Digest, Sha256};

pub const BRIDGE_FRAME_MAX_BYTES: usize = 10 * 1024 * 1024;

pub fn generate_correlation_id() -> String { uuid::Uuid::new_v4().to_string() }

pub fn generate_bridge_token(byte_length: usize) -> Result<String, getrandom::Error> {
    let mut bytes = vec![0; byte_length];
    getrandom::fill(&mut bytes)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Bridge bearer token did not match")]
pub struct BridgeTokenMismatch;

pub fn verify_bridge_token(expected: &str, received: &str) -> Result<(), BridgeTokenMismatch> {
    let expected = Sha256::digest(expected.as_bytes());
    let received = Sha256::digest(received.as_bytes());
    let difference = expected.iter().zip(received).fold(0u8, |difference, (left, right)| difference | (left ^ right));
    if difference == 0 { Ok(()) } else { Err(BridgeTokenMismatch) }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeDecodeErrorCode {
    EmptyFrame,
    FrameTooLarge,
    MultipleFrames,
    MalformedJson,
    InvalidMessage,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct BridgeDecodeError {
    pub code: BridgeDecodeErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeConnectionConfig {
    pub port: u16,
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_roots: Option<std::collections::HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_pool_width: Option<u64>,
}

pub fn encode_bridge_frame(message: &Value) -> Result<String, serde_json::Error> {
    Ok(format!("{}\n", serde_json::to_string(message)?))
}

pub fn parse_bridge_json_line(line: &str, max_bytes: Option<usize>) -> Result<Value, BridgeDecodeError> {
    let max_bytes = max_bytes.unwrap_or(BRIDGE_FRAME_MAX_BYTES);
    if line.len() > max_bytes {
        return Err(BridgeDecodeError { code: BridgeDecodeErrorCode::FrameTooLarge, message: format!("Bridge frame exceeds {max_bytes} bytes") });
    }
    let line = line.strip_suffix('\n').unwrap_or(line);
    if line.is_empty() {
        return Err(BridgeDecodeError { code: BridgeDecodeErrorCode::EmptyFrame, message: "Bridge frame was empty".into() });
    }
    if line.contains('\n') {
        return Err(BridgeDecodeError { code: BridgeDecodeErrorCode::MultipleFrames, message: "Bridge frame contained more than one LF record".into() });
    }
    serde_json::from_str(line).map_err(|error| BridgeDecodeError { code: BridgeDecodeErrorCode::MalformedJson, message: error.to_string() })
}

pub fn decode_bridge_frame(line: &str, max_bytes: Option<usize>) -> Result<Value, BridgeDecodeError> {
    let parsed = parse_bridge_json_line(line, max_bytes)?;
    if valid_host_message(&parsed) || is_kernel_to_host_message(&parsed) { return Ok(parsed); }
    Err(BridgeDecodeError { code: BridgeDecodeErrorCode::InvalidMessage, message: "Invalid bridge message".into() })
}

pub(crate) fn string(value: &Value, key: &str) -> bool { value.get(key).is_some_and(Value::is_string) }
pub(crate) fn nonempty(value: &Value, key: &str) -> bool { value.get(key).and_then(Value::as_str).is_some_and(|s| !s.is_empty()) }
pub(crate) fn optional(value: &Value, key: &str, valid: impl FnOnce(&Value) -> bool) -> bool { value.get(key).is_none_or(valid) }
pub(crate) fn strings(value: &Value) -> bool { value.as_array().is_some_and(|values| values.iter().all(|v| v.as_str().is_some_and(|s| !s.is_empty()))) }
fn map_strings(value: &Value) -> bool { value.as_object().is_some_and(|values| values.values().all(Value::is_string)) }
pub(crate) fn integer(value: &Value, minimum: u64) -> bool {
    value.as_u64().is_some_and(|n| n >= minimum)
        || value.as_f64().is_some_and(|n| n >= minimum as f64 && n.fract() == 0.0)
}
pub(crate) fn error(value: &Value) -> bool {
    value.is_object() && string(value, "message") && ["name", "stack", "code"].iter().all(|key| optional(value, key, Value::is_string))
}
fn connection(value: &Value) -> bool {
    value.is_object() && value.get("port").is_some_and(|v| integer(v, 1) && v.as_f64().is_some_and(|n| n <= 65_535.0))
        && nonempty(value, "token") && optional(value, "localRoots", map_strings)
        && optional(value, "artifactsDir", Value::is_string) && optional(value, "parallelPoolWidth", |v| integer(v, 1))
}
fn valid_host_message(value: &Value) -> bool {
    match value.get("type").and_then(Value::as_str) {
        Some("init") => nonempty(value, "sessionId") && value.get("connection").is_some_and(connection)
            && optional(value, "sessionEnv", map_strings) && optional(value, "kernelGeneration", |v| integer(v, 1))
            && optional(value, "hostToolNames", strings) && optional(value, "foreignLanguageNames", strings),
        Some("run") => nonempty(value, "cellId") && string(value, "code") && optional(value, "timeoutMs", |v| integer(v, 1)),
        Some("tool-reply") => nonempty(value, "callId") && match value.get("ok").and_then(Value::as_bool) {
            Some(true) => value.get("value").is_some(), Some(false) => value.get("error").is_some_and(error), None => false,
        },
        Some("interrupt") => optional(value, "reason", Value::is_string),
        Some("close") => true,
        _ => super::kernel_tools_protocol::valid_host_message(value),
    }
}

pub fn is_kernel_to_host_message(value: &Value) -> bool {
    match value.get("type").and_then(Value::as_str) {
        Some("ready" | "closed") => true,
        Some("init-failed") => value.get("error").is_some_and(error),
        Some("text") => matches!(value.get("stream").and_then(Value::as_str), Some("stdout" | "stderr")) && string(value, "data"),
        Some("display") => nonempty(value, "mimeType") && string(value, "dataBase64"),
        Some("tool-call") => nonempty(value, "callId") && nonempty(value, "toolName") && value.get("args").is_some(),
        Some("log") => string(value, "message"),
        Some("phase") => string(value, "title"),
        Some("status") => value.get("event").is_some_and(|event| event.is_object() && string(event, "op")),
        Some("result") => nonempty(value, "cellId") && value.get("durationMs").is_some_and(|v| integer(v, 0))
            && match value.get("ok").and_then(Value::as_bool) {
                Some(true) => optional(value, "valueRepr", Value::is_string),
                Some(false) => value.get("error").is_some_and(error), None => false,
            },
        _ => super::kernel_tools_protocol::valid_kernel_message(value),
    }
}
