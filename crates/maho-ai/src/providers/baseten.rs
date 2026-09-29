//! Port of senpi packages/ai/src/providers/baseten.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn baseten_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "baseten".into(),
        name: Some("Baseten".into()),
        base_url: Some("https://inference.baseten.co/v1".into()),
        headers: None,
        models: super::baseten::baseten_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
