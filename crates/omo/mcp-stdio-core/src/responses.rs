use serde_json::Value;

use crate::types::JsonRpcId;
use crate::types::JsonRpcResponse;
use crate::types::JsonRpcResult;

pub fn success_response(id: JsonRpcId, result: JsonRpcResult) -> JsonRpcResponse {
    JsonRpcResponse::success(id, result)
}

pub fn error_response(
    id: JsonRpcId,
    code: i64,
    message: impl Into<String>,
    data: Option<Value>,
) -> JsonRpcResponse {
    match data {
        Some(data) => JsonRpcResponse::error_with_data(id, code, message, data),
        None => JsonRpcResponse::error(id, code, message),
    }
}

pub fn json_rpc_id(value: &Value) -> JsonRpcId {
    match value {
        Value::String(text) => JsonRpcId::String(text.clone()),
        Value::Number(number) => JsonRpcId::Number(number.clone()),
        Value::Null => JsonRpcId::Null,
        _ => JsonRpcId::Null,
    }
}

pub fn message_from_error(error: &(impl std::fmt::Display + ?Sized)) -> String {
    error.to_string()
}
