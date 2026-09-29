//! Lifecycle ports (`lifecycle/port.ts`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use serde::Serialize;

use crate::host::HostError;
use crate::manager::ManagedChildHandle;
use crate::state::TaskRecord;

/// Why a task is being torn down. Every destruction routes through the single-writer port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DestroyCause {
    Cancel,
    CancelWithoutAbort,
    Evict,
    Ttl,
    ReconcileLost,
    FallbackHandoff,
}

impl DestroyCause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cancel => "cancel",
            Self::CancelWithoutAbort => "cancel_without_abort",
            Self::Evict => "evict",
            Self::Ttl => "ttl",
            Self::ReconcileLost => "reconcile_lost",
            Self::FallbackHandoff => "fallback_handoff",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentKind {
    InProcess,
    Rpc,
}

/// The teardown seam of a resident child. Only the lifecycle invokes these.
pub trait ResidentHandle: Send + Sync {
    fn task_id(&self) -> &str;
    fn kind(&self) -> ResidentKind;
    fn pid(&self) -> Option<i64>;
    fn abort(&self) -> Result<(), HostError>;
    fn dispose(&self) -> Result<(), HostError>;
    fn terminate(&self) -> Result<(), HostError>;
}

/// The live-handle registry the lifecycle consults and prunes.
pub trait ResidencyRegistry: Send + Sync {
    fn get(&self, task_id: &str) -> Option<Arc<dyn ResidentHandle>>;
    fn entries(&self) -> Vec<Arc<dyn ResidentHandle>>;
    fn forget(&self, task_id: &str);
    fn has_pending_sends(&self, task_id: &str) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanSignal {
    Term,
    Kill,
}

impl OrphanSignal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Term => "SIGTERM",
            Self::Kill => "SIGKILL",
        }
    }
}

pub trait ProcessSignaller: Send + Sync {
    fn is_alive(&self, pid: i64) -> bool;
    fn signal(&self, pid: i64, signal: OrphanSignal);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RespawnFailureCode {
    ModelUnavailable,
    ToolsUnavailable,
    SpawnSpecUnavailable,
    SessionUnavailable,
    TeamInactive,
    RespawnFailed,
}

impl RespawnFailureCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ModelUnavailable => "model_unavailable",
            Self::ToolsUnavailable => "tools_unavailable",
            Self::SpawnSpecUnavailable => "spawn_spec_unavailable",
            Self::SessionUnavailable => "session_unavailable",
            Self::TeamInactive => "team_inactive",
            Self::RespawnFailed => "respawn_failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RespawnDisposition {
    Retryable,
    Unrecoverable,
}

pub enum RespawnResult {
    Ok(Arc<dyn ManagedChildHandle>),
    Failed {
        disposition: RespawnDisposition,
        code: RespawnFailureCode,
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReattachFailureKind {
    AlreadyAttached,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReattachResult {
    Ok,
    Failed {
        kind: ReattachFailureKind,
        reason: String,
    },
}

pub type RespawnPort = Arc<dyn Fn(&TaskRecord, Option<&Path>) -> RespawnResult + Send + Sync>;
pub type ReattachPort =
    Arc<dyn Fn(&TaskRecord, Arc<dyn ManagedChildHandle>) -> ReattachResult + Send + Sync>;

#[derive(Clone)]
pub struct LifecycleReattachPorts {
    pub respawn: RespawnPort,
    pub reattach: ReattachPort,
}

// TS keys a WeakMap by store identity; Rust keys by the store's state directory.
fn registered_ports() -> &'static Mutex<HashMap<PathBuf, LifecycleReattachPorts>> {
    static PORTS: OnceLock<Mutex<HashMap<PathBuf, LifecycleReattachPorts>>> = OnceLock::new();
    PORTS.get_or_init(Mutex::default)
}

pub fn register_lifecycle_reattach_ports(state_dir: &Path, ports: LifecycleReattachPorts) {
    registered_ports()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(state_dir.to_path_buf(), ports);
}

pub fn get_lifecycle_reattach_ports(state_dir: &Path) -> Option<LifecycleReattachPorts> {
    registered_ports()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(state_dir)
        .cloned()
}
