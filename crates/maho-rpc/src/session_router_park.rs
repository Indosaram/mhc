//! Port of senpi `packages/coding-agent/src/modes/rpc/session-router-park.ts`.
//!
//! Speculative pre-building (`park_session`) and zero-wait claiming (`claim_session`, senpi#6350).

use serde_json::{Value, json};

pub fn park_profile_key(
    cwd: &str,
    session_kind: Option<&str>,
    session_context: &[(String, Value)],
) -> String {
    let mut sorted = session_context.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    serde_json::to_string(&(cwd, session_kind.unwrap_or("interactive"), sorted))
        .unwrap_or_else(|_| format!("{cwd}:{}", session_kind.unwrap_or("interactive")))
}

/// Returns `true` when `claim_session` carries fields that require bespoke construction or a
/// distinct admission contract, so the claim must miss and leave any parked session untouched.
pub fn claim_command_must_miss(command: &Value, compact_selected: bool) -> bool {
    let Some(obj) = command.as_object() else {
        return true;
    };
    let has = |k: &str| obj.get(k).is_some_and(|v| !v.is_null());
    let has_provider = has("provider");
    let has_model_id = has("modelId");
    has("sessionPath")
        || has("durableSessionId")
        || has("expectedHostInstance")
        || has("admissionId")
        || obj.get("attended").and_then(Value::as_bool) == Some(false)
        || has("retry")
        || (has_provider != has_model_id)
        || has("spawnedBy")
        || has("env")
        || has("auto_title")
        || has("retryFallback")
        || compact_selected
}

pub fn claim_miss_response(id: Option<&Value>) -> Value {
    json!({
        "id": id,
        "type": "response",
        "command": "claim_session",
        "success": true,
        "data": { "claimed": false },
    })
}

pub fn park_error_response(id: Option<&Value>, command: &str, error: &str) -> Value {
    json!({
        "id": id,
        "type": "response",
        "command": command,
        "success": false,
        "error": error,
        "errorCode": error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_key_sorts_context_and_claim_misses_on_specific_fields() {
        let k1 = park_profile_key(
            "/work",
            None,
            &[("b".into(), json!(2)), ("a".into(), json!(1))],
        );
        let k2 = park_profile_key(
            "/work",
            Some("interactive"),
            &[("a".into(), json!(1)), ("b".into(), json!(2))],
        );
        assert_eq!(k1, k2);
        assert!(claim_command_must_miss(&json!({"sessionPath":"/x"}), false));
        assert!(claim_command_must_miss(
            &json!({"provider":"anthropic"}),
            false
        ));
        assert!(!claim_command_must_miss(&json!({"cwd":"/work"}), false));
        assert!(claim_command_must_miss(&json!({"cwd":"/work"}), true));
    }
}
