use super::system_one_shared::{SystemOneTransport, classify_system_one};
use crate::types::{ClassifierContext, ClassifierModel, ClassifierOptions, ClassifierResult};
use serde_json::Value;

pub struct TypeSafeSystemOne;

impl SystemOneTransport for TypeSafeSystemOne {
    fn api(&self) -> &str {
        "typesafe-system-one"
    }
    fn label(&self) -> &str {
        "System One API"
    }
    fn url(&self, model: &ClassifierModel) -> String {
        format!("{}/systemone", model.base_url.trim_end_matches('/'))
    }
    fn payload(&self, model: &ClassifierModel, mut request: Value) -> Value {
        request["model"] = Value::String(model.id.clone());
        request
    }
    fn output(&self, body: Value) -> Result<Value, String> {
        if body.is_object() {
            Ok(body)
        } else {
            Err("System One API returned an unexpected response".into())
        }
    }
}

pub async fn classify(
    model: &ClassifierModel,
    context: &ClassifierContext,
    options: Option<ClassifierOptions>,
) -> ClassifierResult {
    classify_system_one(&TypeSafeSystemOne, model, context, options).await
}
