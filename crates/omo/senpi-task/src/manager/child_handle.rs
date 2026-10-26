//! The one handle seam the manager, steering and lifecycle program against
//! (`manager/child-handle.ts`). Both runners normalize onto [`ManagedChildHandle`].

use std::sync::Arc;

use crate::host::HostError;
pub type ExactTurnOutcome = Box<dyn FnOnce() -> crate::runners::RunnerOutcome + Send>;
use crate::runners::RunnerOutcome;
use crate::runners::types::{RpcEntriesResult, RpcSpawnSpec, RpcSwitchSessionResult};

pub use crate::shared::ManagedChildEvent;

pub type ManagedChildListener = Arc<dyn Fn(&ManagedChildEvent) + Send + Sync>;
pub type Unsubscribe = Box<dyn FnOnce() + Send>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkpoolChildIdentity {
    pub task_id: String,
    pub state_dir: String,
    pub run_epoch: u32,
}

/// Blocking handle surface. `wait_for_outcome` blocks until the current turn settles.
pub trait ManagedChildHandle: Send + Sync {
    fn on_self_resumed(&self, _listener: Arc<dyn Fn() + Send + Sync>) -> Option<Unsubscribe> {
        None
    }
    fn on_durable_session_id(
        &self,
        _listener: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Option<Unsubscribe> {
        None
    }
    fn task_id(&self) -> &str;
    fn run_pause(&self) -> Option<&dyn crate::pause::native_child_pause::NativeRunPause> {
        None
    }
    fn session_id(&self) -> Option<String>;
    fn pid(&self) -> Option<i64>;
    fn spawn_spec(&self) -> Option<RpcSpawnSpec> {
        None
    }
    fn steer(&self, text: &str) -> Result<(), HostError>;
    fn start_initial_turn(&self, _text: &str) -> Option<crate::pause::ports::ResumedChildTurn> {
        None
    }
    fn follow_up(&self, text: &str) -> Result<(), HostError>;
    fn rebind_workpool_identity(
        &self,
        _identity: WorkpoolChildIdentity,
    ) -> Option<Result<(), HostError>> {
        None
    }
    fn abort(&self) -> Result<(), HostError>;
    fn subscribe(&self, listener: ManagedChildListener) -> Unsubscribe;
    fn wait_for_outcome(&self) -> RunnerOutcome;
    /// `None` when the handle has no session switching (in-process handles).
    fn switch_session(
        &self,
        _session_path: &str,
    ) -> Option<Result<RpcSwitchSessionResult, HostError>> {
        None
    }
    /// `None` when the handle has no entry reader.
    fn get_entries(&self, _since: Option<&str>) -> Option<Result<RpcEntriesResult, HostError>> {
        None
    }
    /// Partial assistant text captured so far (interrupt preserves work-in-progress).
    fn last_assistant_text(&self) -> Option<String>;
    /// Whether [`Self::terminate`] signals an OS process (RPC handles only).
    fn has_terminate(&self) -> bool {
        false
    }
    fn terminate(&self) -> Result<(), HostError> {
        Ok(())
    }
    fn dispose(&self) -> Result<(), HostError>;
    /// True while the child's transport is down and being recovered (TS `transportRecovering`).
    fn transport_recovering(&self) -> bool {
        false
    }
    /// Whether [`Self::stop_when_reachable`] exists (TS `stopWhenReachable !== undefined`).
    fn supports_stop_when_reachable(&self) -> bool {
        false
    }
    /// Blocks until the stop is applied on the host once reachable, or the child ends because its
    /// connection never comes back (TS `stopWhenReachable`).
    fn stop_when_reachable(&self) -> Result<(), HostError> {
        Ok(())
    }
    /// From here no transport recovery may bring the child back (TS `markStopping`).
    fn mark_stopping(&self) {}
    fn is_host_session(&self) -> bool {
        false
    }
    fn publish_task_status(&self, _status: crate::state::TaskStatus) -> Result<(), HostError> {
        Ok(())
    }
}

/// Terminate (when supported) then always dispose.
pub fn discard_managed_handle(handle: &dyn ManagedChildHandle) -> Result<(), HostError> {
    let terminated = if handle.has_terminate() {
        handle.terminate()
    } else {
        Ok(())
    };
    let disposed = handle.dispose();
    terminated.and(disposed)
}
