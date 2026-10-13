//! Port of senpi packages/ai/src/providers/openrouter.ts.

use crate::auth::types::ProviderAuth;
use crate::auth::helpers::{env_api_key_auth, lazy_oauth};
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use indexmap::IndexMap;
use std::sync::Arc;

pub fn openrouter_provider() -> Arc<dyn Provider> {
    let mut api = IndexMap::new();
    api.insert("anthropic-messages".to_owned(), builtin_api_streams("anthropic-messages"));
    api.insert("openai-completions".to_owned(), builtin_api_streams("openai-completions"));
    create_provider(CreateProviderOptions {
        id: "openrouter".into(),
        name: Some("OpenRouter".into()),
        base_url: Some("https://openrouter.ai/api/v1".into()),
        headers: None,
        auth: Some(ProviderAuth { api_key: Some(Arc::new(env_api_key_auth("OpenRouter API key", &["OPENROUTER_API_KEY"]))), oauth: Some(Arc::new(lazy_oauth("OpenRouter OAuth", false, Some("Sign in with OpenRouter".into()), || Box::pin(crate::auth::oauth::load::load_open_router_oauth())))) }),
        models: super::openrouter_models::openrouter_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::ByApi(api),
    })
}
