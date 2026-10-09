use std::path::Path;

use super::types::{WorkpoolError, WorkpoolErrorCode};

pub const WORKPOOL_STATE_DIR_ENV: &str = "SENPI_WORKPOOL_STATE_DIR";
pub const WORKPOOL_TASK_ID_ENV: &str = "SENPI_WORKPOOL_TASK_ID";

pub fn register_process_workpool_worker(state_dir: Option<&str>, task_id: Option<&str>) -> Result<bool, WorkpoolError> {
    if state_dir.is_none() && task_id.is_none() { return Ok(false); }
    let state_dir = state_dir.ok_or_else(|| WorkpoolError::new(WorkpoolErrorCode::WorkerUnassigned, "Incomplete worker launch identity."))?;
    if !Path::new(state_dir).is_absolute() || task_id.is_none() { return Err(WorkpoolError::new(WorkpoolErrorCode::WorkerUnassigned, "Incomplete worker launch identity.")); }
    Ok(true)
}

#[cfg(test)] mod tests { use super::*; #[test] fn absent_identity_is_not_a_worker() { assert!(!register_process_workpool_worker(None, None).expect("result")); } }
