//! JSON record shape guards.

use serde_json::Value;

/// Legacy guard: arrays count as records (`typeof value === "object"`).
pub fn is_record(value: &Value) -> bool {
    matches!(value, Value::Object(_) | Value::Array(_))
}

pub fn is_plain_record(value: &Value) -> bool {
    value.is_object()
}
