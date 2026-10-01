//! Port of senpi packages/ai/src/providers/amazon-bedrock.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn amazon_bedrock_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "amazon-bedrock".into(),
        name: Some("Amazon Bedrock".into()),
        base_url: None,
        headers: None,
        models: super::amazon_bedrock_models::amazon_bedrock_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("bedrock-converse-stream")),
    })
}
