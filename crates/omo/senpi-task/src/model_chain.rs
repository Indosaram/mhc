//! Runtime model-chain policy (`model-chain.ts`).

use std::collections::{BTreeSet, HashSet};

use crate::delegate_adapter::{DelegateFallbackEntry, transform_model_for_provider};
use crate::state::{ResolvedModelRecord, ResolvedModelSource};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChainCandidate {
    pub model: String,
    pub variant: Option<String>,
    pub reasoning_effort: Option<String>,
}

impl ModelChainCandidate {
    pub fn of(model: &str) -> Self {
        Self {
            model: model.to_string(),
            variant: None,
            reasoning_effort: None,
        }
    }
}

pub struct BuildModelChainOptions<'a> {
    pub candidates: Vec<ModelChainCandidate>,
    pub selected_model: &'a str,
    pub available_models: Option<&'a BTreeSet<String>>,
    pub source: ResolvedModelSource,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeModelChain {
    pub requested_model: Option<ResolvedModelRecord>,
    pub fallback_models: Option<Vec<ResolvedModelRecord>>,
}

pub fn build_runtime_model_chain(options: &BuildModelChainOptions<'_>) -> RuntimeModelChain {
    let mut seen = HashSet::new();
    let candidates: Vec<&ModelChainCandidate> = options
        .candidates
        .iter()
        .filter(|candidate| seen.insert(candidate.model.clone()))
        .collect();
    let Some(requested_model) = candidates
        .first()
        .and_then(|candidate| to_resolved_model_record(candidate, options.source))
    else {
        return RuntimeModelChain::default();
    };
    let fallback_models: Vec<ResolvedModelRecord> = candidates
        .iter()
        .position(|candidate| candidate.model == options.selected_model)
        .map(|selected| {
            candidates[selected + 1..]
                .iter()
                .filter(|candidate| {
                    options
                        .available_models
                        .is_none_or(|available| available.contains(&candidate.model))
                })
                .filter_map(|candidate| to_resolved_model_record(candidate, options.source))
                .collect()
        })
        .unwrap_or_default();
    RuntimeModelChain {
        requested_model: Some(requested_model),
        fallback_models: (!fallback_models.is_empty()).then_some(fallback_models),
    }
}

pub struct ChainRungCandidateOptions<'a> {
    pub chain: &'a [DelegateFallbackEntry],
    pub selected_model: &'a str,
    pub selected_rung_entry: Option<&'a DelegateFallbackEntry>,
    pub available_models: &'a BTreeSet<String>,
}

/// The selected rung's concrete model first, then each later rung resolved to its first available
/// provider. Empty when the selected model cannot be located in the chain (a user-forced model).
pub fn chain_rung_candidates(options: &ChainRungCandidateOptions<'_>) -> Vec<ModelChainCandidate> {
    let Some(selected_index) = locate_selected_rung(options) else {
        return Vec::new();
    };
    let mut rungs = vec![ModelChainCandidate::of(options.selected_model)];
    for entry in &options.chain[selected_index + 1..] {
        let concrete = entry
            .providers
            .iter()
            .map(|provider| {
                format!(
                    "{provider}/{}",
                    transform_model_for_provider(provider, &entry.model)
                )
            })
            .find(|concrete| options.available_models.contains(concrete));
        if let Some(concrete) = concrete {
            rungs.push(ModelChainCandidate {
                model: concrete,
                variant: entry.variant.clone(),
                reasoning_effort: None,
            });
        }
    }
    rungs
}

fn locate_selected_rung(options: &ChainRungCandidateOptions<'_>) -> Option<usize> {
    if let Some(selected) = options.selected_rung_entry
        && let Some(index) = options.chain.iter().position(|entry| entry == selected)
    {
        return Some(index);
    }
    options.chain.iter().position(|entry| {
        entry.providers.iter().any(|provider| {
            format!(
                "{provider}/{}",
                transform_model_for_provider(provider, &entry.model)
            ) == options.selected_model
        })
    })
}

fn to_resolved_model_record(
    candidate: &ModelChainCandidate,
    source: ResolvedModelSource,
) -> Option<ResolvedModelRecord> {
    let parsed = crate::host::parse_model(&candidate.model)?;
    Some(ResolvedModelRecord {
        variant: candidate.variant.clone(),
        reasoning_effort: candidate.reasoning_effort.clone(),
        ..ResolvedModelRecord::new(source, &parsed.provider, &parsed.model_id)
    })
}
