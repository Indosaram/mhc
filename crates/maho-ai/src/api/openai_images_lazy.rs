//! Port of senpi packages/ai/src/api/openai-images.lazy.ts.

use std::sync::Arc;

use crate::types::{
    AssistantImages, BoxFuture, ImagesContext, ImagesModel, ImagesOptions, ProviderImages,
};

struct OpenAiImagesApi;

impl ProviderImages for OpenAiImagesApi {
    fn generate_images<'a>(
        &'a self,
        model: &'a ImagesModel,
        context: &'a ImagesContext,
        options: Option<ImagesOptions>,
    ) -> BoxFuture<'a, Result<AssistantImages, String>> {
        Box::pin(async move {
            if model.api != "openai-images" {
                return Err(format!(
                    "Mismatched api: {} expected openai-images",
                    model.api
                ));
            }
            Ok(crate::api::openai_images::generate_images(model, context, options).await)
        })
    }
}

pub fn openai_images_api() -> Arc<dyn ProviderImages> {
    Arc::new(OpenAiImagesApi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mismatched_api_rejects() {
        let api = openai_images_api();
        let mut model = crate::image_models::get_image_model("openai", "gpt-image-2.5-flare")
            .expect("model")
            .clone();
        model.api = "openrouter-images".into();
        let images = api
            .generate_images(&model, &ImagesContext::default(), None)
            .await;
        assert_eq!(
            images,
            Err("Mismatched api: openrouter-images expected openai-images".into())
        );
    }
}
