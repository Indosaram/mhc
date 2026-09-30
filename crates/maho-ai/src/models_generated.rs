//! Port of senpi packages/ai/src/models.generated.ts.
//!
//! The catalog is generated data: `crates/maho-ai/data/models.json` is written by
//! `bun tools/golden/gen-models.mjs` from the pinned senpi `MODELS` export and embedded at build time.

use crate::types::Model;
use indexmap::IndexMap;
use std::sync::LazyLock;

pub const MODELS_JSON: &str = include_str!("../data/models.json");

/// provider id -> model id -> model, in senpi's key order.
pub type ModelsByProvider = IndexMap<String, IndexMap<String, Model>>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CatalogError {
    #[error("Unknown provider: {provider}")]
    UnknownProvider { provider: String },
    #[error("Model not found: {provider}/{model_id}")]
    NotFound { provider: String, model_id: String },
}

pub static MODELS: LazyLock<ModelsByProvider> = LazyLock::new(|| {
    serde_json::from_str(MODELS_JSON).unwrap_or_else(|error| panic!("embedded models.json is invalid: {error}"))
});

pub fn get_builtin_providers() -> Vec<&'static str> {
    MODELS.keys().map(String::as_str).collect()
}

pub fn get_builtin_provider_models(provider: &str) -> Result<Vec<&'static Model>, CatalogError> {
    MODELS
        .get(provider)
        .map(|models| models.values().collect())
        .ok_or_else(|| CatalogError::UnknownProvider { provider: provider.into() })
}

pub fn get_builtin_model(provider: &str, model_id: &str) -> Result<&'static Model, CatalogError> {
    let models = MODELS.get(provider).ok_or_else(|| CatalogError::UnknownProvider { provider: provider.into() })?;
    models
        .get(model_id)
        .ok_or_else(|| CatalogError::NotFound { provider: provider.into(), model_id: model_id.into() })
}
