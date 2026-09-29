//! The one handle seam the manager, steering and lifecycle program against
//! (`manager/child-handle.ts`). Both runners normalize onto [`ManagedChildHandle`].

use std::sync::Arc;

use crate::host::HostError;
use crate::runners::RunnerOutcome;
use crate::runners::types::{RpcEntriesResult, RpcSpawnSpec, RpcSwitchSessionResult};

pub use crate::shared::ManagedChildEvent;

pub type ManagedChildListener = Arc<dyn Fn(&ManagedChildEvent) + Send + Sync>;
pub type Unsubscribe = Box<dyn FnOnce() + Send>;

/// Blocking handle surface. `wait_for_outcome` blocks until the current turn settles.
pub trait ManagedChildHandle: Send + Sync {
    fn task_id(&self) -> &str;
    fn session_id(&self) -> Option<String>;
    fn pid(&self) -> Option<i64>;
    fn spawn_spec(&self) -> Option<RpcSpawnSpec> {
        None
    }
    fn steer(&self, text: &str) -> Result<(), HostError>;
    fn follow_up(&self, text: &str) -> Result<(), HostError>;
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
