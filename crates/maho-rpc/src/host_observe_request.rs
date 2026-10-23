//! Which public-socket connections count as host activity (senpi `host-observe-request.ts`).
//! A read marks itself with `observe: true` so the supervisor does not count it as an attachment.
pub const OBSERVE_REQUEST_FIELD: &str = "observe";
pub const ORIGIN_HOST_INSTANCE_FIELD: &str = "origin_host_instance";
const OBSERVING_COMMANDS: &[&str] = &[
    "get_protocol_info",
    "list_sessions",
    "subscribe_sessions",
    "unsubscribe_sessions",
    "get_effect_status",
    "get_pending_inputs",
    "get_subscription_limits",
];
pub fn is_observing_request(line: &str) -> bool {
    let Ok(serde_json::Value::Object(record)) = serde_json::from_str::<serde_json::Value>(line)
    else {
        return false;
    };
    let kind = record.get("type").and_then(serde_json::Value::as_str);
    if kind == Some("warm") {
        return true;
    }
    record.get(OBSERVE_REQUEST_FIELD) == Some(&serde_json::Value::Bool(true))
        && kind.is_some_and(|kind| OBSERVING_COMMANDS.contains(&kind))
}
/// The generation a request line says its client runs in; `None` when unmarked or not JSON.
pub fn request_origin_host_instance(line: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
    value
        .as_object()?
        .get(ORIGIN_HOST_INSTANCE_FIELD)?
        .as_str()
        .filter(|origin| !origin.is_empty())
        .map(str::to_owned)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_marked_reads_observe_and_warm_always_does() {
        assert!(is_observing_request(
            r#"{"type":"subscribe_sessions","observe":true}"#
        ));
        assert!(!is_observing_request(r#"{"type":"subscribe_sessions"}"#));
        assert!(!is_observing_request(r#"{"type":"prompt","observe":true}"#));
        assert!(is_observing_request(r#"{"type":"warm"}"#));
        assert!(!is_observing_request("nope"));
        assert_eq!(
            request_origin_host_instance(r#"{"origin_host_instance":"x"}"#).as_deref(),
            Some("x")
        );
        assert_eq!(
            request_origin_host_instance(r#"{"origin_host_instance":""}"#),
            None
        );
    }
}
