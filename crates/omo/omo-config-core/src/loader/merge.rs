use serde_json::{Map, Value};

use crate::internal::plain_object::is_unsafe_object_key;

fn sanitize_omo_config_value(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(sanitize_omo_config_value).collect()),
        Value::Object(map) => {
            let mut sanitized = Map::new();
            for (key, entry) in map {
                if is_unsafe_object_key(key) {
                    continue;
                }
                sanitized.insert(key.clone(), sanitize_omo_config_value(entry));
            }
            Value::Object(sanitized)
        }
        other => other.clone(),
    }
}

fn merge_codegraph_excluded_roots(base: &[Value], override_values: &[Value]) -> Vec<Value> {
    let mut merged: Vec<Value> = Vec::new();
    for entry in base.iter().chain(override_values.iter()) {
        if !merged.contains(entry) {
            merged.push(entry.clone());
        }
    }
    merged
}

pub fn merge_omo_config_records(
    base: &Map<String, Value>,
    override_records: &Map<String, Value>,
    parent_key: Option<&str>,
) -> Map<String, Value> {
    let mut result = base.clone();

    for (key, value) in override_records {
        if is_unsafe_object_key(key) {
            continue;
        }
        let safe_value = sanitize_omo_config_value(value);
        let base_value = result.get(key).cloned();
        let merged = match (&base_value, &safe_value) {
            (Some(Value::Array(base_items)), Value::Array(override_items))
                if key == "excluded_roots" && parent_key == Some("codegraph") =>
            {
                Value::Array(merge_codegraph_excluded_roots(base_items, override_items))
            }
            (Some(Value::Object(base_map)), Value::Object(override_map)) => {
                Value::Object(merge_omo_config_records(base_map, override_map, Some(key)))
            }
            _ => safe_value,
        };
        result.insert(key.clone(), merged);
    }

    result
}
