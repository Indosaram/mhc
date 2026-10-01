//! Port of senpi packages/ai/src/providers/xai.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn xai_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "xai".into(),
        name: Some("xAI".into()),
        base_url: Some("https://api.x.ai/v1".into()),
        headers: None,
        models: super::xai_models::xai_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-responses")),
    })
}
