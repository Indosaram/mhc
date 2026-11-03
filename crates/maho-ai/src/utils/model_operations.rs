//! Port of senpi packages/ai/src/utils/model-operations.ts.

use crate::models::{ModelsError, ModelsErrorCode};
use crate::types::{
    AnyModel, AssistantImages, ClassifierModel, ClassifierResult, ClassifierStopReason,
    ImagesModel, ImagesStopReason, Model, ModelType,
};
use crate::utils::diagnostics::now_ms;

/// The type of a model. `AnyModel` carries its type in the variant; legacy chat models without
/// `type` deserialize into `AnyModel::Chat`.
pub fn get_model_type(model: &AnyModel) -> ModelType {
    model.model_type()
}

/// Runtime-checked model type test, including legacy chat models without `type`.
pub fn is_model_type(model: &AnyModel, model_type: ModelType) -> bool {
    get_model_type(model) == model_type
}

fn identity(model: &AnyModel) -> (&str, &str) {
    match model {
        AnyModel::Chat(model) => (&model.provider, &model.id),
        AnyModel::Image(model) => (&model.provider, &model.id),
        AnyModel::Classifier(model) => (&model.provider, &model.id),
    }
}

fn not_a(model: &AnyModel, kind: &str) -> ModelsError {
    let (provider, id) = identity(model);
    ModelsError::new(
        ModelsErrorCode::Provider,
        format!("Model {provider}/{id} is not {kind} model"),
    )
}

pub fn assert_chat_model(model: &AnyModel) -> Result<&Model, ModelsError> {
    match model {
        AnyModel::Chat(model) => Ok(model),
        _ => Err(not_a(model, "a chat")),
    }
}

pub fn assert_image_model(model: &AnyModel) -> Result<&ImagesModel, ModelsError> {
    match model {
        AnyModel::Image(model) => Ok(model),
        _ => Err(not_a(model, "an image")),
    }
}

pub fn assert_classifier_model(model: &AnyModel) -> Result<&ClassifierModel, ModelsError> {
    match model {
        AnyModel::Classifier(model) => Ok(model),
        _ => Err(not_a(model, "a classifier")),
    }
}

pub fn image_error_result(model: &ImagesModel, error: &str, aborted: bool) -> AssistantImages {
    AssistantImages {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        output: Vec::new(),
        response_id: None,
        usage: None,
        background: None,
        stop_reason: if aborted {
            ImagesStopReason::Aborted
        } else {
            ImagesStopReason::Error
        },
        error_message: Some(error.to_owned()),
        provider_diagnostic: None,
        timestamp: now_ms(),
    }
}

pub fn classifier_error_result(
    model: &ClassifierModel,
    error: &str,
    aborted: bool,
) -> ClassifierResult {
    ClassifierResult {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        answers: indexmap::IndexMap::new(),
        usage: None,
        stop_reason: if aborted {
            ClassifierStopReason::Aborted
        } else {
            ClassifierStopReason::Error
        },
        error_message: Some(error.to_owned()),
        timestamp: now_ms(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn classifier() -> AnyModel {
        serde_json::from_value(json!({
            "type": "classifier", "id": "jev", "name": "Jev", "api": "typesafe-system-one",
            "provider": "typesafe", "baseUrl": "https://api.typesafe.ai/v1/", "input": ["text"],
            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}, "contextWindow": 64000
        }))
        .expect("classifier model")
    }

    #[test]
    fn narrows_by_type_and_reports_the_model_identity_on_mismatch() {
        let model = classifier();
        assert_eq!(get_model_type(&model), ModelType::Classifier);
        assert!(is_model_type(&model, ModelType::Classifier));
        assert!(!is_model_type(&model, ModelType::Chat));
        assert!(assert_classifier_model(&model).is_ok());
        let error = assert_chat_model(&model).expect_err("not chat");
        assert_eq!(error.code, ModelsErrorCode::Provider);
        assert_eq!(error.message, "Model typesafe/jev is not a chat model");
        assert_eq!(
            assert_image_model(&model).expect_err("not image").message,
            "Model typesafe/jev is not an image model"
        );
    }

    #[test]
    fn classifier_error_result_marks_aborts() {
        let AnyModel::Classifier(model) = classifier() else {
            unreachable!()
        };
        let result = classifier_error_result(&model, "boom", true);
        assert_eq!(result.stop_reason, ClassifierStopReason::Aborted);
        assert_eq!(result.error_message.as_deref(), Some("boom"));
        assert!(result.answers.is_empty());
        assert_eq!(
            classifier_error_result(&model, "boom", false).stop_reason,
            ClassifierStopReason::Error
        );
    }
}
