//! Native Rust port of `@oh-my-opencode/senpi-desktop-protocol`.
//!
//! Provides engine JSON-RPC wire types, audit records, computer call-chain
//! approval classification, and session snapshots.

pub mod audit;
pub mod call;
pub mod engine_schema_generated;
pub mod json_rpc;
pub mod session;
pub mod wire;

pub use audit::{AuditPrimaryError, AuditRecord, AuditStatus};
pub use call::{
    ComputerCallError, ComputerCallErrorReason, ComputerCallPolicy, ComputerCallStep,
    DESKTOP_METHODS, ELEMENT_METHODS, WINDOW_METHODS, classify_computer_call,
    is_read_only_computer_call,
};
pub use engine_schema_generated::{
    ENGINE_ABI, ENGINE_METHODS, ENGINE_NOTIFICATIONS, ERROR_CODES, ErrorCode, PROTOCOL_VERSION,
};
pub use json_rpc::{
    EngineErrorData, EngineNotificationMessage, JsonRpcError, JsonRpcErrorData, JsonRpcFailure,
    JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, JsonRpcSuccess, MethodRejection,
    MethodRejectionData, PermissionDeniedData, RequestId, StandardRpcErrors, TccPermission,
};
pub use session::{
    ComputerDisplay, ComputerImageDisplay, ComputerRunOk, ComputerScreenshot,
    ComputerSessionSnapshot, ComputerTextDisplay,
};
pub use wire::{
    AuditEvent, DesktopCapabilities, EngineLog, LogLevel, StopPathKind, StopPathStatus,
};
