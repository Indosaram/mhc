//! Port of senpi packages/ai/src/image-models.ts.

use crate::image_models_generated::IMAGE_MODELS;
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
}
