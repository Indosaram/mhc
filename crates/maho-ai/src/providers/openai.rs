//! Port of senpi packages/ai/src/providers/openai.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn openai_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "openai".into(),
        name: Some("OpenAI".into()),
        base_url: Some("https://api.openai.com/v1".into()),
        headers: None,
        models: super::openai_models::openai_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-responses")),
    })
}
