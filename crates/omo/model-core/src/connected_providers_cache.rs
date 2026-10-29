use indexmap::IndexMap;
use serde::Deserialize;
use serde::Serialize;

use crate::provider_cache::ModelMetadata;
use crate::provider_cache::NoopProviderCache;
use crate::provider_cache::ProviderCache;

/// Per-provider model list: either bare ids or full metadata records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProviderModels {
    Ids(Vec<String>),
    Metadata(Vec<ModelMetadata>),
}

/// Snapshot of the provider-models cache file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderModelsCache {
    pub models: IndexMap<String, ProviderModels>,
    pub connected: Vec<String>,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

/// A [`ProviderCache`] that can also expose the full provider-models cache.
///
/// Adapters implement this over their on-disk cache; the core ships only no-op defaults.
pub trait ConnectedProvidersAdapter: ProviderCache {
    /// The provider-models cache, or `None` when it has not been written yet.
    fn read_provider_models_cache(&self) -> Option<ProviderModelsCache>;
}

impl ConnectedProvidersAdapter for NoopProviderCache {
    fn read_provider_models_cache(&self) -> Option<ProviderModelsCache> {
        None
    }
}

/// Default adapter: every lookup misses.
pub const CONNECTED_PROVIDERS_ADAPTER: NoopProviderCache = NoopProviderCache;

#[must_use]
pub fn read_connected_providers_cache() -> Option<Vec<String>> {
    CONNECTED_PROVIDERS_ADAPTER.read_connected_providers_cache()
}

#[must_use]
pub fn find_provider_model_metadata(provider_id: &str, model_id: &str) -> Option<ModelMetadata> {
    CONNECTED_PROVIDERS_ADAPTER.find_provider_model_metadata(provider_id, model_id)
}

#[must_use]
pub fn read_provider_models_cache() -> Option<ProviderModelsCache> {
    CONNECTED_PROVIDERS_ADAPTER.read_provider_models_cache()
}
