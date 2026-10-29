//! `createTaskLifecycle`: binds the resolved context to the lifecycle operations.

use super::context::{LifecycleContext, LifecycleDeps, resolve_context};
use super::errors::LifecycleError;
use super::port::DestroyCause;
use super::types::{AdmissionResult, CleanupResult, ReconcileResult, SuspendInput, SuspendSummary};

#[derive(Clone)]
pub struct TaskLifecycle {
    context: LifecycleContext,
    _recovery: std::sync::Arc<super::live_parent_recovery::RecoverySubscription>,
}

pub fn create_task_lifecycle(deps: LifecycleDeps) -> TaskLifecycle {
    let context = resolve_context(deps);
    let recovery = super::live_parent_recovery::start_live_parent_recovery(context.clone());
    TaskLifecycle {
        context,
        _recovery: recovery,
    }
}

impl TaskLifecycle {
    pub fn rollback_detached_revival(
        &self,
        prior: &crate::state::TaskRecord,
    ) -> Result<super::revive_detached::DetachedRevivalRollbackResult, LifecycleError> {
        super::revive_detached::rollback_detached_revival(&self.context, prior)
    }

    pub fn revive_detached_terminal(
        &self,
        task_id: &str,
    ) -> Result<super::types::ReconcileOutcome, LifecycleError> {
        super::revive_detached::revive_detached_terminal(&self.context, task_id)
    }

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
        if let Some(parent) = parent_session_id {
            self.context
                .live_parents
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(parent.to_string());
        }
        super::reconcile::reconcile_on_session_start(&self.context, parent_session_id)
    }

    pub fn cleanup_expired_records(&self) -> Result<CleanupResult, LifecycleError> {
        super::ttl::cleanup_expired_records(&self.context)
    }

    pub fn suspend_on_session_shutdown(
        &self,
        input: &SuspendInput,
    ) -> Result<SuspendSummary, LifecycleError> {
        self.context
            .live_parents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&input.parent_session_id);
        super::shutdown::suspend_on_session_shutdown(&self.context, input)
    }
}
