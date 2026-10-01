//! Port of senpi packages/ai/src/providers/xiaomi-token-plan-ams.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn xiaomi_token_plan_ams_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "xiaomi-token-plan-ams".into(),
        name: Some("Xiaomi Token Plan AMS".into()),
        base_url: Some("https://token-plan-ams.xiaomimimo.com/v1".into()),
        headers: None,
        models: super::xiaomi_token_plan_ams_models::xiaomi_token_plan_ams_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
