//! Recovery episodes for unfinished children whose parent remains live.
use super::{LifecycleContext, LifecycleError};
use crate::state::{ResidencyState, TaskRecord, TaskStatus};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

pub const LIVE_PARENT_SUSPENSION_BUDGET_MS: i64 = 300_000;
#[derive(Clone)]
struct Episode {
    epoch: i64,
    deadline: f64,
    deadline_attempted: bool,
}
#[derive(Default)]
pub struct LiveParentRecovery {
    episodes: Mutex<HashMap<String, Episode>>,
}

fn eligible(context: &LifecycleContext, record: &TaskRecord) -> bool {
    (context.registry.owns_record(record)
        || context
            .live_parents
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&record.parent_session_id))
        && record.host_pid.is_none_or(|pid| pid == context.host_pid)
        && matches!(record.status, TaskStatus::Pending | TaskStatus::Running)
        && record.cancel_requested.is_none()
        && (record.killed != Some(true) || super::suspended_expiry::is_suspension_expiry(record))
        && (record.residency_state != ResidencyState::Resident
            || record.suspension_reason.is_some()
            || record.recovery_deadline_at.is_some())
}

impl LiveParentRecovery {
    pub fn scan(&self, context: &LifecycleContext) -> Result<(), LifecycleError> {
        let mut episodes = self.episodes.lock().unwrap_or_else(PoisonError::into_inner);
        for record in context.store.list()?.records {
            if !eligible(context, &record) {
                episodes.remove(&record.task_id);
                continue;
            }
            let episode = episodes
                .entry(record.task_id.clone())
                .or_insert_with(|| Episode {
                    epoch: record.notification.run_epoch,
                    deadline: record.recovery_deadline_at.unwrap_or(
                        (context.now)() as f64 + LIVE_PARENT_SUSPENSION_BUDGET_MS as f64,
                    ),
                    deadline_attempted: false,
                });
            if episode.epoch != record.notification.run_epoch {
                *episode = Episode {
                    epoch: record.notification.run_epoch,
                    deadline: record.recovery_deadline_at.unwrap_or(
                        (context.now)() as f64 + LIVE_PARENT_SUSPENSION_BUDGET_MS as f64,
                    ),
                    deadline_attempted: false,
                };
            }
            context.store.mutate(&record.task_id, &mut |fresh| {
                let mut next = fresh.clone();
                if eligible(context, fresh)
                    && fresh.notification.run_epoch == episode.epoch
                    && next.recovery_deadline_at.is_none()
                {
                    next.recovery_deadline_at = Some(episode.deadline);
                }
                next
            })?;
            if context.registry.get(&record.task_id).is_some() {
                continue;
            }
            let paused = record.revival_deferred_reason.as_deref() == Some("run_paused");
            let expired = (context.now)() as f64 >= episode.deadline;
            let permanent = record.revival_deferred_reason.as_deref().is_some_and(|r| {
                super::deferred_revival_reasons::PERMANENT_REVIVAL_REASONS.contains(&r)
            });
            if !paused
                && (super::suspended_expiry::is_suspension_expiry(&record)
                    || permanent
                    || (expired && episode.deadline_attempted))
            {
                super::suspended_expiry::expire_suspended_child(context, &record, &|| true)?;
                continue;
            }
            if expired {
                episode.deadline_attempted = true;
            }
            let mut attempt_context = context.clone();
            for other in context.store.list()?.records {
                if other.task_id != record.task_id {
                    attempt_context
                        .reconcile_admission
                        .exclude_task_ids
                        .insert(other.task_id);
                }
            }
            let resolver = |id: &str| super::reconcile::newest_session_path(context, id);
            let outcomes = super::reconcile_revival::reconcile_scoped_revival(
                &attempt_context,
                &record.parent_session_id,
                std::slice::from_ref(&record),
                &resolver,
            )?;
            for outcome in outcomes {
                if outcome.task_id != record.task_id {
                    continue;
                }
                if let Some(reason) = outcome
                    .reason
                    .filter(|_| outcome.kind == super::types::ReconcileOutcomeKind::Deferred)
                {
                    if reason == "run_paused" {
                        episode.deadline =
                            (context.now)() as f64 + LIVE_PARENT_SUSPENSION_BUDGET_MS as f64;
                        episode.deadline_attempted = false;
                    }
                    context.store.mutate(&record.task_id, &mut |fresh| {
                        let mut next = fresh.clone();
                        if eligible(context, fresh) && fresh.notification.run_epoch == episode.epoch
                        {
                            next.revival_deferred_reason = Some(reason.clone());
                            if reason == "run_paused" {
                                next.recovery_deadline_at = Some(episode.deadline);
                            }
                        }
                        next
                    })?;
                    if reason != "run_paused"
                        && (expired
                            || super::deferred_revival_reasons::PERMANENT_REVIVAL_REASONS
                                .contains(&reason.as_str()))
                        && let Some(fresh) = context.store.load(&record.task_id)? {
                            super::suspended_expiry::expire_suspended_child(
                                context,
                                &fresh,
                                &|| true,
                            )?;
                        }
                }
            }
        }
        Ok(())
    }
}

pub struct RecoverySubscription {
    stop: std::sync::mpsc::Sender<bool>,
    worker: Option<std::thread::JoinHandle<()>>,
    unsubscribe: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}
impl Drop for RecoverySubscription {
    fn drop(&mut self) {
        if let Some(unsubscribe) = self
            .unsubscribe
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            unsubscribe();
        }
        let _ = self.stop.send(true);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
pub fn start_live_parent_recovery(context: LifecycleContext) -> Arc<RecoverySubscription> {
    let (stop, receiver) = std::sync::mpsc::channel();
    let scanning = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let callback_scanning = scanning.clone();
    let unsubscribe = context.on_store_mutation.as_ref().map(|subscribe| {
        let wake = stop.clone();
        subscribe(Arc::new(move || {
            if !callback_scanning.load(std::sync::atomic::Ordering::Acquire) {
                let _ = wake.send(false);
            }
        }))
    });
    let worker = std::thread::spawn(move || {
        let recovery = LiveParentRecovery::default();
        loop {
            match receiver.recv_timeout(std::time::Duration::from_millis(5_000)) {
                Ok(true) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Ok(false) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    scanning.store(true, std::sync::atomic::Ordering::Release);
                    if let Err(error) = recovery.scan(&context) {
                        utils::logger::log(
                            "senpi-task live parent recovery failed",
                            Some(&serde_json::json!({"error": error.to_string()})),
                        );
                    }
                    scanning.store(false, std::sync::atomic::Ordering::Release);
                }
            }
        }
    });
    Arc::new(RecoverySubscription {
        stop,
        worker: Some(worker),
        unsubscribe: Mutex::new(unsubscribe),
    })
}
