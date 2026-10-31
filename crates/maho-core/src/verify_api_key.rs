//! Port of senpi packages/coding-agent/src/core/verify-api-key.ts.
//!
//! Checks an API key against its provider without storing it (desktop #2510): one cheap authenticated
//! read (the model list) with the key in memory only. The answer is ok, rejected (401/403), unreachable
//! (no answer, 429, 5xx) or unsupported (no probe for this API, or no model list), so a client can say
//! "we could not check" instead of guessing. Neither the key nor the headers reach a log or the result.

use std::collections::BTreeMap;
use std::future::Future;

use maho_ai::model::Model;
use serde_json::Value;

use crate::resolve_config_value::{get_config_value_env_var_names, is_command_config_value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKeyFailure {
    Rejected,
    Unreachable,
    Unsupported,
    UnknownProvider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKeyVerification {
    Ok,
    Failed {
        reason: ApiKeyFailure,
        status: Option<u16>,
    },
}

/// One GET of a provider's model list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub url: String,
    pub headers: BTreeMap<String, String>,
}

const UNCHECKABLE_CONTROL_KEY: &str = "senpi-verify-control-not-a-real-key";
const AUTH_HEADERS: [&str; 3] = ["authorization", "x-api-key", "x-goog-api-key"];

fn probe_for(model: &Model, key: &str) -> Option<Probe> {
    let base = model.base_url.trim_end_matches('/');
    // Configured auth headers would be merged with (or replace) the probe's, whatever their case.
    let mut headers: BTreeMap<String, String> = model
        .headers
        .iter()
        .flatten()
        .filter(|(name, _)| !AUTH_HEADERS.contains(&name.to_lowercase().as_str()))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let without_v1 = base.strip_suffix("/v1").unwrap_or(base);
    let url = match model.api.as_str() {
        "openai-completions" | "openai-responses" => {
            headers.insert("authorization".to_owned(), format!("Bearer {key}"));
            format!("{base}/models")
        }
        // The Mistral runtime calls `${base}/v1/...`; its model list lives there too.
        "mistral-conversations" => {
            headers.insert("authorization".to_owned(), format!("Bearer {key}"));
            format!("{without_v1}/v1/models")
        }
        "anthropic-messages" => {
            headers.insert("x-api-key".to_owned(), key.to_owned());
            headers.insert("anthropic-version".to_owned(), "2023-06-01".to_owned());
            format!("{without_v1}/v1/models")
        }
        "google-generative-ai" => {
            headers.insert("x-goog-api-key".to_owned(), key.to_owned());
            format!("{base}/models")
        }
        _ => return None,
    };
    Some(Probe { url, headers })
}

fn failed(reason: ApiKeyFailure, status: Option<u16>) -> ApiKeyVerification {
    ApiKeyVerification::Failed { reason, status }
}

/// A rate limit or an outage means "try again", never "cannot be checked".
fn temporary(status: Option<u16>) -> Option<ApiKeyVerification> {
    match status {
        None => Some(failed(ApiKeyFailure::Unreachable, None)),
        Some(status) if status == 429 || status >= 500 => {
            Some(failed(ApiKeyFailure::Unreachable, Some(status)))
        }
        Some(_) => None,
    }
}

/// `fetch` performs the GET without following redirects and returns the status, or `None` when no answer
/// came (network error, timeout, abort).
pub async fn verify_api_key<F, Fut>(
    models: &[Model],
    provider: &str,
    key: &str,
    fetch: F,
) -> ApiKeyVerification
where
    F: Fn(Probe) -> Fut,
    Fut: Future<Output = Option<u16>>,
{
    let Some(model) = models
        .iter()
        .find(|candidate| candidate.provider == provider)
    else {
        return failed(ApiKeyFailure::UnknownProvider, None);
    };
    let Some(probe) = probe_for(model, key) else {
        return failed(ApiKeyFailure::Unsupported, None);
    };
    let status = fetch(probe).await;
    if let Some(failure) = temporary(status) {
        return failure;
    }
    let status = status.unwrap_or_default();
    if status == 401 || status == 403 {
        return failed(ApiKeyFailure::Rejected, Some(status));
    }
    if !(200..300).contains(&status) {
        return failed(ApiKeyFailure::Unsupported, Some(status));
    }
    // A model list that also answers a made-up key is public (OpenRouter, keyless local servers): its
    // 200 says nothing about this key.
    let Some(control_probe) = probe_for(model, UNCHECKABLE_CONTROL_KEY) else {
        return failed(ApiKeyFailure::Unreachable, None);
    };
    let control = fetch(control_probe).await;
    if matches!(control, Some(401 | 403)) {
        return ApiKeyVerification::Ok;
    }
    temporary(control).unwrap_or(failed(ApiKeyFailure::Unsupported, None))
}

/// The last four characters of a provider's stored literal API key, when the key is long enough that
/// they reveal nothing useful. A command (`!...`) or `$ENV` reference is never hinted: any slice of it
/// would describe where the secret lives, not which key it is.
pub fn stored_key_hint(auth_type: &str, stored: Option<&Value>) -> Option<String> {
    if auth_type != "api_key" {
        return None;
    }
    let credential = stored?;
    if credential.get("type").and_then(Value::as_str) != Some("api_key") {
        return None;
    }
    let key = credential.get("key")?.as_str()?;
    if is_command_config_value(key)
        || !get_config_value_env_var_names(key).is_empty()
        || key.chars().count() < 12
    {
        return None;
    }
    Some(key.chars().skip(key.chars().count() - 4).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    fn model(api: &str, base_url: &str) -> Model {
        let Some(mut model) = maho_ai::providers::all::get_builtin_models("openai")
            .into_iter()
            .next()
        else {
            panic!("builtin openai model")
        };
        model.provider = "p".to_owned();
        model.api = api.to_owned();
        model.base_url = base_url.to_owned();
        model.headers = Some(BTreeMap::from([
            ("X-Api-Key".to_owned(), "c".to_owned()),
            ("x-team".to_owned(), "t".to_owned()),
        ]));
        model
    }

    async fn run(models: &[Model], answers: &[Option<u16>]) -> (ApiKeyVerification, Vec<Probe>) {
        let seen = Mutex::new(Vec::new());
        let result = verify_api_key(models, "p", "sk-test", |probe| {
            let mut seen = seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let answer = answers.get(seen.len()).copied().flatten();
            seen.push(probe);
            async move { answer }
        })
        .await;
        (
            result,
            seen.into_inner()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    #[tokio::test]
    async fn a_key_is_ok_only_when_the_control_key_is_rejected() {
        let models = [model("anthropic-messages", "https://api.anthropic.com/v1/")];
        let (result, probes) = run(&models, &[Some(200), Some(401)]).await;
        assert_eq!(result, ApiKeyVerification::Ok);
        assert_eq!(probes[0].url, "https://api.anthropic.com/v1/models");
        assert_eq!(
            probes[0].headers.get("x-api-key").map(String::as_str),
            Some("sk-test")
        );
        assert!(!probes[0].headers.contains_key("X-Api-Key"));
        assert_eq!(
            probes[1].headers.get("x-api-key").map(String::as_str),
            Some(UNCHECKABLE_CONTROL_KEY)
        );
        let (result, _) = run(&models, &[Some(200), Some(200)]).await;
        assert_eq!(result, failed(ApiKeyFailure::Unsupported, None));
    }

    #[tokio::test]
    async fn classifies_rejection_outage_and_unknown_provider() {
        let models = [model("openai-completions", "https://x/v1")];
        assert_eq!(
            run(&models, &[Some(403)]).await.0,
            failed(ApiKeyFailure::Rejected, Some(403))
        );
        assert_eq!(
            run(&models, &[Some(429)]).await.0,
            failed(ApiKeyFailure::Unreachable, Some(429))
        );
        assert_eq!(
            run(&models, &[None]).await.0,
            failed(ApiKeyFailure::Unreachable, None)
        );
        assert_eq!(
            run(&models, &[Some(404)]).await.0,
            failed(ApiKeyFailure::Unsupported, Some(404))
        );
        assert_eq!(
            run(&models, &[Some(200), Some(503)]).await.0,
            failed(ApiKeyFailure::Unreachable, Some(503))
        );
        assert_eq!(
            run(&[], &[]).await.0,
            failed(ApiKeyFailure::UnknownProvider, None)
        );
        assert_eq!(
            run(&[model("bedrock", "https://b")], &[]).await.0,
            failed(ApiKeyFailure::Unsupported, None)
        );
    }

    #[test]
    fn hints_only_long_literal_keys() {
        let stored = json!({"type": "api_key", "key": "sk-abcdefghijkl"});
        assert_eq!(
            stored_key_hint("api_key", Some(&stored)).as_deref(),
            Some("ijkl")
        );
        assert_eq!(stored_key_hint("oauth", Some(&stored)), None);
        assert_eq!(
            stored_key_hint("api_key", Some(&json!({"type": "api_key", "key": "short"}))),
            None
        );
        assert_eq!(
            stored_key_hint(
                "api_key",
                Some(&json!({"type": "api_key", "key": "!op read secret-key"}))
            ),
            None
        );
        assert_eq!(
            stored_key_hint(
                "api_key",
                Some(&json!({"type": "api_key", "key": "$OPENAI_API_KEY_LONG"}))
            ),
            None
        );
    }
}
