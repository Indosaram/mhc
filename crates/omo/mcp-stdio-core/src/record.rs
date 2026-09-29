use serde_json::Value;

pub fn is_plain_record(value: &Value) -> bool {
    matches!(value, Value::Object(_))
}
