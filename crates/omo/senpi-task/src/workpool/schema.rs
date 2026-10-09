use serde::de::DeserializeOwned;
use serde_json::Value;

use super::types::{PoolId, WorkpoolError, WorkpoolErrorCode};

pub fn nonempty(value: &str) -> bool { !value.trim().is_empty() }
pub fn parse_pool_id(value: &str) -> Result<PoolId, WorkpoolError> {
    if value.len() == 35 && value.starts_with("wp_") && value[3..].bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) { Ok(value.to_string()) }
    else { Err(WorkpoolError::new(WorkpoolErrorCode::InvalidPoolId, "Expected a wp_ pool identifier.")) }
}
pub fn parse_input<T: DeserializeOwned>(value: Value) -> Result<T, WorkpoolError> { serde_json::from_value(value).map_err(|_| WorkpoolError::new(WorkpoolErrorCode::InvalidInput, "Invalid workpool arguments.")) }
pub fn canonical_json(value: &Value) -> String {
    match value { Value::Null|Value::Bool(_)|Value::Number(_)|Value::String(_) => value.to_string(), Value::Array(items) => format!("[{}]", items.iter().map(canonical_json).collect::<Vec<_>>().join(",")), Value::Object(map) => { let mut keys: Vec<_> = map.keys().collect(); keys.sort(); format!("{{{}}}", keys.into_iter().map(|k| format!("{}:{}", serde_json::to_string(k).expect("json"), canonical_json(&map[k]))).collect::<Vec<_>>().join(",")) } }
}
#[cfg(test)] mod tests { use super::*; #[test] fn canonical_json_sorts_keys() { let value=serde_json::json!({"b":1,"a":2}); assert_eq!(canonical_json(&value), r#"{"a":2,"b":1}"#); } }
