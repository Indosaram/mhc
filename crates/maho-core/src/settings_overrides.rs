//! Port of senpi packages/coding-agent/src/core/settings-overrides.ts.

use crate::settings_manager::Settings;

/// The override layer without field (or only field.nestedKey), so an explicit setter for that key
/// wins over a CLI/programmatic override while every other override keeps applying. Empty parents
/// are dropped; the input is not mutated.
pub fn without_override(overrides: &Settings, field: &str, nested_key: Option<&str>) -> Settings {
    if !overrides.contains_key(field) {
        return overrides.clone();
    }
    let mut next = overrides.clone();
    let current = next.get(field).cloned();
    let Some(current) = current else { return overrides.clone() };
    let Some(nested_key) = nested_key else {
        next.remove(field);
        return next;
    };
    let serde_json::Value::Object(mut nested) = current else {
        next.remove(field);
        return next;
    };
    if !nested.contains_key(nested_key) {
        return overrides.clone();
    }
    nested.remove(nested_key);
    if nested.is_empty() {
        next.remove(field);
    } else {
        next.insert(field.to_owned(), serde_json::Value::Object(nested));
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn settings(value: serde_json::Value) -> Settings {
        value.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn an_absent_field_returns_the_input_unchanged() {
        let overrides = settings(json!({ "a": 1 }));
        assert_eq!(without_override(&overrides, "b", None), overrides);
    }

    #[test]
    fn a_top_level_field_is_removed() {
        let overrides = settings(json!({ "a": 1, "b": 2 }));
        let result = without_override(&overrides, "a", None);
        assert!(!result.contains_key("a"));
        assert!(result.contains_key("b"));
    }

    #[test]
    fn a_nested_key_is_removed_and_the_parent_dropped_when_empty() {
        let overrides = settings(json!({ "compaction": { "reserveTokens": 1 } }));
        let result = without_override(&overrides, "compaction", Some("reserveTokens"));
        assert!(!result.contains_key("compaction"));
    }

    #[test]
    fn a_nested_key_keeps_siblings() {
        let overrides = settings(json!({ "compaction": { "reserveTokens": 1, "keepRecentTokens": 2 } }));
        let result = without_override(&overrides, "compaction", Some("reserveTokens"));
        assert_eq!(result["compaction"]["keepRecentTokens"], json!(2));
        assert!(result["compaction"].get("reserveTokens").is_none());
    }

    #[test]
    fn a_missing_nested_key_returns_the_input_unchanged() {
        let overrides = settings(json!({ "compaction": { "reserveTokens": 1 } }));
        assert_eq!(without_override(&overrides, "compaction", Some("nope")), overrides);
    }
}
