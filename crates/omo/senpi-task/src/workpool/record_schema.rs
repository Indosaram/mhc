use serde_json::Value;
use super::schema::parse_pool_id;
use super::types::{WorkpoolError, WorkpoolErrorCode};

pub fn validate_record(value: &Value) -> Result<(), WorkpoolError> {
    let object = value.as_object().ok_or_else(|| WorkpoolError::new(WorkpoolErrorCode::InvalidInput, "Invalid workpool record."))?;
    if object.get("version").and_then(Value::as_u64) != Some(1) { return Err(WorkpoolError::new(WorkpoolErrorCode::InvalidInput, "Invalid workpool record.")); }
    let pool_id = object.get("pool_id").and_then(Value::as_str).ok_or_else(|| WorkpoolError::new(WorkpoolErrorCode::InvalidInput, "Invalid workpool record."))?;
    parse_pool_id(pool_id)?;
    let invalid = || WorkpoolError::new(WorkpoolErrorCode::StoreCorrupt, "Invalid workpool pause metadata.");
    if let Some(hold) = object.get("hold") {
        let hold: super::types::WorkpoolHold = serde_json::from_value(hold.clone()).map_err(|_| invalid())?;
        if hold.pause_seq == 0 { return Err(invalid()); }
    }
    if let Some(workers) = object.get("workers").and_then(Value::as_array) {
        for worker in workers {
            if !matches!(worker.get("status").and_then(Value::as_str), Some("busy" | "idle" | "paused")) { return Err(invalid()); }
        }
    }
    if let Some(items) = object.get("items").and_then(Value::as_array) {
        for item in items {
            if let Some(ids) = item.get("delivery").and_then(|d| d.get("entry_ids"))
                && !ids.as_array().is_some_and(|ids| ids.iter().all(Value::is_string)) { return Err(invalid()); }
        }
    }
    Ok(())
}
#[cfg(test)] mod tests { use super::*; #[test] fn rejects_wrong_version() { assert!(validate_record(&serde_json::json!({"version":2})).is_err()); } }
