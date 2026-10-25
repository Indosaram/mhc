//! Normalized gateway replies from the host's wire frames.
//!
//! Upstream counterpart: `thread/gateway-replies.ts`.

use serde_json::Value;

use super::adapter::{Admission, ExternalAdmissionKind, GatewayWakeReply, ReleaseErrorData, ReleaseSessionReply, ReleaseSuccessData};

fn record(value: Option<&Value>) -> Option<&serde_json::Map<String, Value>> {
    value.and_then(Value::as_object)
}

/// The host's `release_session` frame as the adapter's union: the success payload, or the refusal
/// code with its `errorData`.
pub fn release_reply(frame: &Value) -> ReleaseSessionReply {
    let object = record(Some(frame));
    let success = object.and_then(|object| object.get("success")).and_then(Value::as_bool) == Some(true);
    let data = object.and_then(|object| object.get("data"));
    if success
        && let Some(data) = record(data)
        && data.get("released").and_then(Value::as_bool) == Some(true)
        && let Some(session_path) = data.get("session_path").and_then(Value::as_str)
    {
        return ReleaseSessionReply::Success {
            data: ReleaseSuccessData {
                released: true,
                session_path: session_path.to_owned(),
                attachments: data.get("attachments").and_then(Value::as_i64).unwrap_or(0),
                dropped: serde_json::from_value(data.get("dropped").cloned().unwrap_or(Value::Null)).unwrap_or_default(),
            },
        };
    }
    let error = object
        .and_then(|object| object.get("error"))
        .and_then(Value::as_str)
        .unwrap_or("release_failed")
        .to_owned();
    match object.and_then(|object| object.get("errorData")).and_then(|value| record(Some(value))) {
        Some(error_data) => {
            let details: ReleaseErrorData = serde_json::from_value(Value::Object(error_data.clone())).unwrap_or_default();
            ReleaseSessionReply::Failure { error, error_data: Some(details) }
        }
        None => ReleaseSessionReply::Failure { error, error_data: None },
    }
}

/// `wakeReply`: only well-formed admissions with a known kind survive.
pub fn wake_reply(data: &Value) -> GatewayWakeReply {
    let admitted = data.get("admitted").and_then(Value::as_array);
    let Some(admitted) = admitted else {
        return GatewayWakeReply::default();
    };
    GatewayWakeReply {
        admitted: admitted
            .iter()
            .filter_map(|entry| {
                let entry = record(Some(entry))?;
                let delivery_id = entry.get("delivery_id")?.as_str()?;
                let kind = ExternalAdmissionKind::parse(entry.get("kind")?.as_str()?)?;
                Some(Admission { delivery_id: delivery_id.to_owned(), kind })
            })
            .collect(),
        unroutable: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_released_frame_keeps_its_payload() {
        let reply = release_reply(&json!({ "success": true, "data": { "released": true, "session_path": "/s.jsonl", "attachments": 2, "dropped": { "deliveries": ["d"], "user_messages": [] } } }));
        match reply {
            ReleaseSessionReply::Success { data } => {
                assert_eq!(data.session_path, "/s.jsonl");
                assert_eq!(data.attachments, 2);
                assert_eq!(data.dropped.deliveries, vec!["d".to_owned()]);
            }
            other => panic!("expected success, got {other:?}"),
        }
    }

    #[test]
    fn a_refusal_keeps_its_code_and_error_data() {
        let reply = release_reply(&json!({ "success": false, "error": "host_draining", "errorData": { "detail": "worker_runtime" } }));
        match reply {
            ReleaseSessionReply::Failure { error, error_data } => {
                assert_eq!(error, "host_draining");
                assert_eq!(error_data.and_then(|data| data.detail).as_deref(), Some("worker_runtime"));
            }
            other => panic!("expected failure, got {other:?}"),
        }
        assert_eq!(release_reply(&json!({})), ReleaseSessionReply::Failure { error: "release_failed".to_owned(), error_data: None });
    }

    #[test]
    fn only_well_formed_admissions_survive() {
        let reply = wake_reply(&json!({ "admitted": [
            { "delivery_id": "a", "kind": "started" },
            { "delivery_id": "b", "kind": "not_a_kind" },
            { "delivery_id": 3, "kind": "queued" },
            { "kind": "queued" },
            "junk",
        ] }));
        assert_eq!(reply.admitted.len(), 1);
        assert_eq!(reply.admitted[0].delivery_id, "a");
        assert_eq!(reply.admitted[0].kind, ExternalAdmissionKind::Started);
        assert!(wake_reply(&json!({})).admitted.is_empty());
    }
}
