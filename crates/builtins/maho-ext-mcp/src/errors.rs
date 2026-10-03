use serde_json::Value;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpErrorKind { Connect, Protocol, ToolExec, Auth, Timeout, SessionExpired }
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct McpError {
    pub kind: McpErrorKind, pub message: String, pub phase: Option<String>, pub retriable: bool,
    pub server_name: Option<String>, pub cause: Option<Box<Value>>,
}
impl McpError {
    pub fn new(kind: McpErrorKind, message: impl Into<String>) -> Self { Self { kind, message: message.into(), phase: None, retriable: false, server_name: None, cause: None } }
}
pub fn is_retriable_mcp_error(error: &Value) -> bool {
    if error.get("retriable") == Some(&Value::Bool(true)) || is_mcp_session_expired_error(error) { return true; }
    if numeric_signal(error,false) { return true; }
    let text = collect_text(error).join(" ").to_lowercase();
    ["econnrefused","connection refused","transport closed"].iter().any(|s| text.contains(s)) || has_status_word(&text,&["404","502","503"])
}
pub fn is_mcp_session_expired_error(error: &Value) -> bool {
    if numeric_signal(error,true) { return true; }
    let text = collect_text(error).join(" ").to_lowercase();
    has_status_word(&text,&["404"]) || (text.contains("-32000") && text.contains("session")) || ["session expired","session not found","mcp-session-id"].iter().any(|s| text.contains(s))
}
fn number(value: Option<&Value>) -> Option<f64> {
    value.and_then(|v|v.as_f64().or_else(||v.as_str().and_then(|s| {
        let s=s.trim();let radix=if s.starts_with("0x") || s.starts_with("0X"){Some(16)}else if s.starts_with("0o") || s.starts_with("0O"){Some(8)}else if s.starts_with("0b") || s.starts_with("0B"){Some(2)}else{None};
        if let Some(radix)=radix {let digits=&s[2..];if digits.is_empty(){return None;}digits.chars().try_fold(0.0,|value,digit|digit.to_digit(radix).map(|digit|value*f64::from(radix)+f64::from(digit)))}else{s.parse().ok()}
    }))).filter(|n|n.is_finite())
}
fn numeric_signal(value: &Value, expired: bool) -> bool {
    if !value.is_object() { return false; }
    let code = number(value.get("code"));
    if code == Some(-32001.0) || (expired && code == Some(-32000.0) && value.get("message").and_then(Value::as_str).is_some_and(|s| s.to_lowercase().contains("session"))) { return true; }
    for key in ["status","statusCode"] {
        if let Some(status) = number(value.get(key))
            && (status == 404.0 || (!expired && (status == 502.0 || status == 503.0))) { return true; }
    }
    ["response","cause"].iter().any(|k| value.get(k).is_some_and(|v| numeric_signal(v,expired)))
}
fn collect_text(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) => vec![s.clone()], Value::Number(n) => vec![n.to_string()],
        Value::Object(map) => {
            let mut parts = Vec::new();
            for key in ["message","name","code","status","statusCode"] { if let Some(v @ (Value::String(_) | Value::Number(_))) = map.get(key) { parts.extend(collect_text(v)); } }
            for key in ["response","cause"] { if let Some(v) = map.get(key) { parts.extend(collect_text(v)); } }
            parts
        }
        _ => Vec::new(),
    }
}
pub(crate) fn has_status_word(text: &str, words: &[&str]) -> bool { text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).any(|part| words.contains(&part)) }
