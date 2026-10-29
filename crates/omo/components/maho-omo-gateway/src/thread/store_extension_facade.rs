//! The store facade's extension surface: the public channel, the session channel and its await, and
//! the creator record.
//!
//! Upstream counterpart: `thread/gateway/store-extension-facade.ts`.

use std::path::Path;

use serde_json::Value;

use super::extension_registrations::is_await_request_id;
use super::extension_session_await::{SessionAwait, await_session_request};
use crate::paths::gateway_database_path;
use super::store_extensions::{StoreExtensionRefusalCode, StoreExtensionRegistration, StoreExtensionResult};

/// The worker's registration reply: the caller's result, and whether calls for that name now use it.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtensionRegisterReply {
    pub result: StoreExtensionResult<i64>,
    pub retained: bool,
}

/// A registration the current worker holds, with the time it was made: a restarted worker replays it as of then.
#[derive(Debug, Clone, PartialEq)]
pub struct RetainedRegistration {
    pub extension: StoreExtensionRegistration,
    pub registered_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ThreadCreationRecord {
    pub creator_durable_id: String,
    pub created_durable_id: String,
}

/// A newer core schema refuses extension requests as data.
pub fn schema_too_new(message: impl Into<String>) -> StoreExtensionResult<Value> {
    StoreExtensionResult::refused(StoreExtensionRefusalCode::GatewaySchemaTooNew, message)
}

/// Arguments a store cannot clone are refused before they reach an op.
pub fn clone_args(args: &Value) -> StoreExtensionResult<Value> {
    StoreExtensionResult::ok(args.clone())
}

/// The await file a session op's request id names, under the store's own directory.
pub fn await_file(agent_dir: &Path, wake_dir: &str, await_request_id: &str) -> Option<std::path::PathBuf> {
    if !is_await_request_id(await_request_id) {
        return None;
    }
    gateway_database_path(agent_dir).parent().map(|parent| parent.join(wake_dir).join(await_request_id))
}

/// The facade's await: the op's declared status and expire phases, bounded by its timeout.
pub fn session_await(
    agent_dir: &Path,
    wake_dir: &str,
    await_request_id: &str,
    timeout_ms: i64,
    status: &dyn Fn() -> StoreExtensionResult<Value>,
    expire: &dyn Fn() -> StoreExtensionResult<Value>,
) -> StoreExtensionResult<Value> {
    let Some(file) = await_file(agent_dir, wake_dir, await_request_id) else {
        return StoreExtensionResult::refused(StoreExtensionRefusalCode::InvalidArguments, "await_request_id must match ^tor_[a-f0-9]{32}$.");
    };
    await_session_request(SessionAwait { file, timeout_ms, status, expire, on_armed: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_traversal_request_id_never_names_a_file() {
        assert!(await_file(Path::new("/tmp/agent"), "alpha", "../secret").is_none());
        assert!(await_file(Path::new("/tmp/agent"), "alpha", "tor_0123456789abcdef0123456789abcdef").is_some());
    }

    #[test]
    fn schema_refusals_are_data() {
        let refused = schema_too_new("Gateway schema 11 is newer");
        assert!(matches!(refused, StoreExtensionResult::Refused { code: StoreExtensionRefusalCode::GatewaySchemaTooNew, .. }));
    }
}
