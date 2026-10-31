//! The provider/model pairs the engine can serve, read without starting it (upstream
//! `omo-native/bin/lib/engine-models.js`): the builtin catalog every engine session is composed from
//! (here `maho_ai::providers::all::builtin_providers`, the port of the same pi-ai module), plus what
//! `<agentDir>/models.json` adds.

use super::jsonc::parse_jsonc;
use super::opencode_providers::PlannedProvider;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

/// One catalog provider: its id, its model ids, and whether it lists models only at runtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogProvider {
    pub id: String,
    pub models: Vec<String>,
    pub refreshes: bool,
}

/// Upstream `runtime.loadModelCatalog`.
pub type LoadCatalog = Arc<dyn Fn() -> Result<Vec<CatalogProvider>, String> + Send + Sync>;

/// The engine's own builtin catalog.
///
/// # Errors
/// A builtin provider could not be constructed.
pub fn builtin_catalog() -> Result<Vec<CatalogProvider>, String> {
    let providers = maho_ai::providers::all::builtin_providers().map_err(|e| e.to_string())?;
    Ok(providers
        .iter()
        .map(|provider| CatalogProvider {
            id: provider.id().to_owned(),
            models: provider.get_models().into_iter().map(|m| m.id).collect(),
            refreshes: provider.supports_refresh(),
        })
        .collect())
}

/// `models` maps each provider id to the model ids it serves; `dynamic` names the providers that
/// list their models only at runtime, whose ids cannot be checked offline.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelRegistry {
    pub models: BTreeMap<String, BTreeSet<String>>,
    pub dynamic: BTreeSet<String>,
}

impl ModelRegistry {
    fn add(&mut self, provider: &str, id: &str) {
        if id.is_empty() {
            return;
        }
        self.models
            .entry(provider.to_owned())
            .or_default()
            .insert(id.to_owned());
    }
}

fn read_models_json(path: &Path) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| parse_jsonc(&text).ok())
        .and_then(|doc| {
            doc.get("providers")
                .and_then(serde_json::Value::as_object)
                .cloned()
        })
        .unwrap_or_default()
}

/// `loadEngineModels`. `custom_providers` are the providers the custom-provider stage would add.
///
/// # Errors
/// The catalog could not be loaded.
pub fn load_engine_models(
    agent_dir: &Path,
    custom_providers: &[PlannedProvider],
    load_catalog: Option<&LoadCatalog>,
) -> Result<ModelRegistry, String> {
    let catalog = match load_catalog {
        Some(load) => load()?,
        None => builtin_catalog()?,
    };
    let mut registry = ModelRegistry::default();
    for provider in catalog {
        registry.models.insert(provider.id.clone(), BTreeSet::new());
        for model in &provider.models {
            registry.add(&provider.id, model);
        }
        if provider.refreshes {
            registry.dynamic.insert(provider.id);
        }
    }
    // Registered by the engine's anthropic-subscription extension, not the catalog; it serves exactly
    // the anthropic catalog.
    let anthropic: Vec<String> = registry
        .models
        .get("anthropic")
        .map(|ids| ids.iter().cloned().collect())
        .unwrap_or_default();
    for id in anthropic {
        registry.add("anthropic-subscription", &id);
    }
    for (provider, entry) in read_models_json(&agent_dir.join("models.json")) {
        registry.models.entry(provider.clone()).or_default();
        for model in entry
            .get("models")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(id) = model.get("id").and_then(serde_json::Value::as_str) {
                registry.add(&provider, id);
            }
        }
    }
    for provider in custom_providers {
        for id in provider.model_ids() {
            registry.add(&provider.id, &id);
        }
    }
    Ok(registry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_models_json_and_custom_providers_compose() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("models.json"),
            r#"{"providers":{"mine":{"models":[{"id":"m"}]}}}"#,
        )
        .expect("write");
        let load: LoadCatalog = Arc::new(|| {
            Ok(vec![
                CatalogProvider {
                    id: "anthropic".into(),
                    models: vec!["claude".into()],
                    refreshes: false,
                },
                CatalogProvider {
                    id: "openrouter".into(),
                    models: vec![],
                    refreshes: true,
                },
            ])
        });
        let registry = load_engine_models(dir.path(), &[], Some(&load)).expect("load");
        assert!(registry.models["anthropic-subscription"].contains("claude"));
        assert!(registry.models["mine"].contains("m"));
        assert!(registry.dynamic.contains("openrouter"));
    }
}
