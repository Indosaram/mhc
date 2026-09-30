//! Port of senpi packages/ai/src/providers/openai-images.ts.

use crate::images_models::{CreateImagesProviderOptions, ImagesProvider, create_images_provider};
use crate::providers::{
    builtin_images_provider_models, env_api_key_images_auth, images::register_builtins::generate_images_openai,
};
use std::sync::Arc;

pub fn openai_images_provider() -> Arc<dyn ImagesProvider> {
    create_images_provider(CreateImagesProviderOptions {
        id: "openai".into(),
        name: Some("OpenAI".into()),
        auth: env_api_key_images_auth("openai"),
        models: builtin_images_provider_models("openai"),
        refresh_models: None,
        api: generate_images_openai(),
    })
}
