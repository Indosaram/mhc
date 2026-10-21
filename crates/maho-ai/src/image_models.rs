//! Port of senpi packages/ai/src/image-models.ts.

use crate::image_models_generated::IMAGE_MODELS as GENERATED_IMAGE_MODELS;
use crate::model::{ModelImageInputLimits, ModelImageResizeOptions, ModelInputLimits};
use crate::types::InputModality;
use indexmap::IndexMap;
use std::sync::LazyLock;

static IMAGE_MODELS: LazyLock<IndexMap<String, IndexMap<String, ImagesModel>>> = LazyLock::new(|| {
    let mut catalogs = GENERATED_IMAGE_MODELS.clone();
    for model in catalogs.values_mut().flat_map(IndexMap::values_mut) {
        apply_image_input_metadata(model);
    }
    catalogs
});

fn apply_image_input_metadata(model: &mut ImagesModel) {
    if !model.input.contains(&InputModality::Image) {
        return;
    }
    let limits = model.input_limits.get_or_insert_with(ModelInputLimits::default);
    let (request_bytes, per_message, per_request) = match model.provider.as_str() {
        "anthropic" => (Some(32 * 1024 * 1024), None, Some(600)),
        "amazon-bedrock" => (None, Some(20), None),
        "openai" => (Some(512 * 1024 * 1024), None, Some(1500)),
        "google" => (Some(20 * 1024 * 1024), None, Some(3600)),
        _ => (None, None, None),
    };
    limits.max_request_bytes = limits.max_request_bytes.or(request_bytes);
    let images = limits.images.get_or_insert_with(ModelImageInputLimits::default);
    images.max_per_message = images.max_per_message.or(per_message);
    images.max_per_request = images.max_per_request.or(per_request);
    let resize = images.resize.get_or_insert_with(ModelImageResizeOptions::default);
    resize.max_width = resize.max_width.or(Some(2000));
    resize.max_height = resize.max_height.or(Some(2000));
    resize.max_bytes = resize.max_bytes.or(Some(4_718_592));
    resize.jpeg_quality = resize.jpeg_quality.or(Some(80));
}
use crate::types::ImagesModel;

pub fn get_image_model(provider: &str, model_id: &str) -> Option<&'static ImagesModel> {
    IMAGE_MODELS.get(provider)?.get(model_id)
}

pub fn get_image_providers() -> Vec<&'static str> {
    IMAGE_MODELS.keys().map(String::as_str).collect()
}

pub fn get_image_models(provider: &str) -> Vec<&'static ImagesModel> {
    IMAGE_MODELS.get(provider).map(|models| models.values().collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ImagesOutputModality, InputModality};

    fn registers_with_editing_and_pricing(id: &str) {
        let model = get_image_model("openai", id).expect("image model");
        assert_eq!((model.id.as_str(), model.api.as_str()), (id, "openai-images"));
        assert_eq!(model.input, vec![InputModality::Text, InputModality::Image]);
        assert_eq!(model.output, vec![ImagesOutputModality::Image]);
        assert_eq!(
            (model.cost.input, model.cost.output, model.cost.cache_read, model.cost.cache_write),
            (5.0, 30.0, 1.25, 0.0)
        );
    }

    #[test]
    fn registers_gpt_image_2_5_sunburst_with_editing_and_pricing() {
        registers_with_editing_and_pricing("gpt-image-2.5-sunburst");
    }

    #[test]
    fn registers_gpt_image_2_5_flare_with_editing_and_pricing() {
        registers_with_editing_and_pricing("gpt-image-2.5-flare");
    }

    #[test]
    fn unknown_image_provider_and_model_are_absent() {
        assert!(get_image_model("openai", "missing").is_none());
        assert!(get_image_models("nope").is_empty());
        assert!(get_image_providers().contains(&"openai"));
    }

    #[test]
    fn image_catalog_exposes_upstream_resize_and_request_metadata() {
        // Given the OpenAI image catalog.
        let model = get_image_model("openai", "gpt-image-2.5-sunburst").expect("model");
        // When input metadata is read through the public catalog.
        let limits = model.input_limits.as_ref().expect("limits");
        let images = limits.images.as_ref().expect("images");
        let resize = images.resize.as_ref().expect("resize");
        // Then metadata matches the generator without enforcing a new request policy.
        assert_eq!(limits.max_request_bytes, Some(512 * 1024 * 1024));
        assert_eq!(images.max_per_request, Some(1500));
        assert_eq!(resize.max_width, Some(2000));
        assert_eq!(resize.max_height, Some(2000));
        assert_eq!(resize.max_bytes, Some(4_718_592));
        assert_eq!(resize.jpeg_quality, Some(80));
    }

    #[test]
    fn explicit_image_metadata_takes_precedence_over_provider_defaults() {
        // Given independently configured resize and request metadata.
        let mut model = get_image_model("openai", "gpt-image-2.5-sunburst").expect("model").clone();
        model.input_limits = Some(ModelInputLimits {
            max_request_bytes: Some(1234),
            images: Some(ModelImageInputLimits {
                max_per_message: Some(2), max_per_request: Some(3),
                resize: Some(ModelImageResizeOptions {
                    max_width: None, max_height: Some(480),
                    max_bytes: Some(4096), jpeg_quality: Some(90),
                }),
            }),
        });
        // When upstream metadata defaults are applied.
        apply_image_input_metadata(&mut model);
        // Then explicit values survive and absent dimensions receive defaults.
        let limits = model.input_limits.expect("limits");
        let images = limits.images.expect("images");
        assert_eq!(limits.max_request_bytes, Some(1234));
        assert_eq!(images.max_per_message, Some(2));
        assert_eq!(images.max_per_request, Some(3));
        assert_eq!(images.resize, Some(ModelImageResizeOptions {
            max_width: Some(2000), max_height: Some(480),
            max_bytes: Some(4096), jpeg_quality: Some(90),
        }));
    }
}
