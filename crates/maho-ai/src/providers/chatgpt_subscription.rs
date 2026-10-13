//! Port of senpi packages/ai/src/providers/chatgpt-subscription.ts.

use crate::auth::types::ProviderAuth;
use crate::auth::helpers::lazy_oauth;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn chatgpt_subscription_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "chatgpt-subscription".into(),
        name: Some("ChatGPT Subscription".into()),
        base_url: Some("https://chatgpt.com/backend-api".into()),
        headers: None,
        auth: Some(ProviderAuth { api_key: None, oauth: Some(Arc::new(lazy_oauth("ChatGPT Subscription (Plus/Pro)", true, None, || Box::pin(crate::auth::oauth::load::load_chatgpt_subscription_oauth())))) }),
        models: super::chatgpt_subscription_models::chatgpt_subscription_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-codex-responses")),
    })
}
