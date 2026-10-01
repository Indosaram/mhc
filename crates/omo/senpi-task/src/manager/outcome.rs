//! Terminal outcome tracking (`manager/manager-outcome.ts`). Each tracked handle gets one watcher
//! thread blocking on `wait_for_outcome`; stale epochs and detached handles are ignored.

use std::sync::Arc;

use serde_json::json;

use crate::manager::ManagedChildHandle;
use crate::runners::{RunnerFailure, RunnerOutcome};
use crate::state::{ResidencyState, TaskRecord, TaskRunStats, TaskTransition};
use crate::store::TaskRecordStore;

pub struct ErrorOutcomeInput {
    pub task_id: String,
    pub handle: Arc<dyn ManagedChildHandle>,
    pub model: String,
    pub epoch: i64,
    pub failure: RunnerFailure,
    pub killed: bool,
    pub run_stats: Option<TaskRunStats>,
    pub timestamp: String,
}

pub trait OutcomeTrackerPorts: Send + Sync {
    fn store(&self) -> &TaskRecordStore;
    fn now_iso(&self) -> String;
    fn live_handle(&self, task_id: &str) -> Option<Arc<dyn ManagedChildHandle>>;
    fn try_load(&self, task_id: &str) -> Option<TaskRecord>;
    fn run_stats_snapshot(&self, task_id: &str) -> Option<TaskRunStats>;
    fn release_slot(&self, task_id: &str, model: &str, epoch: i64);
    fn settle_waiters(&self, task_id: &str);
    fn try_runtime_fallback(&self, input: &ErrorOutcomeInput) -> bool;
    /// Called once per tracked outcome after it was applied or ignored.
    fn outcome_processed(&self);
}

pub fn owns_outcome(
    ports: &dyn OutcomeTrackerPorts,
    task_id: &str,
    handle: &Arc<dyn ManagedChildHandle>,
    epoch: i64,
) -> bool {
    let Some(live) = ports.live_handle(task_id) else {
        return false;
    };
    if !Arc::ptr_eq(&live, handle) {
        return false;
    }
    ports.try_load(task_id).is_some_and(|fresh| {
        fresh.residency_state != ResidencyState::PersistedOnly
            && fresh.residency_state != ResidencyState::RpcDetached
            && fresh.notification.run_epoch == epoch
    })
}

pub fn track_outcome(
    ports: Arc<dyn OutcomeTrackerPorts>,
    task_id: String,
    handle: Arc<dyn ManagedChildHandle>,
    model: String,
    epoch: i64,
) {
    std::thread::spawn(move || {
        let outcome = handle.wait_for_outcome();
        settle_outcome(ports.as_ref(), &task_id, &handle, &model, epoch, outcome);
        ports.outcome_processed();
    });
}

fn settle_outcome(
    ports: &dyn OutcomeTrackerPorts,
    task_id: &str,
    handle: &Arc<dyn ManagedChildHandle>,
    model: &str,
    epoch: i64,
    outcome: RunnerOutcome,
) {
    if !owns_outcome(ports, task_id, handle, epoch) {
        return;
    }
    let timestamp = ports.now_iso();
    let run_stats = ports.run_stats_snapshot(task_id);
    let transition = match outcome {
        RunnerOutcome::Error { failure, killed } => {
            let input = ErrorOutcomeInput {
                task_id: task_id.to_string(),
                handle: Arc::clone(handle),
                model: model.to_string(),
                epoch,
                failure,
                killed,
                run_stats,
                timestamp,
            };
            settle_error_outcome(ports, &input);
            return;
        }
        RunnerOutcome::Completed { final_response } => TaskTransition::Complete {
            timestamp,
            final_response,
            run_stats,
        },
        RunnerOutcome::Cancelled => TaskTransition::Cancel {
            timestamp,
            error_message: None,
            run_stats,
        },
    };
    ports.release_slot(task_id, model, epoch);
    if let Err(error) = ports.store().transition(task_id, &transition) {
        log_failure(
            "senpi-task manager outcome tracking failed",
            task_id,
            &error,
        );
    }
    ports.settle_waiters(task_id);
}

fn settle_error_outcome(ports: &dyn OutcomeTrackerPorts, input: &ErrorOutcomeInput) {
    if ports.try_runtime_fallback(input) {
        return;
    }
    if !owns_outcome(ports, &input.task_id, &input.handle, input.epoch) {
        return;
    }
    ports.release_slot(&input.task_id, &input.model, input.epoch);
    let transition = TaskTransition::Fail {
        timestamp: input.timestamp.clone(),
        error_message: input.failure.message.clone(),
        killed: input.killed,
        run_stats: input.run_stats.clone(),
    };
    if let Err(error) = ports.store().transition(&input.task_id, &transition) {
        log_failure(
            "senpi-task manager error outcome tracking failed",
            &input.task_id,
            &error,
        );
    }
    ports.settle_waiters(&input.task_id);
}

fn log_failure(message: &str, task_id: &str, error: &dyn std::fmt::Display) {
    utils::logger::log(
        message,
        Some(&json!({ "taskId": task_id, "error": error.to_string() })),
    );
}

/// Test-only settle/consume accounting that lets the chaos harness apply the TS `flushMicrotasks()`
/// ordering to this port's outcome watcher threads. The pinned TS tracks a child outcome as a
/// promise continuation, so a settle is applied on the microtask queue the chaos bench drains with
/// `flushMicrotasks()`; this port hands a settle to the watcher thread through a channel, so the
/// harness must wait for the watcher to consume it before it reads state. The pair is keyed by the
/// handle's data pointer, and a handle is only waited on while its task is still non-terminal (a
/// terminal task's watcher has already returned, so its later settles have no consumer).
#[cfg(test)]
pub(crate) mod test_barrier {
    use std::collections::HashMap;
    use std::sync::{Condvar, Mutex, OnceLock, PoisonError};

    #[derive(Default, Clone, Copy)]
    struct Entry {
        issued: u64,
        consumed: u64,
    }

    struct State {
        entries: Mutex<HashMap<usize, Entry>>,
        cv: Condvar,
    }

    fn state() -> &'static State {
        static STATE: OnceLock<State> = OnceLock::new();
        STATE.get_or_init(|| State {
            entries: Mutex::new(HashMap::new()),
            cv: Condvar::new(),
        })
    }

    fn lock() -> std::sync::MutexGuard<'static, HashMap<usize, Entry>> {
        state()
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn note_issue(ptr: usize) {
        lock().entry(ptr).or_default().issued += 1;
        state().cv.notify_all();
    }

    pub(crate) fn note_consume(ptr: usize) {
        lock().entry(ptr).or_default().consumed += 1;
        state().cv.notify_all();
    }

    /// Drops every entry; called between chaos iterations so a reused allocation cannot inherit a
    /// previous iteration's accounting.
    pub(crate) fn clear() {
        lock().clear();
    }

    /// Blocks until every settle issued for `ptr` has been consumed by its watcher.
    pub(crate) fn wait_until_consumed(ptr: usize) {
        let mut entries = lock();
        while entries
            .get(&ptr)
            .is_some_and(|entry| entry.consumed < entry.issued)
        {
            entries = state()
                .cv
                .wait(entries)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}
