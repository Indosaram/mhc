//! Port of senpi packages/ai/src/providers/minimax-cn.ts.

use crate::auth::types::ProviderAuth;
use crate::auth::helpers::env_api_key_auth;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn minimax_cn_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "minimax-cn".into(),
        name: Some("MiniMax CN".into()),
        base_url: Some("https://api.minimaxi.com/anthropic".into()),
        headers: None,
        auth: Some(ProviderAuth { api_key: Some(Arc::new(env_api_key_auth("MiniMax CN API key", &["MINIMAX_CN_API_KEY"]))), oauth: None }),
        models: super::minimax_cn_models::minimax_cn_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("anthropic-messages")),
    })
}
