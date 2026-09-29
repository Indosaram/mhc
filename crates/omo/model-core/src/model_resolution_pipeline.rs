use std::sync::Mutex;

use indexmap::IndexSet;
use serde_json::Value;
use serde_json::json;

use crate::model_availability::fuzzy_match_model;
use crate::model_normalization::normalize_model;
use crate::model_requirement_types::FallbackEntry;
use crate::provider_cache::NoopProviderCache;
use crate::provider_cache::ProviderCache;
use crate::provider_model_id_transform::transform_model_for_provider;

/// Test hook receiving every pipeline log line and its optional structured data.
pub type LogImplementation = Box<dyn Fn(&str, Option<&Value>) + Send + Sync>;

static LOG_IMPLEMENTATION_FOR_TESTING: Mutex<Option<LogImplementation>> = Mutex::new(None);

fn log(message: &str, data: Option<Value>) {
    let slot = LOG_IMPLEMENTATION_FOR_TESTING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(log_implementation) = slot.as_ref() {
        log_implementation(message, data.as_ref());
    }
}

/// Installs (or clears with `None`) the process-wide pipeline log hook.
pub fn _set_model_resolution_log_implementation_for_testing(
    log_implementation: Option<LogImplementation>,
) {
    let mut slot = LOG_IMPLEMENTATION_FOR_TESTING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *slot = log_implementation;
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolutionIntent {
    pub ui_selected_model: Option<String>,
    pub user_model: Option<String>,
    pub user_fallback_models: Option<Vec<String>>,
    pub category_default_model: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolutionConstraints {
    pub available_models: IndexSet<String>,
    /// Overrides the provider cache when set.
    pub connected_providers: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolutionPolicy {
    pub fallback_chain: Option<Vec<FallbackEntry>>,
    pub system_default_model: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelResolutionRequest {
    pub intent: ResolutionIntent,
    pub constraints: ResolutionConstraints,
    pub policy: ResolutionPolicy,
}

/// Which pipeline step produced the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelResolutionProvenance {
    Override,
    CategoryDefault,
    ProviderFallback,
    SystemDefault,
}

impl ModelResolutionProvenance {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Override => "override",
            Self::CategoryDefault => "category-default",
            Self::ProviderFallback => "provider-fallback",
            Self::SystemDefault => "system-default",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelResolutionResult {
    pub model: String,
    pub provenance: ModelResolutionProvenance,
    pub variant: Option<String>,
    pub attempted: Option<Vec<String>>,
    pub reason: Option<String>,
}

impl ModelResolutionResult {
    fn new(model: String, provenance: ModelResolutionProvenance) -> Self {
        Self {
            model,
            provenance,
            variant: None,
            attempted: None,
            reason: None,
        }
    }

    fn attempted(mut self, attempted: &[String]) -> Self {
        self.attempted = Some(attempted.to_vec());
        self
    }
}

/// `fuzzy_match_model(target, available, providers)`.
pub type FuzzyMatchFn = fn(&str, &IndexSet<String>, Option<&[String]>) -> Option<String>;

/// Injectable matching and transform functions.
#[derive(Debug, Clone, Copy)]
pub struct ModelResolutionDeps {
    pub fuzzy_match_model: FuzzyMatchFn,
    pub transform_model_for_provider: fn(&str, &str) -> String,
}

impl Default for ModelResolutionDeps {
    fn default() -> Self {
        Self {
            fuzzy_match_model,
            transform_model_for_provider,
        }
    }
}

fn find_fallback_variant_for_model(
    model: &str,
    fallback_chain: Option<&[FallbackEntry]>,
    transform: fn(&str, &str) -> String,
) -> Option<String> {
    let (provider, model_id) = model.split_once('/')?;
    if provider.is_empty() {
        return None;
    }
    fallback_chain?
        .iter()
        .find(|entry| {
            entry
                .providers
                .iter()
                .any(|candidate| candidate == provider)
                && transform(provider, &entry.model) == model_id
                && entry
                    .variant
                    .as_deref()
                    .is_some_and(|variant| !variant.is_empty())
        })
        .and_then(|entry| entry.variant.clone())
}

fn provider_hint(model: &str) -> Option<Vec<String>> {
    model
        .split_once('/')
        .map(|(provider, _)| vec![provider.to_string()])
}

/// `provider/<transformed model>` when `model` is `provider/...` and the provider is connected.
fn connected_transformed_model(
    model: &str,
    connected: &[String],
    deps: ModelResolutionDeps,
) -> Option<String> {
    let (provider, model_name) = model.split_once('/')?;
    connected.iter().any(|entry| entry == provider).then(|| {
        format!(
            "{provider}/{}",
            (deps.transform_model_for_provider)(provider, model_name)
        )
    })
}

/// Resolves with the no-op provider cache and default deps.
#[must_use]
pub fn resolve_model_pipeline(request: &ModelResolutionRequest) -> Option<ModelResolutionResult> {
    resolve_model_pipeline_with(request, &NoopProviderCache, ModelResolutionDeps::default())
}

/// UI selection, user override, category default, user fallbacks, fallback chain, system default.
#[must_use]
pub fn resolve_model_pipeline_with(
    request: &ModelResolutionRequest,
    provider_cache: &dyn ProviderCache,
    deps: ModelResolutionDeps,
) -> Option<ModelResolutionResult> {
    let mut attempted: Vec<String> = Vec::new();
    let intent = &request.intent;
    let constraints = &request.constraints;
    let available_models = &constraints.available_models;
    let fallback_chain = request.policy.fallback_chain.as_deref();
    let connected_providers = || {
        constraints
            .connected_providers
            .clone()
            .or_else(|| provider_cache.read_connected_providers_cache())
    };

    if let Some(model) = normalize_model(intent.ui_selected_model.as_deref()) {
        log(
            "Model resolved via UI selection",
            Some(json!({ "model": model })),
        );
        return Some(ModelResolutionResult::new(
            model,
            ModelResolutionProvenance::Override,
        ));
    }

    if let Some(model) = normalize_model(intent.user_model.as_deref()) {
        let inherited_variant = find_fallback_variant_for_model(
            &model,
            fallback_chain,
            deps.transform_model_for_provider,
        );
        log(
            "Model resolved via config override",
            Some(json!({ "model": model })),
        );
        return Some(ModelResolutionResult {
            variant: inherited_variant,
            ..ModelResolutionResult::new(model, ModelResolutionProvenance::Override)
        });
    }

    if let Some(category_default) = normalize_model(intent.category_default_model.as_deref()) {
        attempted.push(category_default.clone());
        if available_models.is_empty() {
            match connected_providers() {
                None => {
                    log(
                        "Model resolved via category default (no cache, first run)",
                        Some(json!({ "model": category_default })),
                    );
                    return Some(
                        ModelResolutionResult::new(
                            category_default,
                            ModelResolutionProvenance::CategoryDefault,
                        )
                        .attempted(&attempted),
                    );
                }
                Some(connected) => {
                    if let Some(transformed_model) =
                        connected_transformed_model(&category_default, &connected, deps)
                    {
                        log(
                            "Model resolved via category default (connected provider)",
                            Some(
                                json!({ "model": transformed_model, "original": category_default }),
                            ),
                        );
                        return Some(
                            ModelResolutionResult::new(
                                transformed_model,
                                ModelResolutionProvenance::CategoryDefault,
                            )
                            .attempted(&attempted),
                        );
                    }
                }
            }
        } else {
            let hint = provider_hint(&category_default);
            if let Some(found) =
                (deps.fuzzy_match_model)(&category_default, available_models, hint.as_deref())
            {
                log(
                    "Model resolved via category default (fuzzy matched)",
                    Some(json!({ "original": category_default, "matched": found })),
                );
                return Some(
                    ModelResolutionResult::new(found, ModelResolutionProvenance::CategoryDefault)
                        .attempted(&attempted),
                );
            }
        }
        log(
            "Category default model not available, falling through to fallback chain",
            Some(json!({ "model": category_default })),
        );
    }

    // User-configured fallback_models are tried before the hardcoded chain.
    let user_fallback_models = intent.user_fallback_models.as_deref().unwrap_or_default();
    if !user_fallback_models.is_empty() {
        if available_models.is_empty() {
            if let Some(connected) = connected_providers() {
                for model in user_fallback_models {
                    attempted.push(model.clone());
                    if let Some(transformed_model) =
                        connected_transformed_model(model, &connected, deps)
                    {
                        log(
                            "Model resolved via user fallback_models (connected provider)",
                            Some(json!({ "model": transformed_model, "original": model })),
                        );
                        return Some(
                            ModelResolutionResult::new(
                                transformed_model,
                                ModelResolutionProvenance::ProviderFallback,
                            )
                            .attempted(&attempted),
                        );
                    }
                }
                log(
                    "No connected provider found in user fallback_models, falling through to hardcoded chain",
                    None,
                );
            }
        } else {
            for model in user_fallback_models {
                attempted.push(model.clone());
                let hint = provider_hint(model);
                if let Some(found) =
                    (deps.fuzzy_match_model)(model, available_models, hint.as_deref())
                {
                    log(
                        "Model resolved via user fallback_models (availability confirmed)",
                        Some(json!({ "model": model, "match": found })),
                    );
                    return Some(
                        ModelResolutionResult::new(
                            found,
                            ModelResolutionProvenance::ProviderFallback,
                        )
                        .attempted(&attempted),
                    );
                }
            }
            log(
                "No available model found in user fallback_models, falling through to hardcoded chain",
                None,
            );
        }
    }

    if let Some(fallback_chain) = fallback_chain.filter(|chain| !chain.is_empty()) {
        if available_models.is_empty() {
            match connected_providers() {
                None => log(
                    "Model fallback chain skipped (no connected providers cache) - falling through to system default",
                    None,
                ),
                Some(connected) => {
                    for entry in fallback_chain {
                        for provider in &entry.providers {
                            if !connected.contains(provider) {
                                continue;
                            }
                            let transformed_model_id =
                                (deps.transform_model_for_provider)(provider, &entry.model);
                            log(
                                "Model resolved via fallback chain (connected provider)",
                                Some(json!({
                                    "provider": provider,
                                    "model": transformed_model_id,
                                    "variant": entry.variant,
                                })),
                            );
                            return Some(ModelResolutionResult {
                                variant: entry.variant.clone(),
                                ..ModelResolutionResult::new(
                                    format!("{provider}/{transformed_model_id}"),
                                    ModelResolutionProvenance::ProviderFallback,
                                )
                                .attempted(&attempted)
                            });
                        }
                    }
                    log(
                        "No connected provider found in fallback chain, falling through to system default",
                        None,
                    );
                }
            }
        } else {
            for entry in fallback_chain {
                for provider in &entry.providers {
                    let transformed_model_id =
                        (deps.transform_model_for_provider)(provider, &entry.model);
                    let mut candidate_model_ids = vec![entry.model.clone()];
                    if transformed_model_id != entry.model {
                        candidate_model_ids.push(transformed_model_id);
                    }
                    let provider_scope = [provider.clone()];
                    for model_id in candidate_model_ids {
                        let full_model = format!("{provider}/{model_id}");
                        let Some(found) = (deps.fuzzy_match_model)(
                            &full_model,
                            available_models,
                            Some(&provider_scope),
                        ) else {
                            continue;
                        };
                        log(
                            "Model resolved via fallback chain (availability confirmed)",
                            Some(json!({
                                "provider": provider,
                                "model": entry.model,
                                "match": found,
                                "variant": entry.variant,
                            })),
                        );
                        return Some(ModelResolutionResult {
                            variant: entry.variant.clone(),
                            ..ModelResolutionResult::new(
                                found,
                                ModelResolutionProvenance::ProviderFallback,
                            )
                            .attempted(&attempted)
                        });
                    }
                }
            }
            log(
                "No available model found in fallback chain, falling through to system default",
                None,
            );
        }
    }

    let Some(system_default_model) = request.policy.system_default_model.clone() else {
        log(
            "No model resolved - systemDefaultModel not configured",
            None,
        );
        return None;
    };

    log(
        "Model resolved via system default",
        Some(json!({ "model": system_default_model })),
    );
    Some(
        ModelResolutionResult::new(
            system_default_model,
            ModelResolutionProvenance::SystemDefault,
        )
        .attempted(&attempted),
    )
}
