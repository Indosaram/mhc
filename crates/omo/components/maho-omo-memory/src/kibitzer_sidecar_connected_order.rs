//! Connected-first ordering of the resident Kibitzer's category candidates (#9216).
//!
//! Category resolution follows the `task` tool's rule that a user pin wins: when no pinned model is
//! connected it still returns the first pin, because the catalog knows it. A delegated task can let
//! the user see that failure; the sidecar child cannot - it fails its auth check at once and never
//! reaches the connected rung it carries as a fallback. So, only here and only while the registry's
//! availability list is known and non-empty, the first CONNECTED candidate leads: pinned models in pin
//! order, then the category's builtin chain. Unconnected candidates stay reachable behind them (an
//! extension provider can register after the snapshot) but never lead. An empty or malformed list is
//! the stale first-turn snapshot, not an answer, and leaves the resolution as it was.
//!
//! # Deviations (each an N/A, because the Rust crate has no direct counterpart)
//!
//! * The TypeScript `registry.getAvailable()` / `resolveCategory(...)` THROW on failure; the Rust
//!   `SenpiModelRegistry::get_available` / `resolve_category` return `Result<_, HostError>`, so this
//!   function is `Result<_, HostError>` and propagates with `?` exactly where upstream would throw.
//! * `ReflectionThinkingLevel` (a TypeScript string union) is the `Option<String>` already carried by
//!   `ReflectionModelCandidate::thinking`; `OmoConfig` is the crate's `serde_json::Value`.
//! * `parseAvailableModels` returns `{ validContainer, models }`; the Rust `parse_available_models`
//!   returns `Option<Vec<String>>` (`None` for a non-array), so `None` OR an empty vector is the
//!   availability-unknown snapshot the port returns as [`KibitzerCandidateOrder::AvailabilityUnknown`].

use std::collections::BTreeSet;

use serde_json::Value;
use senpi_task::category::{CategoryResolutionResult, ResolveCategoryOptions, resolve_category};
use senpi_task::host::{HostError, SenpiModelRegistry, parse_available_models};

use crate::worker::memory_model_attempts::ReflectionModelCandidate;

/// The ordering request (`KibitzerCandidateOrderInput`): the pinned category, the omo config, the
/// live registry snapshot, the resolved lead model, its thinking level and the fallback candidates.
pub struct KibitzerCandidateOrderInput<'a> {
    pub category: &'a str,
    pub config: &'a Value,
    pub registry: &'a dyn SenpiModelRegistry,
    pub model: &'a str,
    pub thinking: Option<&'a str>,
    pub fallbacks: &'a [ReflectionModelCandidate],
}

/// The ordering verdict (`KibitzerCandidateOrder`).
#[derive(Debug, Clone)]
pub enum KibitzerCandidateOrder {
    /// The availability list was empty or malformed: the stale first-turn snapshot, not an answer.
    AvailabilityUnknown,
    /// The first connected candidate leads; the remaining candidates follow in order.
    Ordered {
        model: String,
        thinking: Option<String>,
        fallbacks: Vec<ReflectionModelCandidate>,
    },
    /// No candidate is connected; the providers a `/login` would revive, deduped and ordered.
    NoneConnected { missing_providers: Vec<String> },
}

/// `orderKibitzerCandidatesByConnection`: leads with the first CONNECTED candidate when the
/// availability list is known and non-empty, leaving the resolution untouched otherwise.
pub fn order_kibitzer_candidates_by_connection(
    input: &KibitzerCandidateOrderInput<'_>,
) -> Result<KibitzerCandidateOrder, HostError> {
    let available = match parse_available_models(&input.registry.get_available()?) {
        Some(models) if !models.is_empty() => models,
        _ => return Ok(KibitzerCandidateOrder::AvailabilityUnknown),
    };
    let connected: BTreeSet<String> = available.into_iter().collect();
    let mut candidates: Vec<ReflectionModelCandidate> = Vec::with_capacity(1 + input.fallbacks.len());
    candidates.push(ReflectionModelCandidate {
        model: input.model.to_string(),
        thinking: input.thinking.map(str::to_string),
    });
    candidates.extend(input.fallbacks.iter().cloned());
    let leading: Vec<ReflectionModelCandidate> = candidates
        .iter()
        .filter(|candidate| connected.contains(&candidate.model))
        .cloned()
        .collect();
    let trailing: Vec<ReflectionModelCandidate> = candidates
        .iter()
        .filter(|candidate| !connected.contains(&candidate.model))
        .cloned()
        .collect();
    let mut ordered = leading.clone();
    ordered.extend(trailing.iter().cloned());
    // `lead === undefined || leading.length === 0`: no candidate at all, or none connected.
    if ordered.is_empty() || leading.is_empty() {
        return Ok(KibitzerCandidateOrder::NoneConnected {
            missing_providers: missing_providers(input, &trailing)?,
        });
    }
    let lead = ordered[0].clone();
    Ok(KibitzerCandidateOrder::Ordered {
        model: lead.model,
        thinking: lead.thinking,
        fallbacks: ordered[1..].to_vec(),
    })
}

/// The providers a `/login` would revive: the unconnected pins' own providers in pin order, then the
/// builtin chain's (asked without the user's pin, which otherwise short-circuits the chain report).
fn missing_providers(
    input: &KibitzerCandidateOrderInput<'_>,
    unconnected: &[ReflectionModelCandidate],
) -> Result<Vec<String>, HostError> {
    let mut providers: Vec<String> = unconnected
        .iter()
        .map(|candidate| provider_of(&candidate.model))
        .collect();
    // `{ [input.category]: _pin, ...otherCategories }`: the pinned category is dropped so the chain
    // is reported for the category itself, not short-circuited by the user's pin.
    let mut config = input.config.clone();
    if let Some(object) = config.as_object_mut() {
        let mut categories = object
            .get("categories")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        categories.remove(input.category);
        object.insert("categories".to_string(), Value::Object(categories));
    }
    let chain = resolve_category(
        input.category,
        &config,
        input.registry,
        &ResolveCategoryOptions::default(),
    )?;
    if let CategoryResolutionResult::ModelUnavailable(unavailable) = chain
        && let Some(missing) = unavailable.missing_providers
    {
        providers.extend(missing);
    }
    let mut seen = BTreeSet::new();
    Ok(providers
        .into_iter()
        .filter(|provider| !provider.is_empty() && seen.insert(provider.clone()))
        .collect())
}

/// JS `model.slice(0, model.indexOf("/"))`: the provider prefix. When no separator exists, `indexOf`
/// is `-1` and `slice(0, -1)` drops the final UTF-16 unit - reproduced exactly, in UTF-16 units.
fn provider_of(model: &str) -> String {
    match model.find('/') {
        Some(index) => model[..index].to_string(),
        None => {
            let mut units: Vec<u16> = model.encode_utf16().collect();
            units.pop();
            String::from_utf16_lossy(&units)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A real-contract registry fake: `get_available` answers from the snapshot it was built with
    /// and `find` is unused by the ordering (which only reads the availability list).
    struct FakeRegistry {
        available: Value,
    }

    impl SenpiModelRegistry for FakeRegistry {
        fn get_available(&self) -> Result<Value, HostError> {
            Ok(self.available.clone())
        }
        fn find(&self, _provider: &str, _model_id: &str) -> Option<Value> {
            None
        }
    }

    fn registry(available: Value) -> FakeRegistry {
        FakeRegistry { available }
    }

    fn candidate(model: &str, thinking: Option<&str>) -> ReflectionModelCandidate {
        ReflectionModelCandidate { model: model.to_string(), thinking: thinking.map(str::to_string) }
    }

    fn models(entries: &[(&str, &str)]) -> Value {
        Value::Array(entries.iter().map(|(provider, id)| json!({ "provider": provider, "id": id })).collect())
    }

    #[test]
    fn given_a_malformed_availability_container_when_ordered_then_availability_is_unknown() {
        let reg = registry(json!({ "oops": true }));
        let order = order_kibitzer_candidates_by_connection(&KibitzerCandidateOrderInput {
            category: "quick",
            config: &json!({}),
            registry: &reg,
            model: "anthropic/pinned",
            thinking: None,
            fallbacks: &[],
        })
        .expect("order");
        assert!(matches!(order, KibitzerCandidateOrder::AvailabilityUnknown));
    }

    #[test]
    fn given_an_empty_availability_list_when_ordered_then_availability_is_unknown() {
        let reg = registry(json!([]));
        let order = order_kibitzer_candidates_by_connection(&KibitzerCandidateOrderInput {
            category: "quick",
            config: &json!({}),
            registry: &reg,
            model: "anthropic/pinned",
            thinking: Some("high"),
            fallbacks: &[candidate("google/a", None)],
        })
        .expect("order");
        assert!(matches!(order, KibitzerCandidateOrder::AvailabilityUnknown));
    }

    #[test]
    fn given_connected_and_disconnected_candidates_when_ordered_then_connected_lead_keeps_order_and_thinking() {
        let reg = registry(models(&[("google", "a"), ("anthropic", "b")]));
        let fallbacks = [candidate("google/a", Some("medium")), candidate("anthropic/b", None)];
        let order = order_kibitzer_candidates_by_connection(&KibitzerCandidateOrderInput {
            category: "quick",
            config: &json!({}),
            registry: &reg,
            model: "anthropic/pinned",
            thinking: Some("high"),
            fallbacks: &fallbacks,
        })
        .expect("order");
        match order {
            KibitzerCandidateOrder::Ordered { model, thinking, fallbacks } => {
                assert_eq!(model, "google/a", "the first CONNECTED candidate leads");
                assert_eq!(thinking.as_deref(), Some("medium"));
                assert_eq!(fallbacks.len(), 2);
                assert_eq!(fallbacks[0].model, "anthropic/b");
                assert_eq!(fallbacks[0].thinking, None);
                assert_eq!(fallbacks[1].model, "anthropic/pinned", "the disconnected pin stays reachable behind");
                assert_eq!(fallbacks[1].thinking.as_deref(), Some("high"));
            }
            other => panic!("expected ordered, got {other:?}"),
        }
    }

    #[test]
    fn given_a_connected_pin_when_ordered_then_the_pin_leads_with_its_thinking() {
        let reg = registry(models(&[("anthropic", "pinned"), ("google", "a")]));
        let fallbacks = [candidate("google/a", Some("low")), candidate("openai/c", None)];
        let order = order_kibitzer_candidates_by_connection(&KibitzerCandidateOrderInput {
            category: "quick",
            config: &json!({}),
            registry: &reg,
            model: "anthropic/pinned",
            thinking: Some("high"),
            fallbacks: &fallbacks,
        })
        .expect("order");
        match order {
            KibitzerCandidateOrder::Ordered { model, thinking, fallbacks } => {
                assert_eq!(model, "anthropic/pinned");
                assert_eq!(thinking.as_deref(), Some("high"));
                let models: Vec<&str> = fallbacks.iter().map(|candidate| candidate.model.as_str()).collect();
                assert_eq!(models, ["google/a", "openai/c"]);
                assert_eq!(fallbacks[0].thinking.as_deref(), Some("low"));
                assert_eq!(fallbacks[1].thinking, None);
            }
            other => panic!("expected ordered, got {other:?}"),
        }
    }

    #[test]
    fn given_no_connected_candidate_when_ordered_then_missing_providers_are_pinned_then_builtin_deduped() {
        // A non-empty availability list that connects nothing here: the ordering refuses and reports
        // the providers a `/login` would revive - the pin's own provider first, then the builtin
        // chain's (asked with the pinned category removed), deduped first-wins.
        let reg = registry(models(&[("omo-mock", "mock-1")]));
        let order = order_kibitzer_candidates_by_connection(&KibitzerCandidateOrderInput {
            category: "quick",
            config: &json!({}),
            registry: &reg,
            model: "anthropic/pinned",
            thinking: None,
            fallbacks: &[],
        })
        .expect("order");
        match order {
            KibitzerCandidateOrder::NoneConnected { missing_providers } => {
                assert_eq!(missing_providers.first().map(String::as_str), Some("anthropic"), "the pin's own provider leads");
                assert!(
                    missing_providers.iter().any(|provider| provider == "kimi-coding"),
                    "the unpinned builtin chain contributes its providers"
                );
                assert!(
                    !missing_providers.iter().any(|provider| provider == "omo-mock"),
                    "a connected provider is never reported missing"
                );
                assert_eq!(
                    missing_providers.iter().filter(|provider| provider.as_str() == "anthropic").count(),
                    1,
                    "the pin's provider (also in the chain) appears once, at the front (first-wins dedupe)"
                );
                let unique: BTreeSet<&String> = missing_providers.iter().collect();
                assert_eq!(unique.len(), missing_providers.len(), "missing providers are deduped");
            }
            other => panic!("expected none-connected, got {other:?}"),
        }
    }
}
