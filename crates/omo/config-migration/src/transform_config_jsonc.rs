use serde_json::{Map, Value};

use crate::legacy_history::legacy_migration_history;
use crate::record_values::copy_record;
use crate::schema_url::OMO_SCHEMA_URL;
use crate::transform_types::{ConfigMigrationTransformResult, TransformConfigJsoncSourcesInput};
use crate::types::LegacyConfigSourceKind;

fn config_document(input: &TransformConfigJsoncSourcesInput<'_>) -> Map<String, Value> {
    let config_paths: Vec<&str> = input
        .discovered
        .iter()
        .filter(|source| source.kind == LegacyConfigSourceKind::ConfigJsonc)
        .map(|source| source.path.as_str())
        .collect();
    input
        .sources
        .iter()
        .filter(|source| config_paths.contains(&source.path.as_str()))
        .find_map(|source| source.value.as_object().cloned())
        .unwrap_or_default()
}

fn record_at(document: &Map<String, Value>, key: &str) -> Option<Map<String, Value>> {
    document
        .get(key)
        .and_then(Value::as_object)
        .map(copy_record)
}

pub fn transform_config_jsonc_sources(
    input: &TransformConfigJsoncSourcesInput<'_>,
) -> ConfigMigrationTransformResult {
    let legacy = config_document(input);
    let omo = record_at(&legacy, "[omo]");
    let senpi = record_at(&legacy, "[senpi]");
    let history = legacy_migration_history(input.discovered, input.sources);
    let diagnostics = if omo.is_some() && senpi.is_some() {
        vec!["conflict: [senpi] legacy [omo] kept [senpi]".to_string()]
    } else {
        Vec::new()
    };
    let mut document = Map::new();
    document.insert("$schema".into(), Value::String(OMO_SCHEMA_URL.into()));
    for key in ["codegraph", "[opencode]", "[codex]"] {
        if let Some(record) = record_at(&legacy, key) {
            document.insert(key.into(), Value::Object(record));
        }
    }
    if let Some(harness) = senpi.or(omo) {
        document.insert("[senpi]".into(), Value::Object(harness));
    }
    if !history.is_empty() {
        document.insert("legacy_migrations".into(), Value::Object(history));
    }
    ConfigMigrationTransformResult {
        diagnostics,
        document,
    }
}
