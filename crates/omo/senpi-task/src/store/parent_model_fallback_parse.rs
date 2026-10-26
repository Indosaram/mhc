use serde_json::Value;

use crate::state::ParentModelFallback;

/// Display provenance is best effort: malformed/newer blocks never orphan the task.
pub fn parse_optional_parent_model_fallback(record: &Value) -> Option<ParentModelFallback> {
    let value = record.get("parent_model_fallback")?;
    let object = value.as_object()?;
    for key in ["detail", "resets"] {
        if object.get(key).is_some_and(|value| !value.is_string()) {
            return None;
        }
    }
    if let Some(tools) = object.get("interrupted_tools")
        && !tools.as_array()?.iter().all(Value::is_string) {
            return None;
        }
    let mut fallback: ParentModelFallback = serde_json::from_value(value.clone()).ok()?;
    if fallback.interrupted_tools.as_ref().is_some_and(Vec::is_empty) {
        fallback.interrupted_tools = None;
    }
    Some(fallback)
}
