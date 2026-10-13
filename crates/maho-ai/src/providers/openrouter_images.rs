//! Port of senpi packages/ai/src/providers/openrouter-images.ts.

use crate::auth::helpers::{env_api_key_auth, lazy_oauth};
use crate::auth::oauth::load::load_open_router_oauth;
use crate::auth::types::ProviderAuth;
use crate::images_models::{CreateImagesProviderOptions, ImagesProvider, create_images_provider};
use crate::providers::{builtin_images_provider_models, images::register_builtins::generate_images_openrouter};
use std::sync::Arc;

pub fn openrouter_images_provider() -> Arc<dyn ImagesProvider> {
    create_images_provider(CreateImagesProviderOptions {
        id: "openrouter".into(),
        name: Some("OpenRouter".into()),
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth("OpenRouter API key", &["OPENROUTER_API_KEY"]))),
            oauth: Some(Arc::new(lazy_oauth("OpenRouter OAuth", false, Some("Sign in with OpenRouter".into()), || {
                Box::pin(load_open_router_oauth())
            }))),
        },
        models: builtin_images_provider_models("openrouter"),
        refresh_models: None,
        api: generate_images_openrouter(),
    })
}
