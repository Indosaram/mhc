use crate::lsp::errors::LspError;
use crate::lsp::types::SeverityFilter;
use serde_json::Map;
use serde_json::Value;

pub fn is_record(value: &Value) -> bool {
    value.is_object()
}

pub(crate) fn require_string(params: &Map<String, Value>, key: &str) -> Result<String, LspError> {
    match params.get(key) {
        Some(Value::String(value)) if !value.is_empty() => Ok(value.clone()),
        _ => Err(LspError::other(format!(
            "Missing required string parameter '{key}'"
        ))),
    }
}

pub(crate) fn optional_string(params: &Map<String, Value>, key: &str) -> Option<String> {
    params.get(key).and_then(Value::as_str).map(str::to_string)
}

pub(crate) fn require_number(params: &Map<String, Value>, key: &str) -> Result<f64, LspError> {
    optional_number(params, key)
        .ok_or_else(|| LspError::other(format!("Missing required number parameter '{key}'")))
}

pub(crate) fn optional_number(params: &Map<String, Value>, key: &str) -> Option<f64> {
    params
        .get(key)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
}

pub(crate) fn optional_boolean(params: &Map<String, Value>, key: &str) -> Option<bool> {
    params.get(key).and_then(Value::as_bool)
}

pub(crate) fn severity_filter(params: &Map<String, Value>) -> SeverityFilter {
    match params.get("severity").and_then(Value::as_str) {
        Some("error") => SeverityFilter::Error,
        Some("warning") => SeverityFilter::Warning,
        Some("information") => SeverityFilter::Information,
        Some("hint") => SeverityFilter::Hint,
        _ => SeverityFilter::All,
    }
}

/// LSP positions are unsigned; JS forwarded the raw number, so truncate toward zero.
pub(crate) fn position_arg(value: f64) -> u32 {
    value.clamp(0.0, f64::from(u32::MAX)) as u32
}

/// Renders a JS number the way template literals do (`3`, not `3.0`).
pub(crate) fn js_number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e21 {
        format!("{}", value as i64)
    } else {
        value.to_string()
    }
}

pub(crate) fn number_value(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() < 9.0e15 {
        Value::from(value as i64)
    } else {
        Value::from(value)
    }
}
