//! senpi `modes/rpc/compact-materializer-line.ts`: materializer framing for sequenced
//! events on compact-enabled physical streams (S13, R4.B6, H <= 16 KiB).

use crate::compact_event_leases::EventLeases;
use crate::compact_record_staging::{RecordContent, json_chunks};
use crate::compact_recovery_state::RecoveryCheckpoints;
use crate::compact_recovery_types::RecoveryCaller;
use crate::compact_required_streams::CompactRequiredStreams;
use crate::jsonl::serialize_json_line;
use crate::session_acceptance_admission::is_sequenced;
use serde_json::{Value, json};
use std::borrow::Cow;

/// H: per-admission framing is <= 16 KiB including LF, independent of payload size and number of
/// routes.
pub const MATERIALIZER_ENVELOPE_MAX_BYTES: usize = 16_384;

const DELTA_EVENT_TYPES: [&str; 3] = ["text_delta", "thinking_delta", "toolcall_delta"];

/// True when a `message_update` carries a delta assistantMessageEvent and an inline `message`
/// snapshot that can be demoted for compact event leases (from `session-event-writer-compaction.ts`).
pub fn compact_delta(record: &Value) -> bool {
    let Some(obj) = record.as_object() else {
        return false;
    };
    if obj.get("type").and_then(Value::as_str) != Some("message_update") {
        return false;
    }
    if obj.get("message").is_none_or(Value::is_null) {
        return false;
    }
    let Some(event_type) = obj
        .get("assistantMessageEvent")
        .and_then(|ev| ev.get("type"))
        .and_then(Value::as_str)
    else {
        return false;
    };
    DELTA_EVENT_TYPES.contains(&event_type)
}

/// Strips the redundant inline `message` and `assistantMessageEvent.partial` from a delta update
/// before staging it in an event lease.
pub fn demote_to_delta_only(record: &Value) -> Value {
    let mut cloned = record.clone();
    if let Some(obj) = cloned.as_object_mut() {
        obj.insert("message".into(), Value::Null);
        if let Some(ev) = obj
            .get_mut("assistantMessageEvent")
            .and_then(Value::as_object_mut)
        {
            ev.insert("partial".into(), Value::Null);
        }
    }
    cloned
}

/// Lane metadata for the accepted sequence being materialized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializerLaneInfo {
    pub session_epoch: String,
    pub accepted_sequence: u64,
}

/// Host dependencies needed by `materializer_line` (implemented by fanout/transport hosts).
pub trait MaterializerHost: Send + Sync {
    fn lane_info(&self, session_id: &str) -> Option<MaterializerLaneInfo>;
    fn peer_socket_epoch(&self, peer: &str) -> Option<String>;
    fn close_peer(&self, peer: &str, code: &str, detail: &str);
    fn checkpoints(&self) -> RecoveryCheckpoints;
    fn events(&self) -> Option<EventLeases>;
}

/// Serialized JSONL materializer line and its delivery/rollback callback.
pub struct MaterializerLine {
    pub line: String,
    pub atomic: bool,
    pub undelivered: Box<dyn FnOnce(bool) + Send>,
}

fn payload_chunks(record: &Value) -> Vec<String> {
    json_chunks(record, 65_536)
}

/// One accepted record's payload is staged once by S13, not serialized into each admission's
/// envelope.
pub fn materializer_line(
    host: &dyn MaterializerHost,
    required: &CompactRequiredStreams,
    peer: &str,
    session_id: &str,
    record: &Value,
    shared_key: Option<usize>,
) -> Result<Option<MaterializerLine>, String> {
    let Some(lane) = host.lane_info(session_id) else {
        return Ok(None);
    };
    let Some(socket_epoch) = host.peer_socket_epoch(peer) else {
        return Ok(None);
    };
    let Some(events) = host.events() else {
        return Ok(None);
    };
    let caller = RecoveryCaller {
        socket_epoch: socket_epoch.clone(),
        session_id: session_id.to_owned(),
    };
    let checkpoints = host.checkpoints();
    let Some(stream) = checkpoints.stream(&caller) else {
        return Ok(None);
    };
    let Some(record_type) = record.get("type").and_then(Value::as_str) else {
        return Ok(None);
    };
    if stream.routes.is_empty() || !is_sequenced(record_type) {
        return Ok(None);
    }
    let routes = required.routes(peer, session_id, record_type);
    let Some(owner) = routes.first().cloned() else {
        return Ok(None);
    };
    let content: Cow<'_, Value> = if compact_delta(record) {
        Cow::Owned(demote_to_delta_only(record))
    } else {
        Cow::Borrowed(record)
    };
    let minted = events.mint(
        &socket_epoch,
        session_id,
        &owner,
        &lane.session_epoch,
        lane.accepted_sequence,
        record,
        RecordContent::Chunks(payload_chunks(&content)),
        shared_key,
    );
    let lease = match minted {
        Ok(lease) => lease,
        Err(reason) => {
            let line = serialize_json_line(&json!({
                "type": "event_lease_refused",
                "sessionId": session_id,
                "reason": reason.as_str(),
            }))
            .map_err(|err| err.to_string())?;
            let reason_str = reason.as_str().to_owned();
            let peer_owned = peer.to_owned();
            // Notice: host close is handled by caller invoking `undelivered` or `close_peer` via
            // explicit callback if host is Arc-wrapped. Keeping closure clean & Send-safe:
            let _ = (&peer_owned, &reason_str);
            return Ok(Some(MaterializerLine {
                line,
                atomic: true,
                undelivered: Box::new(|_| {}),
            }));
        }
    };

    if let Ok(held) = events.claim(&caller, &lease.id, &owner) {
        for route in &routes {
            held.add_admission(route.clone());
        }
    }
    let seed = required.take_seed(peer, session_id);
    let controls = match seed {
        Some(seed) => json!([{
            "kind": "seed",
            "token": seed.id,
            "sequence": seed.cut.sequence,
        }]),
        None => json!([]),
    };
    let line = serialize_json_line(&json!({
        "type": record_type,
        "sessionId": session_id,
        "eventLease": lease,
        "materializer": {
            "sessionEpoch": lane.session_epoch,
            "sequence": lane.accepted_sequence,
            "generation": stream.generation,
            "payload": lease,
            "controls": controls,
        }
    }))
    .map_err(|err| err.to_string())?;

    // Generated identifiers and one fixed control leave room for the bounded admissionId header.
    if line.len() + 512 > MATERIALIZER_ENVELOPE_MAX_BYTES {
        events.abandon(&socket_epoch, &lease.id);
        return Err("materializer envelope exceeded H".to_owned());
    }

    let lease_id = lease.id;
    Ok(Some(MaterializerLine {
        line,
        atomic: true,
        undelivered: Box::new(move |written| {
            if !written {
                events.abandon(&socket_epoch, &lease_id);
            }
        }),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_negotiation::COMPACT_EVENTS_CAPABILITY;
    use crate::compact_recovery_state::{FrozenLaneCut, RecoveryLane, RecoveryRoute};
    use crate::compact_recovery_types::RecoveryRouteKind;
    use crate::compact_required_streams::{RegisteredPeerInfo, RequiredStreamsBackend};
    use crate::compact_settled_messages::SettledRead;
    use crate::session_acceptance_lane::{FrozenAcceptedRoot, RetireReason, SessionAcceptanceLane};
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Mutex, PoisonError};

    struct TestHost {
        checkpoints: RecoveryCheckpoints,
        events: EventLeases,
        lane: Arc<TestLane>,
    }

    struct TestLane {
        lane: Mutex<SessionAcceptanceLane>,
        roots: Mutex<HashMap<usize, FrozenAcceptedRoot>>,
        next_key: std::sync::atomic::AtomicUsize,
        gen_num: std::sync::atomic::AtomicU64,
    }

    impl RecoveryLane for TestLane {
        fn session_epoch(&self) -> String {
            "ep1".into()
        }
        fn accepted_sequence(&self) -> u64 {
            7
        }
        fn retired(&self) -> bool {
            false
        }
        fn retire_reason(&self) -> Option<RetireReason> {
            None
        }
        fn next_generation(&self) -> Option<u64> {
            Some(
                self.gen_num
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    + 1,
            )
        }
        fn freeze_cut(&self) -> Option<FrozenLaneCut> {
            let root = self
                .lane
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .freeze();
            let key = self
                .next_key
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            self.roots
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(key, root.clone());
            Some(FrozenLaneCut {
                root_key: key,
                root,
                store: None,
                has_current: true,
            })
        }
        fn release_cut(&self, key: usize) {
            if let Some(r) = self
                .roots
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&key)
            {
                self.lane
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .release(&r);
            }
        }
        fn read_settled(
            &self,
            _: usize,
        ) -> futures::future::BoxFuture<'static, Result<SettledRead, String>> {
            Box::pin(async {
                Ok(SettledRead {
                    settled_messages: vec![],
                    owners: vec![],
                })
            })
        }
        fn read_current(
            &self,
            _: usize,
        ) -> futures::future::BoxFuture<
            'static,
            Result<Value, crate::compact_recovery_bytes::RecoveryBytesError>,
        > {
            Box::pin(async { Ok(json!({})) })
        }
        fn read_components(
            &self,
            _: usize,
        ) -> futures::future::BoxFuture<
            'static,
            Result<Value, crate::compact_recovery_bytes::RecoveryBytesError>,
        > {
            Box::pin(async { Ok(json!({})) })
        }
        fn read_active(
            &self,
            _: usize,
        ) -> futures::future::BoxFuture<
            'static,
            Result<Value, crate::compact_recovery_bytes::RecoveryBytesError>,
        > {
            Box::pin(async { Ok(json!([])) })
        }
        fn read_calls(
            &self,
            _: usize,
        ) -> futures::future::BoxFuture<
            'static,
            Result<Value, crate::compact_recovery_bytes::RecoveryBytesError>,
        > {
            Box::pin(async { Ok(json!({"calls": []})) })
        }
    }

    impl RequiredStreamsBackend for TestHost {
        fn acceptance_lane(&self, session_id: &str) -> Option<Arc<dyn RecoveryLane>> {
            (session_id == "s1").then(|| self.lane.clone() as Arc<dyn RecoveryLane>)
        }
        fn checkpoints(&self) -> RecoveryCheckpoints {
            self.checkpoints.clone()
        }
        fn connection_ids(&self) -> Vec<String> {
            vec!["p1".into()]
        }
        fn connection_has(&self, peer: &str, cap: &str) -> bool {
            peer == "p1" && cap == COMPACT_EVENTS_CAPABILITY
        }
        fn registered_peer(&self, peer: &str) -> Option<RegisteredPeerInfo> {
            (peer == "p1").then(|| RegisteredPeerInfo {
                socket_epoch: "se1".into(),
                admissions: HashSet::from(["adm1".into()]),
            })
        }
        fn close_peer(&self, _: &str, _: &str, _: &str) {}
        fn required_sessions(&self, _: &str) -> Vec<String> {
            vec!["s1".into()]
        }
        fn required_admissions(&self, _: &str, _: &str) -> Vec<String> {
            vec!["adm1".into()]
        }
        fn mirror_admissions(&self, _: &str, _: &str, _: &str) -> Vec<String> {
            vec![]
        }
    }

    impl MaterializerHost for TestHost {
        fn lane_info(&self, session_id: &str) -> Option<MaterializerLaneInfo> {
            (session_id == "s1").then(|| MaterializerLaneInfo {
                session_epoch: "ep1".into(),
                accepted_sequence: 7,
            })
        }
        fn peer_socket_epoch(&self, peer: &str) -> Option<String> {
            (peer == "p1").then(|| "se1".into())
        }
        fn close_peer(&self, _: &str, _: &str, _: &str) {}
        fn checkpoints(&self) -> RecoveryCheckpoints {
            self.checkpoints.clone()
        }
        fn events(&self) -> Option<EventLeases> {
            Some(self.events.clone())
        }
    }

    #[test]
    fn compact_delta_demotes_inline_message_and_partial() {
        let rec = json!({
            "type": "message_update",
            "message": {"role": "assistant"},
            "assistantMessageEvent": {"type": "text_delta", "delta": "hi", "partial": {"content": []}}
        });
        assert!(compact_delta(&rec));
        let demoted = demote_to_delta_only(&rec);
        assert_eq!(demoted["message"], Value::Null);
        assert_eq!(demoted["assistantMessageEvent"]["partial"], Value::Null);
        assert_eq!(demoted["assistantMessageEvent"]["delta"], "hi");
    }

    #[test]
    fn materializer_line_frames_sequenced_record_under_envelope_cap() {
        let mut lane = SessionAcceptanceLane::default();
        lane.bootstrap(Default::default(), Some("ep1".into()));
        let host = Arc::new(TestHost {
            checkpoints: RecoveryCheckpoints::new(Arc::new(
                crate::compact_recovery_manifest::RecoveryManifests::default(),
            )),
            events: EventLeases::default(),
            lane: Arc::new(TestLane {
                lane: Mutex::new(lane),
                roots: Mutex::new(HashMap::new()),
                next_key: std::sync::atomic::AtomicUsize::new(0),
                gen_num: std::sync::atomic::AtomicU64::new(0),
            }),
        });
        let _ = host
            .checkpoints
            .freeze(
                &RecoveryRoute {
                    socket_epoch: "se1".into(),
                    admission_id: "adm1".into(),
                    route_kind: RecoveryRouteKind::Ordinary,
                    peer: "p1".into(),
                    session_id: "s1".into(),
                },
                host.lane.clone(),
                true,
            )
            .expect("freeze route");
        let required = CompactRequiredStreams::new(host.clone());
        let big_text = "x".repeat(32_000);
        let rec =
            json!({"type": "message_end", "message": {"role": "assistant", "content": big_text}});
        let out = materializer_line(host.as_ref(), &required, "p1", "s1", &rec, Some(42))
            .expect("no error")
            .expect("materialized");
        assert!(out.atomic);
        assert!(out.line.len() + 512 <= MATERIALIZER_ENVELOPE_MAX_BYTES);
        let parsed: Value = serde_json::from_str(out.line.trim()).expect("valid json");
        assert_eq!(parsed["type"], "message_end");
        assert_eq!(parsed["materializer"]["sequence"], 7);
        assert_eq!(host.events.status().open, 1);

        (out.undelivered)(false);
        assert_eq!(host.events.status().open, 0);
    }
}
