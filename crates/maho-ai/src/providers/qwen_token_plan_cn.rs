//! Port of senpi packages/ai/src/providers/qwen-token-plan-cn.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn qwen_token_plan_cn_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "qwen-token-plan-cn".into(),
        name: Some("Qwen Token Plan CN".into()),
        base_url: Some("https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1".into()),
        headers: None,
        models: super::qwen_token_plan_cn_models::qwen_token_plan_cn_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
