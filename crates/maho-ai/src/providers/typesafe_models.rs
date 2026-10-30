//! Port of senpi packages/ai/src/providers/typesafe.models.ts (generated from `providers/data/typesafe.json`).
//!
//! The three exports flatten the shard by model type, like `flatten{Chat,Image,Classifier}ModelCatalog`.
//! They are functions returning owned values (the crate's `*_models.rs` convention) because
//! callers need owned `Vec`s for `CreateProviderOptions`.

use crate::types::{ClassifierModel, ImagesModel, Model};

/// `TYPESAFE_MODELS`.
pub fn typesafe_models() -> Vec<Model> {
    Vec::new()
}

/// `TYPESAFE_IMAGE_MODELS`: the shard has no image models.
pub fn typesafe_image_models() -> Vec<ImagesModel> {
    Vec::new()
}

/// `TYPESAFE_CLASSIFIER_MODELS`.
pub fn typesafe_classifier_models() -> Vec<ClassifierModel> {
    crate::models_generated::CLASSIFIER_MODELS
        .get("typesafe")
        .map(|models| models.values().cloned().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattens_the_shard_by_model_type() {
        assert_eq!(typesafe_models().len(), 0);
        assert!(
            typesafe_models()
                .iter()
                .all(|model| model.provider == "typesafe")
        );
        assert!(typesafe_image_models().is_empty());
        assert_eq!(typesafe_classifier_models().len(), 1);
    }
}
