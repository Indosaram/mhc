//! senpi `modes/rpc/session-record-stream.ts`: the one projection of a session's events into RPC records.
//! `queue_update` entries lose their host-minted ids; an aborted `agent_end` names the client that aborted
//! (`abortedBy`), and a deadline-stopped one its `deadline`; `session_abort` names the client that aborted;
//! `session_lifecycle` loses the durable id (the connection adds the routing handle) and is stamped `observedAt`.
//! A host connection and a terminal control endpoint both stream a session through this, so an attached client
//! reads the same records from either.
//!
//! NATIVE-DIVERGENCE(senpi `subscribeSessionRecords(session, output, onAbortSource, source)`): the native
//! subscription lives in the connection handler (`rpc_session_event_record`), which serialises the event and adds
//! the selection, tool-name and abort-source enrichment; this module is the record half. The turn facts senpi
//! reads off the session (`ClientAdmissions.abortedBy`, `currentDeadline`) arrive as [`TurnFacts`].
use crate::client_admission_record::client_facing_identity;
use crate::client_admissions::ClientAbort;
use crate::lifecycle_delivery::observe_lifecycle;
use serde_json::{Value, json};

/// What the session reports about the current turn when the record is projected.
#[derive(Clone, Debug, Default)]
pub struct TurnFacts {
    /// `ClientAdmissions.forSession(session).abortedBy`.
    pub aborted_by: Option<ClientAbort>,
    /// `session.currentDeadline`: the deadline that stopped the turn.
    pub deadline: Option<Value>,
}

fn aborted_by_value(abort: &ClientAbort) -> Value {
    let mut value = json!({"connectionId": abort.connection_id});
    if let Some(effect_id) = &abort.effect_id {
        value["effectId"] = json!(effect_id);
    }
    value
}

/// The projection without turn facts.
pub fn session_record(record: Value) -> Value {
    session_record_with(record, &TurnFacts::default())
}

/// senpi `subscribeSessionRecords`' per-event projection.
pub fn session_record_with(mut record: Value, facts: &TurnFacts) -> Value {
    let Some(object) = record.as_object_mut() else {
        return record;
    };
    match object.get("type").and_then(Value::as_str) {
        Some("session_lifecycle") => {
            object.remove("sessionId");
            if let Some(lifecycle) = object.get_mut("lifecycle") {
                observe_lifecycle(lifecycle);
            }
        }
        Some("queue_update") => {
            // Same route as every other event; only the minted ids are taken out of the projection.
            if let Some(Value::Array(ordered)) = object.get_mut("ordered") {
                for entry in ordered.iter_mut() {
                    *entry = client_facing_identity(entry.take());
                }
            }
        }
        Some("agent_end") => {
            // `abortedBy` only on an aborted turn: senpi reads it when the session reports an abort source.
            if object
                .get("abortSource")
                .is_some_and(|source| !source.is_null())
                && let Some(abort) = &facts.aborted_by
            {
                object.insert("abortedBy".into(), aborted_by_value(abort));
            }
            if let Some(deadline) = &facts.deadline {
                object.insert("deadline".into(), deadline.clone());
            }
        }
        Some("session_abort") => {
            if let Some(abort) = &facts.aborted_by {
                object.insert("abortedBy".into(), aborted_by_value(abort));
            }
        }
        _ => {}
    }
    record
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_admissions::ClientAdmissions;
    use crate::client_admissions::test_session::FakeSession;
    use crate::effect_record::EffectActor;
    use crate::effect_session::EffectSession;
    use std::sync::Arc;

    #[test]
    fn lifecycle_loses_durable_id_but_other_records_keep_theirs() {
        let projected = session_record(
            json!({"type":"session_lifecycle","sessionId":"durable","lifecycle":{"state":"idle"}}),
        );
        assert!(projected.get("sessionId").is_none());
        assert_eq!(projected["lifecycle"]["state"], "idle");
        assert!(projected["lifecycle"]["observedAt"].is_string());
        let ordinary = json!({"type":"entry_appended","sessionId":"s"});
        assert_eq!(session_record(ordinary.clone()), ordinary);
    }

    #[test]
    fn queue_update_drops_only_host_minted_ids() {
        let projected = session_record(json!({"type":"queue_update","ordered":[
            {"text":"a","clientMessageId":"host:d:1"},
            {"text":"b","clientMessageId":"c1"}
        ]}));
        assert_eq!(
            projected["ordered"],
            json!([{"text":"a"},{"text":"b","clientMessageId":"c1"}])
        );
    }

    // senpi session-record-stream-projection.test.ts: "carries the deadline that stopped the turn on agent_end".
    #[test]
    fn carries_the_deadline_that_stopped_the_turn_on_agent_end() {
        let deadline = json!({"kind":"inactivity","limitMs":120000,"elapsedMs":120000});
        let projected = session_record_with(
            json!({"type":"agent_end","messages":[],"willRetry":false,"aborted":true,"abortSource":"system"}),
            &TurnFacts {
                aborted_by: None,
                deadline: Some(deadline.clone()),
            },
        );
        assert_eq!(projected["aborted"], true);
        assert_eq!(projected["abortSource"], "system");
        assert_eq!(projected["deadline"], deadline);
    }

    // senpi session-record-stream-projection.test.ts: "names the client that aborted on session_abort".
    #[tokio::test]
    async fn names_the_client_that_aborted_on_session_abort() {
        let session: Arc<dyn EffectSession> = Arc::new(FakeSession::new("record-stream-abort"));
        let admissions = ClientAdmissions::for_session(&session).await;
        admissions.note_abort(
            EffectActor {
                connection_id: "socket-1".into(),
            },
            Some("abort-1".into()),
        );
        let facts = TurnFacts {
            aborted_by: admissions.aborted_by(),
            deadline: None,
        };
        assert_eq!(
            session_record_with(json!({"type":"session_abort"}), &facts),
            json!({"type":"session_abort","abortedBy":{"connectionId":"socket-1","effectId":"abort-1"}})
        );
        // An agent_end that was not aborted names no aborter.
        let clean = session_record_with(json!({"type":"agent_end","messages":[]}), &facts);
        assert!(clean.get("abortedBy").is_none());
        ClientAdmissions::forget("record-stream-abort");
    }
}
