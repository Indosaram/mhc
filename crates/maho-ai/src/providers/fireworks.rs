//! Port of senpi packages/ai/src/providers/fireworks.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use indexmap::IndexMap;
use std::sync::Arc;

pub fn fireworks_provider() -> Arc<dyn Provider> {
    let mut api = IndexMap::new();
    api.insert("anthropic-messages".to_owned(), builtin_api_streams("anthropic-messages"));
    api.insert("openai-completions".to_owned(), builtin_api_streams("openai-completions"));
    create_provider(CreateProviderOptions {
        id: "fireworks".into(),
        name: Some("Fireworks".into()),
        base_url: Some("https://api.fireworks.ai/inference".into()),
        headers: None,
        models: super::fireworks_models::fireworks_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::ByApi(api),
    })
}
