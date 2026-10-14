//! Port of senpi packages/ai/src/providers/moonshotai-cn.ts.

use crate::auth::types::ProviderAuth;
use crate::auth::helpers::env_api_key_auth;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn moonshotai_cn_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "moonshotai-cn".into(),
        name: Some("Moonshot AI CN".into()),
        base_url: Some("https://api.moonshot.cn/v1".into()),
        headers: None,
        auth: Some(ProviderAuth { api_key: Some(Arc::new(env_api_key_auth("Moonshot AI API key", &["MOONSHOT_API_KEY"]))), oauth: None }),
        models: super::moonshotai_cn_models::moonshotai_cn_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
