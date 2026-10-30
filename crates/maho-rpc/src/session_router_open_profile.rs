//! `open_session` profile checks (senpi `session-router-open-profile.ts`).
use serde_json::Value;

/// senpi `isSpawnedBy`: the shared CLI / open_session boundary contract; ids are opaque, non-empty strings.
pub fn is_spawned_by(value: &Value) -> bool {
    ["parentSessionId", "rootSessionId", "taskId"]
        .iter()
        .all(|key| {
            value
                .get(key)
                .and_then(Value::as_str)
                .is_some_and(|id| !id.is_empty())
        })
}

/// `open_session.spawnedBy` must name the parent, root and task when present; the open answers
/// `invalid_spawned_by` otherwise.
pub fn invalid_spawned_by(spawned_by: &Value) -> bool {
    !spawned_by.is_null() && !is_spawned_by(spawned_by)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn spawned_by_needs_every_ancestor_when_present() {
        assert!(!invalid_spawned_by(&Value::Null));
        assert!(!invalid_spawned_by(
            &json!({"parentSessionId":"p","rootSessionId":"r","taskId":"t"})
        ));
        assert!(invalid_spawned_by(
            &json!({"parentSessionId":"p","rootSessionId":"r"})
        ));
        assert!(invalid_spawned_by(
            &json!({"parentSessionId":"","rootSessionId":"r","taskId":"t"})
        ));
        assert!(invalid_spawned_by(&json!(["p", "r", "t"])));
    }
}
