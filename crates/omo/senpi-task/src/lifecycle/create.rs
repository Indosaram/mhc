//! `createTaskLifecycle`: binds the resolved context to the lifecycle operations.

use super::context::{LifecycleContext, LifecycleDeps, resolve_context};
use super::errors::LifecycleError;
use super::port::DestroyCause;
use super::types::{AdmissionResult, CleanupResult, ReconcileResult, SuspendInput, SuspendSummary};

#[derive(Clone)]
pub struct TaskLifecycle {
    context: LifecycleContext,
}

pub fn create_task_lifecycle(deps: LifecycleDeps) -> TaskLifecycle {
    TaskLifecycle {
        context: resolve_context(deps),
    }
}

impl TaskLifecycle {
    pub fn context(&self) -> &LifecycleContext {
        &self.context
    }

    pub fn destroy_resident_task(
        &self,
        task_id: &str,
        cause: DestroyCause,
    ) -> Result<(), LifecycleError> {
        super::destroy::destroy_resident_task(&self.context, task_id, cause, None)
    }

    pub fn admit_resident(
        &self,
        parent_session_id: &str,
    ) -> Result<AdmissionResult, LifecycleError> {
        super::residency::admit_resident(&self.context, parent_session_id)
    }

    pub fn reconcile_on_session_start(
        &self,
        parent_session_id: Option<&str>,
    ) -> Result<ReconcileResult, LifecycleError> {
        super::reconcile::reconcile_on_session_start(&self.context, parent_session_id)
    }

    pub fn cleanup_expired_records(&self) -> Result<CleanupResult, LifecycleError> {
        super::ttl::cleanup_expired_records(&self.context)
    }

    pub fn suspend_on_session_shutdown(
        &self,
        input: &SuspendInput,
    ) -> Result<SuspendSummary, LifecycleError> {
        super::shutdown::suspend_on_session_shutdown(&self.context, input)
    }
}
