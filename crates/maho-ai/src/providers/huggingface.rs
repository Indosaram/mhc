//! Port of senpi packages/ai/src/providers/huggingface.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn huggingface_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "huggingface".into(),
        name: Some("Hugging Face".into()),
        base_url: Some("https://router.huggingface.co/v1".into()),
        headers: None,
        models: super::huggingface_models::huggingface_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
