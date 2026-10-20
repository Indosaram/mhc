//! Port of senpi packages/ai/src/providers/meta.ts.

use crate::auth::helpers::{env_api_key_auth, lazy_oauth};
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use std::sync::Arc;

pub fn meta_provider() -> Result<Arc<dyn Provider>, crate::models::ModelsError> {
    create_provider(CreateProviderOptions {
        id: "meta".into(),
        name: Some("Meta".into()),
        base_url: Some("https://api.meta.ai/v1".into()),
        headers: None,
        auth: Some(ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth(
                "Meta Model API key",
                &["META_API_KEY"],
            ))),
            oauth: Some(Arc::new(lazy_oauth(
                "Meta (Muse subscription)",
                true,
                Some("Sign in with Meta".into()),
                || Box::pin(crate::auth::oauth::load::load_meta_oauth()),
            ))),
        }),
        models: super::builtin_provider_models("meta"),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        images: indexmap::IndexMap::new(),
        api: ProviderApi::Single(super::builtin_api_streams("openai-responses")),
    })
}
