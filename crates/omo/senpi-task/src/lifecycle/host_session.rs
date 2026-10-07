//! Port of `lifecycle/host-session.ts`: the lifecycle's view of a daemon-hosted child.
//!
//! A host session has no pid of its own: its liveness is the daemon answering plus the daemon still
//! listing its session path, and it is ended with `close_session`, never with a signal.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use crate::state::{HostSessionIdentity, RunnerKind, TaskRecord};

/// What the lifecycle needs to know about a child that lives as a SESSION of the shared daemon, and
/// nothing more (TS `HostSessionRecord`). Rust has no refinement type, so [`is_host_session_record`]
/// is the predicate and [`host_session_identity`] the narrowing accessor: a host session has no pid
/// of its own, and is ended with `close_session`, never with a signal.
pub fn is_host_session_record(record: &TaskRecord) -> bool {
    record.runner_kind == Some(RunnerKind::HostSession) && record.host_session.is_some()
}

/// The daemon identity of a host-session record, when the record is one.
pub fn host_session_identity(record: &TaskRecord) -> Option<&HostSessionIdentity> {
    if !is_host_session_record(record) {
        return None;
    }
    record.host_session.as_ref()
}

/// The resume path of a daemon-hosted child is its RECORDED session path, never a disk scan
/// (TS `hostSessionResumePath`).
pub fn host_session_resume_path(record: &TaskRecord) -> Option<&str> {
    host_session_identity(record).map(|host| host.session_path.as_str())
}

/// `unknown`: the daemon answers but could not list its sessions, so whether this one still runs is
/// unknown. A caller that would otherwise END a child on a `gone` answer must wait instead (omo#9450).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostSessionLiveness {
    Live,
    Gone,
    Unknown,
}

impl HostSessionLiveness {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Gone => "gone",
            Self::Unknown => "unknown",
        }
    }
}

/// What the lifecycle needs to know about a daemon-hosted child (TS HostSessionProbe).
pub trait HostSessionProbe: Send + Sync {
    fn daemon_alive(&self, host_session: &HostSessionIdentity) -> bool;
    /// True only for a session the daemon listed; a failed listing reads false.
    fn session_live(&self, host_session: &HostSessionIdentity) -> bool;
    fn session_liveness(&self, host_session: &HostSessionIdentity) -> HostSessionLiveness;
    /// Refresh one recorded endpoint, or all snapshots when starting a reconcile/TTL pass.
    fn refresh(&self, socket: Option<&str>);
}

/// The host-owned daemon transport the probe reads (TS HostSessionProbePorts). Assembly-owned:
/// these are the `runners/rpc-host/liveness` entry points the host owner provides.
pub type LiveSessionPaths = Arc<dyn Fn(&str) -> Result<Vec<String>, String> + Send + Sync>;

#[derive(Clone)]
pub struct HostSessionProbePorts {
    /// `probeHost` for a socket; a thrown/erroring call reads as unreachable.
    pub daemon_reachable: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// `list_sessions { include_workers: true }`; `Err` means the daemon answered but could not list.
    pub live_session_paths: LiveSessionPaths,
}

struct HostSnapshot {
    daemon_alive: bool,
    listed: bool,
    live_paths: HashSet<String>,
}

/// ONE `probeHost` + ONE `list_sessions` per socket per pass - never per record. Every record in a
/// reconcile/TTL pass shares the in-flight snapshot; `refresh` is what starts the next pass. A daemon
/// that does not answer reads as `nothing is live`: the lifecycle then reopens from JSONL instead of
/// attaching, and closes nothing. A daemon that answers but cannot list is `unknown` to
/// `session_liveness`, so a pending cancel waits instead of finishing over a running session.
pub fn create_host_session_probe(ports: HostSessionProbePorts) -> Arc<dyn HostSessionProbe> {
    Arc::new(CachingHostSessionProbe {
        ports,
        passes: Mutex::new(HashMap::new()),
    })
}

struct CachingHostSessionProbe {
    ports: HostSessionProbePorts,
    passes: Mutex<HashMap<String, Arc<HostSnapshot>>>,
}

impl CachingHostSessionProbe {
    fn snapshot(&self, socket: &str) -> Arc<HostSnapshot> {
        let mut passes = self.passes.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(cached) = passes.get(socket) {
            return Arc::clone(cached);
        }
        let daemon_alive = (self.ports.daemon_reachable)(socket);
        let (listed, paths) = match (self.ports.live_session_paths)(socket) {
            Ok(paths) => (true, paths),
            Err(_) => (false, Vec::new()),
        };
        let taken = Arc::new(HostSnapshot {
            daemon_alive,
            listed,
            live_paths: paths.iter().map(|path| canonical_session_path(path)).collect(),
        });
        passes.insert(socket.to_string(), Arc::clone(&taken));
        taken
    }
}

impl HostSessionProbe for CachingHostSessionProbe {
    fn daemon_alive(&self, host_session: &HostSessionIdentity) -> bool {
        self.snapshot(&host_session.socket).daemon_alive
    }

    fn session_live(&self, host_session: &HostSessionIdentity) -> bool {
        self.snapshot(&host_session.socket)
            .live_paths
            .contains(&canonical_session_path(&host_session.session_path))
    }

    fn session_liveness(&self, host_session: &HostSessionIdentity) -> HostSessionLiveness {
        let taken = self.snapshot(&host_session.socket);
        if !taken.listed {
            return if taken.daemon_alive {
                HostSessionLiveness::Unknown
            } else {
                HostSessionLiveness::Gone
            };
        }
        if taken
            .live_paths
            .contains(&canonical_session_path(&host_session.session_path))
        {
            HostSessionLiveness::Live
        } else {
            HostSessionLiveness::Gone
        }
    }

    fn refresh(&self, socket: Option<&str>) {
        let mut passes = self.passes.lock().unwrap_or_else(PoisonError::into_inner);
        match socket {
            None => passes.clear(),
            Some(socket) => {
                passes.remove(socket);
            }
        }
    }
}

/// The daemon lists a session by its canonical path while the record keeps the path omo asked for, so
/// a project reached through a symlink would never match (#8932).
pub fn canonical_session_path(path: &str) -> String {
    let path = Path::new(path);
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical.to_string_lossy().into_owned();
    }
    let Some(parent) = path.parent() else {
        return path.to_string_lossy().into_owned();
    };
    match (std::fs::canonicalize(parent), path.file_name()) {
        (Ok(parent), Some(name)) => {
            let mut joined: PathBuf = parent;
            joined.push(name);
            joined.to_string_lossy().into_owned()
        }
        _ => path.to_string_lossy().into_owned(),
    }
}

/// How revival reaches a RECORDED endpoint beyond probing it (TS HostEndpointPort). A lifecycle with
/// no task host passes [`NoHostEndpoint`]: probe only, never an ensure.
pub trait HostEndpointPort: Send + Sync {
    /// Names the endpoint this session runs behind (never ensured from inside).
    fn is_own(&self, socket: &str) -> bool;
    /// Re-ensures any other recorded socket, and only that socket.
    fn ensure(&self, socket: &str) -> HostEndpointEnsure;
    fn notice(&self, reason: HostEndpointNotice, socket: &str);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostEndpointEnsure {
    Ensured,
    Incompatible,
    Unreachable,
}

impl HostEndpointEnsure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ensured => "ensured",
            Self::Incompatible => "incompatible",
            Self::Unreachable => "unreachable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostEndpointNotice {
    HostIncompatible,
    OwnHostUnreachable,
}

impl HostEndpointNotice {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HostIncompatible => "host_incompatible",
            Self::OwnHostUnreachable => "own_host_unreachable",
        }
    }
}

/// The explicit `this lifecycle has no task host` answer: a silent recorded endpoint stays
/// `host_unreachable` (TS NO_HOST_ENDPOINT).
pub struct NoHostEndpoint;

impl HostEndpointPort for NoHostEndpoint {
    fn is_own(&self, _socket: &str) -> bool {
        false
    }

    fn ensure(&self, _socket: &str) -> HostEndpointEnsure {
        HostEndpointEnsure::Unreachable
    }

    fn notice(&self, _reason: HostEndpointNotice, _socket: &str) {}
}

/// The two bounded waits the daemon path owns (TS HostSessionRetryPolicy). The TS `wait` promise is
/// the caller's concern natively; the policy carries the millisecond budgets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSessionRetryPolicy {
    pub max_drain_attempts: u32,
    pub default_retry_after_ms: u64,
    pub daemon_loss_backoff_ms: Vec<u64>,
    /// Background retries of a reconcile that deferred a daemon-hosted child; spans a handoff drain.
    pub deferred_retry_backoff_ms: Vec<u64>,
}

pub const DEFAULT_HOST_SESSION_RETRY_POLICY: HostSessionRetryPolicy = HostSessionRetryPolicy {
    max_drain_attempts: 10,
    default_retry_after_ms: 2_000,
    daemon_loss_backoff_ms: Vec::new(),
    deferred_retry_backoff_ms: Vec::new(),
};

/// The concrete default backoffs (TS literals); a `const` cannot hold a `Vec`, so the defaults are
/// built here.
pub fn default_host_session_retry_policy() -> HostSessionRetryPolicy {
    HostSessionRetryPolicy {
        max_drain_attempts: 10,
        default_retry_after_ms: 2_000,
        daemon_loss_backoff_ms: vec![1_000, 4_000, 16_000],
        deferred_retry_backoff_ms: vec![
            5_000, 15_000, 30_000, 60_000, 120_000, 300_000, 300_000, 300_000, 300_000, 300_000,
        ],
    }
}

/// `session_path_in_use` from a generation that is still draining. Duck-typed on the engine's wire
/// contract (name + code) so the manager never imports the runner's error class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostDrainingHold {
    pub retry_after_ms: Option<u64>,
}

/// Reads a draining hold from an error's name (and an optional nested cause name).
pub fn read_host_draining_hold(error_name: &str, cause_name: Option<&str>) -> Option<HostDrainingHold> {
    if error_name != "SessionHeldElsewhereError" && cause_name != Some("SessionHeldElsewhereError") {
        return None;
    }
    Some(HostDrainingHold {
        retry_after_ms: None,
    })
}

#[cfg(test)]
#[path = "host_session_tests.rs"]
mod tests;
