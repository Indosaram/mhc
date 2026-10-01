//! Port of senpi packages/ai/src/providers/alibaba-token-plan.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn alibaba_token_plan_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "alibaba-token-plan".into(),
        name: Some("Alibaba Token Plan".into()),
        base_url: Some("https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1".into()),
        headers: None,
        models: super::alibaba_token_plan_models::alibaba_token_plan_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
