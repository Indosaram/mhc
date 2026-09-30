//! Port of senpi packages/ai/src/providers/chatgpt-subscription.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn chatgpt_subscription_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "chatgpt-subscription".into(),
        name: Some("ChatGPT Subscription".into()),
        base_url: Some("https://chatgpt.com/backend-api".into()),
        headers: None,
        models: super::chatgpt_subscription_models::chatgpt_subscription_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-codex-responses")),
    })
}
