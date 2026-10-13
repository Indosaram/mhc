//! Port of senpi packages/ai/src/providers/deepseek.ts.

use crate::auth::types::ProviderAuth;
use crate::auth::helpers::env_api_key_auth;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn deepseek_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "deepseek".into(),
        name: Some("DeepSeek".into()),
        base_url: Some("https://api.deepseek.com".into()),
        headers: None,
        auth: Some(ProviderAuth { api_key: Some(Arc::new(env_api_key_auth("DeepSeek API key", &["DEEPSEEK_API_KEY"]))), oauth: None }),
        models: super::deepseek_models::deepseek_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
