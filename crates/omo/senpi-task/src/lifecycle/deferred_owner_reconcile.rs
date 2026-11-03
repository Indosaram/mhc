//! `lifecycle/deferred-owner-reconcile.ts`: a child deferred to a dying foreign owner is retried
//! until that owner is confirmed dead, then reclaimed through the normal legacy reconciliation.

use std::collections::HashSet;
use std::sync::{Condvar, Mutex, PoisonError};

use serde_json::json;

use super::context::{LifecycleContext, is_terminal, reconciliation_active};
use super::deferred_revival::retries_stopped;
use super::errors::LifecycleError;
use super::host_session::is_host_session_record;
use super::provisional_exit::reconcile_provisional_exit;
use super::reconcile::reconcile_legacy_record;
use super::types::{ReconcileOutcome, ReconcileOutcomeKind};
use crate::state::{ResidencyState, TaskRecord};

/// The task ids with a background owner retry in flight (TS `retrying`, a WeakMap per context).
#[derive(Default)]
pub struct DeferredOwnerRetries {
    active: Mutex<HashSet<String>>,
    settled: Condvar,
}

impl DeferredOwnerRetries {
    fn active(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.active.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn finish(&self, task_id: &str) {
        self.active().remove(task_id);
        self.settled.notify_all();
    }

    /// Blocks until no background retry of `task_id` is in flight; false when `timeout` ran out.
    #[cfg(test)]
    pub(crate) fn wait_settled(&self, task_id: &str, timeout: std::time::Duration) -> bool {
        let (_active, result) = self
            .settled
            .wait_timeout_while(self.active(), timeout, |active| active.contains(task_id))
            .unwrap_or_else(PoisonError::into_inner);
        !result.timed_out()
    }
}

/// A short-lived parent must observe recovery of an already-dead child before startup returns.
pub fn reconcile_deferred_dead_children(
    context: &LifecycleContext,
    outcomes: Vec<ReconcileOutcome>,
    parent_session_id: Option<&str>,
) -> Result<Vec<ReconcileOutcome>, LifecycleError> {
    let mut retried: Vec<Option<TaskRecord>> = Vec::with_capacity(outcomes.len());
    for outcome in &outcomes {
        let observed =
            legacy_owner_record(context, outcome, parent_session_id)?.filter(|observed| {
                observed
                    .pid
                    .is_some_and(|pid| !context.signaller.is_alive(pid))
            });
        retried.push(observed);
    }
    let backoff = &context.host_retry.daemon_loss_backoff_ms;
    // PERF-DIVERGENCE(omo lifecycle/deferred-owner-reconcile.ts): upstream awaits Promise.all over
    // the outcomes; the native retries block on their waits, so each runs on a scoped thread.
    let results: Vec<Result<Option<ReconcileOutcome>, LifecycleError>> =
        std::thread::scope(|scope| {
            let workers: Vec<_> = retried
                .iter()
                .map(|observed| {
                    observed.as_ref().map(|observed| {
                        scope.spawn(move || {
                            retry_owner(context, observed, parent_session_id, backoff)
                        })
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| match worker {
                    None => Ok(None),
                    Some(worker) => worker
                        .join()
                        .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
                })
                .collect()
        });
    outcomes
        .into_iter()
        .zip(results)
        .map(|(outcome, result)| result.map(|retried| retried.unwrap_or(outcome)))
        .collect()
}

/// A new session's crash sweep also visits OTHER sessions' resident children. The scoped revival
/// retry cannot take those records: a dying owner seen once at startup otherwise strands them.
/// Retry the same legacy reclamation, never scoped admission or a new prompt.
pub fn retry_deferred_legacy_owners(
    context: &LifecycleContext,
    outcomes: &[ReconcileOutcome],
    parent_session_id: Option<&str>,
) {
    for outcome in outcomes {
        let observed = match legacy_owner_record(context, outcome, parent_session_id) {
            Ok(Some(observed)) => observed,
            Ok(None) => continue,
            Err(error) => {
                log_failure(&outcome.task_id, &error);
                continue;
            }
        };
        if !context
            .deferred_owner_retries
            .active()
            .insert(observed.task_id.clone())
        {
            continue;
        }
        let retry_context = context.clone();
        let parent = parent_session_id.map(str::to_string);
        let task_id = observed.task_id.clone();
        let spawned = std::thread::Builder::new()
            .name("senpi-task-deferred-owner".to_string())
            .spawn(move || {
                let backoff = retry_context.host_retry.deferred_retry_backoff_ms.clone();
                if let Err(error) =
                    retry_owner(&retry_context, &observed, parent.as_deref(), &backoff)
                {
                    log_failure(&observed.task_id, &error);
                }
                retry_context
                    .deferred_owner_retries
                    .finish(&observed.task_id);
            });
        if let Err(error) = spawned {
            context.deferred_owner_retries.finish(&task_id);
            utils::logger::log(
                "senpi-task deferred owner reconcile failed",
                Some(&json!({ "taskId": task_id, "error": error.to_string() })),
            );
        }
    }
}

fn log_failure(task_id: &str, error: &LifecycleError) {
    utils::logger::log(
        "senpi-task deferred owner reconcile failed",
        Some(&json!({ "taskId": task_id, "error": error.to_string() })),
    );
}

fn legacy_owner_record(
    context: &LifecycleContext,
    outcome: &ReconcileOutcome,
    parent_session_id: Option<&str>,
) -> Result<Option<TaskRecord>, LifecycleError> {
    let deferred_to_owner = outcome.kind == ReconcileOutcomeKind::ForeignLiveOwner
        || (outcome.kind == ReconcileOutcomeKind::Deferred
            && outcome.reason.as_deref() == Some("foreign_live_owner"));
    if !deferred_to_owner {
        return Ok(None);
    }
    // The existing scoped retry owns this session's children; daemon sessions have their own
    // recovery authority. Same-process claims can belong to a live sibling engine.
    Ok(context.store.load(&outcome.task_id)?.filter(|observed| {
        Some(observed.parent_session_id.as_str()) != parent_session_id
            && !is_host_session_record(observed)
            && observed.residency_state == ResidencyState::Resident
            && !is_terminal(observed.status)
            && observed.host_pid.is_some_and(|pid| pid != context.host_pid)
    }))
}

fn retry_owner(
    context: &LifecycleContext,
    observed: &TaskRecord,
    parent_session_id: Option<&str>,
    backoff_ms: &[u64],
) -> Result<Option<ReconcileOutcome>, LifecycleError> {
    let session = parent_session_id.unwrap_or(observed.parent_session_id.as_str());
    for &backoff in backoff_ms {
        (context.host_retry_wait)(backoff);
        if !reconciliation_active(context) || retries_stopped(context, session) {
            return Ok(None);
        }
        let Some(fresh) = context.store.load(&observed.task_id)? else {
            return Ok(None);
        };
        if is_terminal(fresh.status)
            || fresh.residency_state != ResidencyState::Resident
            || fresh.host_pid != observed.host_pid
            || fresh.residency_claim != observed.residency_claim
            || fresh.notification.run_epoch != observed.notification.run_epoch
            || context.registry.get(&fresh.task_id).is_some()
        {
            return Ok(None);
        }
        if fresh
            .host_pid
            .is_some_and(|pid| context.signaller.is_alive(pid))
        {
            continue;
        }
        // Reclamation revalidates the owner under the record lock. Terminal writes that win before
        // that claim or while destruction runs are preserved by the normal reconciliation reducer.
        let outcome = if fresh.provisional_exit.is_none() {
            reconcile_legacy_record(context, &fresh)?
        } else {
            reconcile_provisional_exit(context, &fresh)?
        };
        if outcome.kind != ReconcileOutcomeKind::ForeignLiveOwner {
            return Ok(Some(outcome));
        }
    }
    // Exhaustion is not proof of death. A genuinely live owner keeps its record and its child.
    Ok(None)
}
