//! Port of senpi packages/ai/src/providers/azure-openai-responses.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn azure_openai_responses_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "azure-openai-responses".into(),
        name: Some("Azure OpenAI".into()),
        base_url: None,
        headers: None,
        models: super::azure_openai_responses_models::azure_openai_responses_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("azure-openai-responses")),
    })
}
