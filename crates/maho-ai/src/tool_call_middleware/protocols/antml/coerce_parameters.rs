//! Port of senpi packages/ai/src/tool-call-middleware/protocols/antml/coerce-parameters.ts.

use std::collections::HashMap;
use std::sync::LazyLock;

use serde_json::{Map, Value};

use super::repair::{repair_lone_surrogates, repair_strings_deep, repair_unicode_escapes};
use crate::tool_call_middleware::protocols::anthropic_xml::coerce_parameters::trim_raw_boundary_newlines;
use crate::tool_call_middleware::protocols::anthropic_xml::invoke_tag_syntax::InvokeParameter;
use crate::tool_call_middleware::protocols::anthropic_xml::xml_entities::decode_xml_entities;
use crate::types::Tool;

enum Coerced {
    Ok(Value),
    Invalid,
}

const ALIAS_GROUPS: &[&[&str]] =
    &[&["file_path", "path", "filename", "file"], &["old_string", "old_str", "old_text"], &["new_string", "new_str", "new_text"], &["command", "cmd"]];

fn normalize_name(name: &str) -> String {
    name.to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}

static ALIAS_LOOKUP: LazyLock<HashMap<String, &'static [&'static str]>> = LazyLock::new(|| {
    let mut map = HashMap::new();
    for group in ALIAS_GROUPS {
        for name in *group {
            map.insert(normalize_name(name), *group);
        }
    }
    map
});

fn schema_type(schema: &Value) -> Option<&str> {
    schema.get("type").and_then(Value::as_str)
}

fn is_object_schema(schema: &Value) -> bool {
    schema_type(schema) == Some("object")
}

fn is_array_schema(schema: &Value) -> bool {
    schema_type(schema) == Some("array")
}

fn resolve_property_name(raw_name: &str, properties: &Map<String, Value>) -> Option<String> {
    if properties.contains_key(raw_name) {
        return Some(raw_name.to_string());
    }

    let normalized = normalize_name(raw_name);
    let by_normalized: Vec<&String> = properties.keys().filter(|property| normalize_name(property) == normalized).collect();
    if by_normalized.len() == 1 {
        return Some(by_normalized[0].clone());
    }
    if by_normalized.len() > 1 {
        return None;
    }

    let alias_group = ALIAS_LOOKUP.get(&normalized)?;
    let by_alias: Vec<&String> = properties
        .keys()
        .filter(|property| {
            let property_normalized = normalize_name(property);
            property_normalized != normalized && alias_group.iter().any(|alias| normalize_name(alias) == property_normalized)
        })
        .collect();
    if by_alias.len() == 1 { Some(by_alias[0].clone()) } else { None }
}

enum AdditionalSchema<'a> {
    True,
    Schema(&'a Value),
}

fn get_additional_properties_schema(schema: &Value) -> Option<AdditionalSchema<'_>> {
    match schema.get("additionalProperties") {
        Some(Value::Bool(true)) => Some(AdditionalSchema::True),
        Some(other) if other.is_object() => Some(AdditionalSchema::Schema(other)),
        _ => None,
    }
}

fn filter_unknown_keys_deep(value: Value, schema: &Value) -> Value {
    if is_array_schema(schema)
        && let Value::Array(items) = &value
        && let Some(item_schema) = schema.get("items").filter(|s| !s.is_array())
    {
        return Value::Array(items.iter().cloned().map(|entry| filter_unknown_keys_deep(entry, item_schema)).collect());
    }

    if is_object_schema(schema)
        && let Value::Object(object) = &value
    {
        let empty = Map::new();
        let properties = schema.get("properties").and_then(Value::as_object).unwrap_or(&empty);
        let additional = get_additional_properties_schema(schema);
        let mut filtered = Map::new();
        for (raw_key, entry) in object {
            if let Some(property_name) = resolve_property_name(raw_key, properties) {
                let property_schema = properties.get(&property_name);
                let value = match property_schema {
                    Some(schema) => filter_unknown_keys_deep(entry.clone(), schema),
                    None => entry.clone(),
                };
                filtered.insert(property_name, value);
                continue;
            }
            match &additional {
                Some(AdditionalSchema::True) => {
                    filtered.insert(raw_key.clone(), entry.clone());
                }
                Some(AdditionalSchema::Schema(schema)) => {
                    filtered.insert(raw_key.clone(), filter_unknown_keys_deep(entry.clone(), schema));
                }
                None => {}
            }
        }
        return Value::Object(filtered);
    }

    value
}

fn try_parse_json_tolerant(value: &str) -> Coerced {
    for candidate in [value.to_string(), repair_unicode_escapes(value)] {
        if let Ok(parsed) = serde_json::from_str::<Value>(&candidate) {
            return Coerced::Ok(repair_strings_deep(parsed));
        }
    }
    Coerced::Invalid
}

fn unwrap_json_scalar(value: &str) -> String {
    match try_parse_json_tolerant(value.trim()) {
        Coerced::Ok(Value::String(s)) => s,
        Coerced::Ok(Value::Number(n)) => n.to_string(),
        _ => value.to_string(),
    }
}

fn coerce_known_value(raw_value: &str, schema: &Value) -> Coerced {
    let value = decode_xml_entities(raw_value);

    match schema_type(schema) {
        Some("string") => Coerced::Ok(Value::String(repair_lone_surrogates(&value))),
        Some(t) if t == "number" || t == "integer" => {
            let candidate = unwrap_json_scalar(&value).trim().to_string();
            if candidate.is_empty() {
                return Coerced::Invalid;
            }
            match candidate.parse::<f64>() {
                Ok(parsed) if parsed.is_finite() && !(t == "integer" && parsed.fract() != 0.0) => {
                    Coerced::Ok(serde_json::Number::from_f64(parsed).map(Value::Number).unwrap_or(Value::Null))
                }
                _ => Coerced::Invalid,
            }
        }
        Some("boolean") => match unwrap_json_scalar(&value).trim().to_lowercase().as_str() {
            "true" => Coerced::Ok(Value::Bool(true)),
            "false" => Coerced::Ok(Value::Bool(false)),
            _ => Coerced::Invalid,
        },
        Some("array") => match try_parse_json_tolerant(&value) {
            Coerced::Ok(parsed @ Value::Array(_)) => Coerced::Ok(filter_unknown_keys_deep(parsed, schema)),
            _ => Coerced::Invalid,
        },
        Some("object") => match try_parse_json_tolerant(&value) {
            Coerced::Ok(parsed) if parsed.is_object() => Coerced::Ok(filter_unknown_keys_deep(parsed, schema)),
            _ => Coerced::Invalid,
        },
        _ => Coerced::Ok(coerce_unknown_value(raw_value)),
    }
}

fn coerce_unknown_value(raw_value: &str) -> Value {
    let value = decode_xml_entities(raw_value);
    match try_parse_json_tolerant(&value) {
        Coerced::Ok(parsed) => parsed,
        Coerced::Invalid => Value::String(repair_lone_surrogates(&value)),
    }
}

fn schema_accepts(schema: &Value, value: &Value) -> bool {
    let Some(schema_type) = schema_type(schema) else { return true };
    match schema_type {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_f64().is_some_and(|n| n.fract() == 0.0),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "array" => match value.as_array() {
            Some(items) => match schema.get("items") {
                Some(item_schema) if item_schema.is_object() => items.iter().all(|item| schema_accepts(item_schema, item)),
                _ => true,
            },
            None => false,
        },
        "object" => {
            let Some(object) = value.as_object() else { return false };
            let required: Vec<&str> =
                schema.get("required").and_then(Value::as_array).map(|r| r.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
            if !required.iter().all(|key| object.contains_key(*key)) {
                return false;
            }
            let properties = schema.get("properties").and_then(Value::as_object);
            if let Some(properties) = properties {
                for (key, property_schema) in properties {
                    if let Some(present) = object.get(key)
                        && !schema_accepts(property_schema, present)
                    {
                        return false;
                    }
                }
            }
            if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
                let known: std::collections::HashSet<&str> = properties.map(|p| p.keys().map(String::as_str).collect()).unwrap_or_default();
                if object.keys().any(|key| !known.contains(key.as_str())) {
                    return false;
                }
            }
            true
        }
        _ => true,
    }
}

pub fn coerce_antml_parameters(raw_params: &[InvokeParameter], tool: &Tool) -> Option<Map<String, Value>> {
    if !is_object_schema(&tool.parameters) {
        return None;
    }

    let empty = Map::new();
    let properties = tool.parameters.get("properties").and_then(Value::as_object).unwrap_or(&empty);
    let additional = get_additional_properties_schema(&tool.parameters);
    let mut arguments_record = Map::new();

    for raw_param in raw_params {
        let raw_value = trim_raw_boundary_newlines(&raw_param.raw_value);
        let property_name = resolve_property_name(&raw_param.name, properties);

        let Some(property_name) = property_name else {
            match &additional {
                None => continue,
                Some(AdditionalSchema::True) => {
                    arguments_record.insert(raw_param.name.clone(), coerce_unknown_value(&raw_value));
                }
                Some(AdditionalSchema::Schema(schema)) => {
                    if let Coerced::Ok(value) = coerce_known_value(&raw_value, schema) {
                        arguments_record.insert(raw_param.name.clone(), value);
                    }
                }
            }
            continue;
        };

        let Some(property_schema) = properties.get(&property_name) else { continue };
        match coerce_known_value(&raw_value, property_schema) {
            Coerced::Ok(value) => {
                arguments_record.insert(property_name, value);
            }
            Coerced::Invalid => return None,
        }
    }

    let arguments_value = Value::Object(arguments_record.clone());
    schema_accepts(&tool.parameters, &arguments_value).then_some(arguments_record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(parameters: Value) -> Tool {
        Tool { name: "t".into(), description: "d".into(), parameters, freeform: None, constrained_sampling: None }
    }

    fn param(name: &str, raw_value: &str) -> InvokeParameter {
        InvokeParameter { name: name.into(), raw_value: raw_value.into() }
    }

    #[test]
    fn resolves_aliased_parameter_names_to_the_declared_property() {
        let schema = json!({"type": "object", "properties": {"file_path": {"type": "string"}}});
        let result = coerce_antml_parameters(&[param("path", "a.txt")], &tool(schema)).expect("coerces");
        assert_eq!(result.get("file_path"), Some(&json!("a.txt")));
    }

    #[test]
    fn filters_unknown_keys_from_object_and_array_values() {
        let schema = json!({
            "type": "object",
            "properties": {"obj": {"type": "object", "properties": {"a": {"type": "string"}}}}
        });
        let result =
            coerce_antml_parameters(&[param("obj", r#"{"a":"x","unknown":"y"}"#)], &tool(schema)).expect("coerces");
        assert_eq!(result.get("obj"), Some(&json!({"a": "x"})));
    }

    #[test]
    fn rejects_invalid_number_for_declared_number_property() {
        let schema = json!({"type": "object", "properties": {"n": {"type": "number"}}});
        assert!(coerce_antml_parameters(&[param("n", "not-a-number")], &tool(schema)).is_none());
    }

    #[test]
    fn keeps_additional_property_when_schema_allows_it() {
        let schema = json!({"type": "object", "properties": {}, "additionalProperties": true});
        let result = coerce_antml_parameters(&[param("extra", "hello")], &tool(schema)).expect("coerces");
        assert_eq!(result.get("extra"), Some(&json!("hello")));
    }

    #[test]
    fn drops_unknown_property_when_additional_properties_is_false() {
        let schema = json!({"type": "object", "properties": {}, "additionalProperties": false});
        let result = coerce_antml_parameters(&[param("extra", "hello")], &tool(schema)).expect("coerces");
        assert!(result.is_empty());
    }

    #[test]
    fn returns_none_for_non_object_tool_schema() {
        assert!(coerce_antml_parameters(&[], &tool(json!({"type": "string"}))).is_none());
    }

    #[test]
    fn repairs_lone_surrogates_in_string_values() {
        let lone_high = String::from_utf16_lossy(&[0xD800]);
        let schema = json!({"type": "object", "properties": {"s": {"type": "string"}}});
        let result = coerce_antml_parameters(&[param("s", &lone_high)], &tool(schema)).expect("coerces");
        assert_eq!(result.get("s"), Some(&json!("\u{FFFD}")));
    }
}
