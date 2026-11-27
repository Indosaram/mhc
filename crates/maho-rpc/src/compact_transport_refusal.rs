//! senpi `modes/rpc/compact-transport-refusal.ts`: the compact engine-socket transport's public
//! methods (senpi#5748), always in the method table and answering `feature_disabled` when compact
//! updates are off or on the classic stdio handler.

use serde_json::{Value, json};

pub const RPC_ERROR_FEATURE_DISABLED: &str = "feature_disabled";

pub const COMPACT_TRANSPORT_METHODS: [&str; 4] = [
    "reset_stream",
    "cancel_attempt",
    "get_recovery_page",
    "release_recovery",
];

pub fn is_compact_transport_method(method: &str) -> bool {
    COMPACT_TRANSPORT_METHODS.contains(&method)
}

/// `feature_disabled` with `errorData.feature: "compact_updates"`, keeping the `sessionId` a session
/// reply carries.
pub fn compact_transport_disabled(
    id: Option<&str>,
    command_type: &str,
    session_id: Option<&str>,
) -> Value {
    let mut refusal = json!({
        "type": "response",
        "command": command_type,
        "success": false,
        "error": RPC_ERROR_FEATURE_DISABLED,
        "errorCode": RPC_ERROR_FEATURE_DISABLED,
        "errorData": { "feature": "compact_updates" },
    });
    if let Some(id) = id {
        refusal["id"] = id.into();
    }
    if let Some(session_id) = session_id {
        refusal["sessionId"] = session_id.into();
    }
    refusal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_compact_methods_and_builds_feature_disabled_response() {
        for m in COMPACT_TRANSPORT_METHODS {
            assert!(is_compact_transport_method(m));
        }
        assert!(!is_compact_transport_method("prompt"));

        let without_session = compact_transport_disabled(Some("1"), "reset_stream", None);
        assert_eq!(without_session["errorCode"], "feature_disabled");
        assert_eq!(without_session["errorData"]["feature"], "compact_updates");
        assert!(without_session.get("sessionId").is_none());

        let with_session = compact_transport_disabled(None, "get_recovery_page", Some("sess-1"));
        assert_eq!(with_session["sessionId"], "sess-1");
        assert!(with_session.get("id").is_none());
    }
}
