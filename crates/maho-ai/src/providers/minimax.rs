//! Port of senpi packages/ai/src/providers/minimax.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn minimax_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "minimax".into(),
        name: Some("MiniMax".into()),
        base_url: Some("https://api.minimax.io/anthropic".into()),
        headers: None,
        models: super::minimax_models::minimax_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("anthropic-messages")),
    })
}
