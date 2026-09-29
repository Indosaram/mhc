//! Port of senpi packages/ai/src/utils/validation.ts.
//!
//! senpi validates with TypeBox 1.3.34 (`Compile` + `Value.Convert`); `validation/engine.rs`
//! and `validation/formats.rs` port the parts of TypeBox that decide the verdict and the
//! `en_US` error lines. Rust tool parameters are always plain JSON Schema (`Value`), so every
//! schema takes senpi's plain-JSON-schema branch (`coerceWithJsonSchema`); `Value.Convert`
//! is a no-op for plain schemas, which the generated fixture `tests/golden/ai-validation.json`
//! confirms.

mod engine;
mod formats;

use serde_json::{Map, Value};

use crate::types::{Tool, ToolCall};
use crate::utils::js::{json_number, json_stringify_pretty, number_to_string, string_to_number, trim};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("Tool \"{0}\" not found")]
    ToolNotFound(String),
    #[error("{0}")]
    Invalid(String),
}

fn schema_types(schema: &Map<String, Value>) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(name)) => vec![name.as_str()],
        Some(Value::Array(names)) => names.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

fn matches_json_type(value: &Value, type_name: &str) -> bool {
    match type_name {
        "number" => value.is_number(),
        "integer" => value.as_f64().is_some_and(|n| n.fract() == 0.0),
        "boolean" => value.is_boolean(),
        "string" => value.is_string(),
        "null" => value.is_null(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

/// `coercePrimitiveByType`; `None` when the value is returned unchanged.
fn coerce_primitive_by_type(value: &Value, type_name: &str) -> Option<Value> {
    match (type_name, value) {
        ("number" | "integer", Value::Null) => Some(json_number(0.0)),
        ("number" | "integer", Value::Bool(flag)) => Some(json_number(if *flag { 1.0 } else { 0.0 })),
        ("number", Value::String(text)) if !trim(text).is_empty() => {
            let parsed = string_to_number(text);
            parsed.is_finite().then(|| json_number(parsed))
        }
        ("integer", Value::String(text)) if !trim(text).is_empty() => {
            let parsed = string_to_number(text);
            (parsed.is_finite() && parsed.fract() == 0.0).then(|| json_number(parsed))
        }
        ("boolean", Value::Null) => Some(Value::Bool(false)),
        ("boolean", Value::String(text)) if text == "true" || text == "false" => Some(Value::Bool(text == "true")),
        ("boolean", Value::Number(n)) => match n.as_f64() {
            Some(1.0) => Some(Value::Bool(true)),
            Some(0.0) => Some(Value::Bool(false)),
            _ => None,
        },
        ("string", Value::Null) => Some(Value::String(String::new())),
        ("string", Value::Number(n)) => n.as_f64().map(|n| Value::String(number_to_string(n))),
        ("string", Value::Bool(flag)) => Some(Value::String(flag.to_string())),
        ("null", Value::String(text)) if text.is_empty() => Some(Value::Null),
        ("null", Value::Bool(false)) => Some(Value::Null),
        ("null", Value::Number(n)) if n.as_f64() == Some(0.0) => Some(Value::Null),
        _ => None,
    }
}

fn apply_schema_object_coercion(object: &mut Map<String, Value>, schema: &Map<String, Value>) {
    let properties = schema.get("properties").and_then(Value::as_object);
    if let Some(properties) = properties {
        for (key, property_schema) in properties {
            if let Some(slot) = object.get_mut(key) {
                *slot = coerce_with_json_schema(slot.take(), property_schema);
            }
        }
    }
    if let Some(additional) = schema.get("additionalProperties").filter(|v| v.is_object() || v.is_array()) {
        for (key, slot) in object.iter_mut() {
            if !properties.is_some_and(|p| p.contains_key(key)) {
                *slot = coerce_with_json_schema(slot.take(), additional);
            }
        }
    }
}

fn apply_schema_array_coercion(items: &mut [Value], schema: &Map<String, Value>) {
    match schema.get("items") {
        Some(Value::Array(tuple)) => {
            for (slot, item_schema) in items.iter_mut().zip(tuple) {
                *slot = coerce_with_json_schema(slot.take(), item_schema);
            }
        }
        Some(item_schema @ Value::Object(_)) => {
            for slot in items.iter_mut() {
                *slot = coerce_with_json_schema(slot.take(), item_schema);
            }
        }
        _ => {}
    }
}

fn coerce_with_union_schema(value: Value, schemas: &[Value]) -> Value {
    if schemas.iter().any(|schema| engine::check(schema, &value)) {
        return value;
    }
    for schema in schemas {
        let coerced = coerce_with_json_schema(value.clone(), schema);
        if engine::check(schema, &coerced) {
            return coerced;
        }
    }
    value
}

fn coerce_with_json_schema(value: Value, schema: &Value) -> Value {
    let Some(schema) = schema.as_object() else { return value };
    let mut next = value;
    if let Some(Value::Array(all)) = schema.get("allOf") {
        for nested in all {
            next = coerce_with_json_schema(next, nested);
        }
    }
    if let Some(Value::Array(any)) = schema.get("anyOf") {
        next = coerce_with_union_schema(next, any);
    }
    if let Some(Value::Array(one)) = schema.get("oneOf") {
        next = coerce_with_union_schema(next, one);
    }
    let types = schema_types(schema);
    let matches_union_member = types.len() > 1 && types.iter().any(|t| matches_json_type(&next, t));
    if !types.is_empty()
        && !matches_union_member
        && let Some(candidate) = types.iter().find_map(|t| coerce_primitive_by_type(&next, t))
    {
        next = candidate;
    }
    if types.contains(&"object")
        && let Value::Object(object) = &mut next
    {
        apply_schema_object_coercion(object, schema);
    }
    if types.contains(&"array")
        && let Value::Array(items) = &mut next
    {
        apply_schema_array_coercion(items, schema);
    }
    next
}

fn normalize_optional_nulls(value: &mut Value, schema: &Value) {
    let Some(schema) = schema.as_object() else { return };
    if let Value::Array(items) = value {
        match schema.get("items") {
            Some(Value::Array(tuple)) => items.iter_mut().zip(tuple).for_each(|(item, s)| normalize_optional_nulls(item, s)),
            Some(item_schema) if item_schema.is_object() => items.iter_mut().for_each(|item| normalize_optional_nulls(item, item_schema)),
            _ => {}
        }
        return;
    }
    let (Value::Object(object), Some(Value::Object(properties))) = (value, schema.get("properties")) else { return };
    let required: Vec<&str> = schema.get("required").and_then(Value::as_array).map(|r| r.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    for (key, property_schema) in properties {
        let Some(slot) = object.get_mut(key) else { continue };
        let is_ref = property_schema.get("$ref").is_some_and(Value::is_string);
        if slot.is_null() && !required.contains(&key.as_str()) && !is_ref && !engine::check(property_schema, &Value::Null) {
            object.shift_remove(key);
        } else {
            normalize_optional_nulls(slot, property_schema);
        }
    }
}

fn format_validation_path(issue: &engine::Issue) -> String {
    let base = issue.instance_path.strip_prefix('/').unwrap_or(&issue.instance_path).replace('/', ".");
    if issue.keyword == "required"
        && let Some(property) = issue.required_properties.first().filter(|p| !p.is_empty())
    {
        return if base.is_empty() { property.clone() } else { format!("{base}.{property}") };
    }
    if base.is_empty() { "root".into() } else { base }
}

pub fn validate_tool_call(tools: &[Tool], tool_call: &ToolCall) -> Result<Value, ValidationError> {
    let tool = tools.iter().find(|t| t.name == tool_call.name).ok_or_else(|| ValidationError::ToolNotFound(tool_call.name.clone()))?;
    validate_tool_arguments(tool, tool_call)
}

pub fn validate_tool_arguments(tool: &Tool, tool_call: &ToolCall) -> Result<Value, ValidationError> {
    validate_value(&tool.parameters, &tool_call.name, &Value::Object(tool_call.arguments.clone()))
}

fn validate_value(parameters: &Value, tool_name: &str, received: &Value) -> Result<Value, ValidationError> {
    let mut args = received.clone();
    normalize_optional_nulls(&mut args, parameters);
    let coerced = coerce_with_json_schema(args.clone(), parameters);
    if args.is_object() && coerced.is_object() {
        args = coerced;
    } else if coerced != args {
        return Ok(if engine::check(parameters, &coerced) { coerced } else { args });
    }
    let issues = engine::errors(parameters, &args);
    if issues.is_empty() {
        return Ok(args);
    }
    let lines: Vec<String> = issues.iter().map(|issue| format!("  - {}: {}", format_validation_path(issue), issue.message)).collect();
    Err(ValidationError::Invalid(format!(
        "Validation failed for tool \"{tool_name}\":\n{}\n\nReceived arguments:\n{}",
        lines.join("\n"),
        json_stringify_pretty(received)
    )))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn tool(parameters: Value) -> Tool {
        serde_json::from_value(json!({ "name": "echo", "description": "Echo tool", "parameters": parameters })).expect("tool")
    }

    fn call(arguments: Value) -> ToolCall {
        serde_json::from_value(json!({ "id": "tool-1", "name": "echo", "arguments": arguments })).expect("tool call")
    }

    fn plain(schema: Value, input: Value) -> (Tool, ToolCall) {
        (tool(json!({ "type": "object", "properties": { "value": schema }, "required": ["value"] })), call(json!({ "value": input })))
    }

    #[test]
    fn still_validates_when_function_constructor_is_unavailable() {
        // TS disables `Function` to force TypeBox's interpreter; the Rust engine is always interpreted.
        let tool = tool(json!({ "type": "object", "required": ["count"], "properties": { "count": { "type": "number" } } }));
        assert_eq!(validate_tool_arguments(&tool, &call(json!({ "count": "42" }))), Ok(json!({ "count": 42 })));
    }

    #[test]
    fn coerces_serialized_plain_json_schemas_with_ajv_compatible_primitive_rules() {
        let cases = [
            (json!({ "type": "number" }), json!("42"), json!(42)),
            (json!({ "type": "number" }), json!(true), json!(1)),
            (json!({ "type": "number" }), json!(null), json!(0)),
            (json!({ "type": "integer" }), json!("42"), json!(42)),
            (json!({ "type": "boolean" }), json!("true"), json!(true)),
            (json!({ "type": "boolean" }), json!("false"), json!(false)),
            (json!({ "type": "boolean" }), json!(1), json!(true)),
            (json!({ "type": "boolean" }), json!(0), json!(false)),
            (json!({ "type": "string" }), json!(null), json!("")),
            (json!({ "type": "string" }), json!(true), json!("true")),
            (json!({ "type": "null" }), json!(""), json!(null)),
            (json!({ "type": "null" }), json!(0), json!(null)),
            (json!({ "type": "null" }), json!(false), json!(null)),
            (json!({ "type": ["number", "string"] }), json!("1"), json!("1")),
            (json!({ "type": ["boolean", "number"] }), json!("1"), json!(1)),
        ];
        for (schema, input, expected) in cases {
            let (tool, call) = plain(schema.clone(), input.clone());
            assert_eq!(validate_tool_arguments(&tool, &call), Ok(json!({ "value": expected })), "{schema} {input}");
        }
    }

    #[test]
    fn treats_null_as_omission_for_optional_non_nullable_properties() {
        let tool = tool(json!({
            "type": "object",
            "required": ["path", "metadata"],
            "properties": {
                "path": { "type": "string" },
                "offset": { "type": "number" },
                "nullable": { "anyOf": [{ "type": "string" }, { "type": "null" }] },
                "metadata": { "type": "object", "required": [], "properties": { "enabled": { "type": "boolean" } } }
            }
        }));
        let call = call(json!({ "path": "file.txt", "offset": null, "nullable": null, "metadata": { "enabled": null } }));
        assert_eq!(validate_tool_arguments(&tool, &call), Ok(json!({ "path": "file.txt", "nullable": null, "metadata": {} })));
    }

    #[test]
    fn preserves_optional_nulls_whose_referenced_schema_is_nullable() {
        let tool = tool(json!({
            "type": "object",
            "properties": { "value": { "$ref": "#/$defs/value" } },
            "$defs": { "value": { "anyOf": [{ "type": "number" }, { "type": "null" }] } }
        }));
        assert_eq!(validate_tool_arguments(&tool, &call(json!({ "value": null }))), Ok(json!({ "value": null })));
    }

    #[test]
    fn preserves_a_value_that_already_matches_a_nullable_union_arm() {
        let tool = tool(json!({ "type": "object", "required": ["value"], "properties": { "value": { "anyOf": [{ "type": "number" }, { "type": "null" }] } } }));
        assert_eq!(validate_tool_arguments(&tool, &call(json!({ "value": null }))), Ok(json!({ "value": null })));
    }

    #[test]
    fn preserves_a_value_that_already_matches_a_one_of_nullable_union_arm() {
        let (tool, call) = plain(json!({ "oneOf": [{ "type": "number" }, { "type": "null" }] }), json!(null));
        assert_eq!(validate_tool_arguments(&tool, &call), Ok(json!({ "value": null })));
    }

    #[test]
    fn still_coerces_nullable_unions_when_the_original_value_does_not_match_any_arm() {
        let (tool, call) = plain(json!({ "anyOf": [{ "type": "number" }, { "type": "null" }] }), json!("42"));
        assert_eq!(validate_tool_arguments(&tool, &call), Ok(json!({ "value": 42 })));
    }

    #[test]
    fn accepts_null_for_nullable_array_schemas_with_items() {
        let (tool, call) = plain(json!({ "type": ["array", "null"], "items": { "type": "string" } }), json!(null));
        assert!(engine::check(&tool.parameters, &Value::Object(call.arguments.clone())));
        assert_eq!(validate_tool_arguments(&tool, &call), Ok(json!({ "value": null })));
    }

    #[test]
    fn rejects_invalid_coercions_for_serialized_plain_json_schemas() {
        let cases = [
            (json!({ "type": "boolean" }), json!("1")),
            (json!({ "type": "boolean" }), json!("0")),
            (json!({ "type": "null" }), json!("null")),
            (json!({ "type": "integer" }), json!("42.1")),
        ];
        for (schema, input) in cases {
            let (tool, call) = plain(schema, input);
            let Err(ValidationError::Invalid(message)) = validate_tool_arguments(&tool, &call) else { panic!("expected failure") };
            assert!(message.contains("Validation failed"));
        }
    }

    #[test]
    fn validate_tool_call_reports_unknown_tools() {
        let err = validate_tool_call(&[], &call(json!({}))).expect_err("unknown tool");
        assert_eq!(err.to_string(), "Tool \"echo\" not found");
    }

    fn fixture() -> Value {
        // Generated by `bun tools/golden/run.mjs --case ai-validation` from pinned senpi.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/ai-validation.json");
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("fixture json")
    }

    #[test]
    fn engine_matches_generated_typebox_checks_and_errors() {
        let fixture = fixture();
        let checks = fixture["checks"].as_array().expect("checks");
        assert!(!checks.is_empty());
        for case in checks {
            let (schema, value) = (&case["schema"], &case["value"]);
            assert_eq!(engine::check(schema, value), case["ok"].as_bool().expect("ok"), "check {schema} {value}");
            let errors: Vec<Value> = engine::errors(schema, value).into_iter().map(|i| json!([i.keyword, i.instance_path, i.message])).collect();
            assert_eq!(Value::Array(errors), case["errors"], "errors {schema} {value}");
        }
    }

    #[test]
    fn validate_matches_generated_senpi_results() {
        let fixture = fixture();
        let calls = fixture["calls"].as_array().expect("calls");
        assert!(!calls.is_empty());
        for case in calls {
            let got = validate_value(&case["parameters"], "echo", &case["arguments"]);
            if case["ok"] == json!(true) {
                assert_eq!(got, Ok(case["value"].clone()), "{case}");
            } else {
                assert_eq!(got, Err(ValidationError::Invalid(case["error"].as_str().expect("error").to_owned())), "{case}");
            }
        }
    }
}
