use std::sync::LazyLock;

use indexmap::IndexSet;
use regex::Regex;

use crate::fallback_model_object::FallbackModelEntry;
use crate::fallback_model_object::FallbackModelsConfig;
use crate::model_normalization::normalize_model;
use crate::model_requirement_types::FallbackEntry;
use crate::model_resolution_pipeline::ModelResolutionDeps;
use crate::model_resolution_pipeline::ModelResolutionProvenance;
use crate::model_resolution_pipeline::ModelResolutionRequest;
use crate::model_resolution_pipeline::ResolutionConstraints;
use crate::model_resolution_pipeline::ResolutionIntent;
use crate::model_resolution_pipeline::ResolutionPolicy;
use crate::model_resolution_pipeline::resolve_model_pipeline_with;
use crate::provider_cache::NoopProviderCache;
use crate::provider_cache::ProviderCache;
use crate::reasoning_level::is_reasoning_level;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelResolutionInput {
    pub user_model: Option<String>,
    pub inherited_model: Option<String>,
    pub system_default: Option<String>,
}

/// Same values as the pipeline provenance.
pub type ModelSource = ModelResolutionProvenance;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelResolutionResult {
    pub model: String,
    pub source: ModelSource,
    pub variant: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExtendedModelResolutionInput {
    pub ui_selected_model: Option<String>,
    pub user_model: Option<String>,
    pub user_fallback_models: Option<Vec<String>>,
    pub category_default_model: Option<String>,
    pub fallback_chain: Option<Vec<FallbackEntry>>,
    pub available_models: IndexSet<String>,
    pub system_default_model: Option<String>,
}

/// User model, then inherited model (both trimmed, blank = unset), then the system default.
#[must_use]
pub fn resolve_model(input: &ModelResolutionInput) -> Option<String> {
    normalize_model(input.user_model.as_deref())
        .or_else(|| normalize_model(input.inherited_model.as_deref()))
        .or_else(|| input.system_default.clone())
}

/// Full pipeline with the default (empty) connected-providers adapter.
#[must_use]
pub fn resolve_model_with_fallback(
    input: &ExtendedModelResolutionInput,
) -> Option<ModelResolutionResult> {
    resolve_model_with_fallback_using(input, &NoopProviderCache)
}

/// Full pipeline against an injected connected-providers adapter.
#[must_use]
pub fn resolve_model_with_fallback_using(
    input: &ExtendedModelResolutionInput,
    connected_providers_adapter: &dyn ProviderCache,
) -> Option<ModelResolutionResult> {
    let request = ModelResolutionRequest {
        intent: ResolutionIntent {
            ui_selected_model: input.ui_selected_model.clone(),
            user_model: input.user_model.clone(),
            user_fallback_models: input.user_fallback_models.clone(),
            category_default_model: input.category_default_model.clone(),
        },
        constraints: ResolutionConstraints {
            available_models: input.available_models.clone(),
            connected_providers: None,
        },
        policy: ResolutionPolicy {
            fallback_chain: input.fallback_chain.clone(),
            system_default_model: input.system_default_model.clone(),
        },
    };
    let resolved = resolve_model_pipeline_with(
        &request,
        connected_providers_adapter,
        ModelResolutionDeps::default(),
    )?;
    Some(ModelResolutionResult {
        model: resolved.model,
        source: resolved.provenance,
        variant: resolved.variant,
    })
}

/// Normalizes `fallback_models` config (string or mixed array) to a mixed array.
#[must_use]
pub fn normalize_fallback_models(
    models: Option<&FallbackModelsConfig>,
) -> Option<Vec<FallbackModelEntry>> {
    match models? {
        FallbackModelsConfig::Single(model) if model.is_empty() => None,
        FallbackModelsConfig::Single(model) => Some(vec![FallbackModelEntry::Model(model.clone())]),
        FallbackModelsConfig::List(entries) => Some(entries.clone()),
    }
}

/// Flattens object entries to `model` or `model(variant)`, stripping any inline variant first so
/// we never emit `provider/model high(low)`.
#[must_use]
#[expect(clippy::expect_used, reason = "static regex literals are valid")]
pub fn flatten_to_fallback_model_strings(
    models: Option<&[FallbackModelEntry]>,
) -> Option<Vec<String>> {
    static TRAILING_PARENS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\([^()]+\)\s*$").expect("valid regex"));
    static TRAILING_WORD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\s+([a-z][a-z0-9_-]*)\s*$").expect("valid regex"));

    let flattened = models?
        .iter()
        .map(|entry| match entry {
            FallbackModelEntry::Model(model) => model.clone(),
            FallbackModelEntry::Object(object) => match object
                .variant
                .as_deref()
                .filter(|variant| !variant.is_empty())
            {
                None => object.model.clone(),
                Some(variant) => {
                    let without_parens = TRAILING_PARENS.replace(&object.model, "");
                    let without_level =
                        TRAILING_WORD.replace(&without_parens, |captures: &regex::Captures<'_>| {
                            let suffix = captures.get(1).map_or("", |m| m.as_str()).to_lowercase();
                            if is_reasoning_level(&suffix) {
                                String::new()
                            } else {
                                captures.get(0).map_or("", |m| m.as_str()).to_string()
                            }
                        });
                    format!("{}({variant})", without_level.trim())
                }
            },
        })
        .collect();
    Some(flattened)
}
