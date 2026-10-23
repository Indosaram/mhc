//! Agreements side of the facts payload (pin `facts-agreements.ts`).
use memory_core::agreements::{
    AgreementsPolicy, DEFAULT_AGREEMENTS_POLICY, FactsKnownAgreement, agreement_items_of,
    known_agreements_of, read_agreements_worktree,
};

pub const FACTS_AGREEMENTS_FILENAME: &str = "agreements.jsonl";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FactsAgreementsSettings {
    pub enabled: bool,
    pub policy: AgreementsPolicy,
}

/// Agent override, then the global `memory.agreements`, then the defaults. `config` is the full omo config.
pub fn resolve_facts_agreements(
    config: &serde_json::Value,
    identity: &str,
) -> FactsAgreementsSettings {
    let memory = &config["memory"];
    let over = &memory["agents"][identity]["agreements"];
    let base = &memory["agreements"];
    let pick = |key: &str| over[key].as_u64().or_else(|| base[key].as_u64());
    FactsAgreementsSettings {
        enabled: over["enabled"]
            .as_bool()
            .or_else(|| base["enabled"].as_bool())
            .unwrap_or(true),
        policy: AgreementsPolicy {
            needs_human_threshold: pick("needs_human_threshold")
                .unwrap_or(DEFAULT_AGREEMENTS_POLICY.needs_human_threshold),
            max_items: pick("max_items")
                .map_or(DEFAULT_AGREEMENTS_POLICY.max_items, |value| value as usize),
        },
    }
}

/// `agreements` of the payload envelope; `None` (omitted) when the feature is off.
pub struct FactsAgreementsPayload {
    pub agreements: Option<Vec<FactsKnownAgreement>>,
}

pub fn read_facts_agreements_payload(
    repo_dir: &std::path::Path,
    settings: &FactsAgreementsSettings,
) -> FactsAgreementsPayload {
    if !settings.enabled {
        return FactsAgreementsPayload { agreements: None };
    }
    let existing = read_agreements_worktree(repo_dir).ok().flatten();
    FactsAgreementsPayload {
        agreements: Some(known_agreements_of(&agreement_items_of(
            existing.as_deref(),
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_beats_global_beats_defaults() {
        let config = serde_json::json!({"memory": {"agreements": {"enabled": true, "max_items": 10}, "agents": {"a": {"agreements": {"enabled": false, "needs_human_threshold": 5}}}}});
        let agent = resolve_facts_agreements(&config, "a");
        assert_eq!(
            (
                agent.enabled,
                agent.policy.needs_human_threshold,
                agent.policy.max_items
            ),
            (false, 5, 10)
        );
        let other = resolve_facts_agreements(&config, "b");
        assert_eq!(
            (
                other.enabled,
                other.policy.needs_human_threshold,
                other.policy.max_items
            ),
            (true, 3, 10)
        );
        assert_eq!(
            resolve_facts_agreements(&serde_json::json!({}), "a"),
            FactsAgreementsSettings {
                enabled: true,
                policy: DEFAULT_AGREEMENTS_POLICY
            }
        );
    }

    #[test]
    fn disabled_settings_omit_the_payload_field() {
        let dir = tempfile::tempdir().unwrap();
        let off = FactsAgreementsSettings {
            enabled: false,
            policy: DEFAULT_AGREEMENTS_POLICY,
        };
        assert!(
            read_facts_agreements_payload(dir.path(), &off)
                .agreements
                .is_none()
        );
        let on = FactsAgreementsSettings {
            enabled: true,
            policy: DEFAULT_AGREEMENTS_POLICY,
        };
        assert_eq!(
            read_facts_agreements_payload(dir.path(), &on).agreements,
            Some(vec![])
        );
    }
}
