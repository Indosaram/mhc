use super::registry::{JsonRpcError,RegistryConnection};
use serde_json::{Map,Value};
pub use super::registry_listing::{decode_cursor,encode_cursor};

pub fn object_value(value: &Value) -> Map<String,Value> {value.as_object().cloned().unwrap_or_default()}
pub fn required_string<'a>(value: &'a Value,name: &str) -> Result<&'a str,JsonRpcError> {
    value.as_str().filter(|value|!value.is_empty()).ok_or_else(||JsonRpcError::new(-32603,format!("Invalid params: {name} is required")))
}
pub fn optional_string(value: &Value) -> Option<&str> {value.as_str()}
pub fn optional_number(value: &Value) -> Option<f64> {value.as_f64().filter(|number|number.is_finite())}
pub fn connection_id(connection: &RegistryConnection) -> Result<&str,JsonRpcError> {
    if connection.id.is_empty() {Err(JsonRpcError::new(-32603,"Connection id is required for thread lifecycle methods"))} else {Ok(&connection.id)}
}
