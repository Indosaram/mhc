//! Port of senpi `packages/coding-agent/src/modes/rpc/session-router-host-reads.ts`.
//!
//! Host-level read responses (`get_subscription_limits`, `list_sessions`, `get_host_info`, etc.)
//! including senpi#7027 worker status redaction from default listings and senpi#7020 partial
//! handover reporting.

use serde_json::{Map, Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InheritedHandoverView {
    pub pending: usize,
    pub predecessor_draining: bool,
}

pub trait HostReadsPort {
    fn session_rows(&self) -> Vec<Value>;
    fn session_memory(&self, session_id: &str) -> Value;
    fn session_presence(&self, durable_session_id: &str) -> Option<Value>;
    fn inherited_handover(&self) -> Option<InheritedHandoverView>;
}

pub fn answer_subscription_limits(id: Option<&Value>, accounts: Value) -> Value {
    json!({
        "id": id,
        "type": "response",
        "command": "get_subscription_limits",
        "success": true,
        "data": { "accounts": accounts },
    })
}

/// Filters and projects `list_sessions` rows: worker sessions and context blobs travel only when
/// `include_workers` is `true` (senpi#7027).
pub fn answer_list_sessions(
    port: &dyn HostReadsPort,
    id: Option<&Value>,
    include_workers: bool,
) -> Value {
    let mut sessions = Vec::new();
    for row in port.session_rows() {
        let Some(mut map) = row.as_object().cloned() else {
            continue;
        };
        let is_worker = map.get("kind").and_then(Value::as_str) == Some("worker");
        if !include_workers && is_worker {
            continue;
        }
        if let Some(sid) = map
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            map.insert("memory".to_owned(), port.session_memory(&sid));
        }
        if let Some(did) = map
            .get("durableSessionId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            && let Some(presence) = port.session_presence(&did)
        {
            map.insert("presence".to_owned(), presence);
        }
        if !include_workers {
            map.remove("context");
        }
        sessions.push(Value::Object(map));
    }

    let mut data = Map::new();
    data.insert("sessions".to_owned(), Value::Array(sessions));
    if let Some(handover) = port.inherited_handover() {
        data.insert(
            "handover".to_owned(),
            json!({
                "pending": handover.pending,
                "predecessor_draining": handover.predecessor_draining,
            }),
        );
    }

    json!({
        "id": id,
        "type": "response",
        "command": "list_sessions",
        "success": true,
        "data": Value::Object(data),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockHost(Vec<Value>);
    impl HostReadsPort for MockHost {
        fn session_rows(&self) -> Vec<Value> {
            self.0.clone()
        }
        fn session_memory(&self, _: &str) -> Value {
            json!({"main_heap_bytes":0,"kernel_heap_bytes":0,"kernel_count":0})
        }
        fn session_presence(&self, _: &str) -> Option<Value> {
            None
        }
        fn inherited_handover(&self) -> Option<InheritedHandoverView> {
            None
        }
    }

    #[test]
    fn redacts_workers_and_context_unless_include_workers() {
        let host = MockHost(vec![
            json!({"sessionId":"s1","kind":"interactive","context":{"a":1}}),
            json!({"sessionId":"w1","kind":"worker","context":{"b":2}}),
        ]);
        let default_res = answer_list_sessions(&host, None, false);
        let list = default_res["data"]["sessions"].as_array().expect("arr");
        assert_eq!(list.len(), 1);
        assert!(list[0].get("context").is_none());

        let with_workers = answer_list_sessions(&host, None, true);
        let full = with_workers["data"]["sessions"].as_array().expect("arr");
        assert_eq!(full.len(), 2);
        assert!(full[0].get("context").is_some());
    }
}
