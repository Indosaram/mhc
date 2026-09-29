//! Child-local runtime model fallback (`runners/in-process/runtime-fallback-settings.ts`).

use std::collections::BTreeMap;

use crate::state::ResolvedModelRecord;

/// The host `SettingsManager.inMemory({ retry })` payload, read back via `getRetryFallbackSettings`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetryFallbackSettings {
    pub model_fallback: bool,
    pub chains: BTreeMap<String, Vec<String>>,
}

pub fn create_runtime_fallback_settings(
    selected_model: Option<&str>,
    fallback_models: Option<&[ResolvedModelRecord]>,
) -> RetryFallbackSettings {
    match (selected_model, fallback_models) {
        (Some(selected), Some(fallbacks)) if !fallbacks.is_empty() => RetryFallbackSettings {
            model_fallback: true,
            chains: BTreeMap::from([(
                selected.to_string(),
                fallbacks.iter().map(model_selector).collect(),
            )]),
        },
        _ => RetryFallbackSettings::default(),
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
