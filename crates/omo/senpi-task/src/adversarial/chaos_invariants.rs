//! Rust port of `__adversarial__/chaos-invariants.ts`: the instrumented `ParentNotifier`, the
//! epoch tracker that stands in for the TS `WeakMap<CompletionDetails, epoch>` (keyed here by
//! `task_id` since `CompletionDetails` carries no separate identity in Rust), and the thirteen
//! invariant checks the bench asserts every iteration.
//!
//! The TS source is test-only harness code, so the module is compiled only for tests.
#![cfg(test)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use crate::completion::{
    CompletionNotifier, FlushInput, NotifyResult, ParentNotifier, ParentNotifierMessage,
    ReconcileUnnotifiedNotificationsInput,
};
use crate::store::TaskRecordStore;

use super::chaos_actions::ChaosState;
use super::observing_store::is_terminal_status;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `NotificationEpochTracker`: tracks the run_epoch a completion detail was built under, keyed by
/// `task_id` since Rust `CompletionDetails` values carry no separate identity to key a WeakMap by.
#[derive(Default)]
pub struct NotificationEpochTracker {
    active_by_task: Mutex<HashMap<String, i64>>,
    buffered_by_task: Mutex<HashMap<String, Vec<i64>>>,
}

impl NotificationEpochTracker {
    fn remember_buffered(&self, task_id: &str, epoch: i64) {
        let mut buffered = lock(&self.buffered_by_task);
        let epochs = buffered.entry(task_id.to_string()).or_default();
        if !epochs.contains(&epoch) {
            epochs.push(epoch);
        }
    }

    fn resolve_epoch(&self, store: &TaskRecordStore, task_id: &str) -> i64 {
        if let Some(active) = lock(&self.active_by_task).get(task_id).copied() {
            return active;
        }
        let mut buffered = lock(&self.buffered_by_task);
        if let Some(epochs) = buffered.get_mut(task_id)
            && !epochs.is_empty()
        {
            let epoch = epochs.remove(0);
            if epochs.is_empty() {
                buffered.remove(task_id);
            }
            return epoch;
        }
        drop(buffered);
        store
            .load(task_id)
            .ok()
            .flatten()
            .map_or(0, |record| record.notification.run_epoch)
    }
}

/// `ChaosNotifier`: the fake `ParentNotifier` the chaos harness enqueues completions into,
/// recording every enqueue and able to fail the next N calls on demand.
pub struct ChaosNotifier {
    calls: Mutex<Vec<ParentNotifierMessage>>,
    remaining_failures: Mutex<u32>,
    store: Arc<TaskRecordStore>,
    observations: super::observing_store::SharedObservations,
    epochs: Arc<NotificationEpochTracker>,
}

impl ChaosNotifier {
    /// The raw enqueue log (TS `calls`); kept for parity with the TS notifier surface.
    #[allow(dead_code)]
    pub fn calls(&self) -> Vec<ParentNotifierMessage> {
        lock(&self.calls).clone()
    }

    pub fn call_count(&self) -> usize {
        lock(&self.calls).len()
    }

    pub fn fail_next(&self, count: u32) {
        *lock(&self.remaining_failures) = count;
    }
}

impl ParentNotifier for ChaosNotifier {
    fn enqueue(&self, message: &ParentNotifierMessage) -> Result<(), crate::host::HostError> {
        let tagged: Vec<(String, i64)> = message
            .details
            .iter()
            .map(|detail| {
                (
                    detail.task_id.clone(),
                    self.epochs.resolve_epoch(&self.store, &detail.task_id),
                )
            })
            .collect();
        {
            let mut remaining = lock(&self.remaining_failures);
            if *remaining > 0 {
                *remaining -= 1;
                return Err(crate::host::HostError {
                    message: "chaos parent gone".to_string(),
                });
            }
        }
        lock(&self.calls).push(message.clone());
        let mut observations = super::observing_store::lock_observations(&self.observations);
        for (task_id, epoch) in tagged {
            let key = format!("{task_id}:{epoch}");
            *observations.enqueue_by_epoch.entry(key).or_insert(0) += 1;
        }
        Ok(())
    }
}

pub fn create_chaos_notifier(
    store: Arc<TaskRecordStore>,
    observations: super::observing_store::SharedObservations,
    epochs: Arc<NotificationEpochTracker>,
) -> Arc<ChaosNotifier> {
    Arc::new(ChaosNotifier {
        calls: Mutex::default(),
        remaining_failures: Mutex::default(),
        store,
        observations,
        epochs,
    })
}

/// `instrumentCompletionNotifier`: wraps a real `CompletionNotifier` so every `notify_terminal`
/// call records its record's run_epoch as "active" (and, when buffered, remembers it for later
/// flush-time epoch resolution), matching the TS wrapper exactly.
pub struct InstrumentedNotifier {
    inner: CompletionNotifier,
    store: Arc<TaskRecordStore>,
    epochs: Arc<NotificationEpochTracker>,
}

impl InstrumentedNotifier {
    pub fn notify_terminal(
        &self,
        request: &crate::completion::CompletionRequest,
    ) -> Result<NotifyResult, crate::store::StoreError> {
        let task_id = request.record.task_id.clone();
        lock(&self.epochs.active_by_task).insert(task_id.clone(), request.record.notification.run_epoch);
        let result = self.inner.notify_terminal(request);
        if let Ok(NotifyResult::Buffered(_)) = result {
            self.epochs
                .remember_buffered(&task_id, request.record.notification.run_epoch);
        }
        lock(&self.epochs.active_by_task).remove(&task_id);
        result
    }

    pub fn flush_buffered(
        &self,
        input: &FlushInput,
    ) -> Result<crate::completion::FlushResult, crate::store::StoreError> {
        let result = self.inner.flush_buffered(input);
        lock(&self.epochs.buffered_by_task).clear();
        result
    }

    pub fn reconcile_unnotified_notifications(
        &self,
        input: ReconcileUnnotifiedNotificationsInput<'_>,
    ) -> Result<(), crate::store::StoreError> {
        let records = self.store.list()?.records;
        for record in &records {
            lock(&self.epochs.active_by_task)
                .insert(record.task_id.clone(), record.notification.run_epoch);
        }
        let result = self.inner.reconcile_unnotified_notifications(input);
        for record in &records {
            lock(&self.epochs.active_by_task).remove(&record.task_id);
        }
        result
    }

    pub fn reconcile_failed_notifications(
        &self,
        input: ReconcileUnnotifiedNotificationsInput<'_>,
    ) -> Result<(), crate::store::StoreError> {
        self.reconcile_unnotified_notifications(input)
    }

    pub fn buffered_count(&self, session_id: &str) -> usize {
        self.inner.buffered_count(session_id)
    }
}

pub fn instrument_completion_notifier(
    inner: CompletionNotifier,
    store: Arc<TaskRecordStore>,
    epochs: Arc<NotificationEpochTracker>,
) -> InstrumentedNotifier {
    InstrumentedNotifier { inner, store, epochs }
}

pub type InvariantId = u8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub invariant: InvariantId,
    pub detail: String,
}

fn check_exactly_once(state: &ChaosState) -> Vec<Violation> {
    let mut violations = Vec::new();
    let observations = super::observing_store::lock_observations(&state.harness.observations);
    for (key, count) in &observations.enqueue_by_epoch {
        if *count > 1 {
            violations.push(Violation {
                invariant: 1,
                detail: format!("parent enqueued {count} times for (task:epoch) {key}"),
            });
        }
    }
    for (key, count) in &observations.notify_commits {
        if *count > 1 {
            violations.push(Violation {
                invariant: 1,
                detail: format!("notification persisted {count} times for {key}"),
            });
        }
    }
    drop(observations);
    for breach in lock(&state.seam_breaches).iter() {
        violations.push(Violation {
            invariant: 1,
            detail: breach.clone(),
        });
    }
    violations
}

fn check_terminal_idempotence(state: &ChaosState) -> Vec<Violation> {
    super::observing_store::lock_observations(&state.harness.observations)
        .breaches
        .iter()
        .filter(|breach| breach.invariant == 2)
        .map(|breach| Violation {
            invariant: 2,
            detail: breach.detail.clone(),
        })
        .collect()
}

fn check_no_slot_leak(state: &ChaosState) -> Vec<Violation> {
    let mut violations = Vec::new();
    let Ok(listed) = state.harness.store.list() else {
        return violations;
    };
    let pending = listed
        .records
        .iter()
        .filter(|record| record.status == crate::state::TaskStatus::Pending)
        .count();
    if pending > 0 {
        violations.push(Violation {
            invariant: 3,
            detail: format!("{pending} task(s) still pending after drain"),
        });
    }
    let stuck: Vec<_> = listed
        .records
        .iter()
        .filter(|record| {
            !is_terminal_status(record.status)
                && record.residency_state != crate::state::ResidencyState::Disposed
                && record.residency_state != crate::state::ResidencyState::Evicted
        })
        .collect();
    if !stuck.is_empty() {
        let detail = stuck
            .iter()
            .map(|record| {
                let owners: Vec<&str> = state
                    .harness
                    .engines
                    .iter()
                    .filter(|engine| engine.manager.get_resident_handle(&record.task_id).is_some())
                    .map(|engine| engine.id.as_str())
                    .collect();
                format!(
                    "{}:{}/{}:host={}:handles={}",
                    record.task_id,
                    record.status.as_str(),
                    record.residency_state.as_str(),
                    record.host_pid.map_or("none".to_string(), |pid| pid.to_string()),
                    if owners.is_empty() {
                        "none".to_string()
                    } else {
                        owners.join("+")
                    },
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        violations.push(Violation {
            invariant: 3,
            detail: format!("{} non-terminal task(s) after drain: {detail}", stuck.len()),
        });
    }

    let mut probe_ids = Vec::new();
    let mut queued = 0;
    for _ in 0..state.harness.limit {
        let result = state.harness.probe_engine().manager.start(&crate::manager::types::ManagerStartSpec {
            prompt: "slot probe".to_string(),
            parent_session_id: state.harness.session_id.clone(),
            root_session_id: Some(state.harness.session_id.clone()),
            depth: 1,
            category: Some("quick".to_string()),
            model: Some(state.harness.model.clone()),
            ..Default::default()
        });
        let crate::manager::types::StartResult::Started(started) = result else {
            continue;
        };
        probe_ids.push(started.task_id.clone());
        if started.status != crate::state::TaskStatus::Running {
            queued += 1;
        }
    }
    if queued > 0 {
        violations.push(Violation {
            invariant: 3,
            detail: format!(
                "slot leak: {queued}/{} probe task(s) queued despite all prior tasks terminal",
                state.harness.limit
            ),
        });
    }
    for id in &probe_ids {
        for handle in state.harness.probe_engine().in_process_runner.all_handles() {
            if crate::manager::ManagedChildHandle::task_id(handle.as_ref()) == id {
                handle.complete("probe");
            }
        }
        for handle in state.harness.probe_engine().process_runner.all_handles() {
            if crate::manager::ManagedChildHandle::task_id(handle.as_ref()) == id {
                handle.complete("probe");
            }
        }
    }
    violations
}

fn check_waiters_and_pending_cancellation(state: &ChaosState) -> Vec<Violation> {
    let mut violations = Vec::new();
    let (registrations, settlements) = state.harness.waiters.counts();
    if settlements != registrations {
        violations.push(Violation {
            invariant: 5,
            detail: format!("waitFor settled {settlements}/{registrations} registered waiter(s)"),
        });
    }
    let started = state.harness.runner.started_task_ids();
    for task_id in lock(&state.harness.pending_cancelled_task_ids).iter() {
        if started.contains(task_id) {
            violations.push(Violation {
                invariant: 5,
                detail: format!("task {task_id} cancelled from pending reached runner start"),
            });
        }
    }
    violations
}

fn check_lifecycle_invariants(state: &ChaosState) -> Vec<Violation> {
    let mut violations = Vec::new();
    for detail in lock(&state.harness.lifecycle_observations.live_handle_breaches).iter() {
        violations.push(Violation {
            invariant: 6,
            detail: detail.clone(),
        });
    }
    let observations = super::observing_store::lock_observations(&state.harness.observations);
    for detail in &observations.lifecycle_breaches {
        let invariant = if detail.contains("recoverable") {
            7
        } else if detail.contains("run_epoch") || detail.contains("suspension") {
            8
        } else {
            10
        };
        violations.push(Violation {
            invariant,
            detail: detail.clone(),
        });
    }
    let max_residents = observations
        .max_residents_by_parent
        .get(&state.harness.session_id)
        .copied()
        .unwrap_or(0);
    drop(observations);
    for detail in lock(&state.harness.lifecycle_observations.pid_breaches).iter() {
        violations.push(Violation {
            invariant: 9,
            detail: detail.clone(),
        });
    }
    let referenced_pids: std::collections::BTreeSet<i64> = state
        .harness
        .store
        .list()
        .map(|listed| listed.records.iter().filter_map(|record| record.pid).collect())
        .unwrap_or_default();
    for pid in lock(&state.harness.processes.alive).iter() {
        if *pid >= 20_000 && !referenced_pids.contains(pid) {
            violations.push(Violation {
                invariant: 9,
                detail: format!("orphan fake pid {pid} remained alive after reconcile"),
            });
        }
    }
    let killed_residents = state
        .harness
        .store
        .list()
        .map(|listed| {
            listed
                .records
                .into_iter()
                .filter(|record| {
                    record.killed == Some(true)
                        && record.residency_state == crate::state::ResidencyState::Resident
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for record in killed_residents {
        violations.push(Violation {
            invariant: 10,
            detail: format!("killed task {} still pins a residency slot", record.task_id),
        });
    }
    if max_residents > state.harness.residency_max {
        violations.push(Violation {
            invariant: 11,
            detail: format!(
                "resident count {max_residents} exceeded cap {}",
                state.harness.residency_max
            ),
        });
    }
    for detail in lock(&state.harness.lifecycle_observations.reclamation_breaches).iter() {
        violations.push(Violation {
            invariant: 12,
            detail: detail.clone(),
        });
    }
    for detail in lock(&state.harness.lifecycle_observations.terminal_relaunches).iter() {
        violations.push(Violation {
            invariant: 13,
            detail: format!("terminal record without session relaunched: {detail}"),
        });
    }
    violations
}

pub fn collect_invariant_violations(state: &ChaosState) -> Vec<Violation> {
    let mut violations = check_exactly_once(state);
    violations.extend(check_terminal_idempotence(state));
    violations.extend(check_no_slot_leak(state));
    violations.extend(check_waiters_and_pending_cancellation(state));
    violations.extend(check_lifecycle_invariants(state));
    violations
}
