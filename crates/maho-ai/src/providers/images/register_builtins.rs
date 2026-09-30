//! Port of senpi packages/ai/src/providers/images/register-builtins.ts.
//!
//! The TS lazy import("../../api/<api>.ts") boundary maps onto the images api registry todo 5
//! installed: the wire module registers itself as a builtin, and an unregistered api yields the
//! same lazy-load-error AssistantImages instead of throwing.

use crate::images::images_error;
use crate::images_api_registry::get_images_api_provider;
use crate::types::{AssistantImages, BoxFuture, ImagesContext, ImagesModel, ImagesOptions, ProviderImages};
use std::sync::Arc;

struct LazyImagesApi {
    api_id: &'static str,
}

impl ProviderImages for LazyImagesApi {
    fn generate_images<'a>(
        &'a self,
        model: &'a ImagesModel,
        context: &'a ImagesContext,
        options: Option<ImagesOptions>,
    ) -> BoxFuture<'a, AssistantImages> {
        Box::pin(async move {
            match get_images_api_provider(self.api_id) {
                Ok(Some(provider)) => match provider.generate_images(model, context, options) {
                    Ok(future) => future.await,
                    Err(error) => images_error(model, error.to_string()),
                },
                Ok(None) => images_error(model, format!("No images API provider registered for api: {}", self.api_id)),
                Err(error) => images_error(model, error.to_string()),
            }
        })
    }
}

pub fn generate_images_openrouter() -> Arc<dyn ProviderImages> {
    Arc::new(LazyImagesApi { api_id: "openrouter-images" })
}

pub fn generate_images_openai() -> Arc<dyn ProviderImages> {
    Arc::new(LazyImagesApi { api_id: "openai-images" })
}
