//! Port of senpi packages/ai/src/providers/mistral.ts.

use crate::auth::types::ProviderAuth;
use crate::auth::helpers::env_api_key_auth;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn mistral_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "mistral".into(),
        name: Some("Mistral".into()),
        base_url: Some("https://api.mistral.ai".into()),
        headers: None,
        auth: Some(ProviderAuth { api_key: Some(Arc::new(env_api_key_auth("Mistral API key", &["MISTRAL_API_KEY"]))), oauth: None }),
        models: super::mistral_models::mistral_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("mistral-conversations")),
    })
}
