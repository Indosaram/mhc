use serde_json::Value;
use super::schema::parse_pool_id;
use super::types::{WorkpoolError, WorkpoolErrorCode};

pub fn validate_record(value: &Value) -> Result<(), WorkpoolError> {
    let object = value.as_object().ok_or_else(|| WorkpoolError::new(WorkpoolErrorCode::InvalidInput, "Invalid workpool record."))?;
    if object.get("version").and_then(Value::as_u64) != Some(1) { return Err(WorkpoolError::new(WorkpoolErrorCode::InvalidInput, "Invalid workpool record.")); }
    let pool_id = object.get("pool_id").and_then(Value::as_str).ok_or_else(|| WorkpoolError::new(WorkpoolErrorCode::InvalidInput, "Invalid workpool record."))?;
    parse_pool_id(pool_id).map(|_| ())
}
#[cfg(test)] mod tests { use super::*; #[test] fn rejects_wrong_version() { assert!(validate_record(&serde_json::json!({"version":2})).is_err()); } }
