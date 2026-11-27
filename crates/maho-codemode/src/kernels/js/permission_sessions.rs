use crate::kernels::shared::cell_authority_registry::{
    PermissionSessionHandle, SessionKernelControl, register_kernel, revoke, unregister_kernel,
};
use std::sync::Arc;

/// One session's permission state as a kernel bound to it sees it (`permission-sessions.ts`).
#[derive(Clone)]
pub struct PermissionSession {
    handle: PermissionSessionHandle,
}

impl PermissionSession {
    pub fn dead(&self) -> bool {
        self.handle.dead()
    }

    pub fn mark_dead(&self) {
        self.handle.mark_dead();
    }

    pub fn pending(&self) -> bool {
        self.handle.pending()
    }
}

pub fn register_permission_kernel(
    session_id: &str,
    kernel: Arc<dyn SessionKernelControl>,
) -> PermissionSession {
    PermissionSession {
        handle: register_kernel(session_id, kernel),
    }
}

pub fn unregister_permission_kernel(session_id: &str, kernel_id: &str) {
    unregister_kernel(session_id, kernel_id);
}

/// All authority words go dead synchronously before retirement or another admission runs.
pub async fn revoke_session_kernels(session_id: &str) {
    revoke(session_id).await;
}
