//! Port of senpi packages/ai/src/providers/xiaomi-token-plan-sgp.ts.

use crate::auth::types::ProviderAuth;
use crate::auth::helpers::env_api_key_auth;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn xiaomi_token_plan_sgp_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "xiaomi-token-plan-sgp".into(),
        name: Some("Xiaomi Token Plan SGP".into()),
        base_url: Some("https://token-plan-sgp.xiaomimimo.com/v1".into()),
        headers: None,
        auth: Some(ProviderAuth { api_key: Some(Arc::new(env_api_key_auth("Xiaomi Token Plan SGP API key", &["XIAOMI_TOKEN_PLAN_SGP_API_KEY"]))), oauth: None }),
        models: super::xiaomi_token_plan_sgp_models::xiaomi_token_plan_sgp_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
