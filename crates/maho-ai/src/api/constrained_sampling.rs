//! Port of senpi packages/ai/src/api/constrained-sampling.ts.

use serde_json::{Map, Value};
use std::collections::BTreeMap;

use crate::types::{ConstrainedSampling, ConstrainedSamplingConfig, GrammarFormat, JsonSchemaStrictness, Tool};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct UnsupportedStrictJsonSchemaError(pub String);

const UNSUPPORTED_STRICT_SCHEMA_KEYS: &[&str] = &[
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

fn unsupported(message: &str) -> UnsupportedStrictJsonSchemaError {
    UnsupportedStrictJsonSchemaError(message.to_owned())
}

fn as_object(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

fn is_structured_schema(schema: &Value) -> bool {
    let Some(schema) = as_object(schema) else { return false };
    let types: Vec<&Value> = match schema.get("type") {
        Some(Value::String(_)) => schema.get("type").into_iter().collect(),
        Some(Value::Array(types)) => types.iter().collect(),
        _ => Vec::new(),
    };
    let has_type = |name: &str| types.iter().any(|value| value.as_str() == Some(name));
    has_type("object") || has_type("array") || schema.contains_key("properties") || schema.contains_key("items")
}

fn schema_allows_null(schema: &Value) -> bool {
    let Some(schema) = as_object(schema) else { return false };
    if schema.get("type").and_then(Value::as_str) == Some("null") {
        return true;
    }
    if let Some(Value::Array(types)) = schema.get("type")
        && types.iter().any(|value| value.as_str() == Some("null"))
    {
        return true;
    }
    if schema.get("const") == Some(&Value::Null) {
        return true;
    }
    if let Some(Value::Array(values)) = schema.get("enum")
        && values.iter().any(Value::is_null)
    {
        return true;
    }
    match schema.get("anyOf") {
        Some(Value::Array(variants)) => variants.iter().any(schema_allows_null),
        _ => false,
    }
}

fn make_json_schema_node_strict(schema: &mut Value) -> Result<(), UnsupportedStrictJsonSchemaError> {
    if as_object(schema).is_none() {
        return Err(unsupported("boolean schemas are unsupported"));
    }
    for key in UNSUPPORTED_STRICT_SCHEMA_KEYS {
        if as_object(schema).is_some_and(|schema| schema.contains_key(*key)) {
            return Err(unsupported(&format!("{key} schemas are unsupported")));
        }
    }

    if as_object(schema).is_some_and(|schema| schema.contains_key("anyOf")) {
        let variants = match as_object(schema).and_then(|schema| schema.get("anyOf")) {
            Some(Value::Array(variants)) if !variants.is_empty() => variants.clone(),
            _ => return Err(unsupported("anyOf must contain at least one schema")),
        };
        for variant in &variants {
            if is_structured_schema(variant) {
                return Err(unsupported("object and array unions are unsupported"));
            }
        }
        let mut strict_variants = Vec::with_capacity(variants.len());
        for mut variant in variants {
            make_json_schema_node_strict(&mut variant)?;
            strict_variants.push(variant);
        }
        schema.as_object_mut().expect("object").insert("anyOf".into(), Value::Array(strict_variants));
    }

    if as_object(schema).is_some_and(|schema| schema.contains_key("items")) {
        if matches!(as_object(schema).and_then(|schema| schema.get("items")), Some(Value::Array(_))) {
            return Err(unsupported("tuple schemas are unsupported"));
        }
        let mut items = as_object(schema).and_then(|schema| schema.get("items")).cloned().unwrap_or(Value::Null);
        make_json_schema_node_strict(&mut items)?;
        schema.as_object_mut().expect("object").insert("items".into(), items);
    }

    let is_object_schema = as_object(schema).and_then(|schema| schema.get("type")).and_then(Value::as_str)
        == Some("object");
    let has_properties = as_object(schema).is_some_and(|schema| schema.contains_key("properties"));
    if has_properties && !is_object_schema {
        return Err(unsupported("properties require type object"));
    }
    if !is_object_schema {
        return Ok(());
    }
    if let Some(additional) = as_object(schema).and_then(|schema| schema.get("additionalProperties"))
        && additional != &Value::Bool(false)
    {
        return Err(unsupported("schema-valued or true additionalProperties is unsupported"));
    }
    if has_properties && as_object(schema).and_then(|schema| schema.get("properties")).and_then(as_object).is_none() {
        return Err(unsupported("object properties must be a schema map"));
    }
    if let Some(required) = as_object(schema).and_then(|schema| schema.get("required")) {
        let valid = matches!(required, Value::Array(items) if items.iter().all(Value::is_string));
        if !valid {
            return Err(unsupported("object required must be a string array"));
        }
    }

    let mut properties = as_object(schema)
        .and_then(|schema| schema.get("properties"))
        .and_then(as_object)
        .cloned()
        .unwrap_or_default();
    let property_names: Vec<String> = properties.keys().cloned().collect();
    let required: Vec<String> = match as_object(schema).and_then(|schema| schema.get("required")) {
        Some(Value::Array(items)) => items.iter().filter_map(|item| item.as_str().map(str::to_owned)).collect(),
        _ => Vec::new(),
    };
    if required.iter().any(|key| !property_names.contains(key)) {
        return Err(unsupported("required contains an unknown property"));
    }
    for (key, property) in properties.clone() {
        let mut property = property;
        make_json_schema_node_strict(&mut property)?;
        if !required.contains(&key) && !schema_allows_null(&property) {
            let mut wrapper = Map::new();
            wrapper.insert("anyOf".into(), Value::Array(vec![property, serde_json::json!({ "type": "null" })]));
            properties.insert(key, Value::Object(wrapper));
        } else {
            properties.insert(key, property);
        }
    }
    let schema = schema.as_object_mut().expect("object");
    if schema.contains_key("properties") {
        schema.insert("properties".into(), Value::Object(properties));
    }
    schema.insert("required".into(), Value::Array(property_names.into_iter().map(Value::String).collect()));
    schema.insert("additionalProperties".into(), Value::Bool(false));
    Ok(())
}

/// Convert a tool schema to the strict subset expected by provider constrained sampling.
pub fn make_strict_json_schema(schema: &Value) -> Result<Value, UnsupportedStrictJsonSchemaError> {
    let mut cloned = schema.clone();
    if as_object(&cloned).is_none() {
        return Err(unsupported("root schema must have type object"));
    }
    make_json_schema_node_strict(&mut cloned)?;
    if as_object(&cloned).and_then(|cloned| cloned.get("type")).and_then(Value::as_str) != Some("object") {
        return Err(unsupported("root schema must have type object"));
    }
    Ok(cloned)
}

pub fn get_json_schema_tool_parameters(
    tool: &Tool,
    strict: Option<bool>,
) -> Result<Value, UnsupportedStrictJsonSchemaError> {
    if strict == Some(true) {
        make_strict_json_schema(&tool.parameters)
    } else {
        Ok(tool.parameters.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrammarConstrainedSampling {
    pub format: GrammarFormat,
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
    match arguments.get(input_property) {
        Some(Value::String(input)) => Ok(input.clone()),
        _ => Err(format!(
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
    let Some(input_delta) = next_input.strip_prefix(buffer.input.as_str()) else {
        return Err(format!("grammar tool input for property \"{input_property}\" changed non-monotonically"));
    };

    if !close && input_delta.is_empty() {
        return Ok(None);
    }

    let mut delta = String::new();
    if !buffer.started {
        delta.push('{');
        delta.push_str(&js_json_string(input_property));
        delta.push_str(":\"");
        buffer.started = true;
    }
    let quoted = js_json_string(input_delta);
    delta.push_str(&quoted[1..quoted.len() - 1]);
    buffer.input = next_input.to_owned();

    if close {
        delta.push_str("\"}");
        buffer.closed = true;
    }
    Ok(Some(delta))
}

fn js_json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| String::from("\"\""))
}

fn infer_grammar_input_property(tool: &Tool) -> Result<String, String> {
    let Some(schema) = tool.parameters.as_object() else {
        return Err("grammar constrained sampling requires an object parameter schema".to_owned());
    };
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err("grammar constrained sampling requires an object parameter schema".to_owned());
    }
    let required: Vec<&str> = match schema.get("required") {
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if required.len() != 1 {
        return Err("grammar constrained sampling requires exactly one required string property".to_owned());
    }

    let input_property = required[0];
    let properties = schema.get("properties").and_then(Value::as_object);
    let Some(property) = properties.and_then(|properties| properties.get(input_property)) else {
        return Err(format!("grammar constrained sampling requires a properties entry for {input_property}"));
    };
    if property.get("type").and_then(Value::as_str) != Some("string") {
        return Err(format!("grammar constrained sampling property {input_property} must have type string"));
    }
    Ok(input_property.to_owned())
}

pub fn resolve_json_schema_strict_sampling(tool: &Tool, supports_strict_mode: bool) -> Result<Option<bool>, String> {
    let Some(config) = tool.constrained_sampling.as_ref() else { return Ok(None) };
    let ConstrainedSampling::Config(ConstrainedSamplingConfig::JsonSchema { strict }) = config else {
        return Ok(None);
    };

    if supports_strict_mode {
        match make_strict_json_schema(&tool.parameters) {
            Ok(_) => return Ok(Some(true)),
            Err(error) => {
                if *strict != JsonSchemaStrictness::Require {
                    return Ok(None);
                }
                return Err(format!(
                    "Tool \"{}\" requires JSON-schema constrained sampling, but {}.",
                    tool.name, error
                ));
            }
        }
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
    let Some(config) = tool.constrained_sampling.as_ref() else { return Ok(None) };
    let ConstrainedSampling::Config(ConstrainedSamplingConfig::Grammar { variants }) = config else {
        return Ok(None);
    };

    if !supports_openai_grammar_tools {
        return Ok(None);
    }

    let lark_definition = variants.get(&GrammarFormat::OpenaiLark);
    let regex_definition = variants.get(&GrammarFormat::OpenaiRegex);
    let has_lark_definition = lark_definition.is_some_and(|definition| !definition.trim().is_empty());
    let has_regex_definition = regex_definition.is_some_and(|definition| !definition.trim().is_empty());
    if !has_lark_definition && !has_regex_definition {
        return Err(format!(
            "Tool \"{}\" cannot use grammar constrained sampling: no supported grammar variant was provided.",
            tool.name
        ));
    }

    let resolved = (|| {
        let input_property = infer_grammar_input_property(tool)?;
        let (format, definition) = if has_lark_definition {
            (GrammarFormat::OpenaiLark, lark_definition.cloned().unwrap_or_default())
        } else {
            (GrammarFormat::OpenaiRegex, regex_definition.cloned().unwrap_or_default())
        };
        Ok::<_, String>(GrammarConstrainedSampling { format, definition, input_property })
    })();
    resolved.map(Some).map_err(|error| {
        format!("Tool \"{}\" cannot use grammar constrained sampling: {}.", tool.name, error)
    })
}

pub fn create_grammar_tool_input_properties(
    tools: Option<&[Tool]>,
    supports_openai_grammar_tools: bool,
) -> Result<BTreeMap<String, String>, String> {
    let mut properties = BTreeMap::new();
    for tool in tools.unwrap_or_default() {
        if let Some(grammar) = resolve_grammar_constrained_sampling(tool, supports_openai_grammar_tools)? {
            properties.insert(tool.name.clone(), grammar.input_property);
        }
    }
    Ok(properties)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, parameters: Value) -> Tool {
        Tool {
            name: name.into(),
            description: "d".into(),
            parameters,
            freeform: None,
            constrained_sampling: None,
        }
    }

    #[test]
    fn strict_conversion_wraps_optional_properties_and_closes_the_object() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "a": { "type": "string" },
                "b": { "type": "string" },
                "c": { "type": ["string", "null"] }
            },
            "required": ["a"]
        });
        let strict = make_strict_json_schema(&schema).expect("strict");
        assert_eq!(
            strict,
            serde_json::json!({
                "type": "object",
                "properties": {
                    "a": { "type": "string" },
                    "b": { "anyOf": [{ "type": "string" }, { "type": "null" }] },
                    "c": { "type": ["string", "null"] }
                },
                "required": ["a", "b", "c"],
                "additionalProperties": false
            })
        );
    }

    #[test]
    fn unsupported_keys_report_the_ts_message() {
        let schema = serde_json::json!({ "type": "object", "$ref": "#/x" });
        assert_eq!(
            make_strict_json_schema(&schema),
            Err(UnsupportedStrictJsonSchemaError("$ref schemas are unsupported".into()))
        );
        assert_eq!(
            make_strict_json_schema(&serde_json::json!(true)),
            Err(UnsupportedStrictJsonSchemaError("root schema must have type object".into()))
        );
        assert_eq!(
            make_strict_json_schema(&serde_json::json!({
                "type": "object",
                "properties": { "a": true }
            })),
            Err(UnsupportedStrictJsonSchemaError("boolean schemas are unsupported".into()))
        );
        assert_eq!(
            make_strict_json_schema(&serde_json::json!({ "type": "array" })),
            Err(UnsupportedStrictJsonSchemaError("root schema must have type object".into()))
        );
    }

    #[test]
    fn strict_sampling_requires_or_falls_back() {
        let mut required = tool("t", serde_json::json!({ "type": "object", "$ref": "#/x" }));
        required.constrained_sampling = Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::JsonSchema {
            strict: JsonSchemaStrictness::Require,
        }));
        assert_eq!(
            resolve_json_schema_strict_sampling(&required, true),
            Err("Tool \"t\" requires JSON-schema constrained sampling, but $ref schemas are unsupported.".into())
        );
        assert_eq!(
            resolve_json_schema_strict_sampling(&required, false),
            Err("Tool \"t\" requires JSON-schema constrained sampling, but strict tools are unsupported.".into())
        );

        let mut prefer = tool("t", serde_json::json!({ "type": "object", "$ref": "#/x" }));
        prefer.constrained_sampling = Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::JsonSchema {
            strict: JsonSchemaStrictness::Prefer,
        }));
        assert_eq!(resolve_json_schema_strict_sampling(&prefer, true), Ok(None));
        assert_eq!(resolve_json_schema_strict_sampling(&prefer, false), Ok(None));

        let mut disabled = tool("t", serde_json::json!({ "type": "object" }));
        disabled.constrained_sampling = Some(ConstrainedSampling::Disabled(false));
        assert_eq!(resolve_json_schema_strict_sampling(&disabled, true), Ok(None));
    }

    #[test]
    fn grammar_deltas_stream_the_input_property() {
        let mut buffer = GrammarToolInputJsonBuffer::default();
        assert_eq!(append_grammar_tool_input_json_delta(&mut buffer, "q", "ab", false), Ok(Some("{\"q\":\"ab".into())));
        assert_eq!(append_grammar_tool_input_json_delta(&mut buffer, "q", "ab", false), Ok(None));
        assert_eq!(append_grammar_tool_input_json_delta(&mut buffer, "q", "abcd", false), Ok(Some("cd".into())));
        assert_eq!(append_grammar_tool_input_json_delta(&mut buffer, "q", "abcd", true), Ok(Some("\"}".into())));
        assert_eq!(append_grammar_tool_input_json_delta(&mut buffer, "q", "abcd", true), Ok(None));
        assert_eq!(
            append_grammar_tool_input_json_delta(&mut buffer, "q", "abcde", true),
            Err("grammar tool input for property \"q\" changed after it was closed".into())
        );
    }

    #[test]
    fn grammar_non_monotonic_input_is_rejected() {
        let mut buffer = GrammarToolInputJsonBuffer::default();
        assert_eq!(append_grammar_tool_input_json_delta(&mut buffer, "q", "ab", false), Ok(Some("{\"q\":\"ab".into())));
        assert_eq!(
            append_grammar_tool_input_json_delta(&mut buffer, "q", "az", false),
            Err("grammar tool input for property \"q\" changed non-monotonically".into())
        );
    }

    #[test]
    fn grammar_resolution_requires_a_variant_and_one_string_property() {
        let mut tool_with_grammar = tool(
            "g",
            serde_json::json!({ "type": "object", "properties": { "q": { "type": "string" } }, "required": ["q"] }),
        );
        tool_with_grammar.constrained_sampling =
            Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::Grammar { variants: BTreeMap::new() }));
        assert_eq!(
            resolve_grammar_constrained_sampling(&tool_with_grammar, true),
            Err("Tool \"g\" cannot use grammar constrained sampling: no supported grammar variant was provided.".into())
        );

        let mut variants = BTreeMap::new();
        variants.insert(GrammarFormat::OpenaiLark, "start: /[a-z]+/".to_owned());
        tool_with_grammar.constrained_sampling =
            Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::Grammar { variants }));
        assert_eq!(
            resolve_grammar_constrained_sampling(&tool_with_grammar, true),
            Ok(Some(GrammarConstrainedSampling {
                format: GrammarFormat::OpenaiLark,
                definition: "start: /[a-z]+/".into(),
                input_property: "q".into(),
            }))
        );
        assert_eq!(resolve_grammar_constrained_sampling(&tool_with_grammar, false), Ok(None));

        let mut two_required = tool(
            "g",
            serde_json::json!({
                "type": "object",
                "properties": { "q": { "type": "string" }, "r": { "type": "string" } },
                "required": ["q", "r"]
            }),
        );
        let mut variants = BTreeMap::new();
        variants.insert(GrammarFormat::OpenaiRegex, "[a-z]+".to_owned());
        two_required.constrained_sampling =
            Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::Grammar { variants }));
        assert_eq!(
            resolve_grammar_constrained_sampling(&two_required, true),
            Err(
                "Tool \"g\" cannot use grammar constrained sampling: grammar constrained sampling requires exactly \
                 one required string property."
                    .into()
            )
        );
    }

    #[test]
    fn grammar_tool_input_requires_a_string_argument() {
        let mut arguments = Map::new();
        arguments.insert("q".into(), Value::from(7));
        assert_eq!(
            get_grammar_tool_input("t", &arguments, "q"),
            Err("Grammar tool call \"t\" requires argument \"q\" to be a string.".into())
        );
        arguments.insert("q".into(), Value::from("hi"));
        assert_eq!(get_grammar_tool_input("t", &arguments, "q"), Ok("hi".into()));
    }

    #[test]
    fn grammar_tool_input_properties_indexes_each_grammar_tool() {
        let mut variants = BTreeMap::new();
        variants.insert(GrammarFormat::OpenaiLark, "start: /[a-z]+/".to_owned());
        let mut grammar = tool(
            "g",
            serde_json::json!({ "type": "object", "properties": { "q": { "type": "string" } }, "required": ["q"] }),
        );
        grammar.constrained_sampling =
            Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::Grammar { variants }));
        let plain = tool("p", serde_json::json!({ "type": "object" }));
        let properties = create_grammar_tool_input_properties(Some(&[grammar, plain]), true).expect("properties");
        assert_eq!(properties.len(), 1);
        assert_eq!(properties.get("g").map(String::as_str), Some("q"));
    }
}
