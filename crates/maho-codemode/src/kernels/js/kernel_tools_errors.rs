use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelToolErrorCode {
    ToolsUnavailable,
    InvalidToolDefinition,
    ReservedToolName,
    ToolNameCollision,
    KernelToolStale,
    KernelToolMissing,
    KernelToolFailed,
    KernelToolRecursion,
    KernelToolHostDenied,
}

pub const KERNEL_TOOL_ERROR_CODES: [KernelToolErrorCode; 9] = [
    KernelToolErrorCode::ToolsUnavailable,
    KernelToolErrorCode::InvalidToolDefinition,
    KernelToolErrorCode::ReservedToolName,
    KernelToolErrorCode::ToolNameCollision,
    KernelToolErrorCode::KernelToolStale,
    KernelToolErrorCode::KernelToolMissing,
    KernelToolErrorCode::KernelToolFailed,
    KernelToolErrorCode::KernelToolRecursion,
    KernelToolErrorCode::KernelToolHostDenied,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KernelToolHostDenialReason { Allow, Deny }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelToolHostDenial {
    pub tool: String,
    pub call_id: String,
    pub reason: KernelToolHostDenialReason,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct KernelToolError {
    pub name: String,
    pub code: KernelToolErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<KernelToolHostDenial>,
}

pub fn kernel_tool_error(code: KernelToolErrorCode, message: impl Into<String>, details: Option<KernelToolHostDenial>) -> KernelToolError {
    KernelToolError { name: "KernelToolError".into(), code, message: message.into(), details }
}
