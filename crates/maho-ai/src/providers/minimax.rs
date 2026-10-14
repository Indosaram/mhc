//! Port of senpi packages/ai/src/providers/minimax.ts.

use crate::auth::types::ProviderAuth;
use crate::auth::helpers::env_api_key_auth;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn minimax_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "minimax".into(),
        name: Some("MiniMax".into()),
        base_url: Some("https://api.minimax.io/anthropic".into()),
        headers: None,
        auth: Some(ProviderAuth { api_key: Some(Arc::new(env_api_key_auth("MiniMax API key", &["MINIMAX_API_KEY"]))), oauth: None }),
        models: super::minimax_models::minimax_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("anthropic-messages")),
    })
}
