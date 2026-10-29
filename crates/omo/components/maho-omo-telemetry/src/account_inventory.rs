//! Port of `packages/omo-senpi/src/components/telemetry/account-inventory.ts`.
//!
//! Linked accounts per provider kind: for each provider, the number of accounts the engine itself lists,
//! counts only. No account name, email, token, id or env value is kept: only lengths and presence.
//!
//! - Claude (`anthropic-subscription`): the stored `accounts` list (no list, the managed sign-in sentinel, is
//!   zero; an empty list is the flat credential, one), plus every env slot (`CLAUDE_CODE_OAUTH_TOKEN`,
//!   `_2` .. `_16`);
//! - any other provider: the stored `accounts` list when it is non-empty, else the flat credential as one
//!   account; env slots only when nothing is stored.

pub type GetApiKeyEnvVarsCallback = Arc<dyn Fn(&str) -> Option<Vec<String>> + Send + Sync>;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::Value;

use crate::model_vocabulary::KNOWN_PROVIDERS;
use crate::session_inventory::RETIRED_PROVIDER_IDS;

const CLAUDE: &str = "anthropic-subscription";
const MAX_ENV_SLOT_INDEX: u32 = 16;
const CAP: u64 = 10;

/// The engine functions the counting rules mirror.
#[derive(Clone)]
pub struct EngineTables {
    pub normalize_provider_id: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub get_api_key_env_vars: GetApiKeyEnvVarsCallback,
    pub api_key_env_providers: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
}

impl EngineTables {
    /// The live engine's tables; `None` when the host does not expose all three functions.
    pub fn load() -> Option<Self> {
        Some(Self {
            normalize_provider_id: Arc::new(maho_ai::legacy_provider_ids::normalize_provider_id),
            get_api_key_env_vars: Arc::new(|provider: &str| {
                maho_ai::env_api_keys::get_api_key_env_vars(provider)
                    .map(|names| names.into_iter().map(str::to_string).collect())
            }),
            api_key_env_providers: Arc::new(|| {
                maho_ai::env_api_keys::api_key_env_providers()
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            }),
        })
    }
}

/// `kind:count` list over the shipped provider kinds, unknown kinds summed into `custom`, each count capped
/// at `10_plus`.
pub fn read_connected_accounts(
    agent_dir: &Path,
    env: &telemetry_core::TelemetryEnv,
    tables: Option<&EngineTables>,
) -> String {
    let Some(tables) = tables else {
        return String::new();
    };
    let auth = read_auth(&agent_dir.join("auth.json"));
    let mut kinds: BTreeMap<String, u64> = BTreeMap::new();
    for (provider, count) in connected_account_counts(&auth, env, tables) {
        let kind = kind_of(&provider, tables);
        *kinds.entry(kind).or_insert(0) += count;
    }
    kinds
        .into_iter()
        .map(|(kind, count)| {
            format!(
                "{kind}:{}",
                if count >= CAP {
                    "10_plus".to_string()
                } else {
                    count.to_string()
                }
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

pub fn connected_account_counts(
    auth: &serde_json::Map<String, Value>,
    env: &telemetry_core::TelemetryEnv,
    tables: &EngineTables,
) -> BTreeMap<String, u64> {
    let mut stored: BTreeMap<String, &Value> = BTreeMap::new();
    for (key, value) in auth {
        let canonical = (tables.normalize_provider_id)(key);
        if !stored.contains_key(&canonical) || *key == canonical {
            stored.insert(canonical, value);
        }
    }
    let mut providers: Vec<String> = stored.keys().cloned().collect();
    for provider in (tables.api_key_env_providers)() {
        if !providers.contains(&provider) {
            providers.push(provider);
        }
    }
    if !providers.iter().any(|provider| provider == CLAUDE) {
        providers.push(CLAUDE.to_string());
    }
    let mut counts = BTreeMap::new();
    for provider in providers {
        let count = engine_account_count(&provider, stored.get(&provider).copied(), env, tables);
        if count > 0 {
            counts.insert(provider, count);
        }
    }
    counts
}

fn engine_account_count(
    provider: &str,
    credential: Option<&Value>,
    env: &telemetry_core::TelemetryEnv,
    tables: &EngineTables,
) -> u64 {
    let claude = (tables.normalize_provider_id)(provider) == CLAUDE;
    let present = credential.is_some_and(|credential| {
        !matches!(credential, Value::Null)
            && credential.as_bool() != Some(false)
            && credential.as_str() != Some("")
            && credential.as_f64() != Some(0.0)
    });
    if present {
        let accounts = credential.and_then(|credential| credential.get("accounts"));
        let listed = match accounts {
            Some(Value::Array(accounts)) if !accounts.is_empty() => accounts.len() as u64,
            _ => 1,
        };
        if !claude {
            return listed;
        }
        let listed = if matches!(accounts, Some(Value::Array(_))) {
            listed
        } else {
            0
        };
        return listed + env_slot_count(provider, env, tables);
    }
    env_slot_count(provider, env, tables)
}

fn env_slot_count(
    provider: &str,
    env: &telemetry_core::TelemetryEnv,
    tables: &EngineTables,
) -> u64 {
    let Some(primary) = primary_env_var(provider, tables) else {
        return 0;
    };
    let mut count = u64::from(env.get(&primary).is_some_and(|value| !value.is_empty()));
    for index in 2..=MAX_ENV_SLOT_INDEX {
        if env
            .get(&format!("{primary}_{index}"))
            .is_some_and(|value| !value.is_empty())
        {
            count += 1;
        }
    }
    count
}

fn primary_env_var(provider: &str, tables: &EngineTables) -> Option<String> {
    if (tables.normalize_provider_id)(provider) == CLAUDE {
        return Some("CLAUDE_CODE_OAUTH_TOKEN".into());
    }
    let names = (tables.get_api_key_env_vars)(provider)?;
    if names.is_empty() {
        return None;
    }
    Some(
        names
            .iter()
            .find(|name| name.ends_with("_API_KEY"))
            .cloned()
            .unwrap_or_else(|| names[0].clone()),
    )
}

fn kind_of(provider: &str, _tables: &EngineTables) -> String {
    let current = RETIRED_PROVIDER_IDS
        .iter()
        .find(|(retired, _)| *retired == provider)
        .map_or_else(|| provider.to_string(), |(_, canonical)| (*canonical).to_string());
    if KNOWN_PROVIDERS.contains(&current.as_str()) {
        current
    } else {
        "custom".to_string()
    }
}

/// An unreadable auth.json counts nothing here; the session inventory reports it once with a fixed message.
fn read_auth(path: &Path) -> serde_json::Map<String, Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tables() -> EngineTables {
        EngineTables {
            normalize_provider_id: Arc::new(|provider: &str| match provider {
                "claude-sdk-oauth" => "anthropic-subscription".into(),
                "openai-codex" => "chatgpt-subscription".into(),
                other => other.into(),
            }),
            get_api_key_env_vars: Arc::new(|provider: &str| match provider {
                "openai" => Some(vec!["OPENAI_API_KEY".into()]),
                "google" => Some(vec!["GEMINI_API_KEY".into(), "GOOGLE_API_KEY".into()]),
                "anthropic" => Some(vec![
                    "ANTHROPIC_AUTH_TOKEN".into(),
                    "ANTHROPIC_API_KEY".into(),
                ]),
                _ => None,
            }),
            api_key_env_providers: Arc::new(|| vec!["openai".into(), "google".into()]),
        }
    }

    fn env(pairs: &[(&str, &str)]) -> telemetry_core::TelemetryEnv {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    fn auth(value: Value) -> serde_json::Map<String, Value> {
        value.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn claude_managed_sentinel_empty_list_and_env_slots() {
        let tables = tables();
        // The managed sign-in sentinel: no list, so zero stored accounts, plus one env slot.
        let counts = connected_account_counts(
            &auth(json!({"anthropic-subscription": true})),
            &env(&[("CLAUDE_CODE_OAUTH_TOKEN", "x")]),
            &tables,
        );
        assert_eq!(counts.get("anthropic-subscription"), Some(&1));

        // An empty list is the flat credential: one account, plus two env slots.
        let counts = connected_account_counts(
            &auth(json!({"anthropic-subscription": {"accounts": []}})),
            &env(&[
                ("CLAUDE_CODE_OAUTH_TOKEN", "x"),
                ("CLAUDE_CODE_OAUTH_TOKEN_2", "y"),
            ]),
            &tables,
        );
        assert_eq!(counts.get("anthropic-subscription"), Some(&3));

        // A non-empty list counts its entries, plus the primary slot only.
        let counts = connected_account_counts(
            &auth(json!({"anthropic-subscription": {"accounts": [{"a": 1}, {"b": 2}]}})),
            &env(&[("CLAUDE_CODE_OAUTH_TOKEN", "x")]),
            &tables,
        );
        assert_eq!(counts.get("anthropic-subscription"), Some(&3));
    }

    #[test]
    fn other_providers_count_lists_flat_credentials_and_env_only_when_empty() {
        let tables = tables();
        let counts = connected_account_counts(
            &auth(json!({"openai": {"accounts": [{"a": 1}, {"b": 2}, {"c": 3}]}})),
            &env(&[("OPENAI_API_KEY", "x")]),
            &tables,
        );
        assert_eq!(counts.get("openai"), Some(&3));

        let counts = connected_account_counts(
            &auth(json!({"openai": {"key": "secret"}})),
            &env(&[("OPENAI_API_KEY", "x")]),
            &tables,
        );
        assert_eq!(counts.get("openai"), Some(&1));

        let counts = connected_account_counts(&auth(json!({})), &env(&[("OPENAI_API_KEY", "x")]), &tables);
        assert_eq!(counts.get("openai"), Some(&1));
        let counts = connected_account_counts(&auth(json!({})), &env(&[]), &tables);
        assert_eq!(counts.get("openai"), None);
    }

    #[test]
    fn primary_env_var_prefers_the_api_key_spelling() {
        let tables = tables();
        let counts = connected_account_counts(
            &auth(json!({})),
            &env(&[("GEMINI_API_KEY", "x"), ("GOOGLE_API_KEY", "y")]),
            &tables,
        );
        // The first `_API_KEY` spelling wins, so the alternate name alone counts nothing.
        assert_eq!(counts.get("google"), Some(&1));
        let counts = connected_account_counts(&auth(json!({})), &env(&[("GOOGLE_API_KEY", "y")]), &tables);
        assert_eq!(counts.get("google"), None);
    }

    #[test]
    fn env_slots_are_capped_at_sixteen_and_counts_at_ten_plus() {
        let tables = tables();
        let mut env = telemetry_core::TelemetryEnv::new();
        env.insert("CLAUDE_CODE_OAUTH_TOKEN".into(), "x".into());
        for index in 2..=20 {
            env.insert(format!("CLAUDE_CODE_OAUTH_TOKEN_{index}"), "x".into());
        }
        let counts = connected_account_counts(&auth(json!({})), &env, &tables);
        assert_eq!(counts.get("anthropic-subscription"), Some(&16));
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("auth.json"),
            json!({"anthropic-subscription": {"accounts": [{}, {}, {}]}}).to_string(),
        )
        .unwrap();
        assert_eq!(
            read_connected_accounts(dir.path(), &env, Some(&tables)),
            "anthropic-subscription:10_plus"
        );
    }

    #[test]
    fn unknown_kinds_fold_into_custom_and_retired_ids_are_canonical() {
        let tables = tables();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("auth.json"),
            json!({
                "claude-sdk-oauth": {"accounts": [{}, {}]},
                "my-gateway": {"accounts": [{}]},
                "openai": {"accounts": [{}]}
            })
            .to_string(),
        )
        .unwrap();
        let rendered = read_connected_accounts(dir.path(), &env(&[]), Some(&tables));
        assert_eq!(
            rendered,
            "anthropic-subscription:2,custom:1,openai:1"
        );
        assert!(!rendered.contains("my-gateway"));
        assert!(!rendered.contains("claude-sdk-oauth"));
    }

    #[test]
    fn no_tables_and_unreadable_auth_render_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_connected_accounts(dir.path(), &env(&[]), None), "");
        std::fs::write(dir.path().join("auth.json"), "{not json").unwrap();
        assert_eq!(read_connected_accounts(dir.path(), &env(&[]), Some(&tables())), "");
        std::fs::write(dir.path().join("auth.json"), "[]").unwrap();
        assert_eq!(read_connected_accounts(dir.path(), &env(&[]), Some(&tables())), "");
    }

    #[test]
    fn the_engine_tables_are_available() {
        let tables = EngineTables::load().expect("engine tables");
        assert_eq!((tables.normalize_provider_id)("claude-sdk-oauth"), "anthropic-subscription");
        assert!((tables.api_key_env_providers)().iter().any(|provider| provider == "openai"));
    }
}
