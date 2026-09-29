use std::collections::{BTreeSet, HashMap, HashSet};

use serde_json::{Map, Value};

use super::builtins::{
    BUILTIN_CATEGORY_DEFAULTS, builtin_category, is_category_chain_rung_resolvable,
    is_category_chain_viable, is_category_gate_satisfied,
};
use super::fallback_chains::category_fallback_chain;
use crate::delegate_adapter::{
    DelegateFallbackEntry, DelegateModelResolutionInput, resolve_model_for_delegate_task,
};
use crate::host::{
    HostError, SenpiModelRegistry, parse_available_models, parse_model, parse_registry_model,
};
use crate::model_chain::{
    BuildModelChainOptions, ChainRungCandidateOptions, ModelChainCandidate,
    build_runtime_model_chain, chain_rung_candidates,
};
use crate::state::{ResolvedModelRecord, ResolvedModelSource};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolveCategoryOptions {
    pub system_default_model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryModelSelection {
    pub selected_model: String,
    pub variant: Option<String>,
    pub matched_fallback: bool,
    pub fallback_entry: Option<DelegateFallbackEntry>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolvedChildSpec {
    /// The untouched host registry value.
    pub model: Value,
    pub provider: String,
    pub model_id: String,
    pub requested_model: Option<ResolvedModelRecord>,
    pub fallback_models: Option<Vec<ResolvedModelRecord>>,
    pub display_name: Option<String>,
    pub variant: Option<String>,
    pub reasoning: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_tokens: Option<u64>,
    pub thinking: Option<Value>,
    pub reasoning_effort: Option<String>,
    pub tools: Option<Value>,
    pub prompt_append: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CategoryResolutionResult {
    Resolved {
        category: String,
        spec: Box<ResolvedChildSpec>,
        config: Value,
        description: Option<String>,
        model_selection: Box<CategoryModelSelection>,
        available_categories: Vec<String>,
    },
    Disabled {
        category: String,
        reason: String,
        available_categories: Vec<String>,
    },
    NotFound {
        category: String,
        available_categories: Vec<String>,
    },
    ModelUnavailable(Box<ModelUnavailable>),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelUnavailable {
    pub category: String,
    pub attempted_model: Option<String>,
    pub available_models: Vec<String>,
    pub available_categories: Vec<String>,
    pub nearest_fallback: Option<String>,
    pub fallback_entry: Option<DelegateFallbackEntry>,
    /// Present only when the builtin fallback chain had zero resolvable rungs.
    pub attempted_chain: Option<Vec<DelegateFallbackEntry>>,
    pub missing_providers: Option<Vec<String>>,
}

impl CategoryResolutionResult {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Resolved { .. } => "resolved",
            Self::Disabled { .. } => "disabled",
            Self::NotFound { .. } => "not_found",
            Self::ModelUnavailable(_) => "model_unavailable",
        }
    }

    pub fn available_categories(&self) -> &[String] {
        match self {
            Self::Resolved {
                available_categories,
                ..
            }
            | Self::Disabled {
                available_categories,
                ..
            }
            | Self::NotFound {
                available_categories,
                ..
            } => available_categories,
            Self::ModelUnavailable(unavailable) => &unavailable.available_categories,
        }
    }
}

fn user_categories(config: &Value) -> Option<&Map<String, Value>> {
    config.get("categories").and_then(Value::as_object)
}

fn string_field(config: &Value, key: &str) -> Option<String> {
    config.get(key).and_then(Value::as_str).map(str::to_string)
}

fn flatten_fallback_models(fallback_models: Option<&Value>) -> Option<Vec<String>> {
    match fallback_models? {
        Value::String(model) => Some(vec![model.clone()]),
        Value::Array(entries) => Some(
            entries
                .iter()
                .filter_map(|entry| match entry {
                    Value::String(model) => Some(model.clone()),
                    Value::Object(object) => {
                        let model = object.get("model")?.as_str()?;
                        Some(
                            match object
                                .get("variant")
                                .and_then(Value::as_str)
                                .filter(|variant| !variant.is_empty())
                            {
                                Some(variant) => format!("{model} {variant}"),
                                None => model.to_string(),
                            },
                        )
                    }
                    _ => None,
                })
                .collect(),
        ),
        _ => None,
    }
}

fn canonical_models(config: &Value) -> Option<&Vec<Value>> {
    config
        .get("models")
        .and_then(Value::as_array)
        .filter(|models| !models.is_empty())
}

fn first_string(values: &[Option<&Value>]) -> Option<String> {
    values
        .iter()
        .find_map(|value| value.and_then(Value::as_str))
        .map(str::to_string)
}

pub(crate) fn category_model_candidates(config: &Value) -> Vec<ModelChainCandidate> {
    if let Some(models) = canonical_models(config) {
        return models
            .iter()
            .filter_map(|entry| match entry {
                Value::String(model) => Some(ModelChainCandidate::of(model)),
                Value::Object(object) => Some(ModelChainCandidate {
                    model: object.get("model")?.as_str()?.to_string(),
                    variant: string_field(entry, "variant"),
                    reasoning_effort: string_field(entry, "reasoning"),
                }),
                _ => None,
            })
            .collect();
    }
    let config_variant = config.get("variant");
    let config_reasoning = config.get("reasoning");
    let config_effort = config.get("reasoningEffort");
    let mut candidates: Vec<ModelChainCandidate> = string_field(config, "model")
        .map(|model| ModelChainCandidate {
            model,
            variant: first_string(&[config_variant]),
            reasoning_effort: first_string(&[config_reasoning, config_effort]),
        })
        .into_iter()
        .collect();
    let entries = match config.get("fallback_models") {
        Some(Value::String(model)) => vec![Value::String(model.clone())],
        Some(Value::Array(entries)) => entries.clone(),
        _ => return candidates,
    };
    candidates.extend(entries.iter().filter_map(|entry| match entry {
        Value::String(model) => Some(ModelChainCandidate {
            model: model.clone(),
            variant: first_string(&[config_variant]),
            reasoning_effort: first_string(&[config_effort]),
        }),
        Value::Object(_) => Some(ModelChainCandidate {
            model: string_field(entry, "model")?,
            variant: first_string(&[entry.get("variant"), config_variant]),
            reasoning_effort: first_string(&[
                entry.get("reasoning"),
                config_reasoning,
                entry.get("reasoningEffort"),
                config_effort,
            ]),
        }),
        _ => None,
    }));
    candidates
}

fn available_category_names(
    config: &Value,
    available_model_ids: Option<&HashSet<String>>,
) -> Vec<String> {
    let user = user_categories(config);
    let names: BTreeSet<String> = BUILTIN_CATEGORY_DEFAULTS
        .iter()
        .map(|definition| definition.name.to_string())
        .chain(user.into_iter().flat_map(|user| user.keys().cloned()))
        .collect();
    let Some(available_model_ids) = available_model_ids else {
        return names.into_iter().collect();
    };
    names
        .into_iter()
        .filter(|name| {
            let explicit = user.is_some_and(|user| user.contains_key(name));
            is_category_gate_satisfied(name, explicit, available_model_ids)
                && is_category_chain_viable(name, explicit, available_model_ids)
        })
        .collect()
}

/// Best-effort gated listing: a failing or malformed registry degrades to the ungated list.
fn gated_available_categories(config: &Value, registry: &dyn SenpiModelRegistry) -> Vec<String> {
    let parsed = registry
        .get_available()
        .ok()
        .and_then(|available| parse_available_models(&available));
    match parsed {
        Some(models) => available_category_names(config, Some(&model_ids_of(&models))),
        None => available_category_names(config, None),
    }
}

pub fn resolve_available_category_names(
    config: &Value,
    registry: &dyn SenpiModelRegistry,
) -> Vec<String> {
    gated_available_categories(config, registry)
}

fn provider_of(model: &str) -> &str {
    model
        .split_once('/')
        .map_or(model, |(provider, _)| provider)
}

fn missing_chain_providers(
    chain: &[DelegateFallbackEntry],
    available_models: &[String],
) -> Vec<String> {
    let connected: HashSet<&str> = available_models
        .iter()
        .map(|model| provider_of(model))
        .collect();
    let mut missing: Vec<String> = Vec::new();
    for provider in chain.iter().flat_map(|rung| &rung.providers) {
        if !connected.contains(provider.as_str()) && !missing.contains(provider) {
            missing.push(provider.clone());
        }
    }
    missing
}

/// Only these upstream vendor prefixes are unwrapped from `<gateway>/<vendor>/<model-id>`, so an
/// unrelated model that merely ends in a gate model's name cannot open that gate.
const GATEWAY_UPSTREAM_VENDOR_PREFIXES: [&str; 3] = ["openai", "anthropic", "google"];

fn model_ids_of(models: &[String]) -> HashSet<String> {
    let mut ids = HashSet::new();
    for entry in models {
        let model_id = entry.split_once('/').map_or(entry.as_str(), |(_, id)| id);
        ids.insert(model_id.to_string());
        if let Some((vendor, upstream_id)) = model_id.split_once('/')
            && !vendor.is_empty()
            && !upstream_id.contains('/')
            && GATEWAY_UPSTREAM_VENDOR_PREFIXES.contains(&vendor)
        {
            ids.insert(upstream_id.to_string());
        }
    }
    ids
}

fn prompt_append_for_category(
    category_name: &str,
    model: Option<&str>,
    user_prompt_append: Option<&str>,
) -> Option<String> {
    let base = builtin_category(category_name).map_or("", |definition| {
        definition
            .resolve_prompt_append
            .map_or(definition.prompt_append, |resolve| resolve(model))
    });
    match user_prompt_append.filter(|append| !append.is_empty()) {
        None => (!base.is_empty()).then(|| base.to_string()),
        Some(user) if base.is_empty() => Some(user.to_string()),
        Some(user) => Some(format!("{base}\n\n{user}")),
    }
}

fn merge_config(builtin: Option<Value>, user: Option<&Value>) -> Value {
    let mut merged = builtin
        .and_then(|builtin| builtin.as_object().cloned())
        .unwrap_or_default();
    if let Some(user) = user.and_then(Value::as_object) {
        for (key, value) in user {
            merged.insert(key.clone(), value.clone());
        }
    }
    Value::Object(merged)
}

pub fn resolve_category(
    category_name: &str,
    omo_config: &Value,
    registry: &dyn SenpiModelRegistry,
    options: &ResolveCategoryOptions,
) -> Result<CategoryResolutionResult, HostError> {
    let available_categories = available_category_names(omo_config, None);
    let user_config = user_categories(omo_config).and_then(|user| user.get(category_name));
    if user_config.and_then(|user| user.get("disable")) == Some(&Value::Bool(true)) {
        return Ok(CategoryResolutionResult::Disabled {
            category: category_name.to_string(),
            reason: format!("Category \"{category_name}\" is disabled by omo.json"),
            available_categories: gated_available_categories(omo_config, registry),
        });
    }

    let builtin = builtin_category(category_name);
    if builtin.is_none() && user_config.is_none() {
        return Ok(CategoryResolutionResult::NotFound {
            category: category_name.to_string(),
            available_categories: gated_available_categories(omo_config, registry),
        });
    }
    let builtin_model = builtin.map(|definition| definition.config.model.to_string());
    let config = merge_config(
        builtin.map(|definition| definition.config.to_value()),
        user_config,
    );
    let unavailable = |attempted_model: Option<String>,
                       available_models: &[String],
                       available_categories: Vec<String>| ModelUnavailable {
        category: category_name.to_string(),
        attempted_model,
        available_models: available_models.to_vec(),
        available_categories,
        ..ModelUnavailable::default()
    };

    let Some(available_models) = parse_available_models(&registry.get_available()?) else {
        return Ok(CategoryResolutionResult::ModelUnavailable(Box::new(
            unavailable(string_field(&config, "model"), &[], available_categories),
        )));
    };

    let available_model_ids = model_ids_of(&available_models);
    let gated_categories = available_category_names(omo_config, Some(&available_model_ids));
    let fallback_chain = category_fallback_chain(category_name);
    let dead_chain = fallback_chain
        .filter(|chain| {
            !chain.is_empty()
                && !chain
                    .iter()
                    .any(|rung| is_category_chain_rung_resolvable(rung, &available_model_ids))
        })
        .map(|chain| {
            (
                chain.to_vec(),
                missing_chain_providers(chain, &available_models),
            )
        });
    let with_dead_chain = |mut result: ModelUnavailable| {
        if let Some((chain, missing)) = &dead_chain {
            result.attempted_chain = Some(chain.clone());
            result.missing_providers = Some(missing.clone());
        }
        result
    };
    let attempted_default = builtin_model
        .clone()
        .or_else(|| string_field(&config, "model"));
    if !is_category_gate_satisfied(category_name, user_config.is_some(), &available_model_ids) {
        return Ok(CategoryResolutionResult::ModelUnavailable(Box::new(
            with_dead_chain(unavailable(
                attempted_default,
                &available_models,
                gated_categories,
            )),
        )));
    }

    // A dead builtin chain can never produce a model; an explicit user model or fallback list and a
    // caller-supplied system default opt out.
    let user_has_canonical_models = user_config.and_then(canonical_models).is_some();
    let user_has = |key: &str| user_config.and_then(|user| user.get(key)).is_some();
    if dead_chain.is_some()
        && !user_has_canonical_models
        && !user_has("model")
        && !user_has("fallback_models")
        && options.system_default_model.is_none()
    {
        return Ok(CategoryResolutionResult::ModelUnavailable(Box::new(
            with_dead_chain(unavailable(
                attempted_default,
                &available_models,
                gated_categories,
            )),
        )));
    }

    let canonical_chain = canonical_models(&config).map(|_| category_model_candidates(&config));
    let canonical_reasoning_by_model: HashMap<String, Option<String>> = canonical_chain
        .iter()
        .flatten()
        .map(|candidate| (candidate.model.clone(), candidate.reasoning_effort.clone()))
        .collect();
    let user_model = match &canonical_chain {
        Some(chain) => chain.first().map(|candidate| candidate.model.clone()),
        None => user_config.and_then(|user| string_field(user, "model")),
    };
    let user_fallback_models = match &canonical_chain {
        Some(chain) => Some(
            chain
                .iter()
                .skip(1)
                .map(|candidate| candidate.model.clone())
                .collect(),
        ),
        None => flatten_fallback_models(config.get("fallback_models")),
    };
    let available_set: BTreeSet<String> = available_models.iter().cloned().collect();
    let resolution = resolve_model_for_delegate_task(&DelegateModelResolutionInput {
        user_model: user_model.as_deref(),
        user_fallback_models: user_fallback_models.as_deref(),
        category_default_model: builtin_model.as_deref(),
        fallback_chain,
        available_models: available_set.clone(),
        system_default_model: options.system_default_model.as_deref(),
    });
    let Some(resolution) = resolution else {
        return Ok(CategoryResolutionResult::ModelUnavailable(Box::new(
            unavailable(
                string_field(&config, "model"),
                &available_models,
                gated_categories,
            ),
        )));
    };

    let selection = CategoryModelSelection {
        selected_model: resolution.model,
        variant: resolution.variant,
        matched_fallback: resolution.matched_fallback,
        fallback_entry: resolution.fallback_entry,
    };
    let parsed_model = parse_model(&selection.selected_model);
    let found_model = parsed_model.as_ref().and_then(|parsed| {
        registry
            .find(&parsed.provider, &parsed.model_id)
            .and_then(|found| parse_registry_model(&found, Some(parsed)))
    });
    let Some(found_model) = found_model else {
        let nearest_fallback = selection.fallback_entry.as_ref().and_then(|entry| {
            entry
                .providers
                .first()
                .map(|provider| format!("{provider}/{}", entry.model))
        });
        return Ok(CategoryResolutionResult::ModelUnavailable(Box::new(
            ModelUnavailable {
                nearest_fallback,
                fallback_entry: selection.fallback_entry.clone(),
                ..unavailable(
                    Some(selection.selected_model.clone()),
                    &available_models,
                    gated_categories,
                )
            },
        )));
    };

    let prompt_append = prompt_append_for_category(
        category_name,
        Some(&selection.selected_model),
        user_config
            .and_then(|user| user.get("prompt_append"))
            .and_then(Value::as_str),
    );
    let variant = user_config
        .and_then(|user| string_field(user, "variant"))
        .or_else(|| selection.variant.clone())
        .or_else(|| string_field(&config, "variant"));
    let reasoning_effort = canonical_reasoning_by_model
        .get(&selection.selected_model)
        .cloned()
        .flatten()
        .or_else(|| string_field(&config, "reasoning"))
        .or_else(|| string_field(&config, "reasoningEffort"));
    // Remaining builtin rungs extend the runtime retry chain after any user fallback_models.
    let chain_candidates = fallback_chain.map_or_else(Vec::new, |chain| {
        chain_rung_candidates(&ChainRungCandidateOptions {
            chain,
            selected_model: &selection.selected_model,
            selected_rung_entry: selection.fallback_entry.as_ref(),
            available_models: &available_set,
        })
    });
    let mut candidates = category_model_candidates(&config);
    candidates.extend(chain_candidates);
    let runtime_chain = build_runtime_model_chain(&BuildModelChainOptions {
        candidates,
        selected_model: &selection.selected_model,
        available_models: Some(&available_set),
        source: ResolvedModelSource::Category,
    });
    let spec = ResolvedChildSpec {
        model: found_model.model,
        provider: found_model.provider,
        model_id: found_model.model_id,
        requested_model: runtime_chain.requested_model,
        fallback_models: runtime_chain.fallback_models,
        display_name: found_model.display_name,
        variant,
        reasoning: None,
        temperature: config.get("temperature").and_then(Value::as_f64),
        top_p: config.get("top_p").and_then(Value::as_f64),
        max_tokens: config.get("maxTokens").and_then(Value::as_u64),
        thinking: config.get("thinking").cloned(),
        reasoning_effort,
        tools: config.get("tools").cloned(),
        prompt_append,
    };
    let description = user_config
        .and_then(|user| string_field(user, "description"))
        .or_else(|| builtin.map(|definition| definition.description.to_string()));
    Ok(CategoryResolutionResult::Resolved {
        category: category_name.to_string(),
        spec: Box::new(spec),
        config,
        description,
        model_selection: Box::new(selection),
        available_categories: gated_categories,
    })
}
