//! Port of senpi packages/ai/src/providers/opengateway.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn opengateway_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "opengateway".into(),
        name: Some("OpenGateway".into()),
        base_url: Some("https://apis.opengateway.ai/v1".into()),
        headers: None,
        models: super::opengateway::opengateway_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
