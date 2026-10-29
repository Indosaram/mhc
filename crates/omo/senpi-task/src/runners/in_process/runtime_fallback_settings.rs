//! Child-local runtime model fallback (`runners/in-process/runtime-fallback-settings.ts`).

use std::collections::BTreeMap;

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
    let chains = match (selected_model, fallback_models) {
        (Some(selected), Some(fallbacks)) if !fallbacks.is_empty() => BTreeMap::from([(
            selected.to_string(),
            fallbacks.iter().map(model_selector).collect(),
        )]),
        _ => BTreeMap::new(),
    };
    // `modelFallback` mirrors the chain's presence; the retry budget is merged over the engine
    // defaults independently, so a child with a budget but no chain still carries it (upstream
    // spreads `retryOverride` into `retry` regardless of `chained`).
    RetryFallbackSettings {
        model_fallback: !chains.is_empty(),
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
