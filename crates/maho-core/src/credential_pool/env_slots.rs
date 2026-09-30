//! Port of senpi packages/coding-agent/src/core/credential-pool/env-slots.ts.

use crate::credential_pool::slots::CredentialSlotSource;

pub const MAX_ENV_SLOT_INDEX: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvCredentialSlot {
    pub name: String,
    pub env_var_name: String,
    pub key: String,
    pub source: CredentialSlotSource,
}

/// Anthropic maps to bearer/OAuth token vars ahead of its API-key var; numbered slots must extend
/// the API-key lane, so the `*_API_KEY` entry wins when the canonical mapping lists several vars.
pub fn primary_env_var(provider_id: &str) -> Option<String> {
    if maho_ai::legacy_provider_ids::normalize_provider_id(provider_id) == "anthropic-subscription" {
        return Some("CLAUDE_CODE_OAUTH_TOKEN".to_owned());
    }
    let env_vars = maho_ai::env_api_keys::get_api_key_env_vars(provider_id)?;
    if env_vars.is_empty() {
        return None;
    }
    Some(
        env_vars
            .iter()
            .find(|name| name.ends_with("_API_KEY"))
            .map(|name| (*name).to_owned())
            .unwrap_or_else(|| env_vars[0].to_owned()),
    )
}

/// Discovers numbered env credential slots; discovery is gap-tolerant.
pub fn discover_env_slots(provider_id: &str, env: &dyn Fn(&str) -> Option<String>) -> Vec<EnvCredentialSlot> {
    let Some(primary) = primary_env_var(provider_id) else { return Vec::new() };
    let mut slots = Vec::new();
    if let Some(base) = env(&primary).filter(|value| !value.is_empty()) {
        slots.push(EnvCredentialSlot {
            name: "env".to_owned(),
            env_var_name: primary.clone(),
            key: base,
            source: CredentialSlotSource::Env,
        });
    }
    for index in 2..=MAX_ENV_SLOT_INDEX {
        let env_var_name = format!("{primary}_{index}");
        if let Some(value) = env(&env_var_name).filter(|value| !value.is_empty()) {
            slots.push(EnvCredentialSlot {
                name: format!("env-{index}"),
                env_var_name,
                key: value,
                source: CredentialSlotSource::Env,
            });
        }
    }
    slots
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name: &str| pairs.iter().find(|(key, _)| *key == name).map(|(_, value)| (*value).to_owned())
    }

    #[test]
    fn discovers_the_primary_and_numbered_slots_gap_tolerantly() {
        let env = env_of(&[("ANTHROPIC_API_KEY", "base"), ("ANTHROPIC_API_KEY_3", "third")]);
        let slots = discover_env_slots("anthropic", &env);
        assert_eq!(slots.iter().map(|slot| slot.name.as_str()).collect::<Vec<_>>(), vec!["env", "env-3"]);
        assert_eq!(slots[1].env_var_name, "ANTHROPIC_API_KEY_3");
        assert_eq!(slots[1].key, "third");
    }

    #[test]
    fn prefers_the_api_key_variable_and_maps_the_subscription_provider() {
        assert_eq!(primary_env_var("anthropic-subscription").as_deref(), Some("CLAUDE_CODE_OAUTH_TOKEN"));
        assert_eq!(primary_env_var("anthropic").as_deref(), Some("ANTHROPIC_API_KEY"));
        assert!(discover_env_slots("not-a-provider", &env_of(&[])).is_empty());
        assert!(discover_env_slots("anthropic", &env_of(&[])).is_empty());
    }
}
