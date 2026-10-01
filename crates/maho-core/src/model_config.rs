use indexmap::IndexMap;
use maho_ai::legacy_provider_ids::normalize_provider_id;
use std::{collections::HashSet, path::Path};
pub use crate::model_config_schema::{ModelsJsonModel, ModelsJsonModelOverride, ModelsJsonProvider};
use crate::model_config_schema::validate_models_config;
use crate::models_json_migration::{ModelsJsonMigration, migrate_models_json_provider_ids};

#[derive(Debug, Clone, Default)]
pub struct ModelConfig {
    providers: IndexMap<String, ModelsJsonProvider>,
    disabled_providers: HashSet<String>,
    error: Option<String>,
    warnings: Vec<String>,
}

impl ModelConfig {
    pub fn parse(content: &str, path: &str) -> Self {
        let parsed = match crate::settings_manager::parse_settings_json(content) {
            Ok(value) => serde_json::Value::Object(value),
            Err(error) => return Self { error: Some(format!("Failed to parse models.json: {error}\n\nFile: {path}")), ..Self::default() },
        };
        let config = match validate_models_config(parsed) {
            Ok(value) => value,
            Err(error) => return Self { error: Some(format!("Invalid models.json schema:\n  - {error}\n\nFile: {path}")), ..Self::default() },
        };
        let mut providers = IndexMap::new();
        for (id, provider) in config.providers {
            let canonical = normalize_provider_id(&id);
            if canonical != id && providers.contains_key(&canonical) { continue; }
            providers.insert(canonical, provider);
        }
        Self { providers, disabled_providers: config.disabled_providers.unwrap_or_default().iter().map(|id| normalize_provider_id(id)).collect(), ..Self::default() }
    }

    fn parse_and_migrate(content: &str, path: &Path) -> Self {
        let mut config = Self::parse(content, &path.to_string_lossy());
        if config.error.is_some() { return config; }
        if let ModelsJsonMigration::Failed { renamed, reason } = migrate_models_json_provider_ids(path, content) {
            config.warnings.push(format!("models.json uses renamed provider ids ({}) and could not be updated automatically: {reason}. They still work, but update the file to the new ids.", renamed.join(", ")));
        }
        config
    }

    pub fn load_sync(path: Option<&Path>) -> Self {
        let Some(path) = path else { return Self::default(); };
        match std::fs::read_to_string(path) {
            Ok(content) => Self::parse_and_migrate(&content, path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => Self { error: Some(format!("Failed to load models.json: {error}\n\nFile: {}", path.display())), ..Self::default() },
        }
    }

    pub async fn load(path: Option<&Path>) -> Self {
        let Some(path) = path else { return Self::default(); };
        match tokio::fs::read_to_string(path).await {
            Ok(content) => Self::parse_and_migrate(&content, path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => Self { error: Some(format!("Failed to load models.json: {error}\n\nFile: {}", path.display())), ..Self::default() },
        }
    }

    pub fn get_provider(&self, id: &str) -> Option<&ModelsJsonProvider> { self.providers.get(id) }
    pub fn get_provider_ids(&self) -> Vec<&str> { self.providers.keys().map(String::as_str).collect() }
    pub fn is_provider_disabled(&self, id: &str) -> bool { self.disabled_providers.contains(id) || self.providers.get(id).is_some_and(|p| p.disabled == Some(true)) }
    pub fn get_error(&self) -> Option<&str> { self.error.as_deref() }
    pub fn get_warnings(&self) -> &[String] { &self.warnings }
}
