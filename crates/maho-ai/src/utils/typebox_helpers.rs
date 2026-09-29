//! Port of senpi packages/ai/src/utils/typebox-helpers.ts.

use serde_json::{Map, Value, json};

/// `StringEnum`: a JSON-schema string enum (description/default only when non-empty).
pub fn string_enum(values: &[&str], description: Option<&str>, default: Option<&str>) -> Value {
    let mut schema = Map::new();
    schema.insert("type".into(), json!("string"));
    schema.insert("enum".into(), json!(values));
    if let Some(description) = description.filter(|d| !d.is_empty()) {
        schema.insert("description".into(), json!(description));
    }
    if let Some(default) = default.filter(|d| !d.is_empty()) {
        schema.insert("default".into(), json!(default));
    }
    Value::Object(schema)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_string_enum_schema() {
        assert_eq!(string_enum(&["a", "b"], None, Some("")), json!({"type": "string", "enum": ["a", "b"]}));
        assert_eq!(
            string_enum(&["a"], Some("pick"), Some("a")),
            json!({"type": "string", "enum": ["a"], "description": "pick", "default": "a"})
        );
    }
}
