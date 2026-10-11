use serde::de::DeserializeOwned;
use serde_json::Value;

use super::types::{PoolId, WorkpoolError, WorkpoolErrorCode};

pub fn nonempty(value: &str) -> bool { !value.trim().is_empty() }
pub fn parse_pool_id(value: &str) -> Result<PoolId, WorkpoolError> {
    if value.len() == 35 && value.starts_with("wp_") && value[3..].bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) { Ok(value.to_string()) }
    else { Err(WorkpoolError::new(WorkpoolErrorCode::InvalidPoolId, "Expected a wp_ pool identifier.")) }
}
pub fn parse_input<T: DeserializeOwned>(value: Value) -> Result<T, WorkpoolError> { serde_json::from_value(value).map_err(|_| WorkpoolError::new(WorkpoolErrorCode::InvalidInput, "Invalid workpool arguments.")) }

pub fn parse_command(mut value: Value) -> Result<crate::tools::workpool_schema::WorkpoolToolParams, WorkpoolError> {
    let invalid = || WorkpoolError::new(WorkpoolErrorCode::InvalidInput, "Invalid workpool arguments.");
    let object = value.as_object_mut().ok_or_else(invalid)?;
    let operation = object.get("op").and_then(Value::as_str).ok_or_else(invalid)?.to_owned();
    let allowed: &[&str] = match operation.as_str() {
        "create" => &["op", "name", "agent", "mode", "tools"],
        "push" => &["op", "pool_id", "items"],
        "inspect" | "close" | "cancel" => &["op", "pool_id"],
        "yield" => &["op", "results"],
        _ => return Err(invalid()),
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) { return Err(invalid()); }
    let trim = |value: &mut Value| -> Result<(), WorkpoolError> {
        let text = value.as_str().filter(|text| nonempty(text)).ok_or_else(invalid)?.trim().to_owned();
        *value = Value::String(text);
        Ok(())
    };
    match operation.as_str() {
        "create" => {
            trim(object.get_mut("name").ok_or_else(invalid)?)?;
            let agent = object.get_mut("agent").and_then(Value::as_object_mut).ok_or_else(invalid)?;
            let target = if agent.contains_key("category") { "category" } else { "subagent_type" };
            if agent.keys().any(|key| ![target, "prompt", "model"].contains(&key.as_str())) { return Err(invalid()); }
            trim(agent.get_mut(target).ok_or_else(invalid)?)?;
            trim(agent.get_mut("prompt").ok_or_else(invalid)?)?;
            if let Some(model) = agent.get_mut("model") { trim(model)?; }
            if let Some(mode) = object.get("mode") && !matches!(mode.as_str(), Some("fresh" | "keep_alive")) { return Err(invalid()); }
            if let Some(tools) = object.get_mut("tools") {
                for tool in tools.as_array_mut().ok_or_else(invalid)? { trim(tool)?; }
            }
        }
        "push" | "inspect" | "close" | "cancel" => {
            let pool_id = object.get("pool_id").and_then(Value::as_str).ok_or_else(invalid)?;
            parse_pool_id(pool_id).map_err(|_| invalid())?;
            if operation == "push" {
                for item in object.get_mut("items").and_then(Value::as_array_mut).ok_or_else(invalid)? {
                    let item = item.as_object_mut().ok_or_else(invalid)?;
                    if item.len() != 2 || !item.contains_key("input") { return Err(invalid()); }
                    trim(item.get_mut("key").ok_or_else(invalid)?)?;
                }
            }
        }
        "yield" => {
            for result in object.get_mut("results").and_then(Value::as_array_mut).ok_or_else(invalid)? {
                let result = result.as_object_mut().ok_or_else(invalid)?;
                if result.len() != 2 || result.contains_key("data") == result.contains_key("error") { return Err(invalid()); }
                trim(result.get_mut("key").ok_or_else(invalid)?)?;
                if let Some(error) = result.get_mut("error") {
                    let error = error.as_object_mut().ok_or_else(invalid)?;
                    if error.len() != 2 { return Err(invalid()); }
                    trim(error.get_mut("code").ok_or_else(invalid)?)?;
                    trim(error.get_mut("message").ok_or_else(invalid)?)?;
                }
            }
        }
        _ => unreachable!(),
    }
    parse_input(value)
}
pub fn canonical_json(value: &Value) -> String {
    match value { Value::Null|Value::Bool(_)|Value::Number(_)|Value::String(_) => value.to_string(), Value::Array(items) => format!("[{}]", items.iter().map(canonical_json).collect::<Vec<_>>().join(",")), Value::Object(map) => { let mut keys: Vec<_> = map.keys().collect(); keys.sort(); format!("{{{}}}", keys.into_iter().map(|k| format!("{}:{}", serde_json::to_string(k).expect("json"), canonical_json(&map[k]))).collect::<Vec<_>>().join(",")) } }
}
#[cfg(test)] mod tests { use super::*; #[test] fn canonical_json_sorts_keys() { let value=serde_json::json!({"b":1,"a":2}); assert_eq!(canonical_json(&value), r#"{"a":2,"b":1}"#); } }
