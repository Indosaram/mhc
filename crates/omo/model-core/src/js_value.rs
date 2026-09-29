//! Helpers reproducing the JavaScript coercions the TypeScript sources rely on.

use serde_json::Value;

/// JavaScript truthiness of a JSON value (`!value` is false).
pub(crate) fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `typeof value === "object" && value !== null` (arrays included) then `value[key]`.
pub(crate) fn get<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    value?.as_object()?.get(key)
}

/// `JSON.stringify(value)`, keeping key insertion order.
pub(crate) fn json_stringify(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// `String(value)` for JSON values.
pub(crate) fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                if item.is_null() {
                    String::new()
                } else {
                    js_string(item)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_string(),
    }
}
