//! The JSON Schema subset a session-callable op declares for its arguments, and the check the store
//! runs against it.
//!
//! Upstream counterpart: `thread/gateway/session-args-schema.ts`. Hand-written so the worker never
//! loads a schema library: the subset covers what a plain argument object emits, and `register()`
//! refuses every other keyword, so no declared constraint is ever skipped silently.

use serde_json::{Map, Value};

const TYPES: &[&str] = &["object", "string", "number", "integer", "boolean", "array", "null"];
const ANNOTATIONS: &[&str] = &["title", "description", "default", "examples"];
const COUNTS: &[&str] = &["minLength", "maxLength", "minItems", "maxItems"];
const BOUNDS: &[&str] = &["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"];

fn is_keyword(key: &str) -> bool {
    matches!(key, "type" | "properties" | "required" | "additionalProperties" | "enum" | "const" | "pattern" | "items" | "anyOf")
        || COUNTS.contains(&key)
        || BOUNDS.contains(&key)
        || ANNOTATIONS.contains(&key)
}

/// One place a value breaks a schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgumentFailure {
    pub path: String,
    pub message: String,
}

fn is_record(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

fn count_of(value: &Value) -> Option<i64> {
    value.as_i64()
}

/// Why a declared schema is outside the subset, or none. Keys starting with `~` are typebox metadata.
pub fn schema_problem(schema: &Value, path: &str) -> Option<String> {
    let Some(object) = is_record(schema) else {
        return Some(format!("{path} must be a schema object"));
    };
    for key in object.keys() {
        if !key.starts_with('~') && !is_keyword(key) {
            return Some(format!("{path} uses {key}, which the store cannot check"));
        }
    }
    let types: Vec<&Value> = match object.get("type") {
        None => Vec::new(),
        Some(Value::Array(entries)) if entries.is_empty() => return Some(format!("{path}.type must name JSON types")),
        Some(Value::Array(entries)) => entries.iter().collect(),
        Some(single) => vec![single],
    };
    if types.iter().any(|entry| entry.as_str().is_none_or(|name| !TYPES.contains(&name))) {
        return Some(format!("{path}.type must name JSON types"));
    }
    if let Some(properties) = object.get("properties") {
        let Some(properties) = is_record(properties) else {
            return Some(format!("{path}.properties must be an object"));
        };
        for (name, child) in properties {
            if let Some(problem) = schema_problem(child, &format!("{path}.properties.{name}")) {
                return Some(problem);
            }
        }
    }
    if let Some(required) = object.get("required")
        && !required.as_array().is_some_and(|entries| entries.iter().all(|entry| entry.as_str().is_some()))
    {
        return Some(format!("{path}.required must list property names"));
    }
    if let Some(additional) = object.get("additionalProperties")
        && !additional.is_boolean()
    {
        return Some(format!("{path}.additionalProperties must be true or false"));
    }
    if let Some(entries) = object.get("enum")
        && entries.as_array().is_none_or(|entries| entries.is_empty())
    {
        return Some(format!("{path}.enum must be a non-empty array"));
    }
    if let Some(pattern) = object.get("pattern")
        && !pattern.is_string()
    {
        return Some(format!("{path}.pattern must be a string"));
    }
    for key in COUNTS {
        if let Some(value) = object.get(*key)
            && count_of(value).is_none_or(|count| count < 0)
        {
            return Some(format!("{path}.{key} must be a non-negative integer"));
        }
    }
    for key in BOUNDS {
        if let Some(value) = object.get(*key)
            && value.as_f64().is_none_or(|number| !number.is_finite())
        {
            return Some(format!("{path}.{key} must be a finite number"));
        }
    }
    if let Some(items) = object.get("items")
        && let Some(problem) = schema_problem(items, &format!("{path}.items"))
    {
        return Some(problem);
    }
    if let Some(any_of) = object.get("anyOf") {
        let Some(entries) = any_of.as_array().filter(|entries| !entries.is_empty()) else {
            return Some(format!("{path}.anyOf must be a non-empty array"));
        };
        for (index, child) in entries.iter().enumerate() {
            if let Some(problem) = schema_problem(child, &format!("{path}.anyOf.{index}")) {
                return Some(problem);
            }
        }
    }
    None
}

fn type_matches(name: &str, value: &Value) -> bool {
    match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "number" => value.as_f64().is_some_and(f64::is_finite),
        "integer" => value.as_i64().is_some() || value.as_f64().is_some_and(|number| number.fract() == 0.0),
        _ => false,
    }
}

fn equal(left: &Value, right: &Value) -> bool {
    left == right
}

fn pointer(path: &str, key: &str) -> String {
    format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"))
}

/// The first place `value` breaks `schema`, or none.
pub fn argument_failure(schema: &Value, value: &Value, path: &str) -> Option<ArgumentFailure> {
    let fail = |message: String| ArgumentFailure { path: path.to_owned(), message };
    let object = schema.as_object()?;
    if let Some(type_field) = object.get("type") {
        let types: Vec<&str> = match type_field {
            Value::Array(entries) => entries.iter().filter_map(Value::as_str).collect(),
            Value::String(name) => vec![name.as_str()],
            _ => Vec::new(),
        };
        if !types.iter().any(|name| type_matches(name, value)) {
            return Some(fail(format!("must be {}", types.join(" or "))));
        }
    }
    if let Some(constant) = object.get("const")
        && !equal(constant, value)
    {
        return Some(fail(format!("must be {constant}")));
    }
    if let Some(entries) = object.get("enum").and_then(Value::as_array)
        && !entries.iter().any(|entry| equal(entry, value))
    {
        return Some(fail("must be one of the allowed values".to_owned()));
    }
    if let Some(text) = value.as_str() {
        let length = text.chars().count() as i64;
        if let Some(minimum) = object.get("minLength").and_then(count_of)
            && length < minimum
        {
            return Some(fail(format!("must have at least {minimum} characters")));
        }
        if let Some(maximum) = object.get("maxLength").and_then(count_of)
            && length > maximum
        {
            return Some(fail(format!("must have at most {maximum} characters")));
        }
        if let Some(pattern) = object.get("pattern").and_then(Value::as_str) {
            let compiled = regex::Regex::new(pattern).ok()?;
            if !compiled.is_match(text) {
                return Some(fail(format!("must match pattern {pattern}")));
            }
        }
    }
    if let Some(number) = value.as_f64() {
        if let Some(minimum) = object.get("minimum").and_then(Value::as_f64)
            && number < minimum
        {
            return Some(fail(format!("must be >= {minimum}")));
        }
        if let Some(maximum) = object.get("maximum").and_then(Value::as_f64)
            && number > maximum
        {
            return Some(fail(format!("must be <= {maximum}")));
        }
        if let Some(minimum) = object.get("exclusiveMinimum").and_then(Value::as_f64)
            && number <= minimum
        {
            return Some(fail(format!("must be > {minimum}")));
        }
        if let Some(maximum) = object.get("exclusiveMaximum").and_then(Value::as_f64)
            && number >= maximum
        {
            return Some(fail(format!("must be < {maximum}")));
        }
    }
    if let Some(entries) = value.as_array() {
        if let Some(minimum) = object.get("minItems").and_then(count_of)
            && (entries.len() as i64) < minimum
        {
            return Some(fail(format!("must have at least {minimum} items")));
        }
        if let Some(maximum) = object.get("maxItems").and_then(count_of)
            && (entries.len() as i64) > maximum
        {
            return Some(fail(format!("must have at most {maximum} items")));
        }
        if let Some(items) = object.get("items").filter(|items| items.is_object()) {
            for (index, entry) in entries.iter().enumerate() {
                if let Some(failure) = argument_failure(items, entry, &pointer(path, &index.to_string())) {
                    return Some(failure);
                }
            }
        }
    }
    if let Some(properties) = value.as_object() {
        let declared = object.get("properties").and_then(Value::as_object);
        if let Some(required) = object.get("required").and_then(Value::as_array) {
            for name in required.iter().filter_map(Value::as_str) {
                if !properties.contains_key(name) {
                    return Some(fail(format!("must have property {name}")));
                }
            }
        }
        for (name, entry) in properties {
            match declared.and_then(|declared| declared.get(name)) {
                Some(child) if child.is_object() => {
                    if let Some(failure) = argument_failure(child, entry, &pointer(path, name)) {
                        return Some(failure);
                    }
                }
                _ if object.get("additionalProperties") == Some(&Value::Bool(false)) => {
                    return Some(fail(format!("must not have property {name}")));
                }
                _ => {}
            }
        }
    }
    if let Some(branches) = object.get("anyOf").and_then(Value::as_array)
        && !branches.iter().any(|branch| branch.is_object() && argument_failure(branch, value, path).is_none())
    {
        return Some(fail("must match one of the allowed shapes".to_owned()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object_schema(properties: Value, required: Value) -> Value {
        json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false })
    }

    #[test]
    fn a_supported_object_schema_has_no_problem() {
        let schema = object_schema(json!({ "thread": { "type": "string", "minLength": 1 } }), json!(["thread"]));
        assert_eq!(schema_problem(&schema, "parameters"), None);
    }

    #[test]
    fn unknown_keywords_are_refused_not_ignored() {
        let schema = json!({ "type": "object", "oneOf": [] });
        assert_eq!(schema_problem(&schema, "parameters"), Some("parameters uses oneOf, which the store cannot check".to_owned()));
        let metadata = json!({ "type": "object", "~kind": "Object" });
        assert_eq!(schema_problem(&metadata, "parameters"), None);
    }

    #[test]
    fn declared_constraints_are_checked() {
        let schema = object_schema(json!({ "count": { "type": "integer", "minimum": 2 } }), json!(["count"]));
        assert_eq!(argument_failure(&schema, &json!({ "count": 1 }), ""), Some(ArgumentFailure { path: "/count".into(), message: "must be >= 2".into() }));
        assert_eq!(argument_failure(&schema, &json!({}), ""), Some(ArgumentFailure { path: "".into(), message: "must have property count".into() }));
        assert_eq!(argument_failure(&schema, &json!({ "count": 3, "extra": true }), ""), Some(ArgumentFailure { path: "".into(), message: "must not have property extra".into() }));
        assert_eq!(argument_failure(&schema, &json!({ "count": 3 }), ""), None);
    }

    #[test]
    fn arrays_anyof_and_enums_are_checked() {
        let schema = json!({ "type": "array", "minItems": 2, "items": { "type": "string", "enum": ["a", "b"] } });
        assert_eq!(argument_failure(&schema, &json!(["a"]), "").map(|failure| failure.message), Some("must have at least 2 items".to_owned()));
        assert_eq!(argument_failure(&schema, &json!(["a", "c"]), "").map(|failure| failure.path), Some("/1".to_owned()));
        assert_eq!(argument_failure(&schema, &json!(["a", "b"]), ""), None);
        let any = json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] });
        assert_eq!(argument_failure(&any, &json!(null), ""), None);
        assert_eq!(argument_failure(&any, &json!(3), "").map(|failure| failure.message), Some("must match one of the allowed shapes".to_owned()));
    }
}
