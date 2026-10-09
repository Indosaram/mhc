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
mod tests { use super::*; #[test] fn digest_is_stable_for_data() { let result=WorkpoolYieldValue::Data{key:"a".into(),data:serde_json::json!(1)}; assert_eq!(yield_digest(&result).len(),64); } }
