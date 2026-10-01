//! Port of senpi packages/ai/src/api/openrouter-images.lazy.ts.

use std::sync::Arc;

use crate::types::{AssistantImages, BoxFuture, ImagesContext, ImagesModel, ImagesOptions, ProviderImages};

struct OpenRouterImagesApi;

impl ProviderImages for OpenRouterImagesApi {
    fn generate_images<'a>(
        &'a self,
        model: &'a ImagesModel,
        context: &'a ImagesContext,
        options: Option<ImagesOptions>,
    ) -> BoxFuture<'a, AssistantImages> {
        Box::pin(async move { crate::api::openrouter_images::generate_images(model, context, options).await })
    }
}

pub fn openrouter_images_api() -> Arc<dyn ProviderImages> {
    Arc::new(OpenRouterImagesApi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_api_key_yields_the_ts_error_message() {
        let api = openrouter_images_api();
        let mut model = crate::image_models::get_image_model("openrouter", "black-forest-labs/flux.2-pro").expect("model").clone();
        model.api = "openrouter-images".into();
        let images = api.generate_images(&model, &ImagesContext::default(), None).await;
        assert_eq!(images.error_message.as_deref(), Some("No API key for provider: openrouter"));
    }
}
