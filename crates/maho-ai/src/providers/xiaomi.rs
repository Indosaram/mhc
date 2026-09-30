//! Port of senpi packages/ai/src/providers/xiaomi.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn xiaomi_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "xiaomi".into(),
        name: Some("Xiaomi".into()),
        base_url: Some("https://api.xiaomimimo.com/v1".into()),
        headers: None,
        models: super::xiaomi_models::xiaomi_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
