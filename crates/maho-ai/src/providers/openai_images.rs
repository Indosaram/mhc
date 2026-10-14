//! Port of senpi packages/ai/src/providers/openai-images.ts.

use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::images_models::{CreateImagesProviderOptions, ImagesProvider, create_images_provider};
use crate::providers::{builtin_images_provider_models, images::register_builtins::generate_images_openai};
use std::sync::Arc;

pub fn openai_images_provider() -> Arc<dyn ImagesProvider> {
    create_images_provider(CreateImagesProviderOptions {
        id: "openai".into(),
        name: Some("OpenAI".into()),
        auth: ProviderAuth {
            api_key: Some(Arc::new(env_api_key_auth("OpenAI API key", &["OPENAI_API_KEY"]))),
            oauth: None,
        },
        models: builtin_images_provider_models("openai"),
        refresh_models: None,
        api: generate_images_openai(),
    })
}
