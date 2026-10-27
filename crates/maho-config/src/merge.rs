//! Deep merge (spec §5) and control-key handling (§6).

use crate::model_string::HARNESS_BLOCKS;
use crate::schema::is_unsafe_key;
use serde_json::{Map, Value};

/// Merges `incoming` over `base`: objects merge key by key, anything else replaces; unsafe keys
/// on the incoming side are dropped at every depth.
pub fn deep_merge(base: &mut Value, incoming: &Value) {
    match (base, incoming) {
        (Value::Object(target), Value::Object(source)) => merge_maps(target, source),
        (base, incoming) => *base = sanitized(incoming),
    }
}

fn merge_maps(target: &mut Map<String, Value>, source: &Map<String, Value>) {
    for (key, value) in source {
        if is_unsafe_key(key) {
            continue;
        }
        match target.get_mut(key) {
            Some(existing) if existing.is_object() && value.is_object() => {
                deep_merge(existing, value)
            }
            _ => {
                target.insert(key.clone(), sanitized(value));
            }
        }
    }
}

fn sanitized(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| !is_unsafe_key(key))
                .map(|(key, value)| (key.clone(), sanitized(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(sanitized).collect()),
        other => other.clone(),
    }
}

/// Whether `key` steers resolution and never appears in a view.
pub fn is_control_key(key: &str) -> bool {
    key == "profiles" || HARNESS_BLOCKS.contains(&key)
}

/// A copy of `value` without control keys (top level only).
pub fn without_control_keys(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| !is_control_key(key))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The overlay for harness `native`: `[senpi]` first, then `[native]` on top.
pub fn native_overlay(container: &Value) -> Value {
    let mut overlay = Value::Object(Map::new());
    for block in ["[senpi]", "[native]"] {
        if let Some(inner) = container.get(block).filter(|v| v.is_object()) {
            deep_merge(&mut overlay, &without_control_keys(inner));
        }
    }
    overlay
}
