//! Port of senpi packages/ai/src/context-provenance.ts.

use serde_json::{Map, Value};

pub const CONTEXT_PROVENANCE_FIELD: &str = "__piContextProvenance";

pub type ContextProvenance = Map<String, Value>;

pub fn get_context_provenance(value: &Value) -> Option<&ContextProvenance> {
    value.as_object()?.get(CONTEXT_PROVENANCE_FIELD)?.as_object()
}

pub fn copy_context_provenance(source: &Value, mut target: Map<String, Value>) -> Map<String, Value> {
    if let Some(provenance) = get_context_provenance(source) {
        target.insert(CONTEXT_PROVENANCE_FIELD.into(), Value::Object(provenance.clone()));
    }
    target
}

/// JSON of the value without its provenance field, for equality that ignores provenance.
pub fn context_provenance_fingerprint(value: &Value) -> Option<String> {
    let mut clone = value.as_object()?.clone();
    clone.shift_remove(CONTEXT_PROVENANCE_FIELD);
    serde_json::to_string(&clone).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_copies_and_fingerprints() {
        let source = json!({"a": 1, "__piContextProvenance": {"origin": "x"}});
        assert_eq!(get_context_provenance(&source), json!({"origin": "x"}).as_object());
        assert_eq!(get_context_provenance(&json!({"__piContextProvenance": [1]})), None);
        assert_eq!(get_context_provenance(&json!([1])), None);
        let copied = copy_context_provenance(&source, Map::new());
        assert_eq!(Value::Object(copied), json!({"__piContextProvenance": {"origin": "x"}}));
        assert_eq!(context_provenance_fingerprint(&source).as_deref(), Some(r#"{"a":1}"#));
        assert_eq!(context_provenance_fingerprint(&json!("s")), None);
    }
}
