//! Live-only, identity-fenced attachment commands owned by the multi-session host (senpi
//! `session-attachment-commands.ts`, `attachment-types.ts`, `session-live-attachment.ts`). The checks live here;
//! `HostCore` in `multi_session_host.rs` applies them to its registry, fanout and bindings.
use serde_json::{Value, json};

/// senpi `identityMatches`: the durable id must name the session this handle runs now, and an
/// `expectedHostInstance`, when given (always for `reclaim_session`), must name this host.
pub fn identity_matches(
    current_durable_id: Option<&str>,
    durable_session_id: &Value,
    expected_host_instance: &Value,
    host_instance: &str,
) -> bool {
    let Some(durable) = durable_session_id.as_str().filter(|id| !id.is_empty()) else {
        return false;
    };
    Some(durable) == current_durable_id
        && (expected_host_instance.is_null()
            || expected_host_instance.as_str() == Some(host_instance))
}

/// Wire contract v1.1: a draining or stopping host refuses attachment before admission with a v1 code.
pub fn draining_refusal_detail() -> Value {
    json!({ "reason": "host_draining" })
}

/// Attachments held by connections other than the reclaiming one.
pub fn attached_elsewhere(attachments: i64, own: usize) -> u64 {
    u64::try_from(
        attachments
            .saturating_sub(i64::try_from(own).unwrap_or(i64::MAX))
            .max(0),
    )
    .unwrap_or(0)
}

pub fn attach_reply(session_id: &str) -> Value {
    json!({ "sessionId": session_id, "attached": true })
}

pub fn release_reply(deferred: bool) -> Value {
    json!({ "deferred": deferred })
}

pub fn reclaim_reply(session_id: &str) -> Value {
    json!({ "reclaimed": true, "sessionId": session_id })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_needs_the_running_durable_id_and_this_host_when_named() {
        assert!(identity_matches(Some("d"), &json!("d"), &Value::Null, "h"));
        assert!(identity_matches(Some("d"), &json!("d"), &json!("h"), "h"));
        assert!(!identity_matches(
            Some("d"),
            &json!("d"),
            &json!("other"),
            "h"
        ));
        assert!(!identity_matches(Some("d"), &json!("e"), &Value::Null, "h"));
        assert!(!identity_matches(Some(""), &json!(""), &Value::Null, "h"));
        assert!(!identity_matches(None, &json!("d"), &Value::Null, "h"));
        assert!(!identity_matches(Some("d"), &json!(1), &Value::Null, "h"));
    }
    #[test]
    fn only_other_connections_count_as_elsewhere() {
        assert_eq!(attached_elsewhere(3, 1), 2);
        assert_eq!(attached_elsewhere(1, 1), 0);
        assert_eq!(attached_elsewhere(0, 2), 0);
    }
}
