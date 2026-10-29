use serde_json::{Map, Value};

use crate::internal::plain_object::{is_plain_object, is_unsafe_object_key};
use crate::issue::PathSegment;
use crate::writer::types::OmoConfigEdit;

#[derive(Debug, Clone, PartialEq)]
pub struct NoClobberMergeResult {
    pub additions: Value,
    pub diagnostics: Vec<String>,
    pub merged: Value,
}

fn display_value(value: &Value) -> String {
    value.to_string()
}

fn display_path(path: &[String]) -> String {
    path.join(".")
}

pub fn clone_value(value: &Value) -> Value {
    match value {
        Value::Array(arr) => Value::Array(arr.iter().map(clone_value).collect()),
        Value::Object(map) => {
            let mut clone = Map::new();
            for (key, val) in map {
                if is_unsafe_object_key(key) {
                    continue;
                }
                clone.insert(key.clone(), clone_value(val));
            }
            Value::Object(clone)
        }
        other => other.clone(),
    }
}

fn merge_into(
    existing: &Map<String, Value>,
    legacy: &Map<String, Value>,
    path: &[String],
    diagnostics: &mut Vec<String>,
) -> Map<String, Value> {
    let mut additions = Map::new();

    for (key, legacy_value) in legacy {
        if is_unsafe_object_key(key) {
            continue;
        }
        let mut next_path = path.to_vec();
        next_path.push(key.clone());

        if !existing.contains_key(key) {
            additions.insert(key.clone(), clone_value(legacy_value));
            continue;
        }

        let kept_value = &existing[key];
        if is_plain_object(kept_value) && is_plain_object(legacy_value) {
            let nested = merge_into(
                kept_value.as_object().unwrap(),
                legacy_value.as_object().unwrap(),
                &next_path,
                diagnostics,
            );
            if !nested.is_empty() {
                additions.insert(key.clone(), Value::Object(nested));
            }
            continue;
        }

        diagnostics.push(format!(
            "skipped: {} legacy={} kept={}",
            display_path(&next_path),
            display_value(legacy_value),
            display_value(kept_value)
        ));
    }

    additions
}

fn apply_additions(existing: &Value, additions: &Map<String, Value>) -> Value {
    let mut result = clone_value(existing);
    let result_map = match result.as_object_mut() {
        Some(map) => map,
        None => return result,
    };

    for (key, value) in additions {
        if is_unsafe_object_key(key) {
            continue;
        }
        let current = result_map.get(key);
        if let Some(curr_val) = current
            && is_plain_object(curr_val)
            && is_plain_object(value)
        {
            let merged = apply_additions(curr_val, value.as_object().unwrap());
            result_map.insert(key.clone(), merged);
            continue;
        }
        result_map.insert(key.clone(), clone_value(value));
    }

    result
}

pub fn merge_without_clobber(existing: &Value, legacy: &Value) -> NoClobberMergeResult {
    let empty_map = Map::new();
    let existing_map = existing.as_object().unwrap_or(&empty_map);
    let legacy_map = legacy.as_object().unwrap_or(&empty_map);

    let mut diagnostics = Vec::new();
    let additions_map = merge_into(existing_map, legacy_map, &[], &mut diagnostics);
    let merged = apply_additions(existing, &additions_map);

    NoClobberMergeResult {
        additions: Value::Object(additions_map),
        diagnostics,
        merged,
    }
}

pub fn collect_migration_edits(value: &Value, path: &[String]) -> Vec<OmoConfigEdit> {
    let mut edits = Vec::new();
    let map = match value.as_object() {
        Some(m) => m,
        None => return edits,
    };

    for (key, entry) in map {
        if is_unsafe_object_key(key) {
            continue;
        }
        let mut next_path = path.to_vec();
        next_path.push(key.clone());

        if is_plain_object(entry) && !entry.as_object().unwrap().is_empty() {
            edits.extend(collect_migration_edits(entry, &next_path));
        } else {
            let segments = next_path.into_iter().map(PathSegment::Key).collect();
            edits.push(OmoConfigEdit {
                path: segments,
                value: Some(clone_value(entry)),
            });
        }
    }

    edits
}
