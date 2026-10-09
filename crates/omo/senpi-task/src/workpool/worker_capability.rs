use serde_json::Value;

use super::store::WorkpoolStore;
use super::types::{PoolId, WorkpoolError, WorkpoolErrorCode, WorkpoolEvent, WorkpoolItemStatus};

pub fn create_workpool_yield_capability(
    store: WorkpoolStore,
    task_id: impl Into<String>,
    run_epoch: u32,
) -> impl Fn(Value) -> Result<(PoolId, u32, Vec<WorkpoolYieldReceipt>), WorkpoolError> {
    let task_id = task_id.into();
    move |value| {
        let envelope = value.as_object().ok_or_else(|| WorkpoolError::new(WorkpoolErrorCode::InvalidInput, "Invalid workpool arguments."))?;
        if envelope.get("op").and_then(Value::as_str) != Some("yield") {
            return Err(WorkpoolError::new(WorkpoolErrorCode::InvalidInput, "Invalid workpool arguments."));
        }
        let results = envelope.get("results").and_then(Value::as_array).ok_or_else(|| WorkpoolError::new(WorkpoolErrorCode::InvalidInput, "Invalid workpool arguments."))?;
        let pool = store.list().into_iter().find(|candidate| candidate.items.iter().any(|item| item.binding.as_ref().is_some_and(|binding| binding.task_id == task_id)));
        let pool = pool.ok_or_else(|| WorkpoolError::new(WorkpoolErrorCode::WorkerUnassigned, "Worker has no pool assignment."))?;
        if pool.status == super::types::WorkpoolStatus::Cancelled || !pool.items.iter().any(|item| item.binding.as_ref().is_some_and(|binding| binding.task_id == task_id && binding.run_epoch == run_epoch && binding.generation == pool.generation)) {
            return Err(WorkpoolError::new(WorkpoolErrorCode::StaleAssignment, "Yield does not identify this worker's current assignment."));
        }
        let receipts = results.iter().map(|result| {
            let key = result.get("key").and_then(Value::as_str).map(str::to_string);
            match key {
                Some(key) => WorkpoolYieldReceipt::Refused { key: Some(key), code: "yield_unavailable".into(), message: "Yield acceptance is owned by the workpool admission lane.".into() },
                None => WorkpoolYieldReceipt::Refused { key: None, code: "invalid_input".into(), message: "Expected one key and either JSON data or a typed error.".into() },
            }
        }).collect();
        Ok((pool.pool_id, pool.generation, receipts))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkpoolYieldReceipt { Accepted { item_id: String }, Refused { key: Option<String>, code: String, message: String } }

pub fn assignment_is_current(store: &WorkpoolStore, task_id: &str, run_epoch: u32) -> Result<(), WorkpoolError> {
    let assigned = store.list().into_iter().any(|pool| pool.items.iter().any(|item| item.status == WorkpoolItemStatus::Assigned && item.binding.as_ref().is_some_and(|binding| binding.task_id == task_id && binding.run_epoch == run_epoch && binding.generation == pool.generation)));
    if assigned { Ok(()) } else { Err(WorkpoolError::new(WorkpoolErrorCode::StaleAssignment, "Yield does not identify this worker's current assignment.")) }
}

pub type WorkpoolEventSink = Box<dyn Fn(WorkpoolEvent) + Send + Sync>;
