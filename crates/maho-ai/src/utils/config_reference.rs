//! Port of senpi packages/ai/src/utils/config-reference.ts: strict `$file` / `$env` /
//! non-nested `$concat` configuration references.

use serde_json::{Map, Value, json};

pub use super::config_reference_error::{ConfigReferenceError, ConfigReferenceReason};

/// One `$concat` part: a literal string or a non-nested `$file` / `$env` reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigConcatPart {
    Literal(String),
    File(String),
    Env(String),
}

/// `ConfigReference`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigReference {
    File(String),
    Env(String),
    Concat(Vec<ConfigConcatPart>),
}

/// `ConfigValue = string | ConfigReference`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigValue {
    String(String),
    Reference(ConfigReference),
}

fn file_reference_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "$file": { "type": "string", "minLength": 1 } },
        "required": ["$file"],
        "additionalProperties": false
    })
}

fn env_reference_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "$env": { "type": "string", "pattern": "^[A-Za-z_][A-Za-z0-9_]*$" } },
        "required": ["$env"],
        "additionalProperties": false
    })
}

/// `ConfigReferenceSchema` as JSON Schema.
pub fn config_reference_schema() -> Value {
    json!({ "anyOf": [
        file_reference_schema(),
        env_reference_schema(),
        {
            "type": "object",
            "properties": { "$concat": { "type": "array", "items": { "anyOf": [
                { "type": "string" }, file_reference_schema(), env_reference_schema()
            ] } } },
            "required": ["$concat"],
            "additionalProperties": false
        }
    ] })
}

/// `ConfigValueSchema`.
pub fn config_value_schema() -> Value {
    json!({ "anyOf": [{ "type": "string" }, config_reference_schema()] })
}

/// `NonemptyConfigValueSchema`.
pub fn nonempty_config_value_schema() -> Value {
    json!({ "anyOf": [{ "type": "string", "minLength": 1 }, config_reference_schema()] })
}

fn single_key<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    if object.len() == 1 {
        object.get(key)
    } else {
        None
    }
}

fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn leaf_reference(value: &Value) -> Option<ConfigConcatPart> {
    let object = value.as_object()?;
    if let Some(Value::String(path)) = single_key(object, "$file") {
        return (!path.is_empty()).then(|| ConfigConcatPart::File(path.clone()));
    }
    if let Some(Value::String(name)) = single_key(object, "$env") {
        return is_env_name(name).then(|| ConfigConcatPart::Env(name.clone()));
    }
    None
}

/// The typed reference, or `None` when `value` is not exactly one of the three shapes.
pub fn parse_config_reference(value: &Value) -> Option<ConfigReference> {
    match leaf_reference(value) {
        Some(ConfigConcatPart::File(path)) => return Some(ConfigReference::File(path)),
        Some(ConfigConcatPart::Env(name)) => return Some(ConfigReference::Env(name)),
        _ => {}
    }
    let Value::Array(parts) = single_key(value.as_object()?, "$concat")? else {
        return None;
    };
    parts
        .iter()
        .map(|part| match part {
            Value::String(text) => Some(ConfigConcatPart::Literal(text.clone())),
            other => leaf_reference(other),
        })
        .collect::<Option<Vec<_>>>()
        .map(ConfigReference::Concat)
}

pub fn is_config_reference(value: &Value) -> bool {
    parse_config_reference(value).is_some()
}

/// Used only at declared string-value boundaries, never on opaque JSON fields.
pub fn assert_config_value(
    value: &Value,
    field: &str,
) -> Result<ConfigValue, ConfigReferenceError> {
    if let Value::String(text) = value {
        return Ok(ConfigValue::String(text.clone()));
    }
    parse_config_reference(value)
        .map(ConfigValue::Reference)
        .ok_or_else(|| {
            ConfigReferenceError::new(field, value.clone(), ConfigReferenceReason::Malformed)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_exactly_the_three_reference_shapes() {
        assert_eq!(
            parse_config_reference(&json!({ "$file": "/k" })),
            Some(ConfigReference::File("/k".into()))
        );
        assert_eq!(
            parse_config_reference(&json!({ "$env": "OPENAI_API_KEY" })),
            Some(ConfigReference::Env("OPENAI_API_KEY".into()))
        );
        assert_eq!(
            parse_config_reference(
                &json!({ "$concat": ["Bearer ", { "$env": "T" }, { "$file": "/f" }] })
            ),
            Some(ConfigReference::Concat(vec![
                ConfigConcatPart::Literal("Bearer ".into()),
                ConfigConcatPart::Env("T".into()),
                ConfigConcatPart::File("/f".into()),
            ]))
        );
    }

    #[test]
    fn rejects_nested_extra_keyed_empty_and_invalid_references() {
        for value in [
            json!({ "$file": "" }),
            json!({ "$env": "1BAD" }),
            json!({ "$env": "A-B" }),
            json!({ "$file": "/k", "extra": 1 }),
            json!({ "$concat": [{ "$concat": ["x"] }] }),
            json!({ "$concat": [1] }),
            json!({ "$concat": "x" }),
            json!({ "$cmd": "cat" }),
            json!(5),
        ] {
            assert!(!is_config_reference(&value), "{value}");
        }
    }

    #[test]
    fn assert_config_value_reports_malformed_with_the_field_and_never_resolves() {
        assert_eq!(
            assert_config_value(&json!("plain"), "x"),
            Ok(ConfigValue::String("plain".into()))
        );
        let error = assert_config_value(&json!({ "$file": 3 }), "providers.p.apiKey")
            .expect_err("malformed");
        assert_eq!(error.reason, ConfigReferenceReason::Malformed);
        assert_eq!(
            error.to_string(),
            "providers.p.apiKey: configuration reference malformed"
        );
        assert_eq!(error.reference, json!({ "$file": 3 }));
    }
}
