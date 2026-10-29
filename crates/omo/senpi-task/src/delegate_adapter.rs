//! Thin seam over `delegate-core` / `model-core` for senpi-task's model resolution.
//!
//! Category and agent resolution call `delegate_core::resolve_model_for_delegate_task` directly,
//! with the deps every upstream senpi-task caller passes (`connectedProviders: null`, warm
//! provider-models and connected-providers caches). This module only keeps the crate's
//! fallback-chain constructor and that shared deps value in one place.

pub use delegate_core::DelegateFallbackEntry;
pub(crate) use delegate_core::{
    DelegateModelResolutionDeps, DelegateModelResolutionInput, DelegateModelResolutionResult,
    resolve_model_for_delegate_task,
};
pub(crate) use model_core::transform_model_for_provider;

pub(crate) fn fallback_entry(providers: &[&str], model: &str, variant: Option<&str>) -> DelegateFallbackEntry {
    DelegateFallbackEntry {
        providers: providers.iter().map(|provider| (*provider).to_string()).collect(),
        model: model.to_string(),
        variant: variant.map(str::to_string),
    }
}

/// Upstream senpi-task `category/resolver.ts` and `agents/resolve-agent.ts` deps: no connected
/// providers list, warm caches, so a cold registry never yields `{ skipped: true }`.
pub(crate) fn warm_cache_deps() -> DelegateModelResolutionDeps<'static> {
    DelegateModelResolutionDeps {
        connected_providers: None,
        has_provider_models_cache: true,
        has_connected_providers_cache: true,
        log: None,
    }
}
