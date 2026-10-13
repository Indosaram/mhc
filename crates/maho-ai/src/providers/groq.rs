//! Port of senpi packages/ai/src/providers/groq.ts.

use crate::auth::types::ProviderAuth;
use crate::auth::helpers::env_api_key_auth;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn groq_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "groq".into(),
        name: Some("Groq".into()),
        base_url: Some("https://api.groq.com/openai/v1".into()),
        headers: None,
        auth: Some(ProviderAuth { api_key: Some(Arc::new(env_api_key_auth("Groq API key", &["GROQ_API_KEY"]))), oauth: None }),
        models: super::groq_models::groq_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
