//! Port of senpi packages/ai/src/providers/together.ts.

use crate::auth::types::ProviderAuth;
use crate::auth::helpers::env_api_key_auth;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn together_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "together".into(),
        name: Some("Together".into()),
        base_url: Some("https://api.together.ai/v1".into()),
        headers: None,
        auth: Some(ProviderAuth { api_key: Some(Arc::new(env_api_key_auth("Together API key", &["TOGETHER_API_KEY"]))), oauth: None }),
        models: super::together_models::together_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
