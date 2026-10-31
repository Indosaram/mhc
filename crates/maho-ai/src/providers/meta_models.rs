//! Port of senpi packages/ai/src/providers/meta.models.ts (generated from `providers/data/meta.json`).
//!
//! The three exports flatten the shard by model type, like `flatten{Chat,Image,Classifier}ModelCatalog`.
//! They are functions returning owned values (the crate's `*_models.rs` convention) because
//! callers need owned `Vec`s for `CreateProviderOptions`.

use crate::types::{ClassifierModel, ImagesModel, Model};

/// `META_MODELS`.
pub fn meta_models() -> Vec<Model> {
    crate::providers::builtin_provider_models("meta")
}

/// `META_IMAGE_MODELS`: the shard has no image models.
pub fn meta_image_models() -> Vec<ImagesModel> {
    Vec::new()
}

/// `META_CLASSIFIER_MODELS`.
pub fn meta_classifier_models() -> Vec<ClassifierModel> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattens_the_shard_by_model_type() {
        assert_eq!(meta_models().len(), 6);
        assert!(meta_models().iter().all(|model| model.provider == "meta"));
        assert!(meta_image_models().is_empty());
        assert_eq!(meta_classifier_models().len(), 0);
    }
}
