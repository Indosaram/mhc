//! Recursive object merge with prototype-pollution guards.

use serde_json::{Map, Value};

const MAX_DEPTH: usize = 50;

pub use omo_config_core::is_unsafe_object_key;

/// True for JSON objects (the only "plain object" shape a `serde_json::Value` can take).
pub fn is_plain_object(value: &Value) -> bool {
    value.is_object()
}

/// Deep merges two objects, with `override_value` taking precedence.
/// Objects merge recursively, arrays are replaced, unsafe keys are skipped.
pub fn deep_merge(
    base: Option<&Map<String, Value>>,
    override_value: Option<&Map<String, Value>>,
) -> Option<Map<String, Value>> {
    merge_at_depth(base, override_value, 0)
}

fn merge_at_depth(
    base: Option<&Map<String, Value>>,
    override_value: Option<&Map<String, Value>>,
    depth: usize,
) -> Option<Map<String, Value>> {
    let (base, override_value) = match (base, override_value) {
        (None, None) => return None,
        (None, Some(over)) => return Some(over.clone()),
        (Some(base), None) => return Some(base.clone()),
        (Some(base), Some(over)) => (base, over),
    };
    if depth > MAX_DEPTH {
        return Some(override_value.clone());
    }
    let mut result = base.clone();
    for (key, over) in override_value {
        if is_unsafe_object_key(key) {
            continue;
        }
        let merged = match (base.get(key), over) {
            (Some(Value::Object(base_obj)), Value::Object(over_obj)) => {
                merge_at_depth(Some(base_obj), Some(over_obj), depth + 1)
                    .map_or(Value::Null, Value::Object)
            }
            _ => over.clone(),
        };
        result.insert(key.clone(), merged);
    }
    Some(result)
}
