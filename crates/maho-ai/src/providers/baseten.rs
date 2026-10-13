//! Port of senpi packages/ai/src/providers/baseten.ts.

use crate::auth::types::ProviderAuth;
use crate::auth::helpers::env_api_key_auth;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn baseten_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "baseten".into(),
        name: Some("Baseten".into()),
        base_url: Some("https://inference.baseten.co/v1".into()),
        headers: None,
        auth: Some(ProviderAuth { api_key: Some(Arc::new(env_api_key_auth("Baseten API key", &["BASETEN_API_KEY"]))), oauth: None }),
        models: super::baseten_models::baseten_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
