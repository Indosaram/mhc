//! Port of senpi packages/ai/src/providers/openrouter-images.ts.

use crate::images_models::{CreateImagesProviderOptions, ImagesProvider, create_images_provider};
use crate::providers::{
    builtin_images_provider_models, env_api_key_images_auth, images::register_builtins::generate_images_openrouter,
};
use std::sync::Arc;

pub fn openrouter_images_provider() -> Arc<dyn ImagesProvider> {
    create_images_provider(CreateImagesProviderOptions {
        id: "openrouter".into(),
        name: Some("OpenRouter".into()),
        auth: env_api_key_images_auth("openrouter"),
        models: builtin_images_provider_models("openrouter"),
        refresh_models: None,
        api: generate_images_openrouter(),
    })
}
