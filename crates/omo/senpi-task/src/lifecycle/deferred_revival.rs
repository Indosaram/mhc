//! Scoped retries from `lifecycle/deferred-revival.ts`.

use std::collections::HashSet;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::context::LifecycleContext;

/// Sessions whose retries must stop: the engine shut that session down or was disposed. A retry
/// re-checks this after every wait, so it never revives a child under a session the user has left.
/// TS keys two WeakMaps by the context object; every clone of one resolved context shares this.
#[derive(Default)]
pub struct ScopedRetries {
    state: Mutex<StopState>,
    changed: Condvar,
}

#[derive(Default)]
struct StopState {
    stopped_sessions: HashSet<String>,
    retrying: HashSet<String>,
    disposed: bool,
}

impl ScopedRetries {
    fn state(&self) -> std::sync::MutexGuard<'_, StopState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn try_start_retry(&self, task_id: &str) -> bool {
        let mut state = self.state();
        if state.disposed {
            return false;
        }
        state.retrying.insert(task_id.to_string())
    }

    pub fn finish_retry(&self, task_id: &str) {
        let mut state = self.state();
        state.retrying.remove(task_id);
        self.changed.notify_all();
    }

    pub fn is_disposed(&self) -> bool {
        self.state().disposed
    }

    pub fn has_active_retries(&self) -> bool {
        !self.state().retrying.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn wait_until_idle(&self) {
        let (state, timeout) = self
            .changed
            .wait_timeout_while(self.state(), Duration::from_secs(5), |state| {
                !state.retrying.is_empty()
            })
            .expect("retry completion signal");
        assert!(
            !timeout.timed_out() && state.retrying.is_empty(),
            "retries must finish"
        );
    }

    /// The default retry wait: the backoff elapses, or dispose cuts it short.
    // PERF-DIVERGENCE(omo lifecycle/host-session.ts): upstream's `wait` is a timer the disposed
    // engine simply never observes again; a native retry thread would otherwise sleep out its whole
    // backoff after dispose, so the wait returns as soon as the lifecycle is disposed.
    pub fn wait(&self, ms: u64) {
        let deadline = Instant::now() + Duration::from_millis(ms);
        let mut state = self.state();
        while !state.disposed {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return;
            }
            state = self
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

pub fn stop_scoped_retries(context: &LifecycleContext, parent_session_id: &str) {
    context
        .scoped_retries
        .state()
        .stopped_sessions
        .insert(parent_session_id.to_string());
}

/// A session that is resumed again may retry its children again.
pub fn resume_scoped_retries(context: &LifecycleContext, parent_session_id: &str) {
    context
        .scoped_retries
        .state()
        .stopped_sessions
        .remove(parent_session_id);
}

pub fn dispose_scoped_retries(context: &LifecycleContext) {
    context.scoped_retries.state().disposed = true;
    context.scoped_retries.changed.notify_all();
}

pub fn retries_stopped(context: &LifecycleContext, parent_session_id: &str) -> bool {
    let state = context.scoped_retries.state();
    state.disposed || state.stopped_sessions.contains(parent_session_id)
}

fn can_retry(context: &LifecycleContext, record: &crate::state::TaskRecord, parent: &str) -> bool {
    record.parent_session_id == parent
        && !record.status.is_terminal()
        && record.killed != Some(true)
        && context.registry.get(&record.task_id).is_none()
        && matches!(
            record.residency_state,
            crate::state::ResidencyState::PersistedOnly
                | crate::state::ResidencyState::RpcDetached
                | crate::state::ResidencyState::Resident
        )
}

fn foreign_owner_alive(context: &LifecycleContext, record: &crate::state::TaskRecord) -> bool {
    record
        .host_pid
        .is_some_and(|pid| pid != context.host_pid && context.signaller.is_alive(pid))
}

pub fn start_scoped_retries(
    context: &LifecycleContext,
    outcomes: &[super::types::ReconcileOutcome],
    parent: &str,
) {
    for outcome in outcomes {
        if outcome.kind != super::types::ReconcileOutcomeKind::Deferred {
            continue;
        }
        let Some(reason) = outcome.reason.as_deref().filter(|reason| {
            super::deferred_revival_reasons::SCOPED_RETRY_REASONS.contains(reason)
        }) else {
            continue;
        };
        let eligible = match context.store.load(&outcome.task_id) {
            Ok(Some(record)) => can_retry(context, &record, parent),
            Ok(None) => false,
            Err(error) => {
                utils::logger::log(
                    "senpi-task scoped retry load failed",
                    Some(&serde_json::json!({"taskId":outcome.task_id,"error":error.to_string()})),
                );
                false
            }
        };
        if !eligible || !context.scoped_retries.try_start_retry(&outcome.task_id) {
            continue;
        }
        let context = context.clone();
        let task_id = outcome.task_id.clone();
        let parent = parent.to_owned();
        let reason = reason.to_owned();
        let retry_context = context.clone();
        let retry_task = task_id.clone();
        let spawned = std::thread::Builder::new()
            .name("senpi-task-scoped-retry".into())
            .spawn(move || {
                if let Err(error) =
                    retry_deferred_scoped_child(&retry_context, &retry_task, &parent, &reason)
                {
                    utils::logger::log(
                        "senpi-task scoped revival retry failed",
                        Some(&serde_json::json!({"taskId":retry_task,"error":error.to_string()})),
                    );
                }
                retry_context.scoped_retries.finish_retry(&retry_task);
            });
        if let Err(error) = spawned {
            context.scoped_retries.finish_retry(&task_id);
            utils::logger::log(
                "senpi-task scoped retry spawn failed",
                Some(&serde_json::json!({"taskId":task_id,"error":error.to_string()})),
            );
        }
    }
}

pub fn retry_deferred_scoped_child(
    context: &LifecycleContext,
    task_id: &str,
    parent: &str,
    initial_reason: &str,
) -> Result<(), super::errors::LifecycleError> {
    use super::deferred_revival_reasons::{
        LOST_ON_EXHAUSTION, PERMANENT_REVIVAL_REASONS, SCOPED_RETRY_REASONS,
    };
    use super::host_session::host_session_identity;
    use crate::state::{LostReason, ResidencyState, mark_record_lost_for_reconciliation};
    use crate::store::PersistedTaskEvent;
    use serde_json::json;

    let active =
        || !retries_stopped(context, parent) && super::context::reconciliation_active(context);
    let mut reason = initial_reason.to_owned();
    let mut expected = context.store.load(task_id)?;
    let mut attempts = 0;
    for backoff in &context.host_retry.deferred_retry_backoff_ms {
        (context.host_retry_wait)(*backoff);
        if !active() {
            return Ok(());
        }
        let Some(fresh) = context
            .store
            .load(task_id)?
            .filter(|r| can_retry(context, r, parent))
        else {
            return Ok(());
        };
        let Some(prior) = expected.as_ref() else {
            return Ok(());
        };
        if fresh.notification.run_epoch != prior.notification.run_epoch
            || (fresh.residency_claim != prior.residency_claim
                && (reason != "foreign_live_owner"
                    || fresh.host_pid == Some(context.host_pid)
                    || foreign_owner_alive(context, &fresh)))
        {
            return Ok(());
        }
        attempts += 1;
        if foreign_owner_alive(context, &fresh) {
            if reason != "foreign_live_owner" {
                return Ok(());
            }
            continue;
        }
        if let Some(host) = host_session_identity(&fresh) {
            context.host_session_probe.refresh(Some(&host.socket));
            if reason == "foreign_live_owner"
                && fresh.residency_state == ResidencyState::Resident
                && context.host_session_probe.daemon_alive(host)
                && context.host_session_probe.session_live(host)
                && fresh.host_pid.is_none_or(|pid| pid == context.host_pid)
            {
                continue;
            }
        }
        let mut scoped = context.clone();
        // Only the ids are needed, so no record is cloned.
        context.store.list_matching(&mut |record| {
            if record.task_id != task_id {
                scoped
                    .reconcile_admission
                    .exclude_task_ids
                    .insert(record.task_id.clone());
            }
            false
        })?;
        let resolver = |id: &str| super::reconcile::newest_session_path(context, id);
        let outcomes = super::reconcile_revival::reconcile_scoped_revival(
            &scoped,
            parent,
            &[fresh],
            &resolver,
        )?;
        if !active() {
            if let Some(handle) = context.registry.get(task_id) {
                super::shutdown::suspend_handle(context, &handle, "revived_after_shutdown")?;
            }
            return Ok(());
        }
        let Some(outcome) = outcomes.iter().find(|r| {
            r.task_id == task_id && r.kind == super::types::ReconcileOutcomeKind::Deferred
        }) else {
            return Ok(());
        };
        let Some(next_reason) = &outcome.reason else {
            return Ok(());
        };
        reason.clone_from(next_reason);
        expected = context.store.load(task_id)?;
        if let Some(record) = expected
            .as_ref()
            .filter(|r| context.registry.owns_record(r))
            && PERMANENT_REVIVAL_REASONS.contains(&reason.as_str())
        {
            super::suspended_expiry::expire_suspended_child(context, record, &|| {
                active() && context.registry.owns_record(record)
            })?;
            return Ok(());
        }
        if !SCOPED_RETRY_REASONS.contains(&reason.as_str()) {
            return Ok(());
        }
    }
    if !active() {
        return Ok(());
    }
    let Some(observed) = context
        .store
        .load(task_id)?
        .filter(|r| can_retry(context, r, parent))
    else {
        return Ok(());
    };
    let Some(prior) = expected else {
        return Ok(());
    };
    if observed.residency_claim != prior.residency_claim
        || observed.notification.run_epoch != prior.notification.run_epoch
    {
        return Ok(());
    }
    context.store.append_event(
        task_id,
        &PersistedTaskEvent {
            event_type: "revival_retry_exhausted".into(),
            payload: json!({"reason":reason,"attempts":attempts}),
        },
    )?;
    if context.registry.owns_record(&observed)
        || host_session_identity(&observed).is_some()
        || !LOST_ON_EXHAUSTION.contains(&reason.as_str())
        || foreign_owner_alive(context, &observed)
    {
        return Ok(());
    }
    let message = format!("revival deferred: {reason}; exhausted {attempts} retry attempts");
    let mut applied = false;
    context.store.mutate(task_id, &mut |fresh| {
        if !active()
            || !can_retry(context, fresh, parent)
            || host_session_identity(fresh).is_some()
            || foreign_owner_alive(context, fresh)
            || fresh.host_pid != observed.host_pid
            || fresh.residency_claim != observed.residency_claim
            || fresh.notification.run_epoch != observed.notification.run_epoch
            || fresh.updated_at != observed.updated_at
        {
            return fresh.clone();
        }
        let result = mark_record_lost_for_reconciliation(
            fresh,
            &context.now_iso(),
            &crate::tools::control::cancel::with_dropped_steering_notice(
                &message,
                fresh.pending_steering.as_ref().map_or(0, Vec::len),
            ),
            LostReason::KeepExisting,
        );
        applied = result.applied;
        result.record
    })?;
    if applied {
        context.store.append_event(
            task_id,
            &super::reconcile_crashed_resident::reconcile_lost_event(&message),
        )?;
        super::destroy::destroy_resident_task(
            context,
            task_id,
            super::port::DestroyCause::ReconcileLost,
            None,
        )?;
    }
    Ok(())
}
