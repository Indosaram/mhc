//! Port of senpi packages/ai/src/providers/xai.ts.

use crate::auth::types::ProviderAuth;
use crate::auth::helpers::{env_api_key_auth, lazy_oauth};
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn xai_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "xai".into(),
        name: Some("xAI".into()),
        base_url: Some("https://api.x.ai/v1".into()),
        headers: None,
        auth: Some(ProviderAuth { api_key: Some(Arc::new(env_api_key_auth("xAI API key", &["XAI_API_KEY"]))), oauth: Some(Arc::new(lazy_oauth("xAI (Grok/X subscription)", true, Some("Sign in with SuperGrok or X Premium".into()), || Box::pin(crate::auth::oauth::load::load_xai_oauth())))) }),
        models: super::xai_models::xai_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-responses")),
    })
}
