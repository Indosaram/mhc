//! Port of senpi packages/coding-agent/src/core/config-reference-resolver.ts.
//!
//! A configuration value is a plain string or an object reference (`{"$env"}`, `{"$file"}`, `{"$concat"}`;
//! the shapes are parsed by maho-ai `utils::config_reference`). A reference resolves only with a trusted
//! origin: upstream remembers origins in a `WeakMap` keyed by the reference object; here the origin travels
//! inside the value (`ConfigValue::Reference { origin }`), set by `remember_config_reference_origin` at a
//! trusted loader's declared value slots. A reference without one (an extension-supplied value) is
//! `forbidden`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use maho_ai::utils::config_reference::{ConfigConcatPart, ConfigReference, parse_config_reference};
use maho_ai::utils::config_reference_error::{ConfigReferenceError, ConfigReferenceReason};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, json};

use crate::config_reference_env::warn_opaque_config_environment;
use crate::config_reference_file::{FileReference, read_config_reference_file};
use crate::config_reference_redaction::remember_resolved_reference_value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigReferenceContext {
    pub origin: String,
    pub base_dir: Option<PathBuf>,
    pub authority: String,
    pub field: String,
    pub env: Option<HashMap<String, String>>,
}

impl ConfigReferenceContext {
    pub fn trusted(origin: impl Into<String>, field: impl Into<String>) -> Self {
        Self {
            origin: origin.into(),
            base_dir: None,
            authority: "trusted".into(),
            field: field.into(),
            env: None,
        }
    }

    /// A provider extension's value: never trusted, so any reference it carries is `forbidden`.
    pub fn extension(field: impl Into<String>) -> Self {
        Self {
            origin: "provider extension".into(),
            base_dir: None,
            authority: "extension".into(),
            field: field.into(),
            env: None,
        }
    }
}

/// `ConfigValue = string | ConfigReference`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigValue {
    String(String),
    Reference {
        reference: ConfigReference,
        /// The trusted declaration that loaded this reference; `None` until a trusted loader remembers it.
        origin: Option<Box<ConfigReferenceContext>>,
    },
}

impl ConfigValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            Self::Reference { .. } => None,
        }
    }

    pub fn is_reference(&self) -> bool {
        matches!(self, Self::Reference { .. })
    }

    pub fn reference(&self) -> Option<&ConfigReference> {
        match self {
            Self::Reference { reference, .. } => Some(reference),
            Self::String(_) => None,
        }
    }

    pub fn to_json(&self) -> Value {
        match self {
            Self::String(value) => Value::String(value.clone()),
            Self::Reference { reference, .. } => config_reference_json(reference),
        }
    }
}

impl From<&str> for ConfigValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<String> for ConfigValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl Serialize for ConfigValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_json().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ConfigValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        match value {
            Value::String(text) => Ok(Self::String(text)),
            other => parse_config_reference(&other)
                .map(|reference| Self::Reference {
                    reference,
                    origin: None,
                })
                .ok_or_else(|| serde::de::Error::custom("configuration reference malformed")),
        }
    }
}

fn leaf_json(part: &ConfigConcatPart) -> Value {
    match part {
        ConfigConcatPart::Literal(text) => Value::String(text.clone()),
        ConfigConcatPart::File(path) => json!({ "$file": path }),
        ConfigConcatPart::Env(name) => json!({ "$env": name }),
    }
}

pub fn config_reference_json(reference: &ConfigReference) -> Value {
    match reference {
        ConfigReference::File(path) => json!({ "$file": path }),
        ConfigReference::Env(name) => json!({ "$env": name }),
        ConfigReference::Concat(parts) => {
            json!({ "$concat": parts.iter().map(leaf_json).collect::<Vec<_>>() })
        }
    }
}

/// Called at a trusted loader's declared value slots, before any extension merge.
pub fn remember_config_reference_origin(value: &mut ConfigValue, context: ConfigReferenceContext) {
    if let ConfigValue::Reference { origin, .. } = value {
        *origin = Some(Box::new(context));
    }
}

pub fn config_reference_context<'a>(
    value: &'a ConfigValue,
    context: Option<&'a ConfigReferenceContext>,
) -> Option<&'a ConfigReferenceContext> {
    context.or(match value {
        ConfigValue::Reference { origin, .. } => origin.as_deref(),
        ConfigValue::String(_) => None,
    })
}

/// A separate preflight lets a caller reject forbidden siblings before any read.
pub fn assert_config_reference_authority(
    value: &ConfigValue,
    context: Option<&ConfigReferenceContext>,
) -> Result<(), ConfigReferenceError> {
    let ConfigValue::Reference { .. } = value else {
        return Ok(());
    };
    let source = config_reference_context(value, context);
    if source.map(|source| source.authority.as_str()) != Some("trusted") {
        return Err(ConfigReferenceError::new(
            source.map_or("configuration value", |source| source.field.as_str()),
            value.to_json(),
            ConfigReferenceReason::Forbidden,
        ));
    }
    Ok(())
}

fn env_value(name: &str, env: Option<&HashMap<String, String>>) -> Option<String> {
    match env {
        Some(env) => env.get(name).cloned(),
        None => std::env::var(name).ok(),
    }
}

fn resolve_leaf(
    part: &ConfigConcatPart,
    whole: &ConfigValue,
    source: &ConfigReferenceContext,
    env: Option<&HashMap<String, String>>,
) -> Result<String, ConfigReferenceError> {
    match part {
        ConfigConcatPart::Literal(text) => Ok(text.clone()),
        ConfigConcatPart::File(file) => {
            let base_dir = source.base_dir.clone().unwrap_or_else(|| {
                Path::new(&source.origin)
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_default()
            });
            let text = read_config_reference_file(&FileReference {
                field: &source.field,
                file,
                base_dir: &base_dir,
            })?;
            remember_resolved_reference_value(&text);
            Ok(text)
        }
        ConfigConcatPart::Env(name) => {
            let result = env_value(name, env.or(source.env.as_ref()));
            warn_opaque_config_environment(name, result.as_deref());
            match result {
                None => Err(ConfigReferenceError::new(
                    &source.field,
                    whole.to_json(),
                    ConfigReferenceReason::Unset,
                )),
                Some(value) if value.is_empty() => Err(ConfigReferenceError::new(
                    &source.field,
                    whole.to_json(),
                    ConfigReferenceReason::Empty,
                )),
                Some(value) => {
                    remember_resolved_reference_value(&value);
                    Ok(value)
                }
            }
        }
    }
}

/// senpi `resolveConfigReference`. File reads are synchronous here (bounded at 64 KiB).
pub fn resolve_config_reference(
    value: &ConfigValue,
    context: Option<&ConfigReferenceContext>,
    env: Option<&HashMap<String, String>>,
) -> Result<String, ConfigReferenceError> {
    let ConfigValue::Reference { reference, .. } = value else {
        return Ok(value.as_str().unwrap_or_default().to_owned());
    };
    assert_config_reference_authority(value, context)?;
    // Authority validation establishes this context without assigning a default trusted source to unknown or
    // extension-supplied values.
    let Some(source) = config_reference_context(value, context) else {
        return Err(ConfigReferenceError::new(
            "configuration value",
            value.to_json(),
            ConfigReferenceReason::Forbidden,
        ));
    };
    match reference {
        ConfigReference::File(file) => {
            resolve_leaf(&ConfigConcatPart::File(file.clone()), value, source, env)
        }
        ConfigReference::Env(name) => {
            resolve_leaf(&ConfigConcatPart::Env(name.clone()), value, source, env)
        }
        ConfigReference::Concat(parts) => {
            let mut result = String::new();
            for part in parts {
                // A failing part reports itself as the reference, as the recursive upstream call does.
                let whole = ConfigValue::Reference {
                    reference: match part {
                        ConfigConcatPart::File(file) => ConfigReference::File(file.clone()),
                        ConfigConcatPart::Env(name) => ConfigReference::Env(name.clone()),
                        ConfigConcatPart::Literal(_) => reference.clone(),
                    },
                    origin: None,
                };
                result.push_str(&resolve_leaf(part, &whole, source, env)?);
            }
            if result.is_empty() {
                return Err(ConfigReferenceError::new(
                    &source.field,
                    value.to_json(),
                    ConfigReferenceReason::Empty,
                ));
            }
            remember_resolved_reference_value(&result);
            Ok(result)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trusted(value: Value) -> ConfigValue {
        let mut value: ConfigValue = serde_json::from_value(value).expect("reference");
        remember_config_reference_origin(
            &mut value,
            ConfigReferenceContext::trusted("/cfg/models.json", "providers.p.apiKey"),
        );
        value
    }

    /// senpi test/suite/regressions/3858-reference-values.test.ts.
    #[test]
    fn env_and_concat_references_resolve_and_report_unset_and_empty() {
        let env = HashMap::from([
            ("REF_KEY".to_owned(), "abcd-1234".to_owned()),
            ("BLANK".to_owned(), String::new()),
        ]);
        assert_eq!(
            resolve_config_reference(&trusted(json!({"$env": "REF_KEY"})), None, Some(&env))
                .as_deref(),
            Ok("abcd-1234")
        );
        assert_eq!(
            resolve_config_reference(
                &trusted(json!({"$concat": ["Bearer ", {"$env": "REF_KEY"}]})),
                None,
                Some(&env)
            )
            .as_deref(),
            Ok("Bearer abcd-1234")
        );
        let unset = resolve_config_reference(&trusted(json!({"$env": "NOPE"})), None, Some(&env))
            .expect_err("unset");
        assert_eq!(unset.reason, ConfigReferenceReason::Unset);
        assert_eq!(
            unset.to_string(),
            "providers.p.apiKey: configuration reference unset"
        );
        let empty = resolve_config_reference(&trusted(json!({"$env": "BLANK"})), None, Some(&env))
            .expect_err("empty");
        assert_eq!(empty.reason, ConfigReferenceReason::Empty);
    }

    /// senpi test/suite/regressions/3858-reference-capability.test.ts: a reference without a trusted origin is
    /// forbidden, and an extension context never grants one.
    #[test]
    fn untrusted_references_are_forbidden() {
        let bare: ConfigValue =
            serde_json::from_value(json!({"$env": "REF_KEY"})).expect("reference");
        assert_eq!(
            assert_config_reference_authority(&bare, None)
                .expect_err("forbidden")
                .reason,
            ConfigReferenceReason::Forbidden
        );
        let extension = ConfigReferenceContext::extension("extension.apiKey");
        let error =
            assert_config_reference_authority(&bare, Some(&extension)).expect_err("forbidden");
        assert_eq!(error.field, "extension.apiKey");
        assert!(
            assert_config_reference_authority(&ConfigValue::from("plain"), Some(&extension))
                .is_ok()
        );
    }

    /// senpi test/suite/regressions/3858-reference-shape.test.ts: malformed objects are rejected on parse and
    /// references round-trip.
    #[test]
    fn values_round_trip_and_malformed_objects_fail() {
        let value: ConfigValue =
            serde_json::from_value(json!({"$file": "~/key"})).expect("reference");
        assert_eq!(
            serde_json::to_value(&value).expect("json"),
            json!({"$file": "~/key"})
        );
        assert!(serde_json::from_value::<ConfigValue>(json!({"$cmd": "cat"})).is_err());
        assert_eq!(ConfigValue::from("x").as_str(), Some("x"));
    }
}
