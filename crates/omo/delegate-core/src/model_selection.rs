use indexmap::IndexSet;
use model_core::SplitReasoningSuffixOptions;
use model_core::fuzzy_match_model;
use model_core::normalize_model;
use model_core::parse_model_string;
use model_core::parse_variant_from_model_id;
pub use model_core::transform_model_for_provider;
use serde_json::Value;
use serde_json::json;

/// One rung of a fallback chain: `{ providers; model; variant? }`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DelegateFallbackEntry {
    pub providers: Vec<String>,
    pub model: String,
    pub variant: Option<String>,
}

/// Inputs to [`resolve_model_for_delegate_task`]; mirrors `DelegateModelResolutionInput`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DelegateModelResolutionInput {
    pub user_model: Option<String>,
    pub user_fallback_models: Option<Vec<String>>,
    pub category_default_model: Option<String>,
    pub is_user_configured_category_model: bool,
    pub fallback_chain: Option<Vec<DelegateFallbackEntry>>,
    pub available_models: IndexSet<String>,
    pub system_default_model: Option<String>,
}

/// `{ model; variant?; fallbackEntry?; matchedFallback? }`; `matched_fallback == false` means absent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedDelegateModel {
    pub model: String,
    pub variant: Option<String>,
    pub fallback_entry: Option<DelegateFallbackEntry>,
    pub matched_fallback: bool,
}

/// Non-`undefined` outcomes of [`resolve_model_for_delegate_task`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelegateModelResolutionResult {
    Resolved(ResolvedDelegateModel),
    /// `{ skipped: true }`: no provider cache exists yet, so resolution is deferred.
    Skipped,
}

/// Logger callback: `(message, metadata?)`.
pub type DelegateLog<'a> = &'a dyn Fn(&str, Option<&Value>);

/// Cache state the resolver consults; mirrors `DelegateModelResolutionDeps`.
#[derive(Clone, Copy, Default)]
pub struct DelegateModelResolutionDeps<'a> {
    pub connected_providers: Option<&'a [String]>,
    pub has_provider_models_cache: bool,
    pub has_connected_providers_cache: bool,
    pub log: Option<DelegateLog<'a>>,
}

impl DelegateModelResolutionDeps<'_> {
    fn log(&self, message: &str, metadata: Option<&Value>) {
        if let Some(log) = self.log {
            log(message, metadata);
        }
    }
}

struct ParsedUserFallback {
    base_model: String,
    provider_hint: Option<Vec<String>>,
    variant: Option<String>,
}

fn is_explicit_high_model(model: &str) -> bool {
    let last_segment = model.rsplit('/').next().unwrap_or(model);
    last_segment.len() > "-high".len() && last_segment.ends_with("-high")
}

fn get_explicit_high_base_model(model: &str) -> Option<&str> {
    if is_explicit_high_model(model) {
        model.strip_suffix("-high")
    } else {
        None
    }
}

fn parse_user_fallback_model(fallback_model: &str) -> Option<ParsedUserFallback> {
    let normalized_fallback = normalize_model(Some(fallback_model))?;

    if let Some(parsed) = parse_model_string(&normalized_fallback) {
        return Some(ParsedUserFallback {
            base_model: format!("{}/{}", parsed.provider_id, parsed.model_id),
            provider_hint: Some(vec![parsed.provider_id]),
            variant: parsed.variant,
        });
    }

    let parsed =
        parse_variant_from_model_id(&normalized_fallback, SplitReasoningSuffixOptions::default());
    if parsed.model_id.is_empty() {
        return None;
    }
    Some(ParsedUserFallback {
        base_model: parsed.model_id,
        provider_hint: None,
        variant: parsed.variant,
    })
}

fn resolved(model: String, variant: Option<String>) -> Option<DelegateModelResolutionResult> {
    Some(DelegateModelResolutionResult::Resolved(
        ResolvedDelegateModel {
            model,
            variant,
            ..ResolvedDelegateModel::default()
        },
    ))
}

fn fallback_resolved(
    model: String,
    variant: Option<String>,
    fallback_entry: Option<&DelegateFallbackEntry>,
) -> Option<DelegateModelResolutionResult> {
    Some(DelegateModelResolutionResult::Resolved(
        ResolvedDelegateModel {
            model,
            variant,
            fallback_entry: fallback_entry.cloned(),
            matched_fallback: true,
        },
    ))
}

fn resolve_user_model(
    user_model: String,
    input: &DelegateModelResolutionInput,
    deps: &DelegateModelResolutionDeps<'_>,
) -> Option<DelegateModelResolutionResult> {
    let parsed = parse_user_fallback_model(&user_model);
    let (primary_model, primary_variant) = match &parsed {
        Some(ParsedUserFallback {
            base_model,
            variant: Some(variant),
            ..
        }) => (base_model.clone(), Some(variant.clone())),
        _ => (user_model, None),
    };

    let fallback_models = input.user_fallback_models.as_deref().unwrap_or_default();
    if !input.available_models.is_empty() && !fallback_models.is_empty() {
        let provider_hint = parsed.as_ref().and_then(|p| p.provider_hint.as_deref());
        let primary_match =
            fuzzy_match_model(&primary_model, &input.available_models, provider_hint);
        if primary_match.is_none() {
            for fallback_model in fallback_models {
                let Some(parsed_fallback) = parse_user_fallback_model(fallback_model) else {
                    continue;
                };
                if let Some(fb_match) = fuzzy_match_model(
                    &parsed_fallback.base_model,
                    &input.available_models,
                    parsed_fallback.provider_hint.as_deref(),
                ) {
                    deps.log(
                        "[resolveModelForDelegateTask] user primary model unreachable; promoting user fallback_models entry",
                        Some(&json!({ "userPrimary": primary_model, "selectedFallback": fb_match })),
                    );
                    return fallback_resolved(fb_match, parsed_fallback.variant, None);
                }
            }
        }
    }

    resolved(primary_model, primary_variant)
}

/// Picks the model a delegated task should run on, in precedence order: user override,
/// category default, user `fallback_models`, the hardcoded fallback chain, then the system default.
/// `None` mirrors the TS `undefined` (nothing resolvable).
#[must_use]
pub fn resolve_model_for_delegate_task(
    input: &DelegateModelResolutionInput,
    deps: &DelegateModelResolutionDeps<'_>,
) -> Option<DelegateModelResolutionResult> {
    if let Some(user_model) = normalize_model(input.user_model.as_deref()) {
        return resolve_user_model(user_model, input, deps);
    }

    let cold_cache = input.available_models.is_empty();
    let connected_providers = if cold_cache {
        deps.connected_providers
    } else {
        None
    };

    if cold_cache
        && connected_providers.is_none()
        && !deps.has_provider_models_cache
        && !deps.has_connected_providers_cache
    {
        return Some(DelegateModelResolutionResult::Skipped);
    }

    let category_default = normalize_model(input.category_default_model.as_deref());
    let explicit_high_base_model = category_default
        .as_deref()
        .and_then(get_explicit_high_base_model);
    let explicit_high_model = explicit_high_base_model.and(category_default.as_deref());

    if let Some(category_default) = category_default.as_deref() {
        if input.is_user_configured_category_model {
            deps.log(
                "[resolveModelForDelegateTask] using user-configured category model (bypass validation)",
                Some(&json!({ "categoryDefaultModel": category_default })),
            );
            if let Some(ParsedUserFallback {
                base_model,
                variant: Some(variant),
                ..
            }) = parse_user_fallback_model(category_default)
            {
                return resolved(base_model, Some(variant));
            }
            return resolved(category_default.to_string(), None);
        }

        if cold_cache {
            let category_provider = category_default
                .split_once('/')
                .map(|(provider, _)| provider)
                .filter(|provider| !provider.is_empty());
            match (connected_providers, category_provider) {
                (Some(connected), Some(provider))
                    if !connected.iter().any(|candidate| candidate == provider) =>
                {
                    deps.log(
                        "[resolveModelForDelegateTask] skipping disconnected category default on cold cache",
                        Some(&json!({
                            "categoryDefault": category_default,
                            "connectedProviders": connected,
                        })),
                    );
                }
                _ => return resolved(category_default.to_string(), None),
            }
        }

        let provider_hint = category_default
            .split_once('/')
            .map(|(provider, _)| provider)
            .filter(|provider| !provider.is_empty())
            .map(|provider| vec![provider.to_string()]);
        if let Some(matched) = fuzzy_match_model(
            category_default,
            &input.available_models,
            provider_hint.as_deref(),
        ) {
            if is_explicit_high_model(category_default) && matched != category_default {
                return resolved(category_default.to_string(), None);
            }
            return resolved(matched, None);
        }
    }

    let fallback_models = input.user_fallback_models.as_deref().unwrap_or_default();
    for fallback_model in fallback_models {
        let Some(parsed_fallback) = parse_user_fallback_model(fallback_model) else {
            continue;
        };
        if cold_cache {
            if let (Some(connected), Some(hint)) = (
                connected_providers,
                parsed_fallback.provider_hint.as_deref(),
            ) && !hint.iter().any(|provider| connected.contains(provider))
            {
                continue;
            }
            return fallback_resolved(parsed_fallback.base_model, parsed_fallback.variant, None);
        }
        if let Some(matched) = fuzzy_match_model(
            &parsed_fallback.base_model,
            &input.available_models,
            parsed_fallback.provider_hint.as_deref(),
        ) {
            return fallback_resolved(matched, parsed_fallback.variant, None);
        }
    }

    let fallback_chain = input.fallback_chain.as_deref().unwrap_or_default();
    if !fallback_chain.is_empty() {
        let chain_result = if cold_cache {
            resolve_cold_fallback_chain(fallback_chain, connected_providers, deps)
        } else {
            resolve_warm_fallback_chain(
                fallback_chain,
                &input.available_models,
                explicit_high_model.zip(explicit_high_base_model),
            )
        };
        if chain_result.is_some() {
            return chain_result;
        }
    }

    normalize_model(input.system_default_model.as_deref()).and_then(|model| resolved(model, None))
}

fn resolve_cold_fallback_chain(
    fallback_chain: &[DelegateFallbackEntry],
    connected_providers: Option<&[String]>,
    deps: &DelegateModelResolutionDeps<'_>,
) -> Option<DelegateModelResolutionResult> {
    let Some(connected) = connected_providers else {
        let first = fallback_chain.first()?;
        let provider = first.providers.first()?;
        let transformed_model_id = transform_model_for_provider(provider, &first.model);
        return fallback_resolved(
            format!("{provider}/{transformed_model_id}"),
            first.variant.clone(),
            Some(first),
        );
    };

    for entry in fallback_chain {
        for provider in &entry.providers {
            if connected.contains(provider) {
                let transformed_model_id = transform_model_for_provider(provider, &entry.model);
                deps.log(
                    "[resolveModelForDelegateTask] fallback chain resolved via connected provider",
                    Some(&json!({ "provider": provider, "model": entry.model })),
                );
                return fallback_resolved(
                    format!("{provider}/{transformed_model_id}"),
                    entry.variant.clone(),
                    Some(entry),
                );
            }
        }
    }
    deps.log(
        "[resolveModelForDelegateTask] no connected provider found in fallback chain",
        None,
    );
    None
}

/// `explicit_high` is `(categoryDefault, categoryDefault without "-high")` when the category
/// default names an explicit `-high` model; a `high` rung matching the base keeps that name.
fn resolve_warm_fallback_chain(
    fallback_chain: &[DelegateFallbackEntry],
    available_models: &IndexSet<String>,
    explicit_high: Option<(&str, &str)>,
) -> Option<DelegateModelResolutionResult> {
    let choose = |matched: String, entry: &DelegateFallbackEntry| match explicit_high {
        Some((high_model, high_base))
            if entry.variant.as_deref() == Some("high") && matched == high_base =>
        {
            fallback_resolved(high_model.to_string(), None, Some(entry))
        }
        _ => fallback_resolved(matched, entry.variant.clone(), Some(entry)),
    };

    for (entry_index, entry) in fallback_chain.iter().enumerate() {
        for provider in &entry.providers {
            let transformed_model_id = transform_model_for_provider(provider, &entry.model);
            let full_model = format!("{provider}/{transformed_model_id}");
            if let Some(matched) = fuzzy_match_model(
                &full_model,
                available_models,
                Some(std::slice::from_ref(provider)),
            ) {
                return choose(matched, entry);
            }
        }

        let later_rung_providers: IndexSet<&str> = fallback_chain[entry_index + 1..]
            .iter()
            .filter(|candidate| candidate.model == entry.model)
            .flat_map(|candidate| candidate.providers.iter().map(String::as_str))
            .collect();
        let cross_provider_candidates: IndexSet<String> = available_models
            .iter()
            .filter(|model| {
                let provider = model.split('/').next().unwrap_or(model);
                !later_rung_providers.contains(provider)
            })
            .cloned()
            .collect();
        if let Some(matched) = fuzzy_match_model(&entry.model, &cross_provider_candidates, None) {
            return choose(matched, entry);
        }
    }
    None
}
