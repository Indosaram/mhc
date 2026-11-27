//! Child-local runtime model fallback (`runners/in-process/runtime-fallback-settings.ts`).

use std::collections::BTreeMap;

use crate::runners::designated_fallback_profile::designated_fallback_profile;
use crate::state::ResolvedModelRecord;

/// `ChildRetryOverride`: a child's own same-model retry budget, merged over the engine defaults.
/// An absent field keeps the engine's own default (upstream spreads a key only when it is defined),
/// so `None` means "the engine default stands", never a child-invented value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChildRetryOverride {
    /// `maxRetries`.
    pub max_retries: Option<u32>,
    /// `baseDelayMs`, independent of `max_retries` (either may be set alone).
    pub base_delay_ms: Option<u64>,
}

/// The host `SettingsManager.inMemory({ retry })` payload, read back via `getRetryFallbackSettings`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetryFallbackSettings {
    pub model_fallback: bool,
    pub chains: BTreeMap<String, Vec<String>>,
    /// `retry.maxRetries`: `None` when the child sets no budget, so the engine default stands.
    pub max_retries: Option<u32>,
    /// `retry.baseDelayMs`: the same absent-means-default rule, set independently of `max_retries`.
    pub base_delay_ms: Option<u64>,
}

pub fn create_runtime_fallback_settings(
    selected_model: Option<&str>,
    fallback_models: Option<&[ResolvedModelRecord]>,
    retry: Option<&ChildRetryOverride>,
) -> RetryFallbackSettings {
    let (model_fallback, chains) = match selected_model {
        Some(selected) => {
            let selectors: Vec<String> = fallback_models
                .unwrap_or(&[])
                .iter()
                .map(model_selector)
                .collect();
            let profile = designated_fallback_profile(selected, &selectors);
            (profile.model_fallback, profile.fallback_chains)
        }
        None => (false, BTreeMap::new()),
    };
    RetryFallbackSettings {
        model_fallback,
        chains,
        max_retries: retry.and_then(|retry| retry.max_retries),
        base_delay_ms: retry.and_then(|retry| retry.base_delay_ms),
    }
}

fn model_selector(model: &ResolvedModelRecord) -> String {
    let thinking = model
        .reasoning
        .as_ref()
        .or(model.reasoning_effort.as_ref())
        .or(model.variant.as_ref());
    match thinking {
        Some(thinking) => format!("{}/{}:{thinking}", model.provider, model.model_id),
        None => format!("{}/{}", model.provider, model.model_id),
    }
}
