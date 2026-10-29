use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::transform_types::LoadedLegacyConfigSource;
use crate::types::DiscoveredLegacyConfigSource;

fn history_values(value: &Value) -> Vec<String> {
    let Value::Object(record) = value else {
        return Vec::new();
    };
    let mut values: Vec<String> = Vec::new();
    for key in ["_migrations", "appliedMigrations"] {
        let Some(Value::Array(candidate)) = record.get(key) else {
            continue;
        };
        for item in candidate {
            if let Value::String(item) = item
                && !values.contains(item)
            {
                values.push(item.clone());
            }
        }
    }
    values
}

pub(crate) fn legacy_migration_history(
    discovered: &[DiscoveredLegacyConfigSource],
    loaded: &[LoadedLegacyConfigSource],
) -> Map<String, Value> {
    let source_by_path: HashMap<&str, &DiscoveredLegacyConfigSource> = discovered
        .iter()
        .map(|source| (source.path.as_str(), source))
        .collect();
    let mut history: Vec<(String, Vec<String>)> = Vec::new();
    for source in loaded {
        let Some(metadata) = source_by_path.get(source.path.as_str()) else {
            continue;
        };
        let values = history_values(&source.value);
        if values.is_empty() {
            continue;
        }
        let index = match history
            .iter()
            .position(|(path, _)| *path == metadata.config_path)
        {
            Some(index) => index,
            None => {
                history.push((metadata.config_path.clone(), Vec::new()));
                history.len() - 1
            }
        };
        let existing = &mut history[index].1;
        for value in values {
            if !existing.contains(&value) {
                existing.push(value);
            }
        }
    }
    history
        .into_iter()
        .map(|(path, values)| {
            (
                path,
                Value::Array(values.into_iter().map(Value::String).collect()),
            )
        })
        .collect()
}
