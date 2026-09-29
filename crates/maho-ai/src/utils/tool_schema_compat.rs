//! Port of senpi packages/ai/src/utils/tool-schema-compat.ts.
//!
//! JS object key order matters for request bytes: `delete` keeps the remaining order
//! (`shift_remove`) and assignment to an existing key keeps its position.

use serde_json::{Map, Value};

pub use crate::types::ToolSchemaFlavor;

const COMBINER_KEYS: [&str; 3] = ["anyOf", "oneOf", "allOf"];
const SCHEMA_MAP_KEYS: [&str; 4] = ["properties", "patternProperties", "$defs", "definitions"];
const SCHEMA_SINGLE_KEYS: [&str; 8] = ["items", "additionalProperties", "contains", "propertyNames", "if", "then", "else", "not"];

fn is_scalar_type(value: &Value) -> Option<&str> {
    value.as_str().filter(|t| matches!(*t, "string" | "number" | "integer" | "boolean"))
}

fn has_combiner(node: &Map<String, Value>) -> bool {
    COMBINER_KEYS.iter().any(|key| node.get(*key).is_some_and(Value::is_array))
}

fn move_type_into_combiner_branches(node: &mut Map<String, Value>) {
    let Some(parent_type) = node.get("type").cloned() else { return };
    for combiner in COMBINER_KEYS {
        if let Some(Value::Array(branches)) = node.get_mut(combiner) {
            for branch in branches {
                if let Value::Object(branch) = branch
                    && !branch.contains_key("type")
                {
                    branch.insert("type".into(), parent_type.clone());
                }
            }
        }
    }
    node.shift_remove("type");
}

fn collapse_const_union(node: &mut Map<String, Value>) {
    let Some(Value::Array(branches)) = node.get("anyOf") else { return };
    if branches.len() < 2 || node.contains_key("type") {
        return;
    }
    let mut values = Vec::new();
    let mut shared_type: Option<String> = None;
    for branch in branches {
        let Value::Object(branch) = branch else { return };
        if !branch.contains_key("const") || branch.len() != 2 {
            return;
        }
        let Some(branch_type) = branch.get("type").and_then(is_scalar_type) else { return };
        match &shared_type {
            None => shared_type = Some(branch_type.to_owned()),
            Some(shared) if shared != branch_type => return,
            Some(_) => {}
        }
        values.push(branch["const"].clone());
    }
    let Some(shared_type) = shared_type.filter(|_| !values.is_empty()) else { return };
    node.shift_remove("anyOf");
    node.insert("type".into(), Value::String(shared_type));
    node.insert("enum".into(), Value::Array(values));
}

fn string_items(value: Option<&Value>) -> Vec<String> {
    value.and_then(Value::as_array).map(|items| items.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default()
}

fn merge_root_object_union(schema: &Map<String, Value>) -> Option<Map<String, Value>> {
    let branches = match (schema.get("anyOf"), schema.get("oneOf")) {
        (Some(Value::Array(any)), _) => any,
        (_, Some(Value::Array(one))) => one,
        _ => return None,
    };
    if branches.is_empty()
        || schema.get("properties").is_some_and(|p| !p.is_object())
        || schema.get("required").is_some_and(|r| !r.is_array())
    {
        return None;
    }
    let mut object_branches = Vec::new();
    for branch in branches {
        let Value::Object(branch) = branch else { return None };
        if branch.get("type").is_some_and(|t| t != "object")
            || branch.get("properties").is_some_and(|p| !p.is_object())
            || branch.get("required").is_some_and(|r| !r.is_array())
        {
            return None;
        }
        object_branches.push(branch);
    }
    let mut properties = schema.get("properties").and_then(Value::as_object).cloned().unwrap_or_default();
    for branch in &object_branches {
        let Some(Value::Object(branch_properties)) = branch.get("properties") else { continue };
        for (name, property_schema) in branch_properties {
            let merged = match properties.get(name) {
                Some(existing) if existing != property_schema => {
                    Value::Object(Map::from_iter([("anyOf".to_owned(), Value::Array(vec![existing.clone(), property_schema.clone()]))]))
                }
                _ => property_schema.clone(),
            };
            properties.insert(name.clone(), merged);
        }
    }
    let branch_required: Vec<Vec<String>> = object_branches.iter().map(|b| string_items(b.get("required"))).collect();
    let mut required: Vec<String> = Vec::new();
    let mut push_unique = |name: String| {
        if !required.contains(&name) {
            required.push(name);
        }
    };
    string_items(schema.get("required")).into_iter().for_each(&mut push_unique);
    if let Some(first) = branch_required.first() {
        let mut seen = Vec::new();
        for name in first {
            if !seen.contains(name) && branch_required.iter().all(|names| names.contains(name)) {
                seen.push(name.clone());
                push_unique(name.clone());
            }
        }
    }
    let mut result: Map<String, Value> = schema.iter().filter(|(k, _)| *k != "anyOf" && *k != "oneOf").map(|(k, v)| (k.clone(), v.clone())).collect();
    result.insert("type".into(), Value::String("object".into()));
    result.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        result.insert("required".into(), Value::Array(required.into_iter().map(Value::String).collect()));
    }
    Some(result)
}

fn map_children(node: &mut Map<String, Value>, visit: &dyn Fn(Value) -> Value) {
    for key in SCHEMA_SINGLE_KEYS {
        if let Some(child) = node.get_mut(key) {
            *child = visit(child.take());
        }
    }
    for key in SCHEMA_MAP_KEYS {
        if let Some(Value::Object(map)) = node.get_mut(key) {
            for child in map.values_mut() {
                *child = visit(child.take());
            }
        }
    }
}

fn normalize_node(node: Value, is_root: bool) -> Value {
    let mut node = match node {
        Value::Array(items) => return Value::Array(items.into_iter().map(|child| normalize_node(child, false)).collect()),
        Value::Object(node) => node,
        other => return other,
    };
    node.shift_remove("optional");
    if has_combiner(&node) && !is_root {
        move_type_into_combiner_branches(&mut node);
    }
    for combiner in COMBINER_KEYS {
        if let Some(Value::Array(branches)) = node.get_mut(combiner) {
            let taken = std::mem::take(branches);
            *branches = taken.into_iter().map(|b| normalize_node(b, false)).collect();
        }
    }
    if node.get("anyOf").is_some_and(Value::is_array) {
        collapse_const_union(&mut node);
    }
    map_children(&mut node, &|child| normalize_node(child, false));
    Value::Object(node)
}

fn ensure_root_object_schema(schema: Map<String, Value>) -> Map<String, Value> {
    if let Some(merged) = merge_root_object_union(&schema) {
        return merged;
    }
    if schema.contains_key("type") || has_combiner(&schema) {
        return schema;
    }
    let mut schema = schema;
    schema.insert("type".into(), Value::String("object".into()));
    schema
}

pub fn normalize_tool_parameters_for_openai_compat(schema: &Map<String, Value>) -> Map<String, Value> {
    match normalize_node(Value::Object(schema.clone()), true) {
        Value::Object(normalized) => ensure_root_object_schema(normalized),
        _ => unreachable!("an object normalizes to an object"),
    }
}

pub fn normalize_tool_parameters_for_moonshot(schema: &Map<String, Value>) -> Map<String, Value> {
    match strip_moonshot_annotations(Value::Object(normalize_tool_parameters_for_openai_compat(schema))) {
        Value::Object(stripped) => stripped,
        _ => unreachable!("an object strips to an object"),
    }
}

pub fn resolve_root_object_schema(schema: &Map<String, Value>) -> Map<String, Value> {
    merge_root_object_union(schema).unwrap_or_else(|| schema.clone())
}

fn strip_moonshot_annotations(node: Value) -> Value {
    let mut node = match node {
        Value::Array(items) => return Value::Array(items.into_iter().map(strip_moonshot_annotations).collect()),
        Value::Object(node) => node,
        other => return other,
    };
    for key in ["format", "examples", "readOnly", "writeOnly", "deprecated", "$schema", "$id"] {
        node.shift_remove(key);
    }
    for combiner in COMBINER_KEYS {
        if let Some(Value::Array(branches)) = node.get_mut(combiner) {
            let taken = std::mem::take(branches);
            *branches = taken.into_iter().map(strip_moonshot_annotations).collect();
        }
    }
    map_children(&mut node, &strip_moonshot_annotations);
    Value::Object(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("object")
    }

    #[test]
    fn normalizes_nested_combiners_and_const_unions() {
        let schema = obj(json!({
            "type": "object",
            "properties": {
                "mode": {"anyOf": [{"type": "string", "const": "a"}, {"type": "string", "const": "b"}], "optional": true},
                "value": {"type": "string", "anyOf": [{"minLength": 1}, {"type": "number"}]}
            }
        }));
        let normalized = normalize_tool_parameters_for_openai_compat(&schema);
        assert_eq!(
            Value::Object(normalized),
            json!({"type": "object", "properties": {
                "mode": {"type": "string", "enum": ["a", "b"]},
                "value": {"anyOf": [{"minLength": 1, "type": "string"}, {"type": "number"}]}
            }})
        );
        assert_eq!(normalize_tool_parameters_for_openai_compat(&obj(json!({"properties": {}}))), obj(json!({"properties": {}, "type": "object"})));
    }

    #[test]
    fn merges_root_object_unions() {
        let schema = obj(json!({
            "description": "d",
            "anyOf": [
                {"type": "object", "properties": {"a": {"type": "string"}, "b": {"type": "number"}}, "required": ["a", "b"]},
                {"properties": {"a": {"type": "string"}, "b": {"type": "string"}}, "required": ["a"]}
            ]
        }));
        assert_eq!(
            Value::Object(resolve_root_object_schema(&schema)),
            json!({"description": "d", "type": "object", "properties": {
                "a": {"type": "string"}, "b": {"anyOf": [{"type": "number"}, {"type": "string"}]}
            }, "required": ["a"]})
        );
        let mixed = obj(json!({"anyOf": [{"type": "string"}, {"type": "object"}]}));
        assert_eq!(resolve_root_object_schema(&mixed), mixed);
        assert_eq!(normalize_tool_parameters_for_openai_compat(&mixed), mixed);
    }

    #[test]
    fn moonshot_strips_annotations() {
        let schema = obj(json!({"$schema": "x", "type": "object", "properties": {"u": {"type": "string", "format": "uri", "examples": ["a"], "deprecated": true}}}));
        assert_eq!(Value::Object(normalize_tool_parameters_for_moonshot(&schema)), json!({"type": "object", "properties": {"u": {"type": "string"}}}));
    }
}
