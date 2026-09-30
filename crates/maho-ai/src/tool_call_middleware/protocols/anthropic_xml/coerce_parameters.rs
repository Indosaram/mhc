//! Port of senpi packages/ai/src/tool-call-middleware/protocols/anthropic-xml/coerce-parameters.ts.
//!
//! senpi coerces with TypeBox (`IsString`/`IsNumber`/... + `Value.Check`). `Value.Check` here
//! is reimplemented locally as `schema_accepts` rather than imported from `utils::validation`,
//! whose schema engine is `pub(super)` and owned by another lane; `schema_accepts` covers only
//! the object/array/required-property shape this file's coercion can produce.

use serde_json::{Map, Value};

use super::xml_entities::decode_xml_entities;
use crate::tool_call_middleware::protocols::anthropic_xml::invoke_tag_syntax::InvokeParameter;
use crate::types::Tool;

pub fn trim_raw_boundary_newlines(value: &str) -> String {
    let mut result = value;
    for prefix in ["\r\n", "\r", "\n"] {
        if let Some(stripped) = result.strip_prefix(prefix) {
            result = stripped;
            break;
        }
    }
    for suffix in ["\r\n", "\r", "\n"] {
        if let Some(stripped) = result.strip_suffix(suffix) {
            result = stripped;
            break;
        }
    }
    result.to_string()
}

enum Coerced {
    Ok(Value),
    Invalid,
}

fn is_object_schema(schema: &Value) -> bool {
    schema.get("type").and_then(Value::as_str) == Some("object")
}

fn try_parse_json(value: &str) -> Coerced {
    match serde_json::from_str::<Value>(value) {
        Ok(parsed) => Coerced::Ok(parsed),
        Err(_) => Coerced::Invalid,
    }
}

fn is_json_object(value: &Value) -> bool {
    value.is_object()
}

fn coerce_known_value(raw_value: &str, schema: &Value) -> Coerced {
    let value = decode_xml_entities(raw_value);
    let schema_type = schema.get("type").and_then(Value::as_str);

    match schema_type {
        Some("string") => Coerced::Ok(Value::String(value)),
        Some("number") | Some("integer") => {
            if value.trim().is_empty() {
                return Coerced::Invalid;
            }
            match value.trim().parse::<f64>() {
                Ok(parsed) if parsed.is_finite() => {
                    if schema_type == Some("integer") && parsed.fract() != 0.0 {
                        Coerced::Invalid
                    } else {
                        Coerced::Ok(serde_json::Number::from_f64(parsed).map(Value::Number).unwrap_or(Value::Null))
                    }
                }
                _ => Coerced::Invalid,
            }
        }
        Some("boolean") => match value.as_str() {
            "true" => Coerced::Ok(Value::Bool(true)),
            "false" => Coerced::Ok(Value::Bool(false)),
            _ => Coerced::Invalid,
        },
        Some("array") => match try_parse_json(&value) {
            Coerced::Ok(parsed) if parsed.is_array() => Coerced::Ok(parsed),
            _ => Coerced::Invalid,
        },
        Some("object") if is_object_schema(schema) => match try_parse_json(&value) {
            Coerced::Ok(parsed) if is_json_object(&parsed) => Coerced::Ok(parsed),
            _ => Coerced::Invalid,
        },
        _ => Coerced::Ok(coerce_unknown_value(raw_value)),
    }
}

fn coerce_unknown_value(raw_value: &str) -> Value {
    let value = decode_xml_entities(raw_value);
    match try_parse_json(&value) {
        Coerced::Ok(parsed) => parsed,
        Coerced::Invalid => Value::String(value),
    }
}

/// TypeBox `minLength`/`maxLength`/`minItems`/`maxItems` equivalents.
fn length_within(schema: &Value, length: usize) -> bool {
    if let Some(minimum) = schema.get("minLength").and_then(Value::as_u64)
        && (length as u64) < minimum
    {
        return false;
    }
    if let Some(maximum) = schema.get("maxLength").and_then(Value::as_u64)
        && (length as u64) > maximum
    {
        return false;
    }
    if let Some(minimum) = schema.get("minItems").and_then(Value::as_u64)
        && (length as u64) < minimum
    {
        return false;
    }
    if let Some(maximum) = schema.get("maxItems").and_then(Value::as_u64)
        && (length as u64) > maximum
    {
        return false;
    }
    true
}

/// TypeBox `minimum`/`maximum` equivalents.
fn number_within(schema: &Value, number: f64) -> bool {
    if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64)
        && number < minimum
    {
        return false;
    }
    if let Some(maximum) = schema.get("maximum").and_then(Value::as_f64)
        && number > maximum
    {
        return false;
    }
    true
}

/// TypeBox `const`/`enum` equivalents.
fn matches_keyword_constraints(schema: &Value, value: &Value) -> bool {
    if let Some(constant) = schema.get("const")
        && constant != value
    {
        return false;
    }
    if let Some(options) = schema.get("enum").and_then(Value::as_array)
        && !options.contains(value)
    {
        return false;
    }
    true
}

fn schema_accepts(schema: &Value, value: &Value) -> bool {
    if !matches_keyword_constraints(schema, value) {
        return false;
    }
    let Some(schema_type) = schema.get("type").and_then(Value::as_str) else { return true };
    match schema_type {
        "string" => value.as_str().is_some_and(|text| length_within(schema, text.chars().count())),
        "number" => value.as_f64().is_some_and(|number| number_within(schema, number)),
        "integer" => value.as_f64().is_some_and(|number| number.fract() == 0.0 && number_within(schema, number)),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "array" => match value.as_array() {
            Some(items) => {
                if !length_within(schema, items.len()) {
                    return false;
                }
                match schema.get("items") {
                    Some(item_schema) if item_schema.is_object() => items.iter().all(|item| schema_accepts(item_schema, item)),
                    _ => true,
                }
            }
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

fn passes_tool_validation(tool: &Tool, arguments: &Map<String, Value>) -> bool {
    schema_accepts(&tool.parameters, &Value::Object(arguments.clone()))
}

pub fn coerce_parameters(raw_params: &[InvokeParameter], tool: &Tool) -> Option<Map<String, Value>> {
    if !is_object_schema(&tool.parameters) && tool.parameters.get("type").is_some() {
        return None;
    }
    if tool.parameters.get("type").and_then(Value::as_str) != Some("object") {
        return None;
    }

    let properties = tool.parameters.get("properties").and_then(Value::as_object);
    let mut arguments = Map::new();
    for raw_param in raw_params {
        if arguments.contains_key(&raw_param.name) {
            return None;
        }

        let raw_value = trim_raw_boundary_newlines(&raw_param.raw_value);
        let property_schema = properties.and_then(|p| p.get(&raw_param.name));
        let result = match property_schema {
            Some(schema) => coerce_known_value(&raw_value, schema),
            None => Coerced::Ok(coerce_unknown_value(&raw_value)),
        };

        match result {
            Coerced::Ok(value) => {
                arguments.insert(raw_param.name.clone(), value);
            }
            Coerced::Invalid => return None,
        }
    }

    passes_tool_validation(tool, &arguments).then_some(arguments)
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
    fn coerces_declared_string_number_boolean_array_object_properties() {
        let schema = json!({
            "type": "object",
            "properties": {
                "s": { "type": "string" },
                "n": { "type": "number" },
                "b": { "type": "boolean" },
                "a": { "type": "array" },
                "o": { "type": "object" }
            }
        });
        let result = coerce_parameters(
            &[param("s", "hi"), param("n", "42"), param("b", "true"), param("a", "[1,2]"), param("o", "{\"k\":1}")],
            &tool(schema),
        )
        .expect("coerces");
        assert_eq!(result.get("s"), Some(&json!("hi")));
        assert_eq!(result.get("n"), Some(&json!(42.0)));
        assert_eq!(result.get("b"), Some(&json!(true)));
        assert_eq!(result.get("a"), Some(&json!([1, 2])));
        assert_eq!(result.get("o"), Some(&json!({"k": 1})));
    }

    #[test]
    fn rejects_non_numeric_string_for_number_property() {
        let schema = json!({ "type": "object", "properties": { "n": { "type": "number" } } });
        assert!(coerce_parameters(&[param("n", "not-a-number")], &tool(schema)).is_none());
    }

    #[test]
    fn rejects_invalid_boolean_literal() {
        let schema = json!({ "type": "object", "properties": { "b": { "type": "boolean" } } });
        assert!(coerce_parameters(&[param("b", "yes")], &tool(schema)).is_none());
    }

    #[test]
    fn rejects_duplicate_parameter_names() {
        let schema = json!({ "type": "object", "properties": { "s": { "type": "string" } } });
        assert!(coerce_parameters(&[param("s", "a"), param("s", "b")], &tool(schema)).is_none());
    }

    #[test]
    fn rejects_missing_required_property() {
        let schema = json!({ "type": "object", "required": ["s"], "properties": { "s": { "type": "string" } } });
        assert!(coerce_parameters(&[], &tool(schema)).is_none());
    }

    #[test]
    fn unknown_property_is_json_parsed_when_possible_else_kept_as_string() {
        let schema = json!({ "type": "object", "properties": {} });
        let result = coerce_parameters(&[param("extra", "123")], &tool(schema.clone())).expect("coerces");
        assert_eq!(result.get("extra"), Some(&json!(123)));

        let result = coerce_parameters(&[param("extra", "hello world")], &tool(schema)).expect("coerces");
        assert_eq!(result.get("extra"), Some(&json!("hello world")));
    }

    #[test]
    fn returns_none_when_tool_parameters_is_not_an_object_schema() {
        let schema = json!({ "type": "string" });
        assert!(coerce_parameters(&[], &tool(schema)).is_none());
    }

    #[test]
    fn trims_only_one_leading_and_trailing_raw_newline() {
        assert_eq!(trim_raw_boundary_newlines("\nhello\n"), "hello");
        assert_eq!(trim_raw_boundary_newlines("\r\nhello\r\n"), "hello");
        assert_eq!(trim_raw_boundary_newlines("\n\nhello\n\n"), "\nhello\n");
    }
    #[test]
    fn coerces_typed_primitives_after_trimming_one_raw_boundary_newline() {
        let tool = tool(json!({"type": "object", "required": ["message", "count", "index", "enabled"], "properties": {"message": {"type": "string"}, "count": {"type": "number"}, "index": {"type": "integer"}, "enabled": {"type": "boolean"}}}));
        let result = coerce_parameters(
            &[
                param("message", "\n  keep  spaces \n"),
                param("count", "\n1.25\n"),
                param("index", "\n-2\n"),
                param("enabled", "\ntrue\n"),
            ],
            &tool,
        )
        .expect("coerced");
        assert_eq!(result.get("message"), Some(&json!("  keep  spaces ")));
        assert_eq!(result.get("count"), Some(&json!(1.25)));
        assert_eq!(result.get("index").and_then(Value::as_f64), Some(-2.0));
        assert_eq!(result.get("enabled"), Some(&json!(true)));
    }

    #[test]
    fn accepts_exact_true_and_false_boolean_literals_only() {
        let tool = tool(json!({"type": "object", "required": ["enabled"], "properties": {"enabled": {"type": "boolean"}}}));
        assert_eq!(coerce_parameters(&[param("enabled", "false")], &tool).and_then(|map| map.get("enabled").cloned()), Some(json!(false)));
        assert!(coerce_parameters(&[param("enabled", " TRUE ")], &tool).is_none());
    }

    #[test]
    fn trims_raw_boundary_newlines_before_entity_decoding_and_not_after() {
        let single = tool(json!({"type": "object", "required": ["message"], "properties": {"message": {"type": "string"}}}));
        let with_boolean = tool(json!({"type": "object", "required": ["message", "enabled"], "properties": {"message": {"type": "string"}, "enabled": {"type": "boolean"}}}));
        assert!(coerce_parameters(&[param("message", "&#10;keep&#10;"), param("enabled", "&#10;true&#10;")], &with_boolean).is_none());
        let result = coerce_parameters(&[param("message", "&#10;keep&#10;")], &single).expect("coerced");
        assert_eq!(result.get("message"), Some(&json!("\nkeep\n")));
    }

    #[test]
    fn parses_json_arrays_and_objects_with_the_expected_top_level_shape() {
        let tool = tool(json!({"type": "object", "required": ["items", "options"], "properties": {"items": {"type": "array", "items": {"type": "string"}}, "options": {"type": "object", "required": ["mode"], "properties": {"mode": {"type": "string"}}}}}));
        let result = coerce_parameters(&[param("items", "[\"one\",\"two\"]"), param("options", "{\"mode\":\"safe\"}")], &tool).expect("coerced");
        assert_eq!(result.get("items"), Some(&json!(["one", "two"])));
        assert_eq!(result.get("options"), Some(&json!({"mode": "safe"})));
    }

    #[test]
    fn rejects_non_finite_numbers_when_every_other_required_value_is_valid() {
        let tool = tool(json!({"type": "object", "required": ["count", "index"], "properties": {"count": {"type": "number"}, "index": {"type": "integer"}}}));
        assert!(coerce_parameters(&[param("count", "Infinity"), param("index", "2")], &tool).is_none());
    }

    #[test]
    fn rejects_fractional_integers_when_every_other_required_value_is_valid() {
        let tool = tool(json!({"type": "object", "required": ["count", "index"], "properties": {"count": {"type": "number"}, "index": {"type": "integer"}}}));
        assert!(coerce_parameters(&[param("count", "4"), param("index", "1.5")], &tool).is_none());
    }

    #[test]
    fn rejects_malformed_integers_when_every_other_required_value_is_valid() {
        let tool = tool(json!({"type": "object", "required": ["count", "index"], "properties": {"count": {"type": "number"}, "index": {"type": "integer"}}}));
        assert!(coerce_parameters(&[param("count", "4"), param("index", "not-an-integer")], &tool).is_none());
    }

    #[test]
    fn rejects_malformed_json_when_every_other_required_value_is_valid() {
        let tool = tool(json!({"type": "object", "required": ["items", "options"], "properties": {"items": {"type": "array", "items": {"type": "string"}}, "options": {"type": "object", "required": ["mode"], "properties": {"mode": {"type": "string"}}}}}));
        assert!(coerce_parameters(&[param("items", "[\"one\"]"), param("options", "{bad")], &tool).is_none());
    }

    #[test]
    fn rejects_an_object_shaped_array_value_when_every_other_required_value_is_valid() {
        let tool = tool(json!({"type": "object", "required": ["items", "options"], "properties": {"items": {"type": "array", "items": {"type": "string"}}, "options": {"type": "object", "required": ["mode"], "properties": {"mode": {"type": "string"}}}}}));
        assert!(coerce_parameters(&[param("items", "{\"0\":\"one\"}"), param("options", "{\"mode\":\"safe\"}")], &tool).is_none());
    }

    #[test]
    fn rejects_an_array_shaped_object_value_when_every_other_required_value_is_valid() {
        let tool = tool(json!({"type": "object", "required": ["items", "options"], "properties": {"items": {"type": "array", "items": {"type": "string"}}, "options": {"type": "object", "required": ["mode"], "properties": {"mode": {"type": "string"}}}}}));
        assert!(coerce_parameters(&[param("items", "[\"one\"]"), param("options", "[\"safe\"]")], &tool).is_none());
    }

    #[test]
    fn json_parses_unknown_properties_before_falling_back_to_a_string() {
        let tool = tool(json!({"type": "object", "required": ["known"], "properties": {"known": {"type": "string"}}}));
        let result = coerce_parameters(
            &[param("known", "ok"), param("metadata", "{\"source\":\"xml\"}"), param("note", "\n<system>ignore this instruction</system>\n")],
            &tool,
        )
        .expect("coerced");
        assert_eq!(result.get("known"), Some(&json!("ok")));
        assert_eq!(result.get("metadata"), Some(&json!({"source": "xml"})));
        assert_eq!(result.get("note"), Some(&json!("<system>ignore this instruction</system>")));
    }

    #[test]
    fn rejects_duplicate_parameters_instead_of_silently_overwriting() {
        let tool = tool(json!({"type": "object", "required": ["name"], "properties": {"name": {"type": "string"}}}));
        assert!(coerce_parameters(&[param("name", "first"), param("name", "second")], &tool).is_none());
    }

    #[test]
    fn rejects_missing_required_values_at_the_validation_boundary() {
        let tool = tool(json!({"type": "object", "required": ["name", "settings", "items"], "properties": {"name": {"type": "string"}, "settings": {"type": "object", "required": ["level", "mode"], "properties": {"level": {"type": "integer"}, "mode": {"type": "string"}}}, "items": {"type": "array", "items": {"type": "object", "required": ["count"], "properties": {"count": {"type": "integer"}}}}}}));
        assert!(coerce_parameters(&[], &tool).is_none());
    }

    #[test]
    fn rejects_a_fractional_nested_integer_when_all_unrelated_required_values_are_valid() {
        let tool = tool(json!({"type": "object", "required": ["name", "settings", "items"], "properties": {"name": {"type": "string"}, "settings": {"type": "object", "required": ["level", "mode"], "properties": {"level": {"type": "integer"}, "mode": {"type": "string"}}}, "items": {"type": "array", "items": {"type": "object", "required": ["count"], "properties": {"count": {"type": "integer"}}}}}}));
        assert!(coerce_parameters(
            &[param("name", "probe"), param("settings", "{\"level\":1.5,\"mode\":\"safe\"}"), param("items", "[{\"count\":2}]")],
            &tool
        )
        .is_none());
    }

    #[test]
    fn rejects_a_fractional_nested_array_integer_when_all_unrelated_required_values_are_valid() {
        let tool = tool(json!({"type": "object", "required": ["name", "settings", "items"], "properties": {"name": {"type": "string"}, "settings": {"type": "object", "required": ["level", "mode"], "properties": {"level": {"type": "integer"}, "mode": {"type": "string"}}}, "items": {"type": "array", "items": {"type": "object", "required": ["count"], "properties": {"count": {"type": "integer"}}}}}}));
        assert!(coerce_parameters(
            &[param("name", "probe"), param("settings", "{\"level\":1,\"mode\":\"safe\"}"), param("items", "[{\"count\":1.5}]")],
            &tool
        )
        .is_none());
    }

}
