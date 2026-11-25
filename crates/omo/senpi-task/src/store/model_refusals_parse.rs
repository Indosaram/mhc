use serde_json::Value;

use crate::state::{MODEL_REFUSAL_REASONS, ModelRefusal};

/// `model_refusals` is diagnostic provenance: a malformed entry or a reason a newer build added
/// is dropped, never a reason to lose the whole record.
pub fn parse_optional_model_refusals(record: &Value) -> Option<Vec<ModelRefusal>> {
    let items = record.get("model_refusals")?.as_array()?;
    let refusals: Vec<ModelRefusal> = items.iter().filter_map(parse_refusal).collect();
    if refusals.is_empty() {
        None
    } else {
        Some(refusals)
    }
}

fn parse_refusal(value: &Value) -> Option<ModelRefusal> {
    let object = value.as_object()?;
    let reason_str = object.get("reason")?.as_str()?;
    let reason = MODEL_REFUSAL_REASONS
        .into_iter()
        .find(|candidate| candidate.as_str() == reason_str)?;
    let model = object.get("model")?.as_str()?.to_owned();
    let at = object.get("at")?.as_str()?.to_owned();
    let detail = optional_string(object, "detail")?;
    let resets = optional_string(object, "resets")?;
    Some(ModelRefusal {
        model,
        reason,
        at,
        detail,
        resets,
    })
}

fn optional_string(object: &serde_json::Map<String, Value>, key: &str) -> Option<Option<String>> {
    match object.get(key) {
        None => Some(None),
        Some(Value::String(text)) => Some(Some(text.clone())),
        Some(_) => None,
    }
}
