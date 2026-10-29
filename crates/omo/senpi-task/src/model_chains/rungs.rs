use super::types::{ChainRung, ChainRungState};
use crate::delegate_adapter::DelegateFallbackEntry;
use crate::model_chain::available_chain_candidates;
use crate::model_exclusion::excluded_model_pattern;
use std::collections::BTreeSet;

pub struct DeclaredRung {
    pub model: String,
    pub source: String,
    pub providers: Option<Vec<String>>,
}
impl DeclaredRung {
    pub fn new(model: String, source: &str) -> Self {
        Self {
            model,
            source: source.into(),
            providers: None,
        }
    }
}
pub fn builtin_rungs(
    chain: Option<&[DelegateFallbackEntry]>,
    available: &BTreeSet<String>,
) -> Vec<DeclaredRung> {
    chain
        .into_iter()
        .flatten()
        .map(|rung| {
            match available_chain_candidates(std::slice::from_ref(rung), available).first() {
                Some(concrete) => DeclaredRung::new(concrete.model.clone(), "built-in"),
                None => DeclaredRung {
                    model: rung.model.clone(),
                    source: "built-in".into(),
                    providers: Some(rung.providers.clone()),
                },
            }
        })
        .collect()
}
pub fn classify_rungs(
    declared: Vec<DeclaredRung>,
    runtime: &[String],
    available: &BTreeSet<String>,
    exclusions: &[String],
) -> Vec<ChainRung> {
    let mut seen = BTreeSet::new();
    let mut rungs = Vec::new();
    for rung in declared {
        if seen.insert(rung.model.clone()) {
            rungs.push(classify(rung, runtime, available, exclusions));
        }
    }
    for model in runtime {
        if !seen.contains(model) {
            rungs.push(classify(
                DeclaredRung::new(model.clone(), "built-in"),
                runtime,
                available,
                exclusions,
            ));
        }
    }
    rungs
}
fn classify(
    rung: DeclaredRung,
    runtime: &[String],
    available: &BTreeSet<String>,
    exclusions: &[String],
) -> ChainRung {
    let mut providers = None;
    let mut excluded_by = None;
    let state = match runtime.iter().position(|model| model == &rung.model) {
        Some(0) => ChainRungState::Primary,
        Some(_) => ChainRungState::Fallback,
        None => {
            if let Some(pattern) = excluded_model_pattern(&rung.model, exclusions) {
                excluded_by = Some(pattern.into());
                ChainRungState::Excluded
            } else if rung.providers.is_some() || !available.contains(&rung.model) {
                providers = rung.providers;
                ChainRungState::NoLogin
            } else {
                ChainRungState::NotReached
            }
        }
    };
    ChainRung {
        model: rung.model,
        source: rung.source,
        state,
        providers,
        excluded_by,
    }
}
