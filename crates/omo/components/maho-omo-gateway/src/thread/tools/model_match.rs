//! Model pattern matching & thinking suffixes (`thread/tools/model-match.ts`).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::super::errors::thread_tool_failure;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelChoice {
    pub provider: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ModelMatchResult {
    Match {
        model: ModelChoice,
        thinking_level: Option<String>,
    },
    Error(Value),
}

const THINKING_LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max"];

pub fn split_thinking_decorator(pattern: &str) -> (String, Option<String>) {
    let trimmed = pattern.trim();
    if let Some((head, tail)) = trimmed.rsplit_once(':')
        && !head.is_empty()
        && !tail.is_empty()
        && !tail.contains('/')
    {
        let lower = tail.to_ascii_lowercase();
        if THINKING_LEVELS.contains(&lower.as_str()) {
            return (head.to_owned(), Some(lower));
        }
    }
    (trimmed.to_owned(), None)
}

pub fn model_pattern_failure(model: &str) -> Option<Value> {
    if model.trim().is_empty() {
        Some(json!({
            "kind": "error",
            "error": thread_tool_failure(
                "invalid_arguments",
                "The model pattern is empty.",
                "Pass a model id or display-name fragment.",
                None,
            )
        }))
    } else {
        None
    }
}

pub fn exact_model(
    catalog: &[ModelChoice],
    model: &str,
    provider: Option<&str>,
) -> Option<ModelChoice> {
    let pattern = model.trim().to_ascii_lowercase();
    let provider_lower = provider.map(|p| p.trim().to_ascii_lowercase());
    let mut named: Vec<&ModelChoice> = catalog
        .iter()
        .filter(|choice| {
            format!("{}/{}", choice.provider, choice.id).to_ascii_lowercase() == pattern
                && provider_lower
                    .as_deref()
                    .is_none_or(|p| choice.provider.to_ascii_lowercase() == p)
        })
        .collect();
    if named.len() == 1 {
        Some(named.remove(0).clone())
    } else {
        None
    }
}

pub fn match_model(
    catalog: &[ModelChoice],
    model: &str,
    provider: Option<&str>,
) -> ModelMatchResult {
    let reference = model.trim();
    let provider_filter = provider.map(|p| p.trim().to_ascii_lowercase());
    let available: Vec<&ModelChoice> = match provider_filter.as_deref() {
        None => catalog.iter().collect(),
        Some(p) => catalog
            .iter()
            .filter(|choice| choice.provider.to_ascii_lowercase() == p)
            .collect(),
    };

    let find = |pattern: &str| -> Vec<ModelChoice> {
        let by_ref: Vec<ModelChoice> = available
            .iter()
            .filter(|c| format!("{}/{}", c.provider, c.id).to_ascii_lowercase() == pattern)
            .map(|c| (*c).clone())
            .collect();
        if !by_ref.is_empty() {
            return by_ref;
        }
        let by_id: Vec<ModelChoice> = available
            .iter()
            .filter(|c| c.id.to_ascii_lowercase() == pattern)
            .map(|c| (*c).clone())
            .collect();
        if !by_id.is_empty() {
            return by_id;
        }
        available
            .iter()
            .filter(|c| {
                c.id.to_ascii_lowercase().contains(pattern)
                    || c.name
                        .as_deref()
                        .is_some_and(|n| n.to_ascii_lowercase().contains(pattern))
            })
            .map(|c| (*c).clone())
            .collect()
    };

    let literal = find(&reference.to_ascii_lowercase());
    let (matches, thinking_level) = if literal.is_empty() {
        let (base, level) = split_thinking_decorator(reference);
        if let Some(level) = level {
            (find(&base.to_ascii_lowercase()), Some(level))
        } else {
            (literal, None)
        }
    } else {
        (literal, None)
    };

    if matches.is_empty() {
        let available_models: Vec<String> = catalog
            .iter()
            .take(20)
            .map(|c| format!("{}/{}", c.provider, c.id))
            .collect();
        let requested_provider = reference
            .find('/')
            .filter(|slash| *slash > 0)
            .map(|slash| &reference[..slash])
            .or_else(|| provider.map(str::trim));
        if let Some(req_p) = requested_provider
            && !req_p.is_empty()
            && !catalog
                .iter()
                .any(|c| c.provider.eq_ignore_ascii_case(req_p))
        {
            let available_providers: Vec<String> = catalog
                .iter()
                .map(|c| c.provider.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            return ModelMatchResult::Error(json!({
                "kind": "error",
                "error": thread_tool_failure(
                    "model_not_found",
                    format!("Provider \"{req_p}\" has no available models in the target's catalog."),
                    "Choose a provider from the target's available list or enable the requested provider on the target.",
                    Some(json!({
                        "reason": "provider_not_configured",
                        "provider": req_p,
                        "available_providers": available_providers,
                        "available": available_models,
                    })),
                )
            }));
        }
        return ModelMatchResult::Error(json!({
            "kind": "error",
            "error": thread_tool_failure(
                "model_not_found",
                format!("No available model matches \"{model}\"."),
                "Choose a provider/id from the available list and retry.",
                Some(json!({ "available": available_models })),
            )
        }));
    }

    if matches.len() > 1 {
        let candidates: Vec<String> = matches
            .iter()
            .take(10)
            .map(|c| format!("{}/{}", c.provider, c.id))
            .collect();
        return ModelMatchResult::Error(json!({
            "kind": "error",
            "error": thread_tool_failure(
                "model_ambiguous",
                format!("Several available models match \"{model}\"."),
                "Pass an exact provider/id or narrow the pattern with provider.",
                Some(json!({ "candidates": candidates })),
            )
        }));
    }

    ModelMatchResult::Match {
        model: matches[0].clone(),
        thinking_level,
    }
}
