//! What to tell the user about credentials setup found but could not copy (upstream
//! `omo-native/bin/lib/setup-guidance.js`).
//!
//! OAuth credentials are provider-bound tokens, so they are never copied; the user has to sign in
//! again. That sign-in lives in the interactive session (`/login <provider>`), NOT in `omo auth`.

use super::provider_map::ProviderMap;
use serde_json::{Map, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OauthLogin {
    pub provider: String,
    pub target: Option<String>,
    /// `login`, `signed-in` or `unsupported`.
    pub state: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialGuidance {
    pub logins: Vec<OauthLogin>,
    pub unmapped: Vec<String>,
}

fn oauth_login_target(provider: &str, map: &ProviderMap) -> Option<String> {
    if let Some(mapped) = map.oauth_logins.get(provider) {
        return Some(mapped.clone());
    }
    map.oauth_provider_ids
        .iter()
        .any(|id| id == provider)
        .then(|| provider.to_owned())
}

// `login`: sign in with `/login <target>`; `signed-in`: a re-run after the user followed the advice
// must not tell them to sign in a second time; `unsupported`: omo has no provider for it.
fn oauth_login(provider: &str, map: &ProviderMap, existing: &Map<String, Value>) -> OauthLogin {
    let Some(target) = oauth_login_target(provider, map) else {
        return OauthLogin {
            provider: provider.to_owned(),
            target: None,
            state: "unsupported",
        };
    };
    let signed_in = existing
        .get(&target)
        .and_then(|entry| entry.get("type"))
        .and_then(Value::as_str)
        == Some("oauth");
    OauthLogin {
        provider: provider.to_owned(),
        target: Some(target),
        state: if signed_in { "signed-in" } else { "login" },
    }
}

/// `logins`: one entry per skipped OAuth provider. `unmapped`: API-key provider ids no omo provider
/// serves; the next step for them is to define the provider in the engine's models.json.
pub fn credential_guidance(
    skipped_oauth: &[String],
    skipped_unmapped: &[String],
    map: &ProviderMap,
    existing: &Map<String, Value>,
) -> CredentialGuidance {
    CredentialGuidance {
        logins: skipped_oauth
            .iter()
            .map(|provider| oauth_login(provider, map, existing))
            .collect(),
        unmapped: skipped_unmapped.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::provider_map::provider_map;
    use serde_json::json;

    fn guidance(oauth: &[&str], unmapped: &[&str], existing: Value) -> CredentialGuidance {
        let owned = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let existing = existing.as_object().cloned().unwrap_or_default();
        credential_guidance(&owned(oauth), &owned(unmapped), provider_map(), &existing)
    }

    fn login(provider: &str, target: Option<&str>, state: &'static str) -> OauthLogin {
        OauthLogin {
            provider: provider.into(),
            target: target.map(str::to_owned),
            state,
        }
    }

    #[test]
    fn each_oauth_credential_names_the_engine_provider_to_login_to() {
        assert_eq!(
            guidance(&["openai", "anthropic", "github-copilot"], &[], json!({})).logins,
            vec![
                login("openai", Some("chatgpt-subscription"), "login"),
                login("anthropic", Some("anthropic-subscription"), "login"),
                login("github-copilot", Some("github-copilot"), "login"),
            ]
        );
    }

    #[test]
    fn a_login_already_done_is_not_asked_for_again() {
        assert_eq!(
            guidance(
                &["openai"],
                &[],
                json!({"chatgpt-subscription": {"type": "oauth"}})
            )
            .logins,
            vec![login("openai", Some("chatgpt-subscription"), "signed-in")]
        );
    }

    #[test]
    fn an_oauth_credential_with_no_omo_provider_has_no_target() {
        assert_eq!(
            guidance(&["some-unknown-oauth"], &[], json!({})).logins,
            vec![login("some-unknown-oauth", None, "unsupported")]
        );
    }

    #[test]
    fn an_unmatched_api_key_provider_is_listed_as_unmapped() {
        assert_eq!(
            guidance(&[], &["unknown-gateway"], json!({})),
            CredentialGuidance {
                logins: vec![],
                unmapped: vec!["unknown-gateway".into()]
            }
        );
    }
}
