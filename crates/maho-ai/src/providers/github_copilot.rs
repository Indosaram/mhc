//! Port of senpi packages/ai/src/providers/github-copilot.ts.

use crate::auth::helpers::{env_api_key_auth, lazy_oauth};
use crate::auth::types::ProviderAuth;
use crate::models::{
    CreateProviderOptions, Credential, FilterModels, Provider, ProviderApi, create_provider,
};
use crate::providers::builtin_api_streams;
use crate::types::Model;
use indexmap::IndexMap;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;

#[path = "github_copilot_limits.rs"]
pub mod github_copilot_limits;

struct TokenBaseUrlAuth(crate::auth::helpers::EnvApiKeyAuth);

#[async_trait::async_trait]
impl crate::auth::types::ApiKeyAuth for TokenBaseUrlAuth {
    fn name(&self) -> &str {
        self.0.name()
    }
    fn has_login(&self) -> bool {
        self.0.has_login()
    }
    async fn login(
        &self,
        interaction: &crate::auth::types::ProviderAuthInteraction,
    ) -> anyhow::Result<crate::auth::types::ApiKeyCredential> {
        self.0.login(interaction).await
    }
    async fn resolve(
        &self,
        ctx: &dyn crate::auth::types::AuthContext,
        credential: Option<&crate::auth::types::ApiKeyCredential>,
        signal: &crate::utils::abort::AbortSignal,
    ) -> anyhow::Result<Option<crate::auth::types::AuthResult>> {
        let mut result = self.0.resolve(ctx, credential, signal).await?;
        if let Some(result) = &mut result
            && result.auth.base_url.is_none()
        {
            result.auth.base_url = result
                .auth
                .api_key
                .as_deref()
                .and_then(crate::auth::oauth::github_copilot::get_base_url_from_token);
        }
        Ok(result)
    }
}

fn github_copilot_filter_models(models: Vec<Model>, credential: Option<&Credential>) -> Vec<Model> {
    let Some(credential) = credential else {
        return models;
    };
    if credential.get("type").and_then(Value::as_str) != Some("oauth") {
        return models;
    }
    let Some(available_model_ids) = credential
        .get("availableModelIds")
        .and_then(Value::as_array)
    else {
        return models;
    };
    let mut available = HashSet::with_capacity(available_model_ids.len());
    for id in available_model_ids {
        match id.as_str() {
            Some(id) => available.insert(id.to_owned()),
            None => return models,
        };
    }
    github_copilot_limits::apply_github_copilot_model_limits(
        models
            .into_iter()
            .filter(|model| available.contains(&model.id))
            .collect(),
        credential,
    )
}

pub fn github_copilot_provider() -> Result<Arc<dyn Provider>, crate::models::ModelsError> {
    let mut api = IndexMap::new();
    api.insert(
        "anthropic-messages".to_owned(),
        builtin_api_streams("anthropic-messages"),
    );
    api.insert(
        "openai-completions".to_owned(),
        builtin_api_streams("openai-completions"),
    );
    api.insert(
        "openai-responses".to_owned(),
        builtin_api_streams("openai-responses"),
    );
    let filter: FilterModels = Arc::new(github_copilot_filter_models);
    create_provider(CreateProviderOptions {
        id: "github-copilot".into(),
        name: Some("GitHub Copilot".into()),
        base_url: Some("https://api.individual.githubcopilot.com".into()),
        headers: None,
        auth: Some(ProviderAuth {
            api_key: Some(Arc::new(TokenBaseUrlAuth(env_api_key_auth(
                "GitHub Copilot token",
                &["COPILOT_GITHUB_TOKEN"],
            )))),
            oauth: Some(Arc::new(
                lazy_oauth("GitHub Copilot", true, None, || {
                    Box::pin(crate::auth::oauth::load::load_github_copilot_oauth())
                })
                .with_rejected_token_statuses(&[401, 403]),
            )),
        }),
        models: super::github_copilot_models::github_copilot_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: Some(filter),
        images: indexmap::IndexMap::new(),
        api: ProviderApi::ByApi(api),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::types::{ApiKeyAuth, ApiKeyCredential, AuthContext};

    struct Env;
    #[async_trait::async_trait]
    impl AuthContext for Env {
        async fn env(&self, name: &str) -> Option<String> {
            (name == "COPILOT_GITHUB_TOKEN")
                .then(|| "proxy-ep=proxy.business.githubcopilot.com;".into())
        }
        async fn file_exists(&self, _: &str) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn selects_token_host_for_both_env_and_stored_api_keys() {
        let auth = TokenBaseUrlAuth(env_api_key_auth("token", &["COPILOT_GITHUB_TOKEN"]));
        let signal = crate::utils::abort::AbortController::new().signal();
        let ambient = auth.resolve(&Env, None, &signal).await.unwrap().unwrap();
        assert_eq!(
            ambient.auth.base_url.as_deref(),
            Some("https://api.business.githubcopilot.com")
        );
        let stored = ApiKeyCredential {
            key: Some("proxy-ep=proxy.enterprise.githubcopilot.com;".into()),
            env: None,
            ..Default::default()
        };
        let result = auth
            .resolve(&Env, Some(&stored), &signal)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            result.auth.base_url.as_deref(),
            Some("https://api.enterprise.githubcopilot.com")
        );
    }
}
