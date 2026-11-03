//! Lifecycle delivery freshness (senpi `lifecycle-delivery.ts`): additive wire metadata stamped at the host enqueue
//! boundary. `observedAt` describes this copy, not engine progress or liveness, and is never part of the durable
//! snapshot.
use serde_json::Value;

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Stamps `observedAt` on a lifecycle object.
pub fn observe_lifecycle(lifecycle: &mut Value) {
    if let Some(object) = lifecycle.as_object_mut() {
        object.insert("observedAt".into(), now_iso().into());
    }
}

fn observe_state(state: &mut Value) {
    if let Some(lifecycle) = state
        .get_mut("lifecycle")
        .filter(|lifecycle| lifecycle.is_object())
    {
        observe_lifecycle(lifecycle);
    }
}

/// Re-stamps at the host enqueue boundary: `session_lifecycle` records, `get_state` replies and the state of an
/// `open_session` reply, including replies built from cached worker state.
pub fn observe_lifecycle_delivery(record: &mut Value) {
    match record.get("type").and_then(Value::as_str) {
        Some("session_lifecycle") => observe_state(record),
        Some("response") => {
            let command = record
                .get("command")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let Some(data) = record.get_mut("data").filter(|data| data.is_object()) else {
                return;
            };
            match command.as_deref() {
                Some("get_state") => observe_state(data),
                Some("open_session") => {
                    if let Some(state) = data.get_mut("state").filter(|state| state.is_object()) {
                        observe_state(state);
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn lifecycle_records_and_state_replies_are_stamped_and_others_are_not() {
        let mut lifecycle = json!({"type":"session_lifecycle","lifecycle":{"seq":1}});
        observe_lifecycle_delivery(&mut lifecycle);
        assert!(
            lifecycle["lifecycle"]["observedAt"]
                .as_str()
                .unwrap()
                .ends_with('Z')
        );
        let mut state =
            json!({"type":"response","command":"get_state","data":{"lifecycle":{"seq":2}}});
        observe_lifecycle_delivery(&mut state);
        assert!(state["data"]["lifecycle"]["observedAt"].is_string());
        let mut open =
            json!({"type":"response","command":"open_session","data":{"state":{"lifecycle":{}}}});
        observe_lifecycle_delivery(&mut open);
        assert!(open["data"]["state"]["lifecycle"]["observedAt"].is_string());
        let mut other = json!({"type":"response","command":"get_messages","data":{"lifecycle":{}}});
        observe_lifecycle_delivery(&mut other);
        assert!(other["data"]["lifecycle"].get("observedAt").is_none());
        let mut absent = json!({"type":"session_lifecycle"});
        observe_lifecycle_delivery(&mut absent);
        assert_eq!(absent, json!({"type":"session_lifecycle"}));
    }
}
