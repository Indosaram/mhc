//! Port of senpi packages/ai/src/providers/zai.ts.

use crate::auth::types::ProviderAuth;
use crate::auth::helpers::env_api_key_auth;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn zai_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "zai".into(),
        name: Some("Z.AI".into()),
        base_url: Some("https://api.z.ai/api/coding/paas/v4".into()),
        headers: None,
        auth: Some(ProviderAuth { api_key: Some(Arc::new(env_api_key_auth("Z.AI API key", &["ZAI_API_KEY"]))), oauth: None }),
        models: super::zai_models::zai_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
