use std::fmt;

use omo_config_core::{Issues, normalize_legacy_model_entry, normalize_legacy_model_fields};
use serde_json::{Map, Value};

use crate::record_values::copy_record;
use crate::transform_types::ConfigMigrationTransformResult;

pub const REASONING_UNIFICATION_MIGRATION_ID: &str = "2026-08-reasoning-unification";

const CORE_SETTING_KEYS: [&str; 4] = ["reasoning", "variant", "reasoningEffort", "thinking"];
const TYPED_SETTING_KEYS: [&str; 3] = ["provider_options", "providerOptions", "textVerbosity"];
const OPENCODE_PASSTHROUGH_KEYS: [&str; 3] = ["maxTokens", "providerOptions", "textVerbosity"];
const COMBINED_DROPPED_KEYS: [&str; 7] = [
    "reasoning",
    "variant",
    "reasoningEffort",
    "provider_options",
    "providerOptions",
    "thinking",
    "textVerbosity",
];

/// A legacy model entry that does not parse as a model reference after normalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningUnificationError {
    pub issues: Issues,
}

impl fmt::Display for ReasoningUnificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let messages: Vec<String> = self
            .issues
            .iter()
            .map(|issue| format!("{}: {}", issue.path_string(), issue.message))
            .collect();
        write!(formatter, "invalid model entry: {}", messages.join("; "))
    }
}

impl std::error::Error for ReasoningUnificationError {}

type Outcome<T> = Result<T, ReasoningUnificationError>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum DefinitionKind {
    Agent,
    Category,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Harness {
    Typed,
    OpenCode,
}

fn normalize_entry(value: &Value) -> Outcome<Value> {
    normalize_legacy_model_entry(value).map_err(|issues| ReasoningUnificationError { issues })
}

fn normalize_string_model(value: &str) -> Outcome<Value> {
    let mut entry = Map::new();
    entry.insert("model".into(), Value::String(value.to_string()));
    let normalized = normalize_entry(&Value::Object(entry))?;
    Ok(normalized.get("model").cloned().unwrap_or(Value::Null))
}

fn normalize_model_ref(value: &Value) -> Outcome<Value> {
    match value {
        Value::String(model) => normalize_string_model(model),
        Value::Object(_) => normalize_entry(value),
        other => Ok(other.clone()),
    }
}

fn normalized_list(value: Option<&Value>) -> Outcome<Vec<Value>> {
    match value {
        None => Ok(Vec::new()),
        Some(Value::Array(entries)) => entries.iter().map(normalize_model_ref).collect(),
        Some(single) => Ok(vec![normalize_model_ref(single)?]),
    }
}

fn setting_keys(harness: Harness) -> Vec<&'static str> {
    let mut keys = CORE_SETTING_KEYS.to_vec();
    if harness == Harness::Typed {
        keys.extend(TYPED_SETTING_KEYS);
    }
    keys
}

fn primary_model_ref(record: &Map<String, Value>, harness: Harness) -> Outcome<Option<Value>> {
    let Some(Value::String(model)) = record.get("model") else {
        return Ok(None);
    };
    let keys = setting_keys(harness);
    if !keys.iter().any(|key| record.contains_key(*key)) {
        return normalize_string_model(model).map(Some);
    }
    let mut settings = Map::new();
    settings.insert("model".into(), Value::String(model.clone()));
    for key in keys {
        if let Some(value) = record.get(key) {
            settings.insert(key.into(), value.clone());
        }
    }
    normalize_entry(&Value::Object(settings)).map(Some)
}

fn conflict_diagnostic(record: &Map<String, Value>, path: &[String]) -> Option<String> {
    let present: Vec<(&str, &Value)> = ["reasoning", "reasoningEffort", "variant"]
        .into_iter()
        .filter_map(|key| record.get(key).map(|value| (key, value)))
        .collect();
    let [(kept_key, kept_value), dropped @ ..] = present.as_slice() else {
        return None;
    };
    if dropped.is_empty() {
        return None;
    }
    let dropped_text = dropped
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(" ");
    Some(format!(
        "conflict: {} dropped {dropped_text} kept {kept_key}={kept_value}",
        path.join(".")
    ))
}

struct Walk {
    diagnostics: Vec<String>,
}

fn child_path(path: &[String], segment: &str) -> Vec<String> {
    let mut next = path.to_vec();
    next.push(segment.to_string());
    next
}

impl Walk {
    fn normalize_definition(
        &mut self,
        value: &Value,
        kind: DefinitionKind,
        path: &[String],
        harness: Harness,
    ) -> Outcome<Value> {
        let Value::Object(record) = value else {
            return Ok(value.clone());
        };
        if let Some(conflict) = conflict_diagnostic(record, path) {
            self.diagnostics.push(conflict);
        }

        let mut input = copy_record(record);
        if harness == Harness::OpenCode {
            for key in OPENCODE_PASSTHROUGH_KEYS {
                input.shift_remove(key);
            }
        }
        let Value::Object(mut normalized) = normalize_legacy_model_fields(&Value::Object(input))
        else {
            return Ok(value.clone());
        };
        if harness == Harness::OpenCode {
            for key in OPENCODE_PASSTHROUGH_KEYS {
                if let Some(original) = record.get(key) {
                    normalized.insert(key.into(), original.clone());
                }
            }
        }
        if let Some(Value::String(model)) = normalized.get("model") {
            let model = normalize_string_model(model)?;
            normalized.insert("model".into(), model);
        }
        if let Some(Value::Array(models)) = normalized.get("models") {
            let models = normalized_list(Some(&Value::Array(models.clone())))?;
            normalized.insert("models".into(), Value::Array(models));
        }

        let fallback_models = record.get("fallback_models");
        let should_combine = fallback_models.is_some()
            || (kind == DefinitionKind::Agent
                && record.contains_key("model")
                && record.contains_key("models"));
        if !should_combine {
            return Ok(Value::Object(normalized));
        }

        let primary = primary_model_ref(record, harness)?;
        let existing = match kind {
            DefinitionKind::Agent => normalized_list(record.get("models"))?,
            DefinitionKind::Category => Vec::new(),
        };
        let fallbacks = normalized_list(fallback_models)?;
        let combined: Vec<Value> = primary
            .into_iter()
            .chain(existing)
            .chain(fallbacks)
            .collect();
        normalized.insert("models".into(), Value::Array(combined));
        normalized.shift_remove("model");
        normalized.shift_remove("fallback_models");
        for key in COMBINED_DROPPED_KEYS {
            normalized.shift_remove(key);
        }
        Ok(Value::Object(normalized))
    }

    fn normalize_definitions(
        &mut self,
        value: &Value,
        kind: DefinitionKind,
        path: &[String],
        harness: Harness,
    ) -> Outcome<Value> {
        let Value::Object(record) = value else {
            return Ok(value.clone());
        };
        let mut result = Map::new();
        for (name, definition) in record {
            let normalized =
                self.normalize_definition(definition, kind, &child_path(path, name), harness)?;
            result.insert(name.clone(), normalized);
        }
        Ok(Value::Object(result))
    }

    fn normalize_block_definitions(
        &mut self,
        result: &mut Map<String, Value>,
        path: &[String],
        harness: Harness,
    ) -> Outcome<()> {
        for (key, kind) in [
            ("categories", DefinitionKind::Category),
            ("agents", DefinitionKind::Agent),
        ] {
            if let Some(value) = result.get(key) {
                let normalized =
                    self.normalize_definitions(value, kind, &child_path(path, key), harness)?;
                result.insert(key.into(), normalized);
            }
        }
        Ok(())
    }

    fn normalize_typed_block(
        &mut self,
        value: &Value,
        path: &[String],
        recurse_profiles: bool,
    ) -> Outcome<Value> {
        let Value::Object(record) = value else {
            return Ok(value.clone());
        };
        let mut result = copy_record(record);
        self.normalize_block_definitions(&mut result, path, Harness::Typed)?;
        if let Some(Value::Object(catalog)) = result.get("models") {
            let mut normalized = Map::new();
            for (name, entry) in catalog {
                normalized.insert(name.clone(), normalize_model_ref(entry)?);
            }
            result.insert("models".into(), Value::Object(normalized));
        }
        for harness in ["[senpi]", "[codex]"] {
            if let Some(block) = result.get(harness) {
                let normalized =
                    self.normalize_typed_block(block, &child_path(path, harness), false)?;
                result.insert(harness.into(), normalized);
            }
        }
        if let Some(block) = result.get("[opencode]") {
            let normalized =
                self.normalize_open_code_block(block, &child_path(path, "[opencode]"))?;
            result.insert("[opencode]".into(), normalized);
        }
        if recurse_profiles && let Some(Value::Object(profiles)) = result.get("profiles") {
            let mut normalized = Map::new();
            let profiles_path = child_path(path, "profiles");
            for (name, profile) in profiles {
                normalized.insert(
                    name.clone(),
                    self.normalize_typed_block(profile, &child_path(&profiles_path, name), false)?,
                );
            }
            result.insert("profiles".into(), Value::Object(normalized));
        }
        Ok(Value::Object(result))
    }

    fn normalize_open_code_block(&mut self, value: &Value, path: &[String]) -> Outcome<Value> {
        let Value::Object(record) = value else {
            return Ok(value.clone());
        };
        let mut result = copy_record(record);
        self.normalize_block_definitions(&mut result, path, Harness::OpenCode)?;
        Ok(Value::Object(result))
    }
}

/// Rewrites legacy reasoning shapes (`variant`, `reasoningEffort`, `thinking`, `fallback_models`)
/// into the unified `reasoning` + `models` form, reporting overlapping legacy keys as diagnostics.
pub fn transform_reasoning_unification(
    document: Option<&Value>,
) -> Outcome<ConfigMigrationTransformResult> {
    let mut walk = Walk {
        diagnostics: Vec::new(),
    };
    let document = match document {
        Some(value @ Value::Object(_)) => match walk.normalize_typed_block(value, &[], true)? {
            Value::Object(record) => record,
            _ => Map::new(),
        },
        _ => Map::new(),
    };
    Ok(ConfigMigrationTransformResult {
        diagnostics: walk.diagnostics,
        document,
    })
}
