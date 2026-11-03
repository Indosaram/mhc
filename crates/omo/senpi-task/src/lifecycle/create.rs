//! `createTaskLifecycle`: binds the resolved context to the lifecycle operations.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use super::context::{LifecycleContext, LifecycleDeps, resolve_context};
use super::errors::LifecycleError;
use super::port::DestroyCause;
use super::types::{AdmissionResult, CleanupResult, ReconcileResult, SuspendInput, SuspendSummary};
use crate::manager::AbortSignal;

/// Startup generations per parent session (`None` is the unscoped sweep): shutdown bumps both
/// the parent's and the unscoped generation, which permanently invalidates passes in flight.
type Generations = Arc<Mutex<HashMap<Option<String>, u64>>>;

#[derive(Clone)]
pub struct TaskLifecycle {
    context: LifecycleContext,
    _recovery: std::sync::Arc<super::live_parent_recovery::RecoverySubscription>,
    disposed: Arc<AtomicBool>,
    generations: Generations,
}

pub fn create_task_lifecycle(mut deps: LifecycleDeps) -> TaskLifecycle {
    let schedule = deps.live_parent_recovery.take().unwrap_or_default();
    let context = resolve_context(deps);
    let recovery =
        super::live_parent_recovery::start_live_parent_recovery_with(context.clone(), schedule);
    TaskLifecycle {
        context,
        _recovery: recovery,
        disposed: Arc::default(),
        generations: Arc::default(),
    }
}

fn generation_of(generations: &Generations, key: &Option<String>) -> u64 {
    generations
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(key)
        .copied()
        .unwrap_or(0)
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

    /// Parent shutdown: every startup pass in flight is invalidated for good.
    pub fn dispose(&self) {
        self.disposed.store(true, Ordering::Release);
        super::deferred_revival::dispose_scoped_retries(&self.context);
    }

    pub fn reconcile_on_session_start(
        &self,
        parent_session_id: Option<&str>,
    ) -> Result<ReconcileResult, LifecycleError> {
        self.reconcile_on_session_start_with_signal(parent_session_id, None)
    }

    /// `reconcileOnSessionStart(parentSessionId, signal)`: the pass runs under a generation fence.
    /// Dispose, the caller's abort, or a shutdown of the same parent makes every later step of it a
    /// no-op, so a probe or respawn that returns late can neither claim nor revive.
    pub fn reconcile_on_session_start_with_signal(
        &self,
        parent_session_id: Option<&str>,
        signal: Option<&AbortSignal>,
    ) -> Result<ReconcileResult, LifecycleError> {
        if self.disposed.load(Ordering::Acquire) || signal.is_some_and(AbortSignal::aborted) {
            return Ok(ReconcileResult::default());
        }
        let key = parent_session_id.map(str::to_string);
        let generation = {
            let mut generations = self
                .generations
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            *generations.entry(key.clone()).or_insert(0)
        };
        let disposed = Arc::clone(&self.disposed);
        let generations = Arc::clone(&self.generations);
        let signal = signal.cloned();
        let active: super::context::ReconcileActiveFn = Arc::new(move || {
            !disposed.load(Ordering::Acquire)
                && !signal.as_ref().is_some_and(AbortSignal::aborted)
                && generation_of(&generations, &key) == generation
        });
        let mut pass = self.context.clone();
        pass.reconcile_active = Some(Arc::clone(&active));
        if let Some(parent) = parent_session_id {
            super::deferred_revival::resume_scoped_retries(&self.context, parent);
            self.context
                .live_parents
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(parent.to_string());
        }
        let initial = super::reconcile::reconcile_on_session_start(&pass, parent_session_id)?;
        if !active() {
            return Ok(ReconcileResult::default());
        }
        let outcomes = super::deferred_owner_reconcile::reconcile_deferred_dead_children(
            &pass,
            initial.outcomes,
            parent_session_id,
        )?;
        if !active() {
            return Ok(ReconcileResult::default());
        }
        // Scoped retries retain this generation fence; legacy retries belong to other sessions.
        if let Some(parent) = parent_session_id {
            super::deferred_revival::start_scoped_retries(&pass, &outcomes, parent);
        }
        super::host_session_revive::retry_deferred_host_sessions(
            &pass,
            &outcomes,
            parent_session_id,
        );
        super::deferred_owner_reconcile::retry_deferred_legacy_owners(
            &self.context,
            &outcomes,
            parent_session_id,
        );
        Ok(ReconcileResult { outcomes })
    }

    pub fn park_host_session_on_daemon_loss(
        &self,
        task_id: &str,
    ) -> Result<super::host_session_revive::HostSessionParkOutcome, LifecycleError> {
        super::host_session_revive::park_host_session_on_daemon_loss(
            &self.context,
            task_id,
            None,
        )
    }

    pub fn park_host_session_on_daemon_loss_with_options(
        &self,
        task_id: &str,
        options: super::host_session_revive::HostSessionParkOptions,
    ) -> Result<super::host_session_revive::HostSessionParkOutcome, LifecycleError> {
        super::host_session_revive::park_host_session_on_daemon_loss(
            &self.context,
            task_id,
            Some(options),
        )
    }

    pub fn cleanup_expired_records(&self) -> Result<CleanupResult, LifecycleError> {
        super::ttl::cleanup_expired_records(&self.context)
    }

    pub fn suspend_on_session_shutdown(
        &self,
        input: &SuspendInput,
    ) -> Result<SuspendSummary, LifecycleError> {
        {
            let mut generations = self
                .generations
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            *generations
                .entry(Some(input.parent_session_id.clone()))
                .or_insert(0) += 1;
            *generations.entry(None).or_insert(0) += 1;
        }
        super::deferred_revival::stop_scoped_retries(&self.context, &input.parent_session_id);
        self.context
            .live_parents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&input.parent_session_id);
        super::shutdown::suspend_on_session_shutdown(&self.context, input)
    }
}
