//! Terminal outcome tracking (`manager/manager-outcome.ts`). Each tracked handle gets one watcher
//! thread blocking on `wait_for_outcome`; stale epochs and detached handles are ignored.

use std::sync::{Arc, Condvar, Mutex};

use serde_json::json;

use crate::manager::ManagedChildHandle;
use crate::runners::{RunnerFailure, RunnerOutcome};
use crate::state::{ResidencyState, TaskRecord, TaskRunStats, TaskStatus, TaskTransition};
use crate::store::TaskRecordStore;

/// A cancel's settlement signal, shared by its guard and the outcome watcher.
pub type PendingStop = Arc<(Mutex<bool>, Condvar)>;

const STOP_SETTLEMENT_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

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
    fn next_tracking_generation(&self, _task_id: &str) -> u64 {
        0
    }
    fn tracking_generation(&self, _task_id: &str) -> u64 {
        0
    }
    fn store(&self) -> &TaskRecordStore;
    fn now_iso(&self) -> String;
    fn live_handle(&self, task_id: &str) -> Option<Arc<dyn ManagedChildHandle>>;
    fn try_load(&self, task_id: &str) -> Option<TaskRecord>;
    fn run_stats_snapshot(&self, task_id: &str) -> Option<TaskRunStats>;
    fn release_slot(&self, task_id: &str, model: &str, epoch: i64);
    fn settle_waiters(&self, task_id: &str);
    fn try_runtime_fallback(&self, input: &ErrorOutcomeInput) -> bool;
    /// Merges (or retains) an isolated child's clone. Called BEFORE the terminal transition is
    /// written, so every result builder reads one record that already carries merge_result.
    fn settle_isolation(&self, task_id: &str, merge: bool);
    /// Takes the pending cancel signal for THIS run (task id + epoch) before checking the released
    /// live handle. Defaults to "no pending cancel" for an implementer without cancels.
    fn pause_settlement(&self, _task_id: &str, _epoch: i64) -> Option<PendingStop> {
        None
    }
    fn stop_settlement(&self, _task_id: &str, _epoch: i64) -> Option<PendingStop> {
        None
    }
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
        fresh.status == TaskStatus::Running
            && fresh.residency_state != ResidencyState::PersistedOnly
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
    let generation = ports.next_tracking_generation(&task_id);
    #[cfg(test)]
    test_barrier::track(Arc::as_ptr(&handle) as *const () as usize);
    std::thread::spawn(move || {
        let outcome = handle.wait_for_outcome();
        if ports.tracking_generation(&task_id) == generation {
            settle_outcome(
                ports.as_ref(),
                &task_id,
                &handle,
                &model,
                epoch,
                generation,
                outcome,
            );
        }
        ports.outcome_processed();
        #[cfg(test)]
        test_barrier::note_consume(Arc::as_ptr(&handle) as *const () as usize);
    });
}

pub fn track_exact_outcome(
    ports: Arc<dyn OutcomeTrackerPorts>,
    task_id: String,
    handle: Arc<dyn ManagedChildHandle>,
    model: String,
    epoch: i64,
    outcome: crate::manager::child_handle::ExactTurnOutcome,
) {
    let generation = ports.next_tracking_generation(&task_id);
    std::thread::spawn(move || {
        let outcome = outcome();
        if ports.tracking_generation(&task_id) == generation {
            settle_outcome(
                ports.as_ref(),
                &task_id,
                &handle,
                &model,
                epoch,
                generation,
                outcome,
            );
        }
        ports.outcome_processed();
    });
}

fn settle_outcome(
    ports: &dyn OutcomeTrackerPorts,
    task_id: &str,
    handle: &Arc<dyn ManagedChildHandle>,
    model: &str,
    epoch: i64,
    generation: u64,
    outcome: RunnerOutcome,
) {
    if let Some(paused) = ports.pause_settlement(task_id, epoch) {
        let settled = paused
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (settled, _) = paused
            .1
            .wait_timeout_while(settled, STOP_SETTLEMENT_WAIT, |done| !*done)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !*settled {
            log_failure(
                "senpi-task pause acknowledgement timed out",
                task_id,
                &"native pause did not settle",
            );
            return;
        }
        drop(settled);
        if ports
            .try_load(task_id)
            .is_some_and(|r| r.notification.run_epoch == epoch && r.status == TaskStatus::Paused)
        {
            return;
        }
    }
    if ports.tracking_generation(task_id) != generation {
        return;
    }
    if let Some(stopped) = ports.stop_settlement(task_id, epoch) {
        settle_stopped(ports, task_id, handle, model, epoch, stopped);
        return;
    }
    if !owns_outcome(ports, task_id, handle, epoch) {
        return;
    }
    let timestamp = ports.now_iso();
    let run_stats = ports.run_stats_snapshot(task_id);
    // Only a completed child with assistant output earns a merge; every other terminal retains.
    let merge = match &outcome {
        RunnerOutcome::Completed { final_response } => !final_response.is_empty(),
        _ => false,
    };
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
    settle_isolation_for(ports, task_id, merge);
    if let Err(error) = ports.store().transition(task_id, &transition) {
        log_failure(
            "senpi-task manager outcome tracking failed",
            task_id,
            &error,
        );
    }
    ports.settle_waiters(task_id);
}

/// The run a pending cancel stopped. The cancel normally wrote the terminal record and let the handle
/// go; when its write failed the run is still ours, and it ends as cancelled here - never as the
/// failure the stop caused, which would also start a runtime fallback.
fn settle_stopped(
    ports: &dyn OutcomeTrackerPorts,
    task_id: &str,
    handle: &Arc<dyn ManagedChildHandle>,
    model: &str,
    epoch: i64,
    stopped: PendingStop,
) {
    let (settled, wake) = &*stopped;
    let guard = settled
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (guard, _) = wake
        .wait_timeout_while(guard, STOP_SETTLEMENT_WAIT, |settled| !*settled)
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let settled = *guard;
    drop(guard);
    if !settled {
        log_failure(
            "senpi-task cancelled run settlement timed out",
            task_id,
            &"the pending cancel never settled",
        );
        return;
    }
    // A teardown that already let the handle go must not strand the run: the record of this same run,
    // still non-terminal with its cancel on it, is still the cancel's to end.
    let unfinished = ports
        .try_load(task_id)
        .is_some_and(|record| !record.status.is_terminal());
    if unfinished
        && (owns_outcome(ports, task_id, handle, epoch)
            || stopped_run_record(ports, task_id, epoch).is_some())
    {
        ports.release_slot(task_id, model, epoch);
        let timestamp = ports.now_iso();
        let transition = TaskTransition::Cancel {
            timestamp,
            error_message: None,
            run_stats: ports.run_stats_snapshot(task_id),
        };
        if let Err(error) = ports.store().transition(task_id, &transition) {
            log_failure(
                "senpi-task manager terminal record write failed",
                task_id,
                &error,
            );
        }
        ports.settle_waiters(task_id);
    }
    let Some(record) = ports.try_load(task_id) else {
        return;
    };
    let Some(isolation) = record.isolation.as_ref() else {
        return;
    };
    if isolation.merge_result.is_none() {
        ports.settle_isolation(task_id, false);
    }
}

/// Returns the fresh record when this run still carries a pending cancel and has not ended.
fn stopped_run_record(
    ports: &dyn OutcomeTrackerPorts,
    task_id: &str,
    epoch: i64,
) -> Option<TaskRecord> {
    let fresh = ports.try_load(task_id)?;
    if fresh.cancel_requested.is_none() || fresh.notification.run_epoch != epoch {
        return None;
    }
    if STOPPED_RUN_TERMINAL.contains(&fresh.status) {
        None
    } else {
        Some(fresh)
    }
}

const STOPPED_RUN_TERMINAL: [TaskStatus; 5] = [
    TaskStatus::Completed,
    TaskStatus::Error,
    TaskStatus::Cancelled,
    TaskStatus::Interrupted,
    TaskStatus::Lost,
];

fn settle_error_outcome(ports: &dyn OutcomeTrackerPorts, input: &ErrorOutcomeInput) {
    if ports.try_runtime_fallback(input) {
        return;
    }
    if !owns_outcome(ports, &input.task_id, &input.handle, input.epoch) {
        return;
    }
    ports.release_slot(&input.task_id, &input.model, input.epoch);
    settle_isolation_for(ports, &input.task_id, false);
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

/// Settle an isolated child's clone before the terminal record is written. A non-isolated child is
/// untouched, so its terminal path is unchanged.
fn settle_isolation_for(ports: &dyn OutcomeTrackerPorts, task_id: &str, merge: bool) {
    let isolated = ports
        .try_load(task_id)
        .is_some_and(|record| record.isolation.is_some());
    if !isolated {
        return;
    }
    ports.settle_isolation(task_id, merge);
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
        finished: bool,
        /// Watcher threads tracked for this handle that have not yet consumed a settle.
        watching: u32,
    }

    struct State {
        entries: Mutex<HashMap<usize, Entry>>,
        /// Queue grants spawned on their own thread that have not finished launching yet.
        grants: Mutex<u64>,
        grants_cv: Condvar,
        cv: Condvar,
    }

    /// Decrements the in-flight grant count when the grant thread ends, even by panic.
    pub(crate) struct GrantDone;

    impl Drop for GrantDone {
        fn drop(&mut self) {
            *grants() -= 1;
            state().grants_cv.notify_all();
        }
    }

    fn grants() -> std::sync::MutexGuard<'static, u64> {
        state()
            .grants
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn grant_spawned() {
        *grants() += 1;
    }

    fn state() -> &'static State {
        static STATE: OnceLock<State> = OnceLock::new();
        STATE.get_or_init(|| State {
            entries: Mutex::new(HashMap::new()),
            grants: Mutex::new(0),
            grants_cv: Condvar::new(),
            cv: Condvar::new(),
        })
    }

    fn lock() -> std::sync::MutexGuard<'static, HashMap<usize, Entry>> {
        state()
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn register(ptr: usize) {
        lock().insert(ptr, Entry::default());
    }

    pub(crate) fn track(ptr: usize) {
        let mut entries = lock();
        let entry = entries.entry(ptr).or_default();
        if entry.finished {
            *entry = Entry::default();
        }
        entry.watching += 1;
    }

    pub(crate) fn note_issue(ptr: usize) {
        let mut entries = lock();
        let entry = entries.entry(ptr).or_default();
        if !entry.finished {
            entry.issued += 1;
        }
        state().cv.notify_all();
    }

    pub(crate) fn note_consume(ptr: usize) {
        let mut entries = lock();
        let entry = entries.entry(ptr).or_default();
        entry.watching = entry.watching.saturating_sub(1);
        // A handle revived while its previous watcher is still applying a settle has a second
        // watcher in flight; only the last watcher out may close the accounting, otherwise the
        // next settle would be treated as consumer-less and the harness would not wait for it.
        if entry.watching == 0 {
            entry.consumed = entry.issued;
            entry.finished = true;
        } else {
            entry.consumed = (entry.consumed + 1).min(entry.issued);
        }
        state().cv.notify_all();
    }

    /// Blocks until every spawned queue grant has finished launching its child.
    pub(crate) fn wait_until_grants_idle() {
        let mut grants = grants();
        while *grants > 0 {
            grants = state()
                .grants_cv
                .wait(grants)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// A settle is only outstanding while a watcher is tracking the handle. A handle the manager
    /// discarded before tracking it (a refused reattach or a stale launch) never gets a watcher, so
    /// a settle on it can never be consumed and must not be waited for.
    fn is_outstanding(entry: &Entry) -> bool {
        entry.watching > 0 && entry.consumed < entry.issued
    }

    pub(crate) fn has_outstanding(ptr: usize) -> bool {
        lock().get(&ptr).is_some_and(is_outstanding)
    }

    pub(crate) fn wait_until_all_consumed() {
        let mut entries = lock();
        while entries.values().any(is_outstanding) {
            entries = state()
                .cv
                .wait_timeout(entries, std::time::Duration::from_secs(30))
                .map(|(entries, timeout)| {
                    assert!(
                        !timeout.timed_out(),
                        "outcome watcher never consumed its signal"
                    );
                    entries
                })
                .unwrap_or_else(|error| error.into_inner().0);
        }
    }

    /// Drops every entry; called between chaos iterations so a reused allocation cannot inherit a
    /// previous iteration's accounting.
    pub(crate) fn clear() {
        lock().clear();
    }

    /// Blocks until every settle issued for `ptr` has been consumed by its watcher.
    pub(crate) fn wait_until_consumed(ptr: usize) {
        let mut entries = lock();
        while entries.get(&ptr).is_some_and(is_outstanding) {
            entries = state()
                .cv
                .wait_timeout(entries, std::time::Duration::from_secs(30))
                .map(|(entries, timeout)| {
                    assert!(
                        !timeout.timed_out(),
                        "outcome watcher never applied handle {ptr}"
                    );
                    entries
                })
                .unwrap_or_else(|error| error.into_inner().0);
        }
    }

    #[test]
    fn reused_handle_address_starts_fresh_outcome_accounting() {
        let ptr = usize::MAX;
        register(ptr);

        track(ptr);
        note_issue(ptr);
        note_consume(ptr);
        assert!(!has_outstanding(ptr));

        note_issue(ptr);
        assert!(!has_outstanding(ptr));

        register(ptr);
        track(ptr);
        note_issue(ptr);
        assert!(has_outstanding(ptr));

        note_consume(ptr);
        assert!(!has_outstanding(ptr));
        lock().remove(&ptr);
    }

    #[test]
    fn revived_handle_tracks_another_outcome() {
        let ptr = usize::MAX - 1;
        register(ptr);
        track(ptr);
        note_issue(ptr);
        note_consume(ptr);
        track(ptr);
        note_issue(ptr);
        assert!(has_outstanding(ptr));
        note_consume(ptr);
        assert!(!has_outstanding(ptr));
        lock().remove(&ptr);
    }
}
