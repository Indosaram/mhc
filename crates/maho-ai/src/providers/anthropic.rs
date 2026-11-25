//! Port of senpi packages/ai/src/providers/anthropic.ts.

use crate::auth::types::{
    ApiKeyAuth, ApiKeyCredential, AuthContext, AuthPrompt, AuthPromptKind, AuthResult, ModelAuth,
    ProviderAuth, ProviderAuthInteraction,
};
use crate::env_api_keys::{
    ANTHROPIC_API_KEY_ENV, ANTHROPIC_AUTH_TOKEN_ENV, ANTHROPIC_OAUTH_TOKEN_ENV,
};
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use crate::types::ProviderHeaders;
use crate::types::{
    AssistantMessageEventStream, Context, Model, SimpleStreamOptions, StreamOptions,
};
use crate::utils::abort::AbortSignal;
use async_trait::async_trait;
use std::sync::Arc;

pub fn stream_anthropic(
    model: &Model,
    context: &Context,
    options: Option<StreamOptions>,
) -> AssistantMessageEventStream {
    builtin_api_streams("anthropic-messages").stream(model, context, options)
}

pub fn stream_simple_anthropic(
    model: &Model,
    context: &Context,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    builtin_api_streams("anthropic-messages").stream_simple(model, context, options)
}

/// senpi `anthropicApiKeyAuth`: a stored key, then `ANTHROPIC_AUTH_TOKEN` as a bearer header,
/// then `ANTHROPIC_OAUTH_TOKEN` / `ANTHROPIC_API_KEY` as the API key.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnthropicApiKeyAuth;

#[async_trait]
impl ApiKeyAuth for AnthropicApiKeyAuth {
    fn name(&self) -> &str {
        "Anthropic API key"
    }

    fn has_login(&self) -> bool {
        true
    }

    async fn login(
        &self,
        interaction: &ProviderAuthInteraction,
    ) -> anyhow::Result<ApiKeyCredential> {
        interaction
            .signal
            .throw_if_aborted()
            .map_err(|reason| anyhow::anyhow!("{reason}"))?;
        let key = interaction
            .prompt(AuthPrompt {
                kind: AuthPromptKind::Secret {
                    message: "Enter Anthropic API key".into(),
                    placeholder: None,
                },
                signal: None,
            })
            .await?;
        interaction
            .signal
            .throw_if_aborted()
            .map_err(|reason| anyhow::anyhow!("{reason}"))?;
        Ok(ApiKeyCredential {
            key: Some(key),
            env: None,
            ..Default::default()
        })
    }

    async fn resolve(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
        signal: &AbortSignal,
    ) -> anyhow::Result<Option<AuthResult>> {
        signal
            .throw_if_aborted()
            .map_err(|reason| anyhow::anyhow!("{reason}"))?;
        if let Some(credential) = credential
            && let Some(key) = credential.key.as_ref().filter(|key| !key.is_empty())
        {
            return Ok(Some(AuthResult {
                ambient: false,
                auth: ModelAuth {
                    api_key: Some(key.clone()),
                    ..Default::default()
                },
                env: credential.env.clone(),
                source: Some("stored credential".into()),
            }));
        }

        let auth_token = ctx.env(ANTHROPIC_AUTH_TOKEN_ENV).await;
        signal
            .throw_if_aborted()
            .map_err(|reason| anyhow::anyhow!("{reason}"))?;
        if let Some(auth_token) = auth_token.filter(|token| !token.is_empty()) {
            let mut headers = ProviderHeaders::new();
            headers.insert("Authorization".into(), Some(format!("Bearer {auth_token}")));
            return Ok(Some(AuthResult {
                ambient: false,
                auth: ModelAuth {
                    headers: Some(headers),
                    ..Default::default()
                },
                env: None,
                source: Some(ANTHROPIC_AUTH_TOKEN_ENV.into()),
            }));
        }

        for env_var in [ANTHROPIC_OAUTH_TOKEN_ENV, ANTHROPIC_API_KEY_ENV] {
            let api_key = ctx.env(env_var).await;
            signal
                .throw_if_aborted()
                .map_err(|reason| anyhow::anyhow!("{reason}"))?;
            if let Some(api_key) = api_key.filter(|key| !key.is_empty()) {
                return Ok(Some(AuthResult {
                    ambient: false,
                    auth: ModelAuth {
                        api_key: Some(api_key),
                        ..Default::default()
                    },
                    env: None,
                    source: Some(env_var.into()),
                }));
            }
        }
        Ok(None)
    }
}

pub fn anthropic_api_key_auth() -> AnthropicApiKeyAuth {
    AnthropicApiKeyAuth
}

pub fn anthropic_provider() -> Result<Arc<dyn Provider>, crate::models::ModelsError> {
    create_provider(CreateProviderOptions {
        id: "anthropic".into(),
        name: Some("Anthropic".into()),
        base_url: Some("https://api.anthropic.com".into()),
        headers: None,
        // Claude subscription sign-in belongs to the Claude Code CLI lane, not the Messages API.
        auth: Some(ProviderAuth {
            api_key: Some(Arc::new(anthropic_api_key_auth())),
            oauth: None,
        }),
        models: super::anthropic_models::anthropic_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        images: indexmap::IndexMap::new(),
        api: ProviderApi::Single(builtin_api_streams("anthropic-messages")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Env(HashMap<String, String>);

    #[async_trait]
    impl AuthContext for Env {
        async fn env(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
        async fn file_exists(&self, _path: &str) -> bool {
            false
        }
    }

    fn env(pairs: &[(&str, &str)]) -> Env {
        Env(pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect())
    }

    #[test]
    fn builtin_anthropic_providers_offer_no_oauth_login() {
        let providers = crate::providers::all::builtin_providers().expect("built-in providers");
        assert!(!providers.iter().any(|provider| {
            provider.id().contains("anthropic")
                && provider.auth().is_some_and(|auth| auth.oauth.is_some())
        }));
        let anthropic = providers
            .iter()
            .find(|provider| provider.id() == "anthropic")
            .expect("Anthropic provider");
        assert!(anthropic.auth().is_some_and(|auth| auth.api_key.is_some()));
    }

    #[tokio::test]
    async fn auth_token_alone_resolves_to_a_bearer_header() {
        let signal = crate::utils::abort::AbortController::new().signal();
        let result = anthropic_api_key_auth()
            .resolve(&env(&[(ANTHROPIC_AUTH_TOKEN_ENV, "token")]), None, &signal)
            .await
            .expect("resolve")
            .expect("configured");
        assert_eq!(result.auth.api_key, None);
        assert_eq!(
            result.auth.headers.expect("headers")["Authorization"].as_deref(),
            Some("Bearer token")
        );
        assert_eq!(result.source.as_deref(), Some(ANTHROPIC_AUTH_TOKEN_ENV));
    }

    #[tokio::test]
    async fn stored_key_wins_over_ambient_tokens() {
        let signal = crate::utils::abort::AbortController::new().signal();
        let credential = ApiKeyCredential {
            key: Some("stored".into()),
            env: None,
            ..Default::default()
        };
        let result = anthropic_api_key_auth()
            .resolve(
                &env(&[(ANTHROPIC_AUTH_TOKEN_ENV, "token")]),
                Some(&credential),
                &signal,
            )
            .await
            .expect("resolve")
            .expect("configured");
        assert_eq!(result.auth.api_key.as_deref(), Some("stored"));
    }

    #[tokio::test]
    async fn oauth_token_env_precedes_api_key_env() {
        let signal = crate::utils::abort::AbortController::new().signal();
        let result = anthropic_api_key_auth()
            .resolve(
                &env(&[
                    (ANTHROPIC_OAUTH_TOKEN_ENV, "oat"),
                    (ANTHROPIC_API_KEY_ENV, "key"),
                ]),
                None,
                &signal,
            )
            .await
            .expect("resolve")
            .expect("configured");
        assert_eq!(result.auth.api_key.as_deref(), Some("oat"));
    }

    #[tokio::test]
    async fn oauth_token_from_env_is_refused_before_a_messages_request() {
        let token = ["sk-ant", "oat01", "test-only"].join("-");
        let signal = crate::utils::abort::AbortController::new().signal();
        let result = anthropic_api_key_auth()
            .resolve(&env(&[(ANTHROPIC_OAUTH_TOKEN_ENV, &token)]), None, &signal)
            .await
            .expect("resolve")
            .expect("configured");
        let mut model = crate::providers::all::get_builtin_models("anthropic")
            .into_iter()
            .next()
            .expect("Anthropic model");
        // No network even if the refusal regresses.
        model.base_url = "http://127.0.0.1:0".into();
        let mut options = SimpleStreamOptions::default();
        options.stream.request.api_key = result.auth.api_key;
        options.stream.request.max_retries = Some(0);
        let stream = stream_simple_anthropic(&model, &Context::default(), Some(options));
        let message = tokio::time::timeout(std::time::Duration::from_secs(5), stream.result())
            .await
            .expect("refusal must settle")
            .expect("error result");
        assert_eq!(message.stop_reason, crate::types::StopReason::Error);
        let detail = message.error_message.expect("refusal detail");
        assert!(detail.contains("/login anthropic-subscription"), "{detail}");
        assert!(!detail.contains(&token));
    }
}
