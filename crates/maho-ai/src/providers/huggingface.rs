//! Port of senpi packages/ai/src/providers/huggingface.ts.

use crate::auth::types::ProviderAuth;
use crate::auth::helpers::env_api_key_auth;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn huggingface_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "huggingface".into(),
        name: Some("Hugging Face".into()),
        base_url: Some("https://router.huggingface.co/v1".into()),
        headers: None,
        auth: Some(ProviderAuth { api_key: Some(Arc::new(env_api_key_auth("Hugging Face token", &["HF_TOKEN"]))), oauth: None }),
        models: super::huggingface_models::huggingface_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
