//! Port of `lifecycle/host-session-default.ts`: the production daemon adapters.
//!
//! Upstream reaches `../runners/rpc-host/liveness` and `../runners/rpc-host/close` through `import()`.
//! Those transport modules are ASSEMBLY-OWNED here (the CLI/rpc-host wiring), so this consumer takes
//! them as an injected [`HostTransport`] and builds the default probe/closer over it. Nothing here
//! runs unless a host-session record is actually probed or closed.

use std::sync::Arc;

use super::host_session::{
    HostSessionProbe, HostSessionProbePorts, create_host_session_probe,
}
use super::host_session_close::{
    HostSessionCloseError, HostSessionCloseRequest, HostSessionCloser,
}

/// The host-owned transport the lifecycle consumes (the exact adapter signature the host owner must
/// provide). This is the ONLY surface the lifecycle needs from the rpc-host engine.
pub trait HostTransport: Send + Sync {
    /// `rpc-host/liveness.daemonReachable(socket)`: does the daemon answer on this socket?
    fn daemon_reachable(&self, socket: &str) -> bool;
    /// `rpc-host/liveness.liveSessionPaths(socket)`: `list_sessions { include_workers: true }`; `Err`
    /// means the daemon answered but could not list (read as `unknown` liveness).
    fn live_session_paths(&self, socket: &str) -> Result<Vec<String>, String>;
    /// `rpc-host/close.closeHostSession(request)`: abort + close_session. Resolves only when the
    /// daemon confirmed the close; a refused attach or close_session is `Err`.
    fn close_host_session(&self, request: &HostSessionCloseRequest) -> Result<(), HostSessionCloseError>;
}

/// The safe default when no host transport is wired: nothing is reachable, nothing can be closed.
pub struct NoHostTransport;

impl HostTransport for NoHostTransport {
    fn daemon_reachable(&self, _socket: &str) -> bool {
        false
    }

    fn live_session_paths(&self, _socket: &str) -> Result<Vec<String>, String> {
        Err("no host transport is wired into this session".to_string())
    }

    fn close_host_session(&self, _request: &HostSessionCloseRequest) -> Result<(), HostSessionCloseError> {
        Err(HostSessionCloseError {
            message: "no host transport is wired into this session".to_string(),
        })
    }
}

/// The production probe over a host transport (TS `defaultHostSessionProbe`).
pub fn default_host_session_probe(transport: Arc<dyn HostTransport>) -> Arc<dyn HostSessionProbe> {
    let daemon = Arc::clone(&transport);
    let listing = transport;
    create_host_session_probe(HostSessionProbePorts {
        daemon_reachable: Arc::new(move |socket| daemon.daemon_reachable(socket)),
        live_session_paths: Arc::new(move |socket| listing.live_session_paths(socket)),
    })
}

/// The production closer over a host transport (TS `defaultHostSessionCloser`): resolves only when the
/// daemon confirmed the close, otherwise rejects.
pub fn default_host_session_closer(transport: Arc<dyn HostTransport>) -> HostSessionCloser {
    Arc::new(move |request| transport.close_host_session(request))
}
