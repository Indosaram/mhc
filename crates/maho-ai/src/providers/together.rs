//! Port of senpi packages/ai/src/providers/together.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn together_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "together".into(),
        name: Some("Together".into()),
        base_url: Some("https://api.together.ai/v1".into()),
        headers: None,
        models: super::together_models::together_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
