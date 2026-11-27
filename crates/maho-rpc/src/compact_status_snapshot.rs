//! senpi `modes/rpc/compact-status-snapshot.ts`: host-wide seq-0 owner for `subscribe_sessions`
//! snapshots, streaming rows into `EventLeases` so S11 pages the immutable S13 substrate.

use crate::compact_admission_error::{CompactAdmissionError, CompactAdmissionReason};
use crate::compact_event_leases::EventLeases;
use crate::compact_record_staging::{RecordContent, json_chunks};
use crate::compact_recovery_bytes::FRAGMENT_MAX_BYTES;
use crate::compact_recovery_types::EventLease;
use crate::protocol_identity::host_instance_id;
use serde_json::Value;

/// Streams `{"seq":0,"snapshot":[...]}` rows into `EventLeases` without collecting one giant string.
pub fn status_snapshot<'a>(
    events: &EventLeases,
    socket_epoch: &str,
    admission_id: &str,
    session_id: &str,
    rows: impl IntoIterator<Item = &'a Value>,
) -> Result<EventLease, CompactAdmissionError> {
    let mut chunks = Vec::new();
    chunks.push("{\"seq\":0,\"snapshot\":[".to_owned());
    let mut first = true;
    for row in rows {
        if !first {
            chunks.push(",".to_owned());
        }
        first = false;
        chunks.extend(json_chunks(row, FRAGMENT_MAX_BYTES / 2));
    }
    chunks.push("]}".to_owned());
    let empty_obj = Value::Object(serde_json::Map::new());
    events
        .mint(
            socket_epoch,
            session_id,
            admission_id,
            host_instance_id(),
            0,
            &empty_obj,
            RecordContent::Chunks(chunks),
            None,
        )
        .map_err(|_| CompactAdmissionError::new(CompactAdmissionReason::StorageUnavailable))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_event_leases::EVENT_RECORD_COMPONENT;
    use crate::compact_recovery_types::RecoveryCaller;
    use serde_json::json;

    #[tokio::test]
    async fn streams_snapshot_rows_into_event_lease_and_reads_back_valid_json() {
        let events = EventLeases::default();
        let rows = vec![
            json!({"sessionId":"s1","status":"idle"}),
            json!({"sessionId":"s2","status":"running"}),
        ];
        let lease = status_snapshot(&events, "se-1", "adm-1", "sub-1", &rows).expect("mint");
        assert_eq!(lease.sequence, 0);

        let caller = RecoveryCaller {
            socket_epoch: "se-1".into(),
            session_id: "sub-1".into(),
        };
        let held = events.claim(&caller, &lease.id, "adm-1").expect("claim");
        let page = events
            .page(&held, EVENT_RECORD_COMPONENT, 0, None)
            .await
            .expect("page");
        assert!(page.complete);
        let decoded: String = page
            .bytes
            .chunks_exact(2)
            .map(|c| char::from_u32(u32::from(u16::from_le_bytes([c[0], c[1]]))).expect("utf16"))
            .collect();
        let parsed: Value = serde_json::from_str(&decoded).expect("json");
        assert_eq!(parsed["seq"], 0);
        assert_eq!(parsed["snapshot"], json!(rows));
    }
}
