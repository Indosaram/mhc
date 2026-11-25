//! Port of senpi packages/ai/src/providers/chatgpt-subscription.ts.

use crate::auth::helpers::lazy_oauth;
use crate::auth::oauth::load::{load_chatgpt_subscription_oauth, load_openai_chatgpt_oauth};
use crate::auth::types::{
    AuthPrompt, AuthPromptKind, AuthPromptOption, LoginOptions, LogoutOptions, ModelAuth,
    OAuthAuth, OAuthCredential, OAuthRevocationResult, ProviderAuth, ProviderAuthInteraction,
};
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use crate::types::{
    AssistantMessageEventStream, Context, Model, ModelCompat, ProviderStreams, SimpleStreamOptions,
    StreamOptions,
};
use crate::utils::abort::AbortSignal;
use crate::utils::chatgpt_subscription_auth::extract_chatgpt_subscription_identity;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

const PROVIDER: &str = "chatgpt-subscription";
const PLAN_USAGE_BASE_URL: &str = "https://api.openai.com/v1";

struct SubscriptionOAuth {
    legacy: Arc<dyn OAuthAuth>,
    siwc: Arc<dyn OAuthAuth>,
}

impl SubscriptionOAuth {
    fn flow(&self, credential: &OAuthCredential) -> &dyn OAuthAuth {
        if credential.get_extra_str("route") == Some("siwc") {
            self.siwc.as_ref()
        } else {
            self.legacy.as_ref()
        }
    }
}

#[async_trait]
impl OAuthAuth for SubscriptionOAuth {
    fn name(&self) -> &str {
        self.legacy.name()
    }

    fn is_subscription(&self) -> bool {
        true
    }

    fn login_label(&self) -> Option<&str> {
        Some("Continue with ChatGPT")
    }

    fn account_identity(&self, access: &str) -> Option<String> {
        self.legacy.account_identity(access)
    }

    async fn login(
        &self,
        interaction: &ProviderAuthInteraction,
    ) -> anyhow::Result<OAuthCredential> {
        self.login_with_options(interaction, None).await
    }

    async fn login_with_options(
        &self,
        interaction: &ProviderAuthInteraction,
        options: Option<LoginOptions>,
    ) -> anyhow::Result<OAuthCredential> {
        let route = match options
            .as_ref()
            .and_then(|options| options.reauthorize.as_ref())
        {
            Some(credential) if credential.get_extra_str("route") == Some("siwc") => "siwc".into(),
            Some(_) => "legacy".into(),
            None => {
                interaction
                    .prompt(AuthPrompt {
                        kind: AuthPromptKind::Select {
                            message: "Sign in to ChatGPT Subscription:".into(),
                            options: [
                                ("siwc", "Continue with ChatGPT"),
                                ("legacy", "Legacy sign-in (existing users)"),
                            ]
                            .into_iter()
                            .map(|(id, label)| AuthPromptOption {
                                id: id.into(),
                                label: label.into(),
                                description: None,
                            })
                            .collect(),
                        },
                        signal: None,
                    })
                    .await?
            }
        };
        match route.as_str() {
            "siwc" => Ok(self
                .siwc
                .login_with_options(interaction, options)
                .await?
                .with_extra("route", Value::String(route))),
            "legacy" => self.legacy.login_with_options(interaction, options).await,
            _ => anyhow::bail!("Unknown ChatGPT sign-in route: {route}"),
        }
    }

    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: &AbortSignal,
    ) -> anyhow::Result<OAuthCredential> {
        let mut refreshed = self.flow(credential).refresh(credential, signal).await?;
        if credential.get_extra_str("route") == Some("siwc") {
            let mut extra = credential.extra.clone();
            extra.append(&mut refreshed.extra);
            extra.insert("route".into(), Value::String("siwc".into()));
            refreshed.extra = extra;
        }
        Ok(refreshed)
    }

    fn has_revoke(&self) -> bool {
        true
    }

    async fn revoke(
        &self,
        credential: &OAuthCredential,
        options: Option<LogoutOptions>,
    ) -> anyhow::Result<OAuthRevocationResult> {
        self.flow(credential).revoke(credential, options).await
    }

    async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
        let mut auth = self.flow(credential).to_auth(credential).await?;
        if credential.get_extra_str("route") == Some("siwc") {
            auth.base_url = Some(PLAN_USAGE_BASE_URL.into());
        }
        Ok(auth)
    }
}

/// Applies provider routing from the selected credential's resolved URL before dispatch.
/// Pass the auth override, not the model's configured/default endpoint.
pub fn apply_chatgpt_subscription_request_route(model: &mut Model, auth_base_url: Option<&str>) {
    if model.provider != PROVIDER || auth_base_url != Some(PLAN_USAGE_BASE_URL) {
        return;
    }
    model.api = "openai-responses".into();
    model.base_url = PLAN_USAGE_BASE_URL.into();
    shape_siwc_model(model);
}

fn shape_siwc_model(model: &mut Model) {
    if let Some(id) = &model.upstream_model_id {
        model.id.clone_from(id);
    }
    let compat = model
        .compat
        .get_or_insert_with(|| ModelCompat(Default::default()));
    compat.0.insert("siwc".into(), Value::Bool(true));
    compat.0.insert(
        "webSearchToolType".into(),
        Value::String("web_search".into()),
    );
}

struct SiwcResponses(Arc<dyn ProviderStreams>);

impl ProviderStreams for SiwcResponses {
    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        if crate::openai_responses_compat::uses_siwc(model) {
            return self.0.stream(model, context, options);
        }
        let mut model = model.clone();
        shape_siwc_model(&mut model);
        self.0.stream(&model, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        if crate::openai_responses_compat::uses_siwc(model) {
            return self.0.stream_simple(model, context, options);
        }
        let mut model = model.clone();
        shape_siwc_model(&mut model);
        self.0.stream_simple(&model, context, options)
    }
}

pub fn chatgpt_subscription_provider() -> Result<Arc<dyn Provider>, crate::models::ModelsError> {
    let legacy = lazy_oauth("ChatGPT Subscription (Plus/Pro)", true, None, || {
        Box::pin(load_chatgpt_subscription_oauth())
    })
    .with_account_identity(extract_chatgpt_subscription_identity);
    let siwc = lazy_oauth("ChatGPT Subscription", true, None, || {
        Box::pin(load_openai_chatgpt_oauth())
    })
    .with_revocation();
    create_provider(CreateProviderOptions {
        id: PROVIDER.into(),
        name: Some("ChatGPT Subscription".into()),
        base_url: Some("https://chatgpt.com/backend-api".into()),
        headers: None,
        auth: Some(ProviderAuth {
            api_key: None,
            oauth: Some(Arc::new(SubscriptionOAuth {
                legacy: Arc::new(legacy),
                siwc: Arc::new(siwc),
            })),
        }),
        models: super::chatgpt_subscription_models::chatgpt_subscription_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        images: indexmap::IndexMap::new(),
        api: ProviderApi::ByApi(indexmap::indexmap! {
            "openai-codex-responses".into() => builtin_api_streams("openai-codex-responses"),
            "openai-responses".into() => Arc::new(SiwcResponses(builtin_api_streams("openai-responses"))) as Arc<dyn ProviderStreams>,
        }),
    })
}

#[cfg(test)]
#[path = "chatgpt_subscription_login_tests.rs"]
mod login_tests;
