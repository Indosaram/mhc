//! Port of senpi `packages/coding-agent/src/core/startup-model-pick.ts`.
//!
//! `ModelUsabilityBudgetProjection` lives in maho-ext-compaction, which depends on this crate; the
//! messages here take the three projection fields they render, and `can_start_on` takes the session's
//! start admission as a closure whose `Err(true)` means a budget refusal.

use maho_ai::model::Model;

use crate::model_resolver::InitialModelProvenance;
use crate::provider_default_selection::{ProviderAuthStatusSource, select_provider_default};

pub fn is_automatic_pick(provenance: Option<InitialModelProvenance>) -> bool {
    matches!(
        provenance,
        Some(InitialModelProvenance::ProviderDefault | InitialModelProvenance::FirstAvailable)
    )
}

/// The model an automatic startup pick moves to when its first choice cannot start: the next one the
/// same automatic order would choose among the remaining available models, skipping every model
/// `can_start` refuses (senpi#3333).
pub fn next_startable_model(
    available: &[Model],
    rejected: &Model,
    mut can_start: impl FnMut(&Model) -> bool,
    provider_defaults: &[(String, String)],
    source: &dyn ProviderAuthStatusSource,
) -> Option<Model> {
    let mut remaining: Vec<Model> = available
        .iter()
        .filter(|model| !(model.provider == rejected.provider && model.id == rejected.id))
        .cloned()
        .collect();
    while !remaining.is_empty() {
        let candidate = select_provider_default(&remaining, provider_defaults, source)?.model;
        if can_start(&candidate) {
            return Some(candidate);
        }
        remaining
            .retain(|model| !(model.provider == candidate.provider && model.id == candidate.id));
    }
    None
}

/// The projection fields the startup messages render.
#[derive(Debug, Clone, PartialEq)]
pub struct StartupRefusal {
    pub model: String,
    pub context_window: f64,
    pub required_tokens: f64,
}

fn number(value: f64) -> String {
    if value.fract() == 0.0 && value.is_finite() {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

pub fn startup_replacement_message(replacement: &Model, refused: &StartupRefusal) -> String {
    format!(
        "No usable default model is set, so {}/{} was chosen: {} has a {}-token context window and needs {} to start. Use /model to pick another, or set a default model.",
        replacement.provider,
        replacement.id,
        refused.model,
        number(refused.context_window),
        number(refused.required_tokens)
    )
}

pub fn startup_refusal_message(refused: &StartupRefusal, automatic: bool) -> String {
    let why = format!(
        "{} cannot start: its context window is {} tokens and starting needs {}.",
        refused.model,
        number(refused.context_window),
        number(refused.required_tokens)
    );
    if automatic {
        format!(
            "{why} It was picked automatically because no usable default model is set, and no other available model fits. Add a provider with a larger model (/login), or pass --model <provider>/<id>."
        )
    } else {
        format!(
            "{why} Choose a model with a larger context window (--model <provider>/<id>, or /model)."
        )
    }
}

/// `canStartOn`: `Ok` starts; a budget refusal (`Err(true)`) does not; any other error propagates.
pub fn can_start_on<E>(
    assert_start_usable: impl FnOnce() -> Result<(), (bool, E)>,
) -> Result<bool, E> {
    match assert_start_usable() {
        Ok(()) => Ok(true),
        Err((true, _)) => Ok(false),
        Err((false, error)) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_default_selection::tests::{Ambient, model};

    #[test]
    fn only_provider_default_and_first_available_are_automatic() {
        assert!(is_automatic_pick(Some(
            InitialModelProvenance::ProviderDefault
        )));
        assert!(is_automatic_pick(Some(
            InitialModelProvenance::FirstAvailable
        )));
        assert!(!is_automatic_pick(Some(InitialModelProvenance::Cli)));
        assert!(!is_automatic_pick(None));
    }

    #[test]
    fn the_next_startable_model_skips_refused_candidates() {
        let models = vec![model("a", "small"), model("a", "tiny"), model("b", "large")];
        let next =
            next_startable_model(&models, &models[0], |m| m.id == "large", &[], &Ambient(&[]));
        assert_eq!(next.map(|m| m.id), Some("large".to_owned()));
        assert!(next_startable_model(&models, &models[0], |_| false, &[], &Ambient(&[])).is_none());
    }

    #[test]
    fn messages_match_the_pinned_source() {
        let refused = StartupRefusal {
            model: "a/small".into(),
            context_window: 8000.0,
            required_tokens: 12000.0,
        };
        assert_eq!(
            startup_replacement_message(&model("b", "large"), &refused),
            "No usable default model is set, so b/large was chosen: a/small has a 8000-token context window and needs 12000 to start. Use /model to pick another, or set a default model."
        );
        assert!(startup_refusal_message(&refused, false).ends_with(
            "Choose a model with a larger context window (--model <provider>/<id>, or /model)."
        ));
        assert!(startup_refusal_message(&refused, true).contains("It was picked automatically"));
    }

    #[test]
    fn only_budget_refusals_are_swallowed() {
        assert_eq!(can_start_on::<String>(|| Ok(())), Ok(true));
        assert_eq!(
            can_start_on::<String>(|| Err((true, "budget".into()))),
            Ok(false)
        );
        assert_eq!(
            can_start_on::<String>(|| Err((false, "boom".into()))),
            Err("boom".into())
        );
    }
}
