use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::js_value::get;
use crate::js_value::is_truthy;
use crate::js_value::js_string;
use crate::js_value::json_stringify;

/// A "did you mean" suggestion extracted from a model-not-found error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSuggestionInfo {
    pub provider_id: String,
    pub model_id: String,
    pub suggestion: String,
}

fn extract_message(error: &Value) -> String {
    match error {
        Value::String(text) => text.clone(),
        Value::Object(_) | Value::Array(_) => match get(Some(error), "message") {
            Some(Value::String(message)) => message.clone(),
            _ => json_stringify(error),
        },
        Value::Null | Value::Bool(_) | Value::Number(_) => js_string(error),
    }
}

/// Parses structured `ProviderModelNotFoundError` payloads (recursing into `data`/`error`/`cause`)
/// or `Model not found: p/m. Did you mean: x?` messages.
#[must_use]
#[expect(clippy::expect_used, reason = "static regex literals are valid")]
pub fn parse_model_suggestion(error: &Value) -> Option<ModelSuggestionInfo> {
    static MODEL_NOT_FOUND: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)model not found:\s*([^/\s]+)\s*/\s*([^.\s]+)").expect("valid regex")
    });
    static DID_YOU_MEAN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)did you mean:\s*([^,?]+)").expect("valid regex"));

    if !is_truthy(error) {
        return None;
    }

    if error.is_object() || error.is_array() {
        if get(Some(error), "name").and_then(Value::as_str) == Some("ProviderModelNotFoundError") {
            let data = get(Some(error), "data").filter(|data| data.is_object() || data.is_array());
            let suggestion = get(data, "suggestions")?.as_array()?.first()?.as_str()?;
            let field = |key: &str| {
                get(data, key)
                    .filter(|value| !value.is_null())
                    .map_or(String::new(), js_string)
            };
            return Some(ModelSuggestionInfo {
                provider_id: field("providerID"),
                model_id: field("modelID"),
                suggestion: suggestion.to_string(),
            });
        }

        for key in ["data", "error", "cause"] {
            let nested = get(Some(error), key)
                .filter(|nested| is_truthy(nested) && (nested.is_object() || nested.is_array()));
            if let Some(result) = nested.and_then(parse_model_suggestion) {
                return Some(result);
            }
        }
    }

    let message = extract_message(error);
    let model_match = MODEL_NOT_FOUND.captures(&message)?;
    let suggestion_match = DID_YOU_MEAN.captures(&message)?;
    let provider_id = model_match.get(1)?.as_str();
    let model_id = model_match.get(2)?.as_str();
    let suggestion = suggestion_match.get(1)?.as_str();

    Some(ModelSuggestionInfo {
        provider_id: provider_id.trim().to_string(),
        model_id: model_id.trim().to_string(),
        suggestion: suggestion.trim().to_string(),
    })
}
