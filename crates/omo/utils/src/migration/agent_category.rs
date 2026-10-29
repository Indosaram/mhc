use std::collections::HashMap;
use std::sync::{LazyLock, PoisonError, RwLock};

use serde_json::{Map, Value};

use super::ObjectMigration;

/// Legacy-only map from hardcoded model strings to category names. Do not extend.
pub const MODEL_TO_CATEGORY_MAP: [(&str, &str); 7] = [
    ("google/gemini-3.1-pro", "visual-engineering"),
    ("google/gemini-3-flash", "writing"),
    ("openai/gpt-5.4", "ultrabrain"),
    ("anthropic/claude-haiku-4-5", "quick"),
    ("anthropic/claude-opus-4-6", "unspecified-high"),
    ("anthropic/claude-opus-4-7", "unspecified-high"),
    ("anthropic/claude-sonnet-4-6", "unspecified-low"),
];

type CategoryDefaults = HashMap<String, Map<String, Value>>;

static CATEGORY_DEFAULTS: LazyLock<RwLock<CategoryDefaults>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

pub fn configure_migration_category_defaults(defaults: Option<CategoryDefaults>) {
    *CATEGORY_DEFAULTS
        .write()
        .unwrap_or_else(PoisonError::into_inner) = defaults.unwrap_or_default();
}

pub fn migrate_agent_config_to_category(config: &Map<String, Value>) -> ObjectMigration {
    let category = config
        .get("model")
        .and_then(Value::as_str)
        .and_then(|model| {
            MODEL_TO_CATEGORY_MAP
                .iter()
                .find(|(known, _)| *known == model)
        })
        .map(|(_, category)| *category);
    let Some(category) = category else {
        return ObjectMigration {
            migrated: config.clone(),
            changed: false,
        };
    };
    let mut migrated = Map::new();
    migrated.insert("category".to_string(), Value::String(category.to_string()));
    for (key, value) in config {
        if key != "model" {
            migrated.insert(key.clone(), value.clone());
        }
    }
    ObjectMigration {
        migrated,
        changed: true,
    }
}

pub fn should_delete_agent_config(config: &Map<String, Value>, category: &str) -> bool {
    let defaults = CATEGORY_DEFAULTS
        .read()
        .unwrap_or_else(PoisonError::into_inner);
    let Some(category_defaults) = defaults.get(category) else {
        return false;
    };
    config
        .iter()
        .filter(|(key, _)| key.as_str() != "category")
        .all(|(key, value)| category_defaults.get(key) == Some(value))
}
