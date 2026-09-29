//! Port of senpi packages/ai/src/images.ts. The builtin images APIs are registered by
//! providers/images (todo 12); the OpenAI image params re-export lives with api/openai-images.

use crate::images_api_registry::get_images_api_provider;
use crate::types::{AssistantImages, ImagesContext, ImagesModel, ImagesStopReason, ProviderImagesOptions};
use crate::utils::diagnostics::now_ms;

pub(crate) fn images_error(model: &ImagesModel, message: String) -> AssistantImages {
    AssistantImages {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        output: Vec::new(),
        response_id: None,
        usage: None,
        background: None,
        stop_reason: ImagesStopReason::Error,
        error_message: Some(message),
        timestamp: now_ms(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GenerateImagesError {
    #[error(transparent)]
    Scope(#[from] crate::node::provider_scope::ProviderScopeError),
    #[error("No API provider registered for api: {0}")]
    NoProvider(String),
    #[error(transparent)]
    Mismatched(#[from] crate::images_api_registry::MismatchedImagesApi),
}

pub async fn generate_images(
    model: &ImagesModel,
    context: &ImagesContext,
    options: Option<ProviderImagesOptions>,
) -> Result<AssistantImages, GenerateImagesError> {
    let provider = get_images_api_provider(&model.api)?.ok_or_else(|| GenerateImagesError::NoProvider(model.api.clone()))?;
    Ok(provider.generate_images(model, context, options)?.await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unregistered_images_api_rejects() {
        let mut model = crate::image_models::get_image_model("openai", "gpt-image-2.5-flare").expect("model").clone();
        model.api = "maho-unregistered-images".into();
        let result = generate_images(&model, &ImagesContext::default(), None).await;
        assert_eq!(result, Err(GenerateImagesError::NoProvider("maho-unregistered-images".into())));
    }
}
