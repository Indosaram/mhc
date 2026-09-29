//! Port of senpi packages/ai/src/providers/moonshotai.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn moonshotai_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "moonshotai".into(),
        name: Some("Moonshot AI".into()),
        base_url: Some("https://api.moonshot.ai/v1".into()),
        headers: None,
        models: super::moonshotai::moonshotai_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
