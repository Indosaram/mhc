//! `runners/pinned-model-equivalence.ts`: when a started model IS the pinned one.
//!
//! A catalog priority alias (`<base>-fast`) is not a different SKU when the registry pairs it with
//! its base through the service tier and the upstream id; every other substitution is a different
//! model and must be refused before a turn runs on it.

use serde::{Deserialize, Serialize};

/// `ModelIdentity`: what a session reports it started on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelIdentity {
    pub provider: String,
    pub id: String,
}

impl ModelIdentity {
    pub fn new(provider: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            id: id.into(),
        }
    }
}

/// `CatalogModelIdentity`: an identity carrying the alias-pairing fields of a catalog entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogModelIdentity {
    pub provider: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_model_id: Option<String>,
}

impl CatalogModelIdentity {
    pub fn new(provider: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            id: id.into(),
            service_tier: None,
            upstream_model_id: None,
        }
    }

    pub fn identity(&self) -> ModelIdentity {
        ModelIdentity::new(self.provider.clone(), self.id.clone())
    }
}

/// `EffectiveModel`: the identity plus the tier the SESSION reported (never the catalog's request).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveModel {
    pub provider: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
}

impl EffectiveModel {
    pub fn identity(&self) -> ModelIdentity {
        ModelIdentity::new(self.provider.clone(), self.id.clone())
    }

    /// `provider/id`, the spelling a failure message uses.
    pub fn reference(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }
}

/// The alias spelling is shared by catalog validation, lookup gating and rendering.
pub fn priority_alias_base_id(id: &str) -> Option<&str> {
    id.strip_suffix("-fast")
}

/// The two ids differ ONLY by the priority-alias suffix, in either direction.
pub fn differs_only_by_priority_alias(started: &ModelIdentity, pinned: &ModelIdentity) -> bool {
    started.provider == pinned.provider
        && started.id != pinned.id
        && (priority_alias_base_id(&started.id) == Some(pinned.id.as_str())
            || priority_alias_base_id(&pinned.id) == Some(started.id.as_str()))
}

/// senpi's own pairing accessors (`ModelRegistry.getServiceTier` / `getUpstreamModelId`).
///
/// `has_pairing_accessors` is the Rust spelling of the TypeScript "both methods are defined" test:
/// a registry that only knows the entry's own fields answers `false`, and the pairing is then read
/// from the entry.
pub trait PriorityPairingRegistry: Send + Sync {
    fn get_service_tier(&self, model: &ModelIdentity) -> Option<String>;
    fn get_upstream_model_id(&self, model: &ModelIdentity) -> Option<String>;

    fn has_pairing_accessors(&self) -> bool {
        true
    }
}

/// The pairing facts read the way senpi reads them: the registry when it has both accessors, else
/// the entry's own fields.
fn alias_pairing(
    entry: &CatalogModelIdentity,
    registry: Option<&dyn PriorityPairingRegistry>,
) -> (Option<String>, Option<String>) {
    match registry {
        Some(registry) if registry.has_pairing_accessors() => (
            registry.get_service_tier(&entry.identity()),
            registry.get_upstream_model_id(&entry.identity()),
        ),
        _ => (entry.service_tier.clone(), entry.upstream_model_id.clone()),
    }
}

/// A catalog priority alias, not a different SKU that happens to end in `-fast`.
pub fn is_priority_alias_of(
    entry: Option<&CatalogModelIdentity>,
    base: &ModelIdentity,
    registry: Option<&dyn PriorityPairingRegistry>,
) -> bool {
    let Some(entry) = entry else {
        return false;
    };
    if entry.provider != base.provider
        || priority_alias_base_id(&entry.id) != Some(base.id.as_str())
    {
        return false;
    }
    let (service_tier, upstream_model_id) = alias_pairing(entry, registry);
    service_tier.as_deref() == Some("priority")
        && upstream_model_id.as_deref() == Some(base.id.as_str())
}

/// The alias entry is the pairing proof in whichever direction the engine started.
pub fn started_on_pinned_model(
    started: &ModelIdentity,
    pinned: &ModelIdentity,
    pinned_entry: Option<&CatalogModelIdentity>,
    registry: Option<&dyn PriorityPairingRegistry>,
) -> bool {
    if started.provider != pinned.provider {
        return false;
    }
    if started.id == pinned.id {
        return true;
    }
    // The started model is itself an alias entry: read ITS pairing (the same entry shape). It
    // carries no catalog fields of its own, so without a registry there is nothing to pair with.
    let started_entry = CatalogModelIdentity {
        provider: started.provider.clone(),
        id: started.id.clone(),
        service_tier: None,
        upstream_model_id: None,
    };
    is_priority_alias_of(Some(&started_entry), pinned, registry)
        || is_priority_alias_of(pinned_entry, started, registry)
}

/// The effective tier comes from the session, never the catalog's requested tier.
pub fn reported_effective_model(
    model: Option<&ModelIdentity>,
    service_tier: Option<&str>,
) -> Option<EffectiveModel> {
    model.map(|model| EffectiveModel {
        provider: model.provider.clone(),
        id: model.id.clone(),
        service_tier: service_tier.map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        provider: &str,
        id: &str,
        tier: Option<&str>,
        upstream: Option<&str>,
    ) -> CatalogModelIdentity {
        CatalogModelIdentity {
            provider: provider.to_string(),
            id: id.to_string(),
            service_tier: tier.map(str::to_string),
            upstream_model_id: upstream.map(str::to_string),
        }
    }

    #[test]
    fn alias_base_strips_only_the_priority_suffix() {
        assert_eq!(priority_alias_base_id("claude-fast"), Some("claude"));
        assert_eq!(priority_alias_base_id("claude"), None);
        assert_eq!(priority_alias_base_id("-fast"), Some(""));
        assert_eq!(priority_alias_base_id("fast"), None);
    }

    #[test]
    fn differs_only_by_priority_alias_checks_both_directions() {
        let base = ModelIdentity::new("anthropic", "claude");
        assert!(differs_only_by_priority_alias(
            &ModelIdentity::new("anthropic", "claude-fast"),
            &base
        ));
        assert!(differs_only_by_priority_alias(
            &base,
            &ModelIdentity::new("anthropic", "claude-fast")
        ));
        assert!(!differs_only_by_priority_alias(
            &ModelIdentity::new("openai", "claude-fast"),
            &base
        ));
        assert!(!differs_only_by_priority_alias(&base, &base));
    }

    #[test]
    fn entry_fields_prove_the_pairing_without_a_registry() {
        let base = ModelIdentity::new("anthropic", "claude");
        assert!(is_priority_alias_of(
            Some(&entry(
                "anthropic",
                "claude-fast",
                Some("priority"),
                Some("claude")
            )),
            &base,
            None
        ));
        assert!(!is_priority_alias_of(
            Some(&entry(
                "anthropic",
                "claude-fast",
                Some("standard"),
                Some("claude")
            )),
            &base,
            None
        ));
        assert!(!is_priority_alias_of(
            Some(&entry(
                "anthropic",
                "claude-fast",
                Some("priority"),
                Some("other")
            )),
            &base,
            None
        ));
        assert!(!is_priority_alias_of(None, &base, None));
    }

    struct Registry {
        tier: Option<String>,
        upstream: Option<String>,
        accessors: bool,
    }

    impl PriorityPairingRegistry for Registry {
        fn get_service_tier(&self, _model: &ModelIdentity) -> Option<String> {
            self.tier.clone()
        }

        fn get_upstream_model_id(&self, _model: &ModelIdentity) -> Option<String> {
            self.upstream.clone()
        }

        fn has_pairing_accessors(&self) -> bool {
            self.accessors
        }
    }

    #[test]
    fn registry_accessors_win_over_entry_fields() {
        let base = ModelIdentity::new("anthropic", "claude");
        let registry = Registry {
            tier: Some("priority".to_string()),
            upstream: Some("claude".to_string()),
            accessors: true,
        };
        // The entry itself carries no pairing fields: only the registry can prove it.
        let bare = entry("anthropic", "claude-fast", None, None);
        assert!(is_priority_alias_of(Some(&bare), &base, Some(&registry)));
        let without = Registry {
            tier: Some("priority".to_string()),
            upstream: Some("claude".to_string()),
            accessors: false,
        };
        assert!(!is_priority_alias_of(Some(&bare), &base, Some(&without)));
    }

    #[test]
    fn started_on_pinned_model_accepts_the_alias_direction_the_engine_chose() {
        let base = ModelIdentity::new("anthropic", "claude");
        let alias = ModelIdentity::new("anthropic", "claude-fast");
        let registry = Registry {
            tier: Some("priority".to_string()),
            upstream: Some("claude".to_string()),
            accessors: true,
        };
        // Started on the alias, pinned the base: the started id pairs with the pin.
        assert!(started_on_pinned_model(
            &alias,
            &base,
            None,
            Some(&registry)
        ));
        // Started on the base, pinned the alias: the pinned entry pairs with the started id.
        assert!(started_on_pinned_model(
            &base,
            &alias,
            Some(&CatalogModelIdentity::new("anthropic", "claude-fast")),
            Some(&registry)
        ));
        // A different provider never matches.
        assert!(!started_on_pinned_model(
            &ModelIdentity::new("openai", "claude"),
            &base,
            None,
            None
        ));
        // An unrelated model is refused.
        assert!(!started_on_pinned_model(
            &ModelIdentity::new("anthropic", "opus"),
            &base,
            None,
            None
        ));
    }

    #[test]
    fn reported_effective_model_carries_the_session_tier() {
        let model = ModelIdentity::new("anthropic", "claude");
        assert_eq!(
            reported_effective_model(Some(&model), Some("priority")),
            Some(EffectiveModel {
                provider: "anthropic".to_string(),
                id: "claude".to_string(),
                service_tier: Some("priority".to_string()),
            })
        );
        assert_eq!(
            reported_effective_model(Some(&model), None).and_then(|model| model.service_tier),
            None
        );
        assert_eq!(reported_effective_model(None, Some("priority")), None);
    }
}
