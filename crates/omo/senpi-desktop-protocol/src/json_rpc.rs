//! JSON-RPC protocol types mirrored from `src/json-rpc.ts`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use senpi_desktop_core::error::{PermissionDeniedData, TccPermission};
pub use senpi_desktop_core::protocol::{
    INVALID_PARAMS, INVALID_REQUEST, JsonRpcVersion, METHOD_NOT_FOUND, MethodRejection,
    PARSE_ERROR, RequestId, rpc_error_code,
};

use crate::engine_schema_generated::ErrorCode;
use crate::wire::{AuditEvent, EngineLog, StopPathStatus};

/// A client request; `id` is absent only for client notifications such as `$/cancel`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct JsonRpcRequest<P = Value> {
    pub jsonrpc: JsonRpcVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<RequestId>,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<P>,
}

/// Error data carried by every `ErrorCode` failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EngineErrorData {
    pub code: ErrorCode,
    /// Recovery hint for the model, e.g. `capture it again`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<PermissionDeniedData>,
}

/// Error data carried by every `-32601` rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MethodRejectionData {
    pub reason: MethodRejection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum JsonRpcErrorData {
    Engine(EngineErrorData),
    Rejected(MethodRejectionData),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<JsonRpcErrorData>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct JsonRpcSuccess<R = Value> {
    pub jsonrpc: JsonRpcVersion,
    pub id: RequestId,
    pub result: R,
}

/// `id` is `null` only when the engine could not read the request id (parse error).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct JsonRpcFailure {
    pub jsonrpc: JsonRpcVersion,
    pub id: Option<RequestId>,
    pub error: JsonRpcError,
}

/// A reply to one request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum JsonRpcResponse<R = Value> {
    Success(JsonRpcSuccess<R>),
    Failure(JsonRpcFailure),
}

/// Payload of each server-to-client notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "method", content = "params")]
pub enum EngineNotificationMessage {
    #[serde(rename = "audit")]
    Audit(AuditEvent),
    #[serde(rename = "stopPath.changed")]
    StopPathChanged(StopPathStatus),
    #[serde(rename = "engine.log")]
    EngineLog(EngineLog),
}

/// A server-to-client notification, discriminated by `method`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct JsonRpcNotification {
    pub jsonrpc: JsonRpcVersion,
    #[serde(flatten)]
    pub message: EngineNotificationMessage,
}

/// JSON-RPC 2.0 standard error codes the engine emits.
pub struct StandardRpcErrors;

impl StandardRpcErrors {
    pub const PARSE_ERROR: i64 = PARSE_ERROR;
    pub const INVALID_REQUEST: i64 = INVALID_REQUEST;
    pub const METHOD_NOT_FOUND: i64 = METHOD_NOT_FOUND;
    pub const INVALID_PARAMS: i64 = INVALID_PARAMS;
}
