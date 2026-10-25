//! Store extension types: registrations, session-callable ops, results and the transaction handle.
//!
//! Upstream counterpart: `thread/gateway/store-extensions.ts`. A compiled extension module replaces
//! the upstream JavaScript import: the worker resolves `moduleUrl` in a registry of built modules,
//! so a session op never loads code at call time.

pub type RegisterStoreExtensionCallback = Arc<dyn Fn(&StoreExtensionRegistration, i64) -> StoreExtensionResult<i64> + Send + Sync>;
pub type ExtensionCallCallback = Arc<dyn Fn(&str, &str, &Value, i64) -> StoreExtensionResult<Value> + Send + Sync>;
pub type ExtensionSessionAwaitCallback = Arc<dyn Fn(&str, &str, &Value, &SessionCaller, i64) -> StoreExtensionResult<Value> + Send + Sync>;
use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::store_ops::SqlRow;
use super::types::{BindingRef, GatewayDeliveryResult};

/// One declared await: the ops that read and expire the request, and its deadline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AwaitSpec {
    pub status_op: String,
    pub expire_op: String,
    pub timeout_ms: i64,
}

/// One session-callable op: through its own named tool, or only from the session's own component.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCallableOp {
    pub op: String,
    pub parameters: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_arg: Option<String>,
    /// `true` for an internal op: it gets no tool, so it declares no `toolName`, `await` or `wake`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub internal: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, rename = "await", skip_serializing_if = "Option::is_none")]
    pub await_spec: Option<AwaitSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake: Option<String>,
}

impl SessionCallableOp {
    pub fn is_tool(&self) -> bool {
        self.internal != Some(true)
    }
}

/// What a registration persists: everything a process that never registered it needs to call it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreExtensionRegistration {
    pub name: String,
    pub migrations: Vec<Vec<String>>,
    pub module_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_callable: Option<Vec<SessionCallableOp>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_dir: Option<String>,
}

/// The persisted half of a registration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedDescriptor {
    pub module_url: String,
    pub migrations: Vec<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_callable: Option<Vec<SessionCallableOp>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_dir: Option<String>,
}

impl PersistedDescriptor {
    pub fn of(descriptor: &StoreExtensionRegistration) -> Self {
        Self {
            module_url: descriptor.module_url.clone(),
            migrations: descriptor.migrations.clone(),
            session_callable: descriptor.session_callable.clone(),
            wake_dir: descriptor.wake_dir.clone(),
        }
    }

    pub fn named(&self, name: &str) -> StoreExtensionRegistration {
        StoreExtensionRegistration {
            name: name.to_owned(),
            migrations: self.migrations.clone(),
            module_url: self.module_url.clone(),
            session_callable: self.session_callable.clone(),
            wake_dir: self.wake_dir.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreExtensionRefusalCode {
    InvalidArguments,
    CallerNotAllowed,
    CallerContextMissing,
    ExtensionImportFailed,
    ExtensionUnknownOp,
    ExtensionUnknownName,
    ExtensionSchemaViolation,
    ExtensionOperationFailed,
    GatewayLockWaitExceeded,
    GatewaySchemaTooNew,
    AwaitUnresolved,
}

impl StoreExtensionRefusalCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArguments => "invalid_arguments",
            Self::CallerNotAllowed => "caller_not_allowed",
            Self::CallerContextMissing => "caller_context_missing",
            Self::ExtensionImportFailed => "extension_import_failed",
            Self::ExtensionUnknownOp => "extension_unknown_op",
            Self::ExtensionUnknownName => "extension_unknown_name",
            Self::ExtensionSchemaViolation => "extension_schema_violation",
            Self::ExtensionOperationFailed => "extension_operation_failed",
            Self::GatewayLockWaitExceeded => "gateway_lock_wait_exceeded",
            Self::GatewaySchemaTooNew => "gateway_schema_too_new",
            Self::AwaitUnresolved => "await_unresolved",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoreExtensionResult<T> {
    Ok { value: T },
    Refused { code: StoreExtensionRefusalCode, message: String },
}

impl<T> StoreExtensionResult<T> {
    pub fn ok(value: T) -> Self {
        Self::Ok { value }
    }

    pub fn refused(code: StoreExtensionRefusalCode, message: impl Into<String>) -> Self {
        Self::Refused { code, message: message.into() }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }

    pub fn into_result(self) -> Result<T, (StoreExtensionRefusalCode, String)> {
        match self {
            Self::Ok { value } => Ok(value),
            Self::Refused { code, message } => Err((code, message)),
        }
    }
}

/// A declared session op as every process lists it from the store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeclaredSessionOp {
    #[serde(flatten)]
    pub entry: SessionCallableOp,
    pub extension: String,
    /// The tool's name in a session: `ext_<extension>_<toolName>`.
    pub registered_name: String,
}

/// The engine-resolved caller a session call runs as; never taken from the op's arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCaller {
    pub caller_durable_id: String,
}

pub type TransactionAll<'a> = std::rc::Rc<dyn Fn(&[String], &str, &[Value], Option<&str>) -> Result<Vec<SqlRow>, String> + 'a>;
pub type TransactionOne<'a> = std::rc::Rc<dyn Fn(&[String], &str, &[Value]) -> Result<Option<SqlRow>, String> + 'a>;
pub type TransactionExec<'a> = std::rc::Rc<dyn Fn(&str, &[Value]) -> Result<usize, String> + 'a>;
pub type TransactionSessions<'a> = std::rc::Rc<dyn Fn(&str) -> Result<Vec<Value>, String> + 'a>;
/// The transaction helpers an extension op may call, all inside its own transaction.
#[derive(Clone)]
pub struct StoreExtensionTransaction<'a> {
    pub all: TransactionAll<'a>,
    pub one: TransactionOne<'a>,
    pub exec: TransactionExec<'a>,
    pub enqueue: std::rc::Rc<dyn Fn(ExtensionEnqueueRequest) -> Result<GatewayDeliveryResult, String> + 'a>,
    pub outbox_ack: std::rc::Rc<dyn Fn(ExtensionAckRequest) -> Result<ExtensionAckReply, String> + 'a>,
    pub bind: std::rc::Rc<dyn Fn(ExtensionBindRequest) -> Result<ExtensionBindReply, String> + 'a>,
    pub unbind: std::rc::Rc<dyn Fn(ExtensionUnbindRequest) -> Result<ExtensionUnbindReply, String> + 'a>,
    pub rebind: std::rc::Rc<dyn Fn(ExtensionRebindRequest) -> Result<ExtensionRebindReply, String> + 'a>,
    pub binding_for: std::rc::Rc<dyn Fn(ExtensionBindingLookup) -> Result<Option<Value>, String> + 'a>,
    pub bindings_for_session: TransactionSessions<'a>,
    pub outbox_pending: std::rc::Rc<dyn Fn(ExtensionOutboxRequest) -> Result<Value, String> + 'a>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionEnqueueRequest {
    pub binding_id: String,
    pub event_id: String,
    pub text: String,
    pub mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionAckRequest {
    pub binding_id: String,
    pub cursor: i64,
    pub provider_message_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionAckReply {
    pub binding_id: String,
    pub acked_cursor: i64,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionBindRequest {
    pub input: Value,
    pub principal: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionBindReply {
    pub binding: Value,
    pub deduplicated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionUnbindRequest {
    pub binding_id: String,
    pub expected_revision: i64,
    pub principal: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionUnbindReply {
    pub binding: Value,
    pub already_closed: bool,
    pub in_flight: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionRebindRequest {
    pub binding_id: String,
    pub expected_revision: i64,
    pub session_durable_id: String,
    pub principal: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionRebindReply {
    pub binding: Value,
    pub closed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionBindingLookup {
    pub platform: String,
    pub account_id: String,
    pub chat_id: String,
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionOutboxRequest {
    pub binding_id: String,
    pub after_cursor: Option<i64>,
    pub limit: Option<i64>,
}

/// One operation a compiled extension module exports.
pub type StoreExtensionOperation = Arc<dyn Fn(&StoreExtensionTransaction, Value) -> Result<Value, StoreExtensionError> + Send + Sync>;

#[derive(Debug, Clone, PartialEq)]
pub struct StoreExtensionError {
    pub code: StoreExtensionRefusalCode,
    pub message: String,
}

impl StoreExtensionError {
    pub fn invalid_arguments(message: impl Into<String>) -> Self {
        Self { code: StoreExtensionRefusalCode::InvalidArguments, message: message.into() }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self { code: StoreExtensionRefusalCode::ExtensionOperationFailed, message: message.into() }
    }
}

/// The compiled modules this binary carries, keyed by the `moduleUrl` a registration names.
pub type ExtensionModuleRegistry = BTreeMap<String, BTreeMap<String, StoreExtensionOperation>>;

/// The public extension surface the store facade exposes.
pub struct StoreExtensionApi {
    pub register_store_extension: RegisterStoreExtensionCallback,
    pub extension_call: ExtensionCallCallback,
}

/// The session channel: reachable only from the session's own extension tools.
pub struct StoreExtensionSessionApi {
    pub session_callable_ops: Arc<dyn Fn() -> Vec<DeclaredSessionOp> + Send + Sync>,
    pub extension_session_await: ExtensionSessionAwaitCallback,
}

/// The delivery facts an extension enqueue carries back.
pub fn binding_ref(binding_id: &str, revision: i64) -> BindingRef {
    BindingRef { binding_id: binding_id.to_owned(), revision }
}
