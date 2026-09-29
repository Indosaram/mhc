//! Port of senpi packages/ai/src/providers/groq.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn groq_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "groq".into(),
        name: Some("Groq".into()),
        base_url: Some("https://api.groq.com/openai/v1".into()),
        headers: None,
        models: super::groq::groq_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
