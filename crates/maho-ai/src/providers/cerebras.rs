//! Port of senpi packages/ai/src/providers/cerebras.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn cerebras_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "cerebras".into(),
        name: Some("Cerebras".into()),
        base_url: Some("https://api.cerebras.ai/v1".into()),
        headers: None,
        models: super::cerebras::cerebras_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
