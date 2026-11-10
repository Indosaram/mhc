use serde_json::{Map, Value, json};
use std::collections::HashSet;

const KIND_NAMES: [&str; 18] = [
    "any",
    "array",
    "boolean",
    "enum",
    "integer",
    "intersect",
    "intersection",
    "literal",
    "never",
    "null",
    "nullable",
    "number",
    "object",
    "optional",
    "string",
    "undefined",
    "union",
    "unknown",
];

const SCHEMA_KEYS: [&str; 16] = [
    "$ref",
    "type",
    "properties",
    "required",
    "items",
    "enum",
    "const",
    "anyOf",
    "oneOf",
    "allOf",
    "definitions",
    "$defs",
    "description",
    "title",
    "additionalProperties",
    "pattern",
];

const INTERNAL_KEYS: [&str; 9] = [
    "kind", "optional", "wrapped", "inner", "element", "values", "variants", "value", "nullable",
];

const MAP_KEYS: [&str; 5] = [
    "properties",
    "$defs",
    "definitions",
    "dependentSchemas",
    "patternProperties",
];

const ARRAY_KEYS: [&str; 4] = ["allOf", "anyOf", "oneOf", "prefixItems"];

const SCHEMA_VALUE_KEYS: [&str; 12] = [
    "items",
    "not",
    "if",
    "then",
    "else",
    "contains",
    "additionalItems",
    "additionalProperties",
    "propertyNames",
    "contentSchema",
    "unevaluatedItems",
    "unevaluatedProperties",
];

pub fn to_json_schema(parameters: &Value) -> Value {
    to_json_schema_with_cycle_guard(parameters)
}

pub fn to_json_schema_with_cycle_guard(value: &Value) -> Value {
    convert(value, &mut HashSet::new())
}

fn convert(value: &Value, seen: &mut HashSet<*const Value>) -> Value {
    let Some(schema) = value.as_object() else {
        return if value.is_boolean() {
            value.clone()
        } else {
            json!({})
        };
    };
    let pointer = value as *const Value;
    if !seen.insert(pointer) {
        return json!({});
    }
    let out = convert_object(schema, seen);
    seen.remove(&pointer);
    out
}

fn convert_object(schema: &Map<String, Value>, seen: &mut HashSet<*const Value>) -> Value {
    let kind = schema.get("kind").and_then(Value::as_str).or_else(|| {
        schema
            .get("type")
            .and_then(Value::as_str)
            .filter(|kind| KIND_NAMES.contains(kind))
    });
    let Some(kind) = kind else {
        return if schema.is_empty() || schema.keys().any(|key| SCHEMA_KEYS.contains(&key.as_str()))
        {
            copy(schema, seen)
        } else {
            json!({})
        };
    };
    match kind {
        "optional" => convert(inner(schema), seen),
        "nullable" => nullable(convert(inner(schema), seen)),
        "undefined" | "never" | "any" | "unknown" => json!({}),
        "union" | "intersect" | "intersection" => union(kind, schema, seen),
        "object" => object(schema, seen),
        "array" => array(schema, seen),
        "enum" => with_values(schema, seen, "enum"),
        "literal" => with_values(schema, seen, "const"),
        "string" | "number" | "boolean" | "integer" | "null" => {
            let mut out = copy(schema, seen);
            out["type"] = json!(kind);
            out
        }
        _ => json!({}),
    }
}

fn inner(schema: &Map<String, Value>) -> &Value {
    schema
        .get("wrapped")
        .or_else(|| schema.get("inner"))
        .unwrap_or(&Value::Null)
}

fn union(kind: &str, schema: &Map<String, Value>, seen: &mut HashSet<*const Value>) -> Value {
    let variants = schema
        .get("variants")
        .or_else(|| schema.get("anyOf"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let converted: Vec<Value> = variants
        .iter()
        .map(|variant| convert(variant, seen))
        .filter(|variant| variant.as_object().is_none_or(|object| !object.is_empty()))
        .collect();
    match converted.len() {
        0 => json!({}),
        1 => converted[0].clone(),
        _ if kind == "union" => json!({ "anyOf": converted }),
        _ => json!({ "allOf": converted }),
    }
}

fn object(schema: &Map<String, Value>, seen: &mut HashSet<*const Value>) -> Value {
    let mut out = copy(schema, seen);
    out["type"] = json!("object");
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return out;
    };
    let mut required = Vec::new();
    let mut converted = Map::new();
    for (key, value) in properties {
        converted.insert(key.clone(), convert(value, seen));
        let listed_optional = schema
            .get("optional")
            .and_then(Value::as_array)
            .is_some_and(|optional| optional.contains(&json!(key)));
        let marked_optional = value.get("kind").and_then(Value::as_str) == Some("optional");
        if !listed_optional && !marked_optional {
            required.push(json!(key));
        }
    }
    out["properties"] = Value::Object(converted);
    if let Some(listed) = schema.get("required").and_then(Value::as_array) {
        out["required"] = json!(listed);
    } else if !required.is_empty() {
        out["required"] = json!(required);
    } else if let Some(object) = out.as_object_mut() {
        object.remove("required");
    }
    out
}

fn array(schema: &Map<String, Value>, seen: &mut HashSet<*const Value>) -> Value {
    let mut out = copy(schema, seen);
    out["type"] = json!("array");
    if out.get("items").is_none()
        && let Some(element) = schema.get("element")
    {
        out["items"] = convert(element, seen);
    }
    out
}

fn with_values(
    schema: &Map<String, Value>,
    seen: &mut HashSet<*const Value>,
    target: &str,
) -> Value {
    let mut out = copy(schema, seen);
    if out.get(target).is_none()
        && let Some(value) = schema.get("value")
    {
        out[target] = value.clone();
    }
    out
}

fn copy(schema: &Map<String, Value>, seen: &mut HashSet<*const Value>) -> Value {
    let mut out = Map::new();
    for (key, value) in schema {
        if INTERNAL_KEYS.contains(&key.as_str()) {
            continue;
        }
        let converted = if MAP_KEYS.contains(&key.as_str()) {
            let mut map = Map::new();
            if let Some(source) = value.as_object() {
                for (name, child) in source {
                    map.insert(name.clone(), convert(child, seen));
                }
            }
            Value::Object(map)
        } else if ARRAY_KEYS.contains(&key.as_str()) {
            Value::Array(
                value
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|child| convert(child, seen))
                    .collect(),
            )
        } else if SCHEMA_VALUE_KEYS.contains(&key.as_str()) {
            convert(value, seen)
        } else {
            value.clone()
        };
        out.insert(key.clone(), converted);
    }
    let out = Value::Object(out);
    if schema.get("nullable") == Some(&json!(true)) {
        nullable(out)
    } else {
        out
    }
}

fn nullable(value: Value) -> Value {
    if let Some(kind) = value.get("type").and_then(Value::as_str) {
        let mut out = value.as_object().cloned().unwrap_or_default();
        out.insert("type".into(), json!([kind, "null"]));
        Value::Object(out)
    } else {
        json!({ "anyOf": [value, { "type": "null" }] })
    }
}
