use super::rungs::{DeclaredRung, builtin_rungs, classify_rungs};
use super::types::ModelChainEntry;
use crate::category::category_model_candidates;
use crate::category::{
    CategoryResolutionResult, ResolveCategoryOptions, builtin_category, category_fallback_chain,
    resolve_category,
};
use crate::host::{HostError, SenpiModelRegistry};
use crate::category::exclude_model_patterns;
use serde_json::Value;
use std::collections::BTreeSet;

pub fn category_runtime_chain(result: &CategoryResolutionResult) -> Vec<String> {
    match result {
        CategoryResolutionResult::Resolved { spec, .. } => {
            std::iter::once(format!("{}/{}", spec.provider, spec.model_id))
                .chain(
                    spec.fallback_models
                        .iter()
                        .flatten()
                        .map(|model| model.display.clone()),
                )
                .collect()
        }
        _ => Vec::new(),
    }
}
fn unavailable_reason(result: &CategoryResolutionResult) -> Option<String> {
    match result {
        CategoryResolutionResult::Resolved { .. } => None,
        CategoryResolutionResult::Disabled { reason, .. } => Some(reason.clone()),
        CategoryResolutionResult::NotFound { category, .. } => {
            Some(format!("no category named \"{category}\""))
        }
        CategoryResolutionResult::ModelUnavailable(result) => Some(
            match result
                .missing_providers
                .as_ref()
                .filter(|providers| !providers.is_empty())
            {
                Some(providers) => {
                    format!("no rung is usable; connect one of {}", providers.join(", "))
                }
                None => result.attempted_model.as_ref().map_or_else(
                    || "no rung is usable".into(),
                    |model| format!("{model} is not usable"),
                ),
            },
        ),
    }
}
pub fn describe_category_chain(
    name: &str,
    config: &Value,
    registry: &dyn SenpiModelRegistry,
    available: &BTreeSet<String>,
) -> Result<ModelChainEntry, HostError> {
    let result = resolve_category(name, config, registry, &ResolveCategoryOptions::default())?;
    let user = config
        .get("categories")
        .and_then(|categories| categories.get(name));
    let user_models: BTreeSet<String> = user
        .map(category_model_candidates)
        .unwrap_or_default()
        .into_iter()
        .map(|candidate| candidate.model)
        .collect();
    let chain = category_fallback_chain(name);
    let ids: BTreeSet<&str> = chain
        .into_iter()
        .flatten()
        .map(|rung| rung.model.as_str())
        .collect();
    let mut merged = builtin_category(name)
        .map(|builtin| builtin.config.to_value())
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(user) = user.and_then(Value::as_object)
        && let Some(merged) = merged.as_object_mut() {
            merged.extend(user.clone());
        }
    let mut declared = Vec::new();
    for candidate in category_model_candidates(&merged) {
        if user_models.contains(&candidate.model) {
            declared.push(DeclaredRung::new(candidate.model, "omo.json"));
        } else if !ids.contains(
            candidate
                .model
                .split_once('/')
                .map_or(candidate.model.as_str(), |(_, id)| id),
        ) {
            declared.push(DeclaredRung::new(candidate.model, "built-in"));
        }
    }
    declared.extend(builtin_rungs(chain, available));
    let runtime = category_runtime_chain(&result);
    Ok(ModelChainEntry {
        kind: "category".into(),
        name: name.into(),
        status: match &result {
            CategoryResolutionResult::Resolved { .. } => "resolved",
            CategoryResolutionResult::Disabled { .. } => "disabled",
            _ => "unavailable",
        }
        .into(),
        primary: runtime.first().cloned(),
        rungs: classify_rungs(
            declared,
            &runtime,
            available,
            &exclude_model_patterns(config).unwrap_or_default(),
        ),
        runtime,
        reason: unavailable_reason(&result),
    })
}
