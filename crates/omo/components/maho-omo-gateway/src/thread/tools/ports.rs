//! Host and surface ports for local & gateway thread tools (`thread/tools/ports.ts`).

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::super::adapter::{
    GatewayEndpointPort, GatewayWakeReply, ReleaseSessionReply, ReleaseSessionRequest,
};
use super::super::address::RemoteAddressSurface;
use super::super::address_book::{AddressBookHost, DiskSession};
use super::super::contracts::state::ThreadActivity;
use super::super::endpoint_controls::ThreadControl;
use super::super::endpoint_registry::EndpointKind;
use super::super::paged_reader::EntryPageRead;
use super::super::provenance_resolver::ProvenanceWorkspace;
use super::super::reader::ThreadTranscriptEntry;
use super::super::same_device_wake::SameDeviceWake;
use super::super::store_api::GatewayStoreApi;
use super::model_match::ModelChoice;
use super::reply_permission::ReplySender;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

pub const UNKNOWN_CALLER: &str = "unknown-caller";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadCloseOutcome {
    Closed,
    NotLive,
    HeldElsewhere,
}

impl ThreadCloseOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::NotLive => "not_live",
            Self::HeldElsewhere => "held_elsewhere",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadCloseOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graceful: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadCloseGraceful {
    pub turn: i64,
    pub waited_ms: i64,
    pub stopped_wake_sources: Vec<String>,
    pub returned_user_messages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadCloseAnswer {
    pub outcome: ThreadCloseOutcome,
    pub dropped_deliveries: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graceful: Option<ThreadCloseGraceful>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterruptRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub background: String,
}

#[derive(Debug, Clone, Default)]
pub struct ThreadHostSession {
    pub session_id: String,
    pub durable_session_id: Option<String>,
    pub session_path: Option<String>,
    pub cwd: String,
    pub name: Option<String>,
    pub status: Option<String>,
    pub kind: Option<String>,
    pub archived: Option<bool>,
    pub activity: Option<ThreadActivity>,
    pub error_code: Option<String>,
    pub turn_id: Option<String>,
    pub waiting: Option<Value>,
    pub model: Option<ModelChoice>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub endpoint_kind: Option<EndpointKind>,
    pub controls: Option<Vec<ThreadControl>>,
    pub engine_version: Option<String>,
    pub socket: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ThreadHostView {
    pub sessions: Vec<ThreadHostSession>,
    pub hosts: Vec<AddressBookHost>,
    pub disk: Vec<DiskSession>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ThreadHostViewRequest {
    pub offline: bool,
}

#[derive(Debug, Clone, Default)]
pub struct OpenSessionParams {
    pub cwd: Option<String>,
    pub session_path: Option<String>,
    pub name: Option<String>,
    pub fork_from: Option<String>,
    pub initial_skills: Option<Vec<String>>,
    pub permission_preset: Option<String>,
    pub provider: Option<String>,
    pub model_id: Option<String>,
    pub thinking_level: Option<String>,
    pub durable_session_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SessionStateSnapshot {
    pub is_streaming: Option<bool>,
    pub active_turn_id: Option<String>,
    pub model: Option<Value>,
    pub thinking_level: Option<Value>,
}

#[derive(Debug, Clone, Default)]
pub struct PromptDeliveryOptions {
    pub streaming_behavior: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionWatchStatusChange {
    pub from: Option<String>,
    pub to: String,
    pub cause: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SessionWatchResyncState {
    pub status: String,
    pub current_turn_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SessionWatchResync {
    pub state: SessionWatchResyncState,
    pub settled_turns: Vec<String>,
}

pub trait SessionWatchHandlers: Send + Sync {
    fn record(&self, record: &Value);
    fn status(&self, change: SessionWatchStatusChange);
    fn resynced(&self, resync: SessionWatchResync);
}

#[derive(Debug, Clone)]
pub struct SessionWatchInitial {
    pub status: String,
    pub current_turn_id: Option<String>,
}

pub trait SessionWatch: Send + Sync {
    fn initial(&self) -> Option<SessionWatchInitial>;
    fn ended(&self) -> BoxFuture<String>;
    fn stop(&self);
}

pub enum SessionWatchOutcome {
    Ok(Arc<dyn SessionWatch>),
    Refused { reason: String },
}

pub trait ThreadSessionPort: Send + Sync {
    fn get_messages(
        &self,
        session_id: &str,
    ) -> BoxFuture<Result<Vec<ThreadTranscriptEntry>, String>>;
    fn get_state(
        &self,
        session_id: &str,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<SessionStateSnapshot, String>>;
    fn prompt(
        &self,
        session_id: &str,
        message: &str,
        options: Option<PromptDeliveryOptions>,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<Option<String>, String>>;
    fn interrupt(
        &self,
        session_id: &str,
        request: InterruptRequest,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<Value, String>>;
    fn set_session_name(
        &self,
        session_id: &str,
        name: &str,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<(), String>>;
    fn set_session_archived(
        &self,
        session_id: &str,
        archived: bool,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<(), String>> {
        let _ = (session_id, archived, durable_session_id);
        Box::pin(async { Err("unsupported:set_session_archived".to_owned()) })
    }
    fn set_model(
        &self,
        session_id: &str,
        provider: &str,
        model_id: &str,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<ModelChoice, String>>;
    fn get_available_models(&self, session_id: &str)
    -> BoxFuture<Result<Vec<ModelChoice>, String>>;
    fn set_thinking_level(
        &self,
        session_id: &str,
        level: &str,
        scope: Option<&str>,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<(), String>>;
    fn get_available_thinking_levels(
        &self,
        session_id: &str,
    ) -> BoxFuture<Result<Vec<String>, String>>;
    fn wake(
        &self,
        session_id: &str,
        delivery_ids: &[String],
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<GatewayWakeReply, String>> {
        let _ = (session_id, delivery_ids, durable_session_id);
        Box::pin(async { Err("unsupported:wake".to_owned()) })
    }
    fn release_session(
        &self,
        session_id: &str,
        request: ReleaseSessionRequest,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<ReleaseSessionReply, String>> {
        let _ = (session_id, request, durable_session_id);
        Box::pin(async { Err("unsupported:release_session".to_owned()) })
    }
    fn close_session(
        &self,
        session_id: &str,
        durable_session_id: Option<&str>,
        options: ThreadCloseOptions,
    ) -> BoxFuture<Result<ThreadCloseAnswer, String>> {
        let _ = (session_id, durable_session_id, options);
        Box::pin(async { Err("unsupported:close_session".to_owned()) })
    }
    fn watch_session(
        &self,
        session_id: &str,
        handlers: Arc<dyn SessionWatchHandlers>,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<SessionWatchOutcome, String>> {
        let _ = (session_id, handlers, durable_session_id);
        Box::pin(async {
            Ok(SessionWatchOutcome::Refused {
                reason: "unsupported:subscribe_sessions".to_owned(),
            })
        })
    }
    fn read_entry_page(
        &self,
        session_id: &str,
        max_bytes: Option<usize>,
        cursor: Option<&str>,
    ) -> BoxFuture<Result<Option<EntryPageRead>, String>> {
        let _ = (session_id, max_bytes, cursor);
        Box::pin(async { Ok(None) })
    }
}

pub trait ThreadHost: ThreadSessionPort + Send + Sync {
    fn socket(&self) -> &str;
    fn list_sessions(&self) -> BoxFuture<Result<Vec<ThreadHostSession>, String>>;
    fn open_session(
        &self,
        params: OpenSessionParams,
    ) -> BoxFuture<Result<ThreadHostSession, String>>;
    fn list_view(
        &self,
        request: ThreadHostViewRequest,
    ) -> BoxFuture<Result<Option<ThreadHostView>, String>> {
        let _ = request;
        Box::pin(async { Ok(None) })
    }
    fn list_target(
        &self,
        durable_id: &str,
        endpoint_socket: &str,
        endpoint_kind: EndpointKind,
    ) -> BoxFuture<Result<Option<ThreadHostView>, String>> {
        let _ = (durable_id, endpoint_socket, endpoint_kind);
        Box::pin(async { Ok(None) })
    }
    fn endpoint(&self, socket: &str) -> Option<Arc<dyn ThreadSessionPort>> {
        let _ = socket;
        None
    }
    fn gateway(&self) -> Option<GatewayEndpointPort> {
        None
    }
}

pub type EnsureHostFn = Arc<dyn Fn() -> BoxFuture<Result<(), String>> + Send + Sync>;
pub type ProvenanceWorkspacesLoader =
    Arc<dyn Fn(&std::path::Path) -> Vec<ProvenanceWorkspace> + Send + Sync>;
pub type ProvenanceWarningFn = Arc<dyn Fn(&str) + Send + Sync>;
pub type CompletionArmedFn = Arc<dyn Fn(&str, i64) + Send + Sync>;

#[derive(Clone)]
pub struct ThreadToolSurfaceOptions {
    pub host: Arc<dyn ThreadHost>,
    pub store: Arc<dyn GatewayStoreApi>,
    pub caller_session_id: Arc<dyn Fn() -> String + Send + Sync>,
    pub caller_workspace_root: Arc<dyn Fn() -> String + Send + Sync>,
    pub state_directory: PathBuf,
    pub disk_sessions: Option<Arc<dyn Fn() -> Vec<DiskSession> + Send + Sync>>,
    pub sessions_directory: Option<Arc<dyn Fn() -> PathBuf + Send + Sync>>,
    pub ensure_host: Option<EnsureHostFn>,
    pub same_device_wake: Option<Arc<SameDeviceWake>>,
    pub load_provenance_workspaces: Option<ProvenanceWorkspacesLoader>,
    pub on_provenance_warning: Option<ProvenanceWarningFn>,
    pub caller_turn_id: Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>,
    pub caller_name: Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>,
    pub caller_cause: Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>,
    pub caller_chain_hop: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    pub caller_run_deliveries: Option<Arc<dyn Fn() -> Vec<String> + Send + Sync>>,
    pub caller_reply_targets: Option<Arc<dyn Fn() -> Vec<String> + Send + Sync>>,
    pub caller_reply_senders: Option<Arc<dyn Fn() -> Vec<ReplySender> + Send + Sync>>,
    pub caller_run_has_local_input: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    pub on_completion_armed: Option<CompletionArmedFn>,
    pub permission_preset: Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>,
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    pub first_prompt_wait_ms: Option<u64>,
    pub remote: Option<Arc<dyn RemoteAddressSurface>>,
    pub worktree_root: Option<Arc<dyn Fn() -> PathBuf + Send + Sync>>,
}
