//! Host approval-mode push contract (`runners/rpc-host/permission-push.ts`, `#6189 F2e`).

use crate::host::HostError;

pub const CHILD_DETACHED_PERMISSION_ERROR: &str =
    "the child's session is no longer on its task host; its approval mode was not changed";

pub trait PermissionPushPort: Send + Sync {
    /// Applies `permission_preset` to a live hosted child over a non-retaining side open; returns
    /// `Err` if the child was not attached (`attached != true`) or the transport failed.
    fn reapply_permission_preset(&self, permission_preset: &str) -> Result<(), HostError>;
}
