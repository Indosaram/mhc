//! Port of senpi packages/ai/src/providers/opengateway.ts.

use crate::auth::types::ProviderAuth;
use crate::auth::helpers::env_api_key_auth;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn opengateway_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "opengateway".into(),
        name: Some("OpenGateway".into()),
        base_url: Some("https://apis.opengateway.ai/v1".into()),
        headers: None,
        auth: Some(ProviderAuth { api_key: Some(Arc::new(env_api_key_auth("OpenGateway API key", &["OPENGATEWAY_API_KEY"]))), oauth: None }),
        models: super::opengateway_models::opengateway_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
