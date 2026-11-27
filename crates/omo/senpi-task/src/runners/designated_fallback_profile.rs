//! Designated fallback profile and chain formatting (`runners/designated-fallback-profile.ts`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use omo_config_core::schema::reasoning_vocabulary::split_reasoning_suffix;

/// Wire/settings retry fallback profile applied per child session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRetryFallbackProfile {
    #[serde(rename = "modelFallback")]
    pub model_fallback: bool,
    #[serde(rename = "fallbackChains")]
    pub fallback_chains: BTreeMap<String, Vec<String>>,
}

/// The designated models in try order, as a refusal names them: `a -> b -> c`.
#[must_use]
pub fn designated_chain_text(model: &str, fallback_models: &[String]) -> String {
    if fallback_models.is_empty() {
        model.to_owned()
    } else {
        format!("{model} -> {}", fallback_models.join(" -> "))
    }
}

/// The designated models a profile from [`designated_fallback_profile`] holds, in try order.
#[must_use]
pub fn designated_chain_of(profile: &HostRetryFallbackProfile) -> String {
    let found = profile
        .fallback_chains
        .iter()
        .find(|(_, chain)| !chain.is_empty())
        .or_else(|| profile.fallback_chains.iter().next());
    match found {
        Some((model, fallbacks)) => designated_chain_text(model, fallbacks),
        None => String::new(),
    }
}

/// The fallback policy that holds a child to exactly its designated models (#12006 / #11781): its
/// own chain keyed by its model, and an empty chain (senpi's tombstone) for every designated
/// model's base, so neither senpi's shipped default chains nor anything else can extend the walk.
#[must_use]
pub fn designated_fallback_profile(
    model: &str,
    fallback_models: &[String],
) -> HostRetryFallbackProfile {
    let mut chains = BTreeMap::new();
    for selector in std::iter::once(model).chain(fallback_models.iter().map(String::as_str)) {
        let base = split_reasoning_suffix(selector, Some(true)).base;
        if base != model {
            chains.insert(base, Vec::new());
        }
    }
    chains.insert(model.to_owned(), fallback_models.to_vec());
    HostRetryFallbackProfile {
        model_fallback: !fallback_models.is_empty(),
        fallback_chains: chains,
    }
}
