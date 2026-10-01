//! Port of senpi packages/ai/src/providers/mistral.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn mistral_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "mistral".into(),
        name: Some("Mistral".into()),
        base_url: Some("https://api.mistral.ai".into()),
        headers: None,
        models: super::mistral_models::mistral_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("mistral-conversations")),
    })
}
