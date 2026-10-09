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
    for (index, line) in bytes.split('\n').enumerate() {
        if line.is_empty() { continue; }
        let raw: Value = match serde_json::from_str(line) { Ok(raw) => raw, Err(_) if index == bytes.split('\n').count() - 1 => break, Err(error) => return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, error)) };
        let Ok(event) = serde_json::from_value::<WorkpoolYieldEvent>(raw) else { continue };
        if event.pool_id == pool_id && event.task_id == task_id && event.run_epoch == run_epoch && event.generation == generation { events.push(event); }
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn digest_is_stable_for_data() { let result=WorkpoolYieldValue::Data{key:"a".into(),data:serde_json::json!(1)}; assert_eq!(yield_digest(&result).len(),64); }
    #[test] fn data_digest_is_key_sensitive() { let a=WorkpoolYieldValue::Data{key:"a".into(),data:serde_json::json!(1)}; let b=WorkpoolYieldValue::Data{key:"b".into(),data:serde_json::json!(1)}; assert_ne!(yield_digest(&a), yield_digest(&b)); }
    #[test] fn error_digest_is_stable_and_distinct() { let a=WorkpoolYieldValue::Error{key:"a".into(),error:super::super::types::WorkpoolYieldError{code:"kernel_tool_stale".into(),message:"stale".into()}}; let b=WorkpoolYieldValue::Error{key:"a".into(),error:super::super::types::WorkpoolYieldError{code:"cancelled".into(),message:"cancelled".into()}}; assert_eq!(yield_digest(&a).len(),64); assert_ne!(yield_digest(&a), yield_digest(&b)); }
    #[test] fn missing_yield_log_is_empty() { let store=TaskRecordStore::new(&crate::store::StateDirConfig { project_dir: "/tmp/workpool-stale-yield".into(), task_state_dir: None }); assert!(read_yield_events(&store,"wp_test","st_00000001",0,1).expect("read").is_empty()); }
    #[test] fn unrelated_yield_events_are_filtered() { let store=TaskRecordStore::new(&crate::store::StateDirConfig { project_dir: "/tmp/workpool-filter-yield".into(), task_state_dir: None }); assert!(read_yield_events(&store,"wp_test","st_00000001",0,1).expect("read").is_empty()); }
    #[test] fn malformed_trailing_log_line_is_ignored() { let store=TaskRecordStore::new(&crate::store::StateDirConfig { project_dir: "/tmp/workpool-malformed-yield".into(), task_state_dir: None }); assert!(read_yield_events(&store,"wp_test","st_00000001",0,1).expect("read").is_empty()); }
    #[test] fn stale_kernel_tool_error_remains_a_keyed_value() { let value=WorkpoolYieldValue::Error { key: "a".into(), error: super::super::types::WorkpoolYieldError { code: "kernel_tool_stale".into(), message: "Kernel tool descriptor generation is stale".into() } }; assert_eq!(value, WorkpoolYieldValue::Error { key: "a".into(), error: super::super::types::WorkpoolYieldError { code: "kernel_tool_stale".into(), message: "Kernel tool descriptor generation is stale".into() } }); }
    #[test] fn stale_error_digest_does_not_equal_cancelled() { let stale=WorkpoolYieldValue::Error { key: "a".into(), error: super::super::types::WorkpoolYieldError { code: "kernel_tool_stale".into(), message: "stale".into() } }; let cancelled=WorkpoolYieldValue::Error { key: "a".into(), error: super::super::types::WorkpoolYieldError { code: "cancelled".into(), message: "stale".into() } }; assert_ne!(yield_digest(&stale), yield_digest(&cancelled)); }
}
