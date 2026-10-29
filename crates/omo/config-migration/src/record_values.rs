use serde_json::{Map, Value};

const UNSAFE_KEYS: [&str; 3] = ["__proto__", "constructor", "prototype"];

pub(crate) fn is_unsafe_key(key: &str) -> bool {
    UNSAFE_KEYS.contains(&key)
}

pub(crate) fn clone_value(value: &Value) -> Value {
    match value {
        Value::Array(entries) => Value::Array(entries.iter().map(clone_value).collect()),
        Value::Object(record) => Value::Object(copy_record(record)),
        other => other.clone(),
    }
}

pub fn copy_record(value: &Map<String, Value>) -> Map<String, Value> {
    value
        .iter()
        .filter(|(key, _)| !is_unsafe_key(key))
        .map(|(key, entry)| (key.clone(), clone_value(entry)))
        .collect()
}

pub fn merge_records(base: &Map<String, Value>, over: &Map<String, Value>) -> Map<String, Value> {
    let mut merged = copy_record(base);
    for (key, override_value) in over {
        if is_unsafe_key(key) {
            continue;
        }
        let next = match (merged.get(key), override_value) {
            (Some(Value::Object(base_value)), Value::Object(override_record)) => {
                Value::Object(merge_records(base_value, override_record))
            }
            _ => clone_value(override_value),
        };
        merged.insert(key.clone(), next);
    }
    merged
}

pub fn without_legacy_metadata(value: &Map<String, Value>) -> Map<String, Value> {
    let mut copy = copy_record(value);
    for key in ["$schema", "_migrations", "appliedMigrations"] {
        copy.shift_remove(key);
    }
    copy
}
