//! Persisted provenance validation and the insert-time constraint.
//!
//! Upstream counterpart: `thread/gateway/provenance-record.ts`.

use serde_json::Value;

use super::types::Provenance;

/// The worker boundary also serves untyped callers, so the shape is checked, not assumed.
pub fn is_provenance(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let non_empty = |key: &str| object.get(key).and_then(Value::as_str).is_some_and(|text| !text.trim().is_empty());
    let optional_non_empty = |key: &str| match object.get(key) {
        None => true,
        Some(Value::String(text)) => !text.trim().is_empty(),
        Some(_) => false,
    };
    non_empty("workspace") && non_empty("project") && optional_non_empty("lane") && optional_non_empty("session")
}

/// A validated provenance from JSON, or none when the shape is not one.
pub fn provenance_from_value(value: &Value) -> Option<Provenance> {
    if !is_provenance(value) {
        return None;
    }
    serde_json::from_value(value.clone()).ok()
}

/// Insert-only: existing JSON remains readable and receivers can update historical delivery state.
pub const PROVENANCE_TRIGGER: &str = r#"CREATE TRIGGER IF NOT EXISTS deliveries_require_provenance BEFORE INSERT ON deliveries
    WHEN CASE WHEN json_valid(NEW.envelope) THEN
      coalesce(json_type(NEW.envelope, '$.provenance'), '') != 'object'
      OR coalesce(json_type(NEW.envelope, '$.provenance.workspace'), '') != 'text'
      OR coalesce(length(trim(json_extract(NEW.envelope, '$.provenance.workspace'))), 0) = 0
      OR coalesce(json_type(NEW.envelope, '$.provenance.project'), '') != 'text'
      OR coalesce(length(trim(json_extract(NEW.envelope, '$.provenance.project'))), 0) = 0
      OR coalesce(json_type(NEW.envelope, '$.provenance.lane'), 'text') != 'text'
      OR coalesce(json_type(NEW.envelope, '$.provenance.session'), 'text') != 'text'
    ELSE 1 END
    BEGIN SELECT RAISE(ABORT, 'delivery provenance is required: restart the sending session on the upgraded omo'); END"#;

pub const PROVENANCE_OBJECT: &str = "INSERT OR IGNORE INTO extension_objects (type, name, owner) VALUES ('trigger', 'deliveries_require_provenance', NULL)";

pub const PROVENANCE_MIGRATION: [&str; 2] = [PROVENANCE_TRIGGER, PROVENANCE_OBJECT];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn required_labels_must_be_non_empty_strings() {
        for invalid in [json!(null), json!({}), json!({ "workspace": "Example" }), json!({ "workspace": "", "project": "project" }), json!({ "workspace": "Example", "project": 1 }), json!({ "workspace": "Example", "project": "project", "lane": null })] {
            assert!(!is_provenance(&invalid), "{invalid}");
        }
        assert!(is_provenance(&json!({ "workspace": "Example", "project": "project" })));
        assert!(is_provenance(&json!({ "workspace": "Example", "project": "project", "lane": "review", "session": "planner" })));
    }

    #[test]
    fn validated_provenance_round_trips() {
        let provenance = provenance_from_value(&json!({ "workspace": "Example", "project": "project", "lane": "review" })).expect("valid");
        assert_eq!(provenance.workspace, "Example");
        assert_eq!(provenance.lane.as_deref(), Some("review"));
        assert!(provenance.session.is_none());
        assert!(provenance_from_value(&json!({ "workspace": " " , "project": "p" })).is_none());
    }
}
