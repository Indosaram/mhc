//! Rust port of `__adversarial__/chaos-actions.ts`: the weighted random action table and the
//! `ChaosState` every action reads and mutates.
//!
//! The TS source is test-only harness code, so the module is compiled only for tests.
#![cfg(test)]

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, PoisonError};

use crate::completion::{CompletionRequest, FlushInput, NotifyResult, ParentState, ReconcileUnnotifiedNotificationsInput};
use crate::manager::types::ManagerStartSpec;
use crate::state::{DeliverAs, TaskRecord, TaskStatus};

use super::chaos_engine_factory::ChaosEngine;
use super::chaos_harness::{CHAOS_MODEL, CHAOS_SESSION, ChaosHarness};
use super::chaos_lifecycle_actions::{crash_mid_suspend, mass_revive_at_cap, resume_session, sibling_reconcile_race, suspend_session};
pub use super::observing_store::is_terminal_status;
use super::prng::{RandomSource, WeightedChoice};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `ChaosState`: the per-iteration mutable state the action table and invariant checks share.
pub struct ChaosState {
    pub harness: ChaosHarness,
    pub rng: Mutex<RandomSource>,
    pub task_ids: Mutex<Vec<String>>,
    pub background: Mutex<HashMap<String, bool>>,
    pub seam_breaches: Mutex<Vec<String>>,
    pub observed_edges: Mutex<HashSet<String>>,
    pub max_tasks: u32,
}

impl ChaosState {
    pub fn new(harness: ChaosHarness, rng: RandomSource, max_tasks: u32) -> Self {
        Self {
            harness,
            rng: Mutex::new(rng),
            task_ids: Mutex::default(),
            background: Mutex::default(),
            seam_breaches: Mutex::default(),
            observed_edges: Mutex::default(),
            max_tasks,
        }
    }

    pub fn task_ids(&self) -> Vec<String> {
        self.task_ids_snapshot()
    }

    pub fn rng_int(&self, min_inclusive: i64, max_inclusive: i64) -> i64 {
        lock(&self.rng).int(min_inclusive, max_inclusive)
    }

    pub fn push_task_id(&self, task_id: String) {
        lock(&self.task_ids).push(task_id);
    }

    pub fn mark_background(&self, task_id: &str, background: bool) {
        lock(&self.background).insert(task_id.to_string(), background);
    }

    fn rng_pick(&self, items: &[String]) -> Option<String> {
        if items.is_empty() {
            return None;
        }
        lock(&self.rng).pick(items).ok().cloned()
    }

    fn task_ids_snapshot(&self) -> Vec<String> {
        lock(&self.task_ids).clone()
    }

    fn load(&self, task_id: &str) -> Option<TaskRecord> {
        self.harness.store.load(task_id).ok().flatten()
    }

    fn by_status(&self, predicate: impl Fn(&TaskRecord) -> bool) -> Vec<String> {
        self.task_ids_snapshot()
            .into_iter()
            .filter(|id| self.load(id).is_some_and(|record| predicate(&record)))
            .collect()
    }

    fn non_terminal(&self) -> Vec<String> {
        self.by_status(|record| !is_terminal_status(record.status))
    }

    fn running_with_handle(&self) -> Vec<String> {
        self.task_ids_snapshot()
            .into_iter()
            .filter(|id| {
                self.load(id).is_some_and(|record| record.status == TaskStatus::Running)
                    && self
                        .harness
                        .engines
                        .iter()
                        .any(|engine| engine.manager.get_resident_handle(id).is_some())
            })
            .collect()
    }

    fn owner_for(&self, task_id: &str) -> Option<&ChaosEngine> {
        self.harness
            .engines
            .iter()
            .find(|engine| engine.manager.get_resident_handle(task_id).is_some())
    }

    fn fake_handle_for(&self, task_id: &str) -> Option<std::sync::Arc<crate::manager::manager_tests::fakes::FakeHandle>> {
        for engine in &self.harness.engines {
            if let Some(handle) = engine.in_process_runner.handle(task_id) {
                return Some(handle);
            }
            if let Some(handle) = engine.process_runner.handle(task_id) {
                return Some(handle);
            }
        }
        None
    }
}

const NOTIFYING_TERMINALS: [TaskStatus; 3] = [TaskStatus::Completed, TaskStatus::Error, TaskStatus::Lost];
const REVIVABLE: [TaskStatus; 3] = [TaskStatus::Completed, TaskStatus::Error, TaskStatus::Interrupted];

fn deliver_states() -> [ParentState; 2] {
    [ParentState::Idle, ParentState::Streaming]
}

fn buffer_states() -> [ParentState; 3] {
    [ParentState::Compacting, ParentState::SessionSwitching, ParentState::SessionShutdown]
}

fn act_start(state: &ChaosState) {
    if state.task_ids_snapshot().len() as u32 >= state.max_tasks {
        return;
    }
    let background = lock(&state.rng).bool(0.7);
    let execution_mode = if lock(&state.rng).coin() {
        crate::manager::execution_mode::ExecutionMode::InProcess
    } else {
        crate::manager::execution_mode::ExecutionMode::Process
    };
    let result = state.harness.manager.start(&ManagerStartSpec {
        prompt: "chaos turn".to_string(),
        parent_session_id: CHAOS_SESSION.to_string(),
        root_session_id: Some(CHAOS_SESSION.to_string()),
        depth: 1,
        category: Some("quick".to_string()),
        model: Some(CHAOS_MODEL.to_string()),
        execution_mode: Some(execution_mode),
        run_in_background: background,
        ..ManagerStartSpec::default()
    });
    if let crate::manager::types::StartResult::Started(started) = result {
        lock(&state.task_ids).push(started.task_id.clone());
        lock(&state.background).insert(started.task_id, background);
    }
}

fn settle_with(state: &ChaosState, outcome: &str) {
    let Some(id) = state.rng_pick(&state.running_with_handle()) else {
        return;
    };
    let Some(handle) = state.fake_handle_for(&id) else {
        return;
    };
    state.harness.observe_mutation(&id, || {
        if outcome == "clean" {
            handle.complete("clean exit");
        } else {
            handle.fail(
                crate::runners::RunnerFailureKind::ChildPromptFailed,
                "terminated by signal SIGKILL",
            );
        }
    });
}

fn act_steer(state: &ChaosState) {
    let Some(id) = state.rng_pick(&state.by_status(|record| {
        record.status == TaskStatus::Running && record.residency_state == crate::state::ResidencyState::Resident
    })) else {
        return;
    };
    if let Some(engine) = state.owner_for(&id) {
        state.harness.observe_mutation(&id, || {
            let _ = engine.manager.continue_task(&id, "keep going", Some(DeliverAs::Steer));
        });
    }
}

fn act_interrupt(state: &ChaosState) {
    let Some(id) = state.rng_pick(&state.by_status(|record| record.status == TaskStatus::Running)) else {
        return;
    };
    if let Some(engine) = state.owner_for(&id) {
        state.harness.observe_mutation(&id, || {
            let _ = engine.manager.interrupt_task(&id);
        });
    }
}

fn act_cancel(state: &ChaosState) {
    let Some(id) = state.rng_pick(&state.by_status(|record| record.status == TaskStatus::Running)) else {
        return;
    };
    let reason = lock(&state.rng).coin().then(|| "user aborted".to_string());
    if let Some(engine) = state.owner_for(&id) {
        state.harness.observe_mutation(&id, || {
            let _ = engine
                .manager
                .cancel_task(&id, reason.as_deref(), crate::steering::CancelOptions::default());
        });
    }
}

fn act_cancel_pending(state: &ChaosState) {
    let Some(id) = state.rng_pick(&state.by_status(|record| record.status == TaskStatus::Pending)) else {
        return;
    };
    let reason = lock(&state.rng).coin().then(|| "cancelled while queued".to_string());
    let outcome = state.harness.observe_mutation(&id, || {
        state
            .harness
            .manager
            .cancel_task(&id, reason.as_deref(), crate::steering::CancelOptions::default())
    });
    if let Ok(crate::steering::CancelOutcome::Cancelled {
        task_id,
        previous_status: TaskStatus::Pending,
    }) = outcome
    {
        lock(&state.harness.pending_cancelled_task_ids).insert(task_id);
    }
}

fn act_abort_parent_wait(state: &ChaosState) {
    let Some(id) = state.rng_pick(&state.non_terminal()) else {
        return;
    };
    let steps = lock(&state.rng).int(1, 6) as u64;
    state.harness.waiters.register(&state.harness.manager, &id, steps);
}

fn act_revive(state: &ChaosState) {
    let Some(id) = state.rng_pick(&state.by_status(|record| {
        REVIVABLE.contains(&record.status) && record.residency_state == crate::state::ResidencyState::Resident
    })) else {
        return;
    };
    if let Some(engine) = state.owner_for(&id) {
        state.harness.observe_mutation(&id, || {
            let _ = engine.manager.continue_task(&id, "revive please", None);
        });
    }
}

fn expected_delta(result: &NotifyResult) -> i64 {
    matches!(result, NotifyResult::Delivered(_)) as i64
}

fn check_delta(state: &ChaosState, before: usize, result: &NotifyResult) {
    let delta = (state.harness.parent_notifier.call_count() as i64) - before as i64;
    let expected = expected_delta(result);
    if delta != expected {
        lock(&state.seam_breaches).push(format!(
            "notify result {result:?} produced {delta} enqueue(s), expected {expected}"
        ));
    }
}

fn unobserved_notifying(state: &ChaosState) -> Vec<String> {
    state.by_status(|record| {
        if !NOTIFYING_TERMINALS.contains(&record.status)
            || !lock(&state.background).get(&record.task_id).copied().unwrap_or(false)
        {
            return false;
        }
        !lock(&state.observed_edges).contains(&format!("{}:{}", record.task_id, record.notification.run_epoch))
    })
}

fn already_notified(state: &ChaosState) -> Vec<String> {
    state.by_status(|record| {
        NOTIFYING_TERMINALS.contains(&record.status)
            && record.notification.notified_epoch >= record.notification.run_epoch
    })
}

fn notify_edge(state: &ChaosState, id: &str, parent_state: ParentState) {
    let Some(record) = state.load(id) else {
        return;
    };
    lock(&state.observed_edges).insert(format!("{id}:{}", record.notification.run_epoch));
    let before = state.harness.parent_notifier.call_count();
    let result = state.harness.observe_mutation(id, || {
        state.harness.notifier.notify_terminal(&CompletionRequest {
            record,
            parent_state,
            run_in_background: true,
            tokens: None,
        })
    });
    if let Ok(result) = result {
        check_delta(state, before, &result);
    }
}

fn act_notify(state: &ChaosState) {
    let Some(id) = state.rng_pick(&unobserved_notifying(state)) else {
        return;
    };
    let parent_state = if lock(&state.rng).bool(0.6) {
        *lock(&state.rng).pick(&deliver_states()).expect("non-empty")
    } else {
        *lock(&state.rng).pick(&buffer_states()).expect("non-empty")
    };
    notify_edge(state, &id, parent_state);
}

fn act_replay_notify(state: &ChaosState) {
    let Some(id) = state.rng_pick(&already_notified(state)) else {
        return;
    };
    let Some(record) = state.load(&id) else {
        return;
    };
    let epoch = record.notification.run_epoch;
    let before = state.harness.parent_notifier.call_count();
    let result = state.harness.observe_mutation(&id, || {
        state.harness.notifier.notify_terminal(&CompletionRequest {
            record,
            parent_state: ParentState::Idle,
            run_in_background: true,
            tokens: None,
        })
    });
    let Ok(result) = result else {
        return;
    };
    check_delta(state, before, &result);
    if !matches!(result, NotifyResult::Skipped(_)) {
        lock(&state.seam_breaches).push(format!(
            "replay of already-notified {id} epoch {epoch} was {result:?}, expected skipped"
        ));
    }
}

fn act_flush(state: &ChaosState) {
    let before = state.harness.parent_notifier.call_count();
    let replaced = lock(&state.rng).bool(0.3);
    let result = state.harness.notifier.flush_buffered(&FlushInput {
        session_id: CHAOS_SESSION.to_string(),
        replaced,
    });
    state.harness.observe_reconcile();
    let Ok(result) = result else {
        return;
    };
    let delta = (state.harness.parent_notifier.call_count() as i64) - before as i64;
    let expected = matches!(result, crate::completion::FlushResult::Flushed(_)) as i64;
    if delta != expected {
        lock(&state.seam_breaches).push(format!(
            "flush result {result:?} produced {delta} enqueue(s), expected {expected}"
        ));
    }
}

fn act_evict(state: &ChaosState) {
    if let Some(engine) = state.harness.engines.first() {
        let _ = engine.lifecycle.admit_resident(CHAOS_SESSION);
        state.harness.observe_reconcile();
    }
}

fn act_reconcile(state: &ChaosState) {
    resume_session(state, CHAOS_SESSION);
    let _ = state.harness.notifier.reconcile_failed_notifications(ReconcileUnnotifiedNotificationsInput {
        session_id: CHAOS_SESSION,
        parent_state: ParentState::Idle,
    });
    state.harness.observe_reconcile();
}

fn act_notifier_fail_then_retry(state: &ChaosState) {
    let pending_retries = state.harness.retry_scheduler.pending_count();
    if pending_retries > 0 && lock(&state.rng).bool(0.7) {
        let index = lock(&state.rng).int(0, pending_retries as i64 - 1) as usize;
        state.harness.retry_scheduler.run(index);
        state.harness.observe_reconcile();
        return;
    }
    let Some(id) = state.rng_pick(&unobserved_notifying(state)) else {
        if pending_retries > 0 {
            let index = lock(&state.rng).int(0, pending_retries as i64 - 1) as usize;
            state.harness.retry_scheduler.run(index);
            state.harness.observe_reconcile();
        }
        return;
    };
    let count = lock(&state.rng).int(2, 5) as u32;
    state.harness.parent_notifier.fail_next(count);
    notify_edge(state, &id, ParentState::Idle);
}

type ActionFn = fn(&ChaosState);

struct Action {
    name: &'static str,
    run: ActionFn,
    weight: f64,
}

fn actions() -> [Action; 18] {
    [
        Action { name: "start", run: act_start, weight: 8.0 },
        Action { name: "settle_clean", run: |state| settle_with(state, "clean"), weight: 7.0 },
        Action { name: "settle_signal", run: |state| settle_with(state, "signal"), weight: 4.0 },
        Action { name: "steer", run: act_steer, weight: 3.0 },
        Action { name: "interrupt", run: act_interrupt, weight: 3.0 },
        Action { name: "cancel", run: act_cancel, weight: 3.0 },
        Action { name: "cancel_pending", run: act_cancel_pending, weight: 4.0 },
        Action { name: "abort_wait", run: act_abort_parent_wait, weight: 4.0 },
        Action { name: "revive", run: act_revive, weight: 3.0 },
        Action { name: "notify", run: act_notify, weight: 6.0 },
        Action { name: "replay_notify", run: act_replay_notify, weight: 3.0 },
        Action { name: "flush", run: act_flush, weight: 4.0 },
        Action { name: "evict", run: act_evict, weight: 3.0 },
        Action { name: "reconcile", run: act_reconcile, weight: 2.0 },
        Action { name: "suspend_session", run: suspend_session, weight: 2.0 },
        Action { name: "crash_mid_suspend", run: crash_mid_suspend, weight: 1.0 },
        Action { name: "sibling_reconcile_race", run: sibling_reconcile_race, weight: 1.0 },
        Action { name: "mass_revive_at_cap", run: mass_revive_at_cap, weight: 1.0 },
    ]
}

fn actions_with_notify_retry() -> Vec<Action> {
    let mut list: Vec<Action> = actions().into_iter().collect();
    list.push(Action { name: "notify_retry", run: act_notifier_fail_then_retry, weight: 4.0 });
    list
}

pub fn apply_random_action(state: &ChaosState) {
    state.harness.waiters.advance();
    let table = actions_with_notify_retry();
    let choices: Vec<WeightedChoice<usize>> = table
        .iter()
        .enumerate()
        .map(|(index, action)| WeightedChoice { value: index, weight: action.weight })
        .collect();
    let chosen = *lock(&state.rng).weighted(&choices).expect("non-empty action table");
    let action = &table[chosen];
    state.harness.lifecycle_observations.push_trace(action.name);
    (action.run)(state);
    // TS calls `flushMicrotasks()` here, gated on a coin flip, so buffered promise continuations
    // settle before the next action reads state. This port's manager/lifecycle calls are
    // synchronous with no microtask queue to flush, but the coin flip must still be drawn to keep
    // this port's RandomSource draw sequence identical to the pinned TS seed's.
    let _ = lock(&state.rng).bool(0.5);
}
