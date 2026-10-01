//! Port of senpi packages/ai/src/api/constrained-sampling.ts.
//!
//! Node-private copy: `src/api/constrained_sampling.rs` is owned by node 12-misc. Kept here so this
//! lane compiles standalone; delete it once 12-misc merges.

use crate::types::{ConstrainedSampling, ConstrainedSamplingConfig, GrammarFormat, JsonSchemaStrictness, Tool};
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct UnsupportedStrictJsonSchemaError {
    pub message: String,
}

impl UnsupportedStrictJsonSchemaError {
    fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }
}

const UNSUPPORTED_STRICT_SCHEMA_KEYS: [&str; 16] = [
    "$ref",
    "$defs",
    "definitions",
    "allOf",
    "oneOf",
    "patternProperties",
    "dependentSchemas",
    "dependencies",
    "unevaluatedProperties",
    "propertyNames",
    "contains",
    "prefixItems",
    "not",
    "if",
    "then",
    "else",
];

fn is_json_schema_object(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

fn is_structured_schema(schema: &Value) -> bool {
    let Some(schema) = is_json_schema_object(schema) else { return false };
    let types: Vec<&Value> = match schema.get("type") {
        Some(Value::String(_)) => schema.get("type").into_iter().collect(),
        Some(Value::Array(items)) => items.iter().collect(),
        _ => Vec::new(),
    };
    let has_type = |name: &str| types.iter().any(|value| value.as_str() == Some(name));
    has_type("object")
        || has_type("array")
        || schema.contains_key("properties")
        || schema.contains_key("items")
}

fn schema_allows_null(schema: &Value) -> bool {
    let Some(schema) = is_json_schema_object(schema) else { return false };
    if schema.get("type").and_then(Value::as_str) == Some("null") {
        return true;
    }
    if schema
        .get("type")
        .and_then(Value::as_array)
        .is_some_and(|types| types.iter().any(|value| value.as_str() == Some("null")))
    {
        return true;
    }
    if matches!(schema.get("const"), Some(Value::Null)) {
        return true;
    }
    if schema
        .get("enum")
        .and_then(Value::as_array)
        .is_some_and(|values| values.iter().any(Value::is_null))
    {
        return true;
    }
    schema
        .get("anyOf")
        .and_then(Value::as_array)
        .is_some_and(|variants| variants.iter().any(schema_allows_null))
}

fn make_json_schema_node_strict(schema: &mut Value) -> Result<(), UnsupportedStrictJsonSchemaError> {
    if is_json_schema_object(schema).is_none() {
        return Err(UnsupportedStrictJsonSchemaError::new("boolean schemas are unsupported"));
    }
    for key in UNSUPPORTED_STRICT_SCHEMA_KEYS.iter() {
        if schema.as_object().is_some_and(|object| object.contains_key(*key)) {
            return Err(UnsupportedStrictJsonSchemaError::new(format!("{key} schemas are unsupported")));
        }
    }

    if let Some(any_of) = schema.get("anyOf") {
        let variants = match any_of.as_array() {
            Some(variants) if !variants.is_empty() => variants.clone(),
            _ => return Err(UnsupportedStrictJsonSchemaError::new("anyOf must contain at least one schema")),
        };
        for variant in &variants {
            if is_structured_schema(variant) {
                return Err(UnsupportedStrictJsonSchemaError::new("object and array unions are unsupported"));
            }
            let mut variant = variant.clone();
            make_json_schema_node_strict(&mut variant)?;
        }
    }

    if let Some(items) = schema.get("items") {
        if items.is_array() {
            return Err(UnsupportedStrictJsonSchemaError::new("tuple schemas are unsupported"));
        }
        let mut items = items.clone();
        make_json_schema_node_strict(&mut items)?;
    }

    let is_object_schema = schema.get("type").and_then(Value::as_str) == Some("object");
    let has_properties = schema.as_object().is_some_and(|object| object.contains_key("properties"));
    if has_properties && !is_object_schema {
        return Err(UnsupportedStrictJsonSchemaError::new("properties require type object"));
    }
    if !is_object_schema {
        return Ok(());
    }
    if let Some(additional) = schema.get("additionalProperties")
        && additional != &Value::Bool(false) {
            return Err(UnsupportedStrictJsonSchemaError::new(
                "schema-valued or true additionalProperties is unsupported",
            ));
        }
    if schema.get("properties").is_some_and(|properties| is_json_schema_object(properties).is_none()) {
        return Err(UnsupportedStrictJsonSchemaError::new("object properties must be a schema map"));
    }
    if let Some(required) = schema.get("required") {
        let valid = required.as_array().is_some_and(|items| items.iter().all(Value::is_string));
        if !valid {
            return Err(UnsupportedStrictJsonSchemaError::new("object required must be a string array"));
        }
    }

    let properties = schema.get("properties").and_then(Value::as_object).cloned().unwrap_or_default();
    let property_names: Vec<String> = properties.keys().cloned().collect();
    let required: Vec<String> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    if required.iter().any(|key| !property_names.contains(key)) {
        return Err(UnsupportedStrictJsonSchemaError::new("required contains an unknown property"));
    }

    let mut next_properties = Map::new();
    for (key, property) in &properties {
        let mut property = property.clone();
        make_json_schema_node_strict(&mut property)?;
        if !required.iter().any(|name| name == key) && !schema_allows_null(&property) {
            let mut wrapped = Map::new();
            wrapped.insert("anyOf".to_owned(), Value::Array(vec![property, serde_json::json!({ "type": "null" })]));
            next_properties.insert(key.clone(), Value::Object(wrapped));
            continue;
        }
        next_properties.insert(key.clone(), property);
    }

    let object = schema.as_object_mut().expect("object schema");
    object.insert("properties".to_owned(), Value::Object(next_properties));
    object.insert("required".to_owned(), Value::Array(property_names.into_iter().map(Value::String).collect()));
    object.insert("additionalProperties".to_owned(), Value::Bool(false));
    Ok(())}

pub fn make_strict_json_schema(schema: &Value) -> Result<Value, UnsupportedStrictJsonSchemaError> {
    let mut cloned = schema.clone();
    if is_json_schema_object(&cloned).is_none() {
        return Err(UnsupportedStrictJsonSchemaError::new("root schema must have type object"));
    }
    make_json_schema_node_strict(&mut cloned)?;
    if cloned.get("type").and_then(Value::as_str) != Some("object") {
        return Err(UnsupportedStrictJsonSchemaError::new("root schema must have type object"));
    }
    Ok(cloned)
}

pub fn get_json_schema_tool_parameters(tool: &Tool, strict: Option<bool>) -> Result<Value, UnsupportedStrictJsonSchemaError> {
    if strict == Some(true) { make_strict_json_schema(&tool.parameters) } else { Ok(tool.parameters.clone()) }
}

pub struct GrammarConstrainedSampling {
    pub format: String,
    pub definition: String,
    pub input_property: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GrammarToolInputJsonBuffer {
    pub input: String,
    pub started: bool,
    pub closed: bool,
}

pub fn get_grammar_tool_input(
    tool_name: &str,
    arguments: &Map<String, Value>,
    input_property: &str,
) -> Result<String, String> {
    match arguments.get(input_property).and_then(Value::as_str) {
        Some(input) => Ok(input.to_owned()),
        None => Err(format!(
            "Grammar tool call \"{tool_name}\" requires argument \"{input_property}\" to be a string."
        )),
    }
}

pub fn append_grammar_tool_input_json_delta(
    buffer: &mut GrammarToolInputJsonBuffer,
    input_property: &str,
    next_input: &str,
    close: bool,
) -> Result<Option<String>, String> {
    if buffer.closed {
        if close && next_input == buffer.input {
            return Ok(None);
        }
        return Err(format!("grammar tool input for property \"{input_property}\" changed after it was closed"));
    }
    if !next_input.starts_with(buffer.input.as_str()) {
        return Err(format!("grammar tool input for property \"{input_property}\" changed non-monotonically"));
    }

    let input_delta = &next_input[buffer.input.len()..];
    if !close && input_delta.is_empty() {
        return Ok(None);
    }

    let mut delta = String::new();
    if !buffer.started {
        delta.push('{');
        delta.push_str(&json_string(input_property));
        delta.push_str(":\"");
        buffer.started = true;
    }
    let escaped = json_string(input_delta);
    delta.push_str(&escaped[1..escaped.len() - 1]);
    buffer.input = next_input.to_owned();

    if close {
        delta.push_str("\"}");
        buffer.closed = true;
    }
    Ok(Some(delta))
}

fn json_string(value: &str) -> String {
    serde_json::to_string(&Value::String(value.to_owned())).unwrap_or_else(|_| format!("\"{value}\""))
}

fn infer_grammar_input_property(tool: &Tool) -> Result<String, String> {
    let schema = tool.parameters.as_object().cloned().unwrap_or_default();
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err("grammar constrained sampling requires an object parameter schema".into());
    }
    let required = schema.get("required").and_then(Value::as_array);
    let required = match required {
        Some(items) if items.len() == 1 && items[0].is_string() => items[0].as_str().unwrap_or_default().to_owned(),
        _ => return Err("grammar constrained sampling requires exactly one required string property".into()),
    };

    let properties = schema.get("properties").and_then(Value::as_object);
    let property = properties.and_then(|properties| properties.get(&required));
    if property.is_none() {
        return Err(format!("grammar constrained sampling requires a properties entry for {required}"));
    }
    if property.and_then(|property| property.get("type")).and_then(Value::as_str) != Some("string") {
        return Err(format!("grammar constrained sampling property {required} must have type string"));
    }
    Ok(required)
}

pub fn resolve_json_schema_strict_sampling(tool: &Tool, supports_strict_mode: bool) -> Result<Option<bool>, String> {
    let config = match &tool.constrained_sampling {
        Some(ConstrainedSampling::Config(config)) => config,
        _ => return Ok(None),
    };
    let ConstrainedSamplingConfig::JsonSchema { strict } = config else { return Ok(None) };

    if supports_strict_mode {
        return match make_strict_json_schema(&tool.parameters) {
            Ok(_) => Ok(Some(true)),
            Err(error) => {
                if *strict != JsonSchemaStrictness::Require {
                    return Ok(None);
                }
                Err(format!(
                    "Tool \"{}\" requires JSON-schema constrained sampling, but {}.",
                    tool.name, error.message
                ))
            }
        };
    }
    if *strict == JsonSchemaStrictness::Require {
        return Err(format!(
            "Tool \"{}\" requires JSON-schema constrained sampling, but strict tools are unsupported.",
            tool.name
        ));
    }
    Ok(None)
}

pub fn resolve_grammar_constrained_sampling(
    tool: &Tool,
    supports_openai_grammar_tools: bool,
) -> Result<Option<GrammarConstrainedSampling>, String> {
    let config = match &tool.constrained_sampling {
        Some(ConstrainedSampling::Config(config)) => config,
        _ => return Ok(None),
    };
    let ConstrainedSamplingConfig::Grammar { variants } = config else { return Ok(None) };
    if !supports_openai_grammar_tools {
        return Ok(None);
    }

    let lark_definition = variants.get(&GrammarFormat::OpenaiLark);
    let regex_definition = variants.get(&GrammarFormat::OpenaiRegex);
    let has_lark = lark_definition.is_some_and(|definition| !definition.trim().is_empty());
    let has_regex = regex_definition.is_some_and(|definition| !definition.trim().is_empty());
    if !has_lark && !has_regex {
        return Err(format!(
            "Tool \"{}\" cannot use grammar constrained sampling: no supported grammar variant was provided.",
            tool.name
        ));
    }

    match infer_grammar_input_property(tool) {
        Ok(input_property) => Ok(Some(GrammarConstrainedSampling {
            format: if has_lark { "lark".into() } else { "regex".into() },
            definition: if has_lark {
                lark_definition.cloned().unwrap_or_default()
            } else {
                regex_definition.cloned().unwrap_or_default()
            },
            input_property,
        })),
        Err(message) => Err(format!(
            "Tool \"{}\" cannot use grammar constrained sampling: {message}.",
            tool.name
        )),
    }
}

pub fn create_grammar_tool_input_properties(
    tools: Option<&[Tool]>,
    supports_openai_grammar_tools: bool,
) -> Result<Map<String, Value>, String> {
    let mut properties = Map::new();
    for tool in tools.unwrap_or_default() {
        if let Some(grammar) = resolve_grammar_constrained_sampling(tool, supports_openai_grammar_tools)? {
            properties.insert(tool.name.clone(), Value::String(grammar.input_property));
        }
    }
    Ok(properties)
}
