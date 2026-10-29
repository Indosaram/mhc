use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::store::TaskRecordStore;
use super::schema::canonical_json;
use super::types::WorkpoolYieldValue;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkpoolYieldEvent {
    pub pool_id: String,
    pub generation: u32,
    pub task_id: String,
    pub run_epoch: u32,
    pub item_id: String,
    pub result: WorkpoolYieldValue,
    pub sha256: String,
}

#[derive(Deserialize)]
struct EventEnvelope {
    #[serde(rename = "type")]
    event_type: String,
    payload: Value,
}

/// Same required shape as [`EventEnvelope`] without materializing the payload.
#[derive(Deserialize)]
struct EventEnvelopeShape {
    #[serde(rename = "type")]
    _event_type: String,
    #[serde(rename = "payload")]
    _payload: serde::de::IgnoredAny,
}

pub fn yield_digest(result: &WorkpoolYieldValue) -> String {
    let value = match result {
        WorkpoolYieldValue::Data { key, data } => serde_json::json!({"key": key, "data": data}),
        WorkpoolYieldValue::Error { key, error } => serde_json::json!({"key": key, "error": error}),
    };
    Sha256::digest(canonical_json(&value).as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn read_yield_events(tasks: &TaskRecordStore, pool_id: &str, task_id: &str, run_epoch: u32, generation: u32) -> Result<Vec<WorkpoolYieldEvent>, std::io::Error> {
    let path = tasks.state_dir().join("logs").join(format!("{task_id}.jsonl"));
    let bytes = match std::fs::read_to_string(path) { Ok(bytes) => bytes, Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]), Err(error) => return Err(error) };
    let mut events = Vec::new();
    let last_index = bytes.split('\n').count() - 1;
    for (index, line) in bytes.split('\n').enumerate() {
        if line.is_empty() { continue; }
        // Most log lines are unrelated task events: validate their envelope without building a Value.
        if !line.contains("workpool_yield") {
            match serde_json::from_str::<EventEnvelopeShape>(line) { Ok(_) => continue, Err(_) if index == last_index => break, Err(error) => return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, error)) }
        }
        let envelope: EventEnvelope = match serde_json::from_str(line) { Ok(raw) => raw, Err(_) if index == last_index => break, Err(error) => return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, error)) };
        if envelope.event_type != "workpool_yield" { continue; }
        let Ok(event) = serde_json::from_value::<WorkpoolYieldEvent>(envelope.payload) else { continue };
        if event.pool_id == pool_id && event.task_id == task_id && event.run_epoch == run_epoch && event.generation == generation { events.push(event); }
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use crate::store::{PersistedTaskEvent, StateDirConfig};

    fn task_store(name: &str) -> TaskRecordStore {
        TaskRecordStore::new(&StateDirConfig {
            project_dir: std::env::temp_dir().join(format!("maho-yield-event-{name}-{}", std::process::id())),
            task_state_dir: None,
        })
    }

    fn event() -> WorkpoolYieldEvent {
        let result = WorkpoolYieldValue::Data { key: "a".into(), data: serde_json::json!({"v": 1}) };
        WorkpoolYieldEvent {
            pool_id: "wp_00000000000000000000000000000000".into(), generation: 1,
            task_id: "st_00000001".into(), run_epoch: 2, item_id: "item-a".into(),
            sha256: yield_digest(&result), result,
        }
    }

    fn append(tasks: &TaskRecordStore, event: &WorkpoolYieldEvent) {
        tasks.append_event("st_00000001", &PersistedTaskEvent {
            event_type: "workpool_yield".into(),
            payload: serde_json::to_value(event).expect("event payload"),
        }).expect("append event");
    }

    #[test] fn digest_is_stable_for_data() { let result=WorkpoolYieldValue::Data{key:"a".into(),data:serde_json::json!(1)}; assert_eq!(yield_digest(&result).len(),64); }
    #[test] fn data_digest_is_key_sensitive() { let a=WorkpoolYieldValue::Data{key:"a".into(),data:serde_json::json!(1)}; let b=WorkpoolYieldValue::Data{key:"b".into(),data:serde_json::json!(1)}; assert_ne!(yield_digest(&a), yield_digest(&b)); }
    #[test] fn error_digest_is_stable_and_distinct() { let a=WorkpoolYieldValue::Error{key:"a".into(),error:super::super::types::WorkpoolYieldError{code:"kernel_tool_stale".into(),message:"stale".into()}}; let b=WorkpoolYieldValue::Error{key:"a".into(),error:super::super::types::WorkpoolYieldError{code:"cancelled".into(),message:"cancelled".into()}}; assert_eq!(yield_digest(&a).len(),64); assert_ne!(yield_digest(&a), yield_digest(&b)); }
    #[test]
    fn missing_yield_log_is_empty() {
        let store = task_store("missing");
        assert!(read_yield_events(&store, "wp_00000000000000000000000000000000", "st_00000001", 2, 1).expect("read").is_empty());
    }

    #[test]
    fn appended_envelope_is_read_and_duplicate_receipt_matches_digest() {
        let store = task_store("append-read-deduplicate");
        let event = event();
        append(&store, &event);
        let read = read_yield_events(&store, &event.pool_id, &event.task_id, event.run_epoch, event.generation).expect("read");
        let prior = read.iter().find(|prior| prior.item_id == event.item_id && prior.result.key() == event.result.key()).expect("matching prior yield");
        assert_eq!(prior.sha256, event.sha256);
        assert_eq!(read.len(), 1);
    }

    #[test]
    fn unrelated_envelope_is_filtered_by_full_turn_tuple() {
        let store = task_store("unrelated");
        let event = event();
        append(&store, &event);
        assert!(read_yield_events(&store, &event.pool_id, &event.task_id, 3, event.generation).expect("read").is_empty());
    }

    #[test]
    fn malformed_trailing_log_line_is_ignored() {
        let store = task_store("malformed-tail");
        let event = event();
        append(&store, &event);
        std::fs::OpenOptions::new().append(true).open(store.state_dir().join("logs/st_00000001.jsonl")).expect("open log")
            .write_all(b"{broken").expect("append malformed trailing line");
        assert_eq!(read_yield_events(&store, &event.pool_id, &event.task_id, event.run_epoch, event.generation).expect("read"), vec![event]);
    }
    #[test] fn stale_kernel_tool_error_remains_a_keyed_value() { let value=WorkpoolYieldValue::Error { key: "a".into(), error: super::super::types::WorkpoolYieldError { code: "kernel_tool_stale".into(), message: "Kernel tool descriptor generation is stale".into() } }; assert_eq!(value, WorkpoolYieldValue::Error { key: "a".into(), error: super::super::types::WorkpoolYieldError { code: "kernel_tool_stale".into(), message: "Kernel tool descriptor generation is stale".into() } }); }
    #[test] fn stale_error_digest_does_not_equal_cancelled() { let stale=WorkpoolYieldValue::Error { key: "a".into(), error: super::super::types::WorkpoolYieldError { code: "kernel_tool_stale".into(), message: "stale".into() } }; let cancelled=WorkpoolYieldValue::Error { key: "a".into(), error: super::super::types::WorkpoolYieldError { code: "cancelled".into(), message: "stale".into() } }; assert_ne!(yield_digest(&stale), yield_digest(&cancelled)); }
}
