//! Port of senpi packages/ai/src/providers/images/register-builtins.ts.
//!
//! The TS lazy `import("../../api/<api>.ts")` maps onto a direct call into the wire module (Rust
//! has no import failure to turn into a lazy-load error). The TS import-time side effect
//! `registerBuiltInImagesApiProviders()` maps onto `ensure_built_in_images_api_providers`, which
//! `images::generate_images` runs once (images.ts imports this module), plus an explicit call from
//! the CLI setup.

use crate::images_api_registry::register_builtin_images_api_provider;
use crate::types::{AssistantImages, BoxFuture, ImagesContext, ImagesModel, ImagesOptions, ProviderImages};
use std::sync::{Arc, Once};

struct OpenRouterImages;

impl ProviderImages for OpenRouterImages {
    fn generate_images<'a>(
        &'a self,
        model: &'a ImagesModel,
        context: &'a ImagesContext,
        options: Option<ImagesOptions>,
    ) -> BoxFuture<'a, AssistantImages> {
        Box::pin(crate::api::openrouter_images::generate_images(model, context, options))
    }
}

struct OpenAiImages;

impl ProviderImages for OpenAiImages {
    fn generate_images<'a>(
        &'a self,
        model: &'a ImagesModel,
        context: &'a ImagesContext,
        options: Option<ImagesOptions>,
    ) -> BoxFuture<'a, AssistantImages> {
        Box::pin(crate::api::openai_images::generate_images(model, context, options))
    }
}

pub fn generate_images_openrouter() -> Arc<dyn ProviderImages> {
    Arc::new(OpenRouterImages)
}

pub fn generate_images_openai() -> Arc<dyn ProviderImages> {
    Arc::new(OpenAiImages)
}

pub fn register_built_in_images_api_providers() {
    register_builtin_images_api_provider("openrouter-images", generate_images_openrouter());
    register_builtin_images_api_provider("openai-images", generate_images_openai());
}

/// The once-per-process equivalent of the TS module-load registration.
pub fn ensure_built_in_images_api_providers() {
    static REGISTERED: Once = Once::new();
    REGISTERED.call_once(register_built_in_images_api_providers);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::images_api_registry::get_images_api_provider;

    #[test]
    fn registers_a_lazy_generate_images_for_each_builtin_images_api() {
        ensure_built_in_images_api_providers();
        for api in ["openai-images", "openrouter-images"] {
            let provider = get_images_api_provider(api).expect("registry").expect("builtin registered");
            assert_eq!(provider.api, api);
        }
    }
}
