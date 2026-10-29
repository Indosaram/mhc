use serde_json::Value;

pub const DANGEROUS_KEYS: [&str; 3] = ["__proto__", "constructor", "prototype"];

pub fn is_unsafe_object_key(key: &str) -> bool {
    DANGEROUS_KEYS.contains(&key)
}

pub fn is_plain_object(value: &Value) -> bool {
    matches!(value, Value::Object(_))
}

pub fn to_record(value: &Value) -> Option<&serde_json::Map<String, Value>> {
    match value {
        Value::Object(map) => Some(map),
        _ => None,
    }
}
