//! Port of senpi packages/ai/src/utils/diagnostics.ts.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DiagnosticCode {
    Text(String),
    Number(f64),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticErrorInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<DiagnosticCode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantMessageDiagnostic {
    #[serde(rename = "type")]
    pub kind: String,
    pub timestamp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<DiagnosticErrorInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Map<String, Value>>,
}

/// A thrown value: an `Error` (name/message/code) or any other value.
#[derive(Debug, Clone, PartialEq)]
pub enum Thrown {
    Error { name: String, message: String, stack: Option<String>, code: Option<DiagnosticCode> },
    Value(Value),
}

impl Thrown {
    pub fn error(name: impl Into<String>, message: impl Into<String>) -> Self {
        Thrown::Error { name: name.into(), message: message.into(), stack: None, code: None }
    }
}

pub fn format_thrown_value(value: &Thrown) -> String {
    match value {
        Thrown::Error { name, message, .. } => if message.is_empty() { name.clone() } else { message.clone() },
        Thrown::Value(Value::String(text)) => text.clone(),
        Thrown::Value(other) => js_string(other),
    }
}

/// `String(value)` for JSON-representable values.
pub fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(|v| if v.is_null() { String::new() } else { js_string(v) }).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

pub fn extract_diagnostic_error(error: &Thrown) -> DiagnosticErrorInfo {
    match error {
        Thrown::Error { name, message, stack, code } => DiagnosticErrorInfo {
            name: if name.is_empty() { None } else { Some(name.clone()) },
            message: if message.is_empty() { name.clone() } else { message.clone() },
            stack: stack.clone(),
            code: code.clone(),
        },
        Thrown::Value(_) => DiagnosticErrorInfo {
            name: Some("ThrownValue".into()),
            message: format_thrown_value(error),
            stack: None,
            code: None,
        },
    }
}

pub fn create_assistant_message_diagnostic(
    kind: &str,
    error: &Thrown,
    details: Option<Map<String, Value>>,
) -> AssistantMessageDiagnostic {
    AssistantMessageDiagnostic {
        kind: kind.into(),
        timestamp: now_ms(),
        error: Some(extract_diagnostic_error(error)),
        details,
    }
}

pub fn append_assistant_message_diagnostic(
    diagnostics: &mut Option<Vec<AssistantMessageDiagnostic>>,
    diagnostic: AssistantMessageDiagnostic,
) {
    diagnostics.get_or_insert_with(Vec::new).push(diagnostic);
}

/// `Date.now()`.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn formats_and_extracts() {
        assert_eq!(format_thrown_value(&Thrown::error("TypeError", "")), "TypeError");
        assert_eq!(format_thrown_value(&Thrown::Value(json!("s"))), "s");
        assert_eq!(format_thrown_value(&Thrown::Value(json!(3))), "3");
        let info = extract_diagnostic_error(&Thrown::Value(json!({})));
        assert_eq!(info.name.as_deref(), Some("ThrownValue"));
        assert_eq!(info.message, "[object Object]");
        let info = extract_diagnostic_error(&Thrown::error("", "m"));
        assert_eq!(info.name, None);
        let mut diagnostics = None;
        append_assistant_message_diagnostic(&mut diagnostics, create_assistant_message_diagnostic("t", &Thrown::error("E", "m"), None));
        assert_eq!(diagnostics.map(|d| d.len()), Some(1));
    }
}
