use serde_json::Map;
use serde_json::Value;

/// Provider-advertised model metadata (`id`, `limit`, `capabilities`, ... plus arbitrary keys).
pub type ModelMetadata = Map<String, Value>;

/// Dependency-injection seam for connected-provider and model-metadata lookups.
///
/// Adapters implement this over their runtime cache state so the resolution core stays pure.
/// Implementations match `model_id` exactly; suffix/prefix tolerance is applied by callers.
pub trait ProviderCache {
    /// Connected provider ids, or `None` when no cache exists yet (first run).
    fn read_connected_providers_cache(&self) -> Option<Vec<String>>;
    /// Metadata for one provider model, if the provider advertised it.
    fn find_provider_model_metadata(
        &self,
        provider_id: &str,
        model_id: &str,
    ) -> Option<ModelMetadata>;
}

/// A cache that knows nothing: no connected providers, no metadata.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopProviderCache;

impl ProviderCache for NoopProviderCache {
    fn read_connected_providers_cache(&self) -> Option<Vec<String>> {
        None
    }

    fn find_provider_model_metadata(
        &self,
        _provider_id: &str,
        _model_id: &str,
    ) -> Option<ModelMetadata> {
        None
    }
}
