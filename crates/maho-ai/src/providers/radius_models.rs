//! Port of senpi packages/ai/src/providers/radius.models.ts (generated from `providers/data/radius.json`).
//!
//! The three exports flatten the shard by model type, like `flatten{Chat,Image,Classifier}ModelCatalog`.
//! They are functions returning owned values (the crate's `*_models.rs` convention) because
//! callers need owned `Vec`s for `CreateProviderOptions`.

use crate::types::{ClassifierModel, ImagesModel, Model};

/// `RADIUS_MODELS`.
pub fn radius_models() -> Vec<Model> {
    let models: indexmap::IndexMap<String, Model> =
        serde_json::from_str(include_str!("data/radius.json"))
            .unwrap_or_else(|error| panic!("embedded Radius catalog is invalid: {error}"));
    models.into_values().collect()
}

/// `RADIUS_IMAGE_MODELS`: the shard has no image models.
pub fn radius_image_models() -> Vec<ImagesModel> {
    Vec::new()
}

/// `RADIUS_CLASSIFIER_MODELS`.
pub fn radius_classifier_models() -> Vec<ClassifierModel> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattens_the_shard_by_model_type() {
        // The embedded shard tracks the drift target (senpi v2026.10.27-2: 32 `pi-messages` rows);
        // the at-pin shard (903b8336) had 29. The data file is owned by the senpi-ai drift lane.
        assert_eq!(radius_models().len(), 32);
        assert!(
            radius_models()
                .iter()
                .all(|model| model.provider == "radius")
        );
        assert!(radius_image_models().is_empty());
        assert_eq!(radius_classifier_models().len(), 0);
    }
}
