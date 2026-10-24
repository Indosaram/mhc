//! Recorded risk consent for providers that present themselves as a first-party client.
//!
//! Consent lives in global settings as `providerRiskConsent.<provider>` (an ISO timestamp).
use crate::settings_manager::{
    Settings, SettingsManager, SettingsScope, get_settings_path, parse_settings_json,
};
use serde_json::Value;

pub const CONSENT_SETTINGS_KEY: &str = "providerRiskConsent";
pub const CONSENT_REQUIRED_CODE: &str = "provider_consent_required";
pub const CONSENT_REQUIRED_CLASS: &str = "ConsentRequired";

/// (provider id, plain description of the first-party client it impersonates).
pub const IMPERSONATING_PROVIDERS: &[(&str, &str)] = &[
    (
        "anthropic",
        "Claude Code CLI identity when signed in with a Claude subscription (OAuth token)",
    ),
    (
        "anthropic-subscription",
        "Claude Code CLI identity via the Claude subscription lane",
    ),
    ("chatgpt-subscription", "the OpenAI Codex client"),
    ("github-copilot", "the VS Code GitHub Copilot Chat extension"),
    ("kimi-coding", "the Kimi CLI"),
    ("devin", "Cognition's internal Go client"),
    ("cursor", "the Cursor desktop/CLI client"),
];

/// Builtin lane ids mapped to the provider id that carries their consent.
const LANE_ALIASES: &[(&str, &str)] = &[
    ("cursor-cli-oauth", "cursor"),
    ("claude-sdk-oauth", "anthropic-subscription"),
];

/// Canonical provider id for `provider`, resolving builtin lane ids.
pub fn canonical_provider(provider: &str) -> &str {
    LANE_ALIASES
        .iter()
        .find(|(lane, _)| *lane == provider)
        .map_or(provider, |(_, id)| id)
}

pub fn impersonation_description(provider: &str) -> Option<&'static str> {
    let provider = canonical_provider(provider);
    IMPERSONATING_PROVIDERS
        .iter()
        .find(|(id, _)| *id == provider)
        .map(|(_, description)| *description)
}

/// Whether using `provider` needs consent. `anthropic` needs it only for OAuth tokens.
pub fn needs_consent(provider: &str, anthropic_oauth_token: bool) -> bool {
    match canonical_provider(provider) {
        "anthropic" => anthropic_oauth_token,
        other => impersonation_description(other).is_some(),
    }
}

pub fn is_oauth_token(value: &str) -> bool {
    value.contains("sk-ant-oat")
}

fn consent_in(settings: &Settings, provider: &str) -> bool {
    settings
        .get(CONSENT_SETTINGS_KEY)
        .and_then(|consent| consent.get(canonical_provider(provider)))
        .and_then(Value::as_str)
        .is_some_and(|stamp| !stamp.trim().is_empty())
}

/// Reads `providerRiskConsent.<provider>` from global settings only.
pub fn consent_recorded(settings: &SettingsManager, provider: &str) -> bool {
    consent_in(settings.get_global(), provider)
}

/// Same check against the global settings file under `agent_dir`, read fresh from disk.
pub fn consent_recorded_in_agent_dir(agent_dir: &str, provider: &str) -> bool {
    let path = get_settings_path("", agent_dir, SettingsScope::Global, "");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| parse_settings_json(&content).ok())
        .is_some_and(|settings| consent_in(&settings, provider))
}

pub fn record_consent(settings: &mut SettingsManager, provider: &str) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let mut nested = Settings::new();
    nested.insert(canonical_provider(provider).to_owned(), Value::String(now));
    settings.set_nested(SettingsScope::Global, CONSENT_SETTINGS_KEY, &nested)
}

pub fn revoke_consent(settings: &mut SettingsManager, provider: &str) -> Result<(), String> {
    let mut nested = Settings::new();
    nested.insert(canonical_provider(provider).to_owned(), Value::Null);
    settings.set_nested(SettingsScope::Global, CONSENT_SETTINGS_KEY, &nested)
}

fn rotation_key(provider: &str) -> String {
    format!("{}:rotation", canonical_provider(provider))
}

fn rotation_consent_in(settings: &Settings, provider: &str) -> bool {
    settings
        .get(CONSENT_SETTINGS_KEY)
        .and_then(|consent| consent.get(&rotation_key(provider)))
        .and_then(Value::as_str)
        .is_some_and(|stamp| !stamp.trim().is_empty())
}

/// Reads `providerRiskConsent.<provider>:rotation` from global settings only.
pub fn rotation_consent_recorded(settings: &SettingsManager, provider: &str) -> bool {
    rotation_consent_in(settings.get_global(), provider)
}

/// Same check against the global settings file under `agent_dir`, read fresh from disk.
pub fn rotation_consented_in_agent_dir(agent_dir: &str, provider: &str) -> bool {
    let path = get_settings_path("", agent_dir, SettingsScope::Global, "");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| parse_settings_json(&content).ok())
        .is_some_and(|settings| rotation_consent_in(&settings, provider))
}

/// Reads `providerRiskConsent.<provider>:rotation` using the active agent dir.
pub fn rotation_consented(provider: &str) -> bool {
    let agent_dir = crate::config::get_agent_dir();
    rotation_consented_in_agent_dir(&agent_dir, provider)
}

pub fn record_rotation_consent(settings: &mut SettingsManager, provider: &str) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let mut nested = Settings::new();
    nested.insert(rotation_key(provider), Value::String(now));
    settings.set_nested(SettingsScope::Global, CONSENT_SETTINGS_KEY, &nested)
}

pub fn revoke_rotation_consent(settings: &mut SettingsManager, provider: &str) -> Result<(), String> {
    let mut nested = Settings::new();
    nested.insert(rotation_key(provider), Value::Null);
    settings.set_nested(SettingsScope::Global, CONSENT_SETTINGS_KEY, &nested)
}

pub fn rotation_disabled_notice(provider: &str, count: usize) -> String {
    let provider = canonical_provider(provider);
    format!("Automatic rotation across {count} accounts is off. Enable it with `mhc auth consent {provider} --rotation`.")
}

pub fn append_rotation_notice(original: &str, provider: &str, count: usize) -> String {
    let notice = rotation_disabled_notice(provider, count);
    let trimmed = original.trim();
    if trimmed.is_empty() {
        notice
    } else if trimmed.ends_with('.') || trimmed.ends_with('!') || trimmed.ends_with('?') {
        format!("{trimmed} {notice}")
    } else {
        format!("{trimmed}. {notice}")
    }
}

/// Whether a stored anthropic credential is an OAuth subscription token. With `slot_name` only
/// that account slot is inspected (falling back to all slots when the name is unknown).
pub fn anthropic_credential_needs_consent(credential: &Value, slot_name: Option<&str>) -> bool {
    fn oauth_like(value: &Value) -> bool {
        value.get("type").and_then(Value::as_str) == Some("oauth")
            || ["access", "key"].iter().any(|field| {
                value
                    .get(*field)
                    .and_then(Value::as_str)
                    .is_some_and(is_oauth_token)
            })
    }
    let slots = crate::credential_accounts::stored_slots(credential);
    let named: Vec<&Value> = slots
        .iter()
        .filter(|slot| slot.get("name").and_then(Value::as_str) == slot_name && slot_name.is_some())
        .collect();
    let targets: Vec<&Value> = if named.is_empty() {
        slots.iter().collect()
    } else {
        named
    };
    targets.into_iter().any(oauth_like) || oauth_like(credential)
}

/// The single consent gate shared by `ModelRegistry` auth resolution and every `ModelRuntime`
/// request path. Runs before credential refresh, header resolution or any network work: only the
/// stored credential kind is inspected and global settings are read fresh from disk.
// SAFETY-DIVERGENCE(packages/coding-agent/src/core/model-runtime.ts): upstream does not gate
// impersonating providers behind risk consent; mhc requires recorded consent before any request.
pub fn consent_failure(
    credentials: &crate::auth_storage::AuthStorage,
    provider: &str,
    slot_name: Option<&str>,
) -> Option<String> {
    let anthropic_oauth = canonical_provider(provider) == "anthropic"
        && (credentials
            .get(provider)
            .is_some_and(|credential| anthropic_credential_needs_consent(&credential, slot_name))
            || credentials
                .runtime_api_key(provider)
                .is_some_and(|key| is_oauth_token(&key))
            || maho_ai::env_api_keys::get_env_api_key(provider, None)
                .is_some_and(|key| is_oauth_token(&key)));
    if !needs_consent(provider, anthropic_oauth) {
        return None;
    }
    let agent_dir = credentials
        .storage_path()
        .and_then(|path| std::path::Path::new(path).parent())
        .map_or_else(crate::config::get_agent_dir, |dir| {
            dir.to_string_lossy().into_owned()
        });
    if consent_recorded_in_agent_dir(&agent_dir, provider) {
        return None;
    }
    Some(consent_required_message(provider))
}

pub fn consent_required_message(provider: &str) -> String {
    let description = impersonation_description(provider).unwrap_or("a first-party client");
    format!(
        "{provider} uses {description}. This may violate the provider's terms and can get the account suspended. Record consent with /login {provider} (interactive) or `mhc auth consent {provider}`; it is stored as providerRiskConsent.{provider} in settings.json."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager() -> SettingsManager {
        SettingsManager::in_memory(Settings::new(), false)
    }

    #[test]
    fn consent_is_absent_until_recorded_and_then_global() {
        let mut settings = manager();
        assert!(!consent_recorded(&settings, "github-copilot"));
        record_consent(&mut settings, "github-copilot").expect("record");
        assert!(consent_recorded(&settings, "github-copilot"));
        assert!(!consent_recorded(&settings, "kimi-coding"));
        let stamp = settings.get_global()[CONSENT_SETTINGS_KEY]["github-copilot"]
            .as_str()
            .expect("timestamp");
        assert!(chrono::DateTime::parse_from_rfc3339(stamp).is_ok());
    }

    #[test]
    fn consent_keeps_siblings_and_revoke_removes_only_one_key() {
        let mut settings = manager();
        record_consent(&mut settings, "kimi-coding").expect("record");
        record_consent(&mut settings, "devin").expect("record");
        revoke_consent(&mut settings, "kimi-coding").expect("revoke");
        assert!(!consent_recorded(&settings, "kimi-coding"));
        assert!(consent_recorded(&settings, "devin"));
    }

    #[test]
    fn builtin_lanes_share_their_provider_consent() {
        let mut settings = manager();
        record_consent(&mut settings, "cursor-cli-oauth").expect("record");
        assert!(consent_recorded(&settings, "cursor"));
        assert!(consent_recorded(&settings, "cursor-cli-oauth"));
        assert!(needs_consent("claude-sdk-oauth", false));
    }

    #[test]
    fn anthropic_needs_consent_only_for_oauth_tokens() {
        assert!(needs_consent("anthropic", true));
        assert!(!needs_consent("anthropic", false));
        assert!(!needs_consent("openai", true));
        assert!(is_oauth_token("sk-ant-oat01-abc"));
        assert!(!is_oauth_token("sk-ant-api03-abc"));
    }

    #[test]
    fn consent_message_names_the_cli_command() {
        let message = consent_required_message("github-copilot");
        assert!(message.contains("`mhc auth consent github-copilot`"));
        assert!(message.contains("providerRiskConsent.github-copilot"));
    }

    #[test]
    fn rotation_consent_records_and_revokes_independently() {
        let mut settings = manager();
        assert!(!rotation_consent_recorded(&settings, "anthropic-subscription"));
        record_rotation_consent(&mut settings, "anthropic-subscription").expect("record");
        assert!(rotation_consent_recorded(&settings, "anthropic-subscription"));
        // Base consent is not automatically granted by rotation consent.
        assert!(!consent_recorded(&settings, "anthropic-subscription"));
        revoke_rotation_consent(&mut settings, "anthropic-subscription").expect("revoke");
        assert!(!rotation_consent_recorded(&settings, "anthropic-subscription"));
    }

    #[test]
    fn rotation_notice_formatting() {
        let notice = rotation_disabled_notice("anthropic-subscription", 2);
        assert_eq!(
            notice,
            "Automatic rotation across 2 accounts is off. Enable it with `mhc auth consent anthropic-subscription --rotation`."
        );
        let appended = append_rotation_notice("rate limit exceeded", "anthropic-subscription", 2);
        assert_eq!(
            appended,
            "rate limit exceeded. Automatic rotation across 2 accounts is off. Enable it with `mhc auth consent anthropic-subscription --rotation`."
        );
        let appended_dot = append_rotation_notice("rate limit exceeded.", "claude-sdk-oauth", 3);
        assert_eq!(
            appended_dot,
            "rate limit exceeded. Automatic rotation across 3 accounts is off. Enable it with `mhc auth consent anthropic-subscription --rotation`."
        );
    }
}
