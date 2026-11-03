//! Port of senpi packages/coding-agent/src/core/session-spawned-by.ts.

use serde_json::Value;

use crate::session_lifecycle_types::SpawnedBy;

fn non_empty_string<'a>(object: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a str> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

/// The shared CLI / open_session boundary contract; IDs are opaque, non-empty strings.
pub fn is_spawned_by(value: &Value) -> bool {
    read_spawned_by(value).is_some()
}

/// `isSpawnedBy` as a typed read: the value when it is a valid `SpawnedBy`.
pub fn read_spawned_by(value: &Value) -> Option<SpawnedBy> {
    let object = value.as_object()?;
    Some(SpawnedBy {
        parent_session_id: non_empty_string(object, "parentSessionId")?.to_owned(),
        root_session_id: non_empty_string(object, "rootSessionId")?.to_owned(),
        task_id: non_empty_string(object, "taskId")?.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_only_objects_with_three_non_empty_string_ids() {
        assert!(is_spawned_by(
            &json!({"parentSessionId":"p","rootSessionId":"r","taskId":"t"})
        ));
        assert!(is_spawned_by(
            &json!({"parentSessionId":"p","rootSessionId":"r","taskId":"t","extra":1})
        ));
        for value in [
            json!(null),
            json!([]),
            json!(["p", "r", "t"]),
            json!("p"),
            json!({"parentSessionId":"p","rootSessionId":"r"}),
            json!({"parentSessionId":"","rootSessionId":"r","taskId":"t"}),
            json!({"parentSessionId":"p","rootSessionId":1,"taskId":"t"}),
            json!({"parentSessionId":"p","rootSessionId":"r","taskId":null}),
        ] {
            assert!(!is_spawned_by(&value), "{value}");
        }
    }

    #[test]
    fn read_returns_the_typed_value() {
        let spawned =
            read_spawned_by(&json!({"parentSessionId":"p","rootSessionId":"r","taskId":"t"}))
                .expect("valid");
        assert_eq!(spawned.parent_session_id, "p");
        assert_eq!(spawned.root_session_id, "r");
        assert_eq!(spawned.task_id, "t");
    }
}
