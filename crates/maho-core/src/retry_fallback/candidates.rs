//! Port of senpi packages/coding-agent/src/core/retry-fallback/candidates.ts.

use std::collections::HashSet;

use maho_ai::models::clamp_thinking_level;
use maho_ai::types::{Model, ModelThinkingLevel};

use super::chains::{
    FallbackSelector, base_selector, candidates_after, format_selector, parse_fallback_selector,
};

/// senpi `rungSelector`: the base selector plus an explicit level, when the entry names one.
pub fn rung_selector(
    provider: &str,
    id: &str,
    thinking_level: Option<ModelThinkingLevel>,
) -> String {
    let base = format!("{provider}/{id}");
    thinking_level.map_or_else(
        || base.clone(),
        |level| format!("{base}:{}", level.as_str()),
    )
}

pub struct CandidateFilters<'a> {
    pub models: &'a [Model],
    pub tried: &'a HashSet<String>,
    pub is_suppressed: &'a mut dyn FnMut(&str) -> bool,
    pub is_auth_available: &'a dyn Fn(&str) -> bool,
    pub is_circuit_open: &'a dyn Fn(&str) -> bool,
    /// A model whose every rung is out for this walk (a network blip on it, senpi#3344).
    pub is_model_skipped: Option<&'a dyn Fn(&str) -> bool>,
    /// Providers whose account-wide usage limit or billing failure this session saw: their entries are skipped.
    pub is_provider_spent: Option<&'a dyn Fn(&str) -> bool>,
    pub skip: &'a mut dyn FnMut(&str, &str),
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsableCandidate {
    pub selector: FallbackSelector,
    pub model: Model,
    /// True only when every usable entry is circuit-open: the chain then probes instead of refusing.
    pub circuit_open: bool,
}

pub struct CurrentRung<'a> {
    pub model: &'a Model,
    pub thinking_level: Option<ModelThinkingLevel>,
}

pub fn first_usable_candidate(
    entries: &[String],
    current: &CurrentRung<'_>,
    filters: &mut CandidateFilters<'_>,
) -> Option<UsableCandidate> {
    let spent = filters.is_provider_spent;
    scan_candidates(entries, current, filters, &|provider| {
        spent.is_some_and(|spent| spent(provider))
    })
}

/// Whether an entry names the rung already in flight: the same model, and either no explicit level (it
/// inherits the running one) or the running level itself. The same model at another explicit level is a
/// rung of its own and stays a candidate.
fn is_current_rung(selector: &FallbackSelector, current: &CurrentRung<'_>) -> bool {
    if selector.provider != current.model.provider || selector.id != current.model.id {
        return false;
    }
    selector.thinking_level.is_none() || selector.thinking_level == current.thinking_level
}

fn scan_candidates(
    entries: &[String],
    current: &CurrentRung<'_>,
    filters: &mut CandidateFilters<'_>,
    avoid_provider: &dyn Fn(&str) -> bool,
) -> Option<UsableCandidate> {
    let mut probe: Option<UsableCandidate> = None;
    let running = format_selector(current.model, current.thinking_level);
    for raw in candidates_after(entries, &running) {
        let Some(selector) = parse_fallback_selector(raw, filters.models) else {
            (filters.skip)(raw, "unknown");
            continue;
        };
        if is_current_rung(&selector, current) {
            (filters.skip)(raw, "self");
            continue;
        }
        if avoid_provider(&selector.provider) {
            (filters.skip)(raw, "account-limit");
            continue;
        }
        let base = base_selector(&selector);
        if filters
            .is_model_skipped
            .is_some_and(|skipped| skipped(&base))
        {
            (filters.skip)(raw, "network-blip");
            continue;
        }
        if filters.tried.contains(&rung_selector(
            &selector.provider,
            &selector.id,
            selector.thinking_level,
        )) {
            (filters.skip)(raw, "tried");
            continue;
        }
        if (filters.is_suppressed)(&base) {
            (filters.skip)(raw, "suppressed");
            continue;
        }
        if !(filters.is_auth_available)(&selector.provider) {
            (filters.skip)(raw, "unauthenticated");
            continue;
        }
        let Some(model) = filters
            .models
            .iter()
            .find(|m| m.provider == selector.provider && m.id == selector.id)
        else {
            (filters.skip)(raw, "unknown");
            continue;
        };
        // A hard-error park is keyed by the level the rung ran at, so check the level this entry would run
        // at: its own, or the inherited one, clamped like the switch does (senpi#3311).
        let level = clamp_thinking_level(
            model,
            selector
                .thinking_level
                .or(current.thinking_level)
                .unwrap_or(ModelThinkingLevel::Off),
        );
        // An explicit level that clamps to the running one is the rung in flight (senpi#3321).
        if base == format_selector(current.model, None)
            && level == current.thinking_level.unwrap_or(ModelThinkingLevel::Off)
        {
            (filters.skip)(raw, "self");
            continue;
        }
        if (filters.is_suppressed)(&rung_selector(
            &selector.provider,
            &selector.id,
            Some(level),
        )) {
            (filters.skip)(raw, "suppressed");
            continue;
        }
        if (filters.is_circuit_open)(&base) {
            (filters.skip)(raw, "circuit-open");
            if probe.is_none() {
                probe = Some(UsableCandidate {
                    selector,
                    model: model.clone(),
                    circuit_open: true,
                });
            }
            continue;
        }
        return Some(UsableCandidate {
            selector,
            model: model.clone(),
            circuit_open: false,
        });
    }
    probe
}

#[cfg(test)]
mod tests {
    use super::*;

    fn models() -> Vec<Model> {
        let template = maho_ai::providers::all::get_builtin_models("openai").remove(0);
        ["a", "b", "c"]
            .into_iter()
            .map(|id| {
                let mut model = template.clone();
                model.provider = "p".into();
                model.id = id.into();
                model
            })
            .chain(std::iter::once({
                let mut model = template.clone();
                model.provider = "q".into();
                model.id = "d".into();
                model
            }))
            .collect()
    }

    struct Probe {
        skipped: Vec<(String, String)>,
    }

    fn scan(
        entries: &[&str],
        tried: &[&str],
        suppressed: &[&str],
        circuit_open: &[&str],
        spent: &[&str],
        probe: &mut Probe,
    ) -> Option<UsableCandidate> {
        let models = models();
        let entries: Vec<String> = entries.iter().map(|e| (*e).to_owned()).collect();
        let tried: HashSet<String> = tried.iter().map(|e| (*e).to_owned()).collect();
        let suppressed: Vec<String> = suppressed.iter().map(|e| (*e).to_owned()).collect();
        let circuit_open: Vec<String> = circuit_open.iter().map(|e| (*e).to_owned()).collect();
        let spent: Vec<String> = spent.iter().map(|e| (*e).to_owned()).collect();
        let mut is_suppressed = |base: &str| suppressed.iter().any(|s| s == base);
        let is_circuit_open = |base: &str| circuit_open.iter().any(|s| s == base);
        let is_provider_spent = |provider: &str| spent.iter().any(|s| s == provider);
        let mut skip =
            |raw: &str, reason: &str| probe.skipped.push((raw.to_owned(), reason.to_owned()));
        let mut filters = CandidateFilters {
            models: &models,
            tried: &tried,
            is_suppressed: &mut is_suppressed,
            is_auth_available: &|_| true,
            is_circuit_open: &is_circuit_open,
            is_model_skipped: None,
            is_provider_spent: Some(&is_provider_spent),
            skip: &mut skip,
        };
        let current = CurrentRung {
            model: &models[0],
            thinking_level: None,
        };
        first_usable_candidate(&entries, &current, &mut filters)
    }

    #[test]
    fn returns_the_first_usable_entry_after_the_running_one() {
        let mut probe = Probe {
            skipped: Vec::new(),
        };
        let found = scan(&["p/a", "p/b", "p/c"], &[], &[], &[], &[], &mut probe);
        assert_eq!(
            found.map(|c| (c.model.id, c.circuit_open)),
            Some(("b".to_owned(), false))
        );
    }

    #[test]
    fn skips_tried_suppressed_and_spent_entries_with_their_reasons() {
        let mut probe = Probe {
            skipped: Vec::new(),
        };
        let found = scan(
            &["q/d", "p/b", "p/c"],
            &["p/b"],
            &[],
            &[],
            &["q"],
            &mut probe,
        );
        assert_eq!(found.map(|c| c.model.id), Some("c".to_owned()));
        assert_eq!(
            probe.skipped,
            vec![
                ("q/d".to_owned(), "account-limit".to_owned()),
                ("p/b".to_owned(), "tried".to_owned())
            ]
        );
        let mut probe = Probe {
            skipped: Vec::new(),
        };
        assert_eq!(scan(&["p/b"], &[], &["p/b"], &[], &[], &mut probe), None);
        assert_eq!(
            probe.skipped,
            vec![("p/b".to_owned(), "suppressed".to_owned())]
        );
    }

    #[test]
    fn probes_a_circuit_open_entry_only_when_every_usable_entry_is_open() {
        let mut probe = Probe {
            skipped: Vec::new(),
        };
        let found = scan(&["p/b", "p/c"], &[], &[], &["p/b", "p/c"], &[], &mut probe);
        assert_eq!(
            found.map(|c| (c.model.id, c.circuit_open)),
            Some(("b".to_owned(), true))
        );
        let mut probe = Probe {
            skipped: Vec::new(),
        };
        let found = scan(&["p/b", "p/c"], &[], &[], &["p/b"], &[], &mut probe);
        assert_eq!(
            found.map(|c| (c.model.id, c.circuit_open)),
            Some(("c".to_owned(), false))
        );
    }

    #[test]
    fn treats_the_running_model_as_self_and_unknown_entries_as_unknown() {
        let mut probe = Probe {
            skipped: Vec::new(),
        };
        assert_eq!(scan(&["x/none"], &[], &[], &[], &[], &mut probe), None);
        assert_eq!(
            probe.skipped,
            vec![("x/none".to_owned(), "unknown".to_owned())]
        );
    }
}
