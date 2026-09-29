use std::collections::HashSet;

use serde_json::{Map, Value};

pub const MODEL_VERSION_MAP: [(&str, &str); 1] =
    [("anthropic/claude-opus-4-4", "anthropic/claude-opus-4-8")];

const CURRENT_USER_SELECTABLE_MODELS: [&str; 3] = [
    "anthropic/claude-opus-4-5",
    "anthropic/claude-opus-4-6",
    "anthropic/claude-sonnet-4-5",
];

#[derive(Debug, Clone, PartialEq)]
pub struct ModelVersionMigration {
    pub migrated: Map<String, Value>,
    pub changed: bool,
    pub new_migrations: Vec<String>,
}

fn migration_key(old_model: &str, new_model: &str) -> String {
    format!("model-version:{old_model}->{new_model}")
}

pub fn migrate_model_versions(
    configs: &Map<String, Value>,
    applied_migrations: Option<&HashSet<String>>,
) -> ModelVersionMigration {
    let mut result = ModelVersionMigration {
        migrated: Map::new(),
        changed: false,
        new_migrations: Vec::new(),
    };
    for (key, value) in configs {
        let upgrade = value
            .as_object()
            .and_then(|config| {
                config
                    .get("model")
                    .and_then(Value::as_str)
                    .map(|model| (config, model))
            })
            .filter(|(_, model)| !CURRENT_USER_SELECTABLE_MODELS.contains(model))
            .and_then(|(config, model)| {
                MODEL_VERSION_MAP
                    .iter()
                    .find(|(old, _)| *old == model)
                    .map(|(old, new)| (config, *old, *new))
            });
        let Some((config, old_model, new_model)) = upgrade else {
            result.migrated.insert(key.clone(), value.clone());
            continue;
        };
        let key_name = migration_key(old_model, new_model);
        if applied_migrations.is_some_and(|applied| applied.contains(&key_name)) {
            result.migrated.insert(key.clone(), value.clone());
            continue;
        }
        let mut upgraded = config.clone();
        upgraded.insert("model".to_string(), Value::String(new_model.to_string()));
        result.migrated.insert(key.clone(), Value::Object(upgraded));
        result.changed = true;
        result.new_migrations.push(key_name);
    }
    result
}
