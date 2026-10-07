//! Rust port of `__adversarial__/chaos-lifecycle-actions.ts`: the suspend/revive/crash/reconcile
//! actions the chaos bench and the dedicated lifecycle probe both drive.
//!
//! The TS source is test-only harness code, so the module is compiled only for tests.
#![cfg(test)]

use std::sync::{Mutex, PoisonError};

use crate::state::TaskRecord;

use super::chaos_actions::ChaosState;
use super::chaos_engine_factory::ChaosEngine;
use super::chaos_harness::{CHAOS_MODEL, CHAOS_SESSION};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn candidates(state: &ChaosState, predicate: impl Fn(&TaskRecord) -> bool) -> Vec<String> {
    lock(&state.task_ids)
        .iter()
        .filter(|id| {
            state
                .harness
                .store
                .load(id)
                .ok()
                .flatten()
                .is_some_and(|record| predicate(&record))
        })
        .cloned()
        .collect()
}

fn pick(state: &ChaosState, ids: &[String]) -> Option<String> {
    if ids.is_empty() {
        return None;
    }
    lock(&state.rng).pick(ids).ok().cloned()
}

/// `suspendSession`: the adapter composes shutdown with the manager's private pending-queue
/// dequeue port, which this runner-level harness has no equivalent public seam for, so suspension
/// is only driven once the queue is empty (pending suspension itself is pinned by
/// `lifecycle/shutdown.test.ts`, ported separately).
pub fn suspend_session(state: &ChaosState) {
    lock(&state.harness.lifecycle_observations.action_counts).suspend_session += 1;
    let Ok(listed) = state.harness.store.list() else {
        return;
    };
    if listed
        .records
        .iter()
        .any(|record| record.status == crate::state::TaskStatus::Pending)
    {
        return;
    }
    if let Some(engine) = state.harness.engines.first() {
        let _ = engine.lifecycle.suspend_on_session_shutdown(&crate::lifecycle::SuspendInput {
            parent_session_id: CHAOS_SESSION.to_string(),
            reason: "quit".to_string(),
        });
        state.harness.observe_reconcile();
    }
}

pub fn resume_session(state: &ChaosState, parent_session_id: &str) {
    lock(&state.harness.lifecycle_observations.action_counts).resume_session += 1;
    if let Some(engine) = state.harness.engines.get(1) {
        let _ = engine.lifecycle.reconcile_on_session_start(Some(parent_session_id));
        state.harness.observe_reconcile();
    }
    state.harness.observe_live_handles();
}

pub fn crash_mid_suspend(state: &ChaosState) {
    lock(&state.harness.lifecycle_observations.action_counts).crash_mid_suspend += 1;
    let Some(id) = pick(
        state,
        &candidates(state, |record| {
            record.execution_mode == "process"
                && record.status == crate::state::TaskStatus::Running
                && record.residency_state == crate::state::ResidencyState::Resident
        }),
    ) else {
        return;
    };
    let Some(engine) = state
        .harness
        .engines
        .iter()
        .find(|engine| engine.manager.get_resident_handle(&id).is_some())
    else {
        return;
    };
    let Some(handle) = engine.manager.get_resident_handle(&id) else {
        return;
    };
    engine.manager.forget(&id);
    let _ = handle.abort();
    let _ = engine.lifecycle.reconcile_on_session_start(Some(CHAOS_SESSION));
    state.harness.observe_reconcile();
    state.harness.observe_live_handles();
}

pub fn sibling_reconcile_race(state: &ChaosState) {
    lock(&state.harness.lifecycle_observations.action_counts).sibling_reconcile_race += 1;
    for engine in state.harness.engines.iter().take(2) {
        let _ = engine.lifecycle.reconcile_on_session_start(Some(CHAOS_SESSION));
        state.harness.observe_reconcile();
    }
    state.harness.observe_live_handles();
}

fn synthetic_id(state: &ChaosState, offset: usize) -> String {
    let suffix = 0xca00 + lock(&state.task_ids).len() + offset;
    format!("st_0000{suffix:04x}")
}

pub fn observe_reclamation_postcondition(state: &ChaosState, task_id: &str, owner: Option<&ChaosEngine>) {
    let record = state.harness.store.load(task_id).ok().flatten();
    let attached = owner.is_some_and(|engine| engine.manager.get_resident_handle(task_id).is_some());
    if record
        .as_ref()
        .is_some_and(|record| record.residency_state == crate::state::ResidencyState::Resident)
        && attached
    {
        return;
    }
    lock(&state.harness.lifecycle_observations.reclamation_breaches).push(format!(
        "orphan {task_id} remained ownerless after capacity-gated reconcile at full cap"
    ));
}

pub fn mass_revive_at_cap(state: &ChaosState) {
    lock(&state.harness.lifecycle_observations.action_counts).mass_revive_at_cap += 1;
    let count = state.harness.residency_max + 2;
    for index in 0..count {
        let task_id = synthetic_id(state, index);
        if state.harness.store.load(&task_id).ok().flatten().is_some() {
            continue;
        }
        let terminal = index >= state.harness.residency_max.max(1);
        let timestamp = format!("2027-01-01T00:00:{index:02}.000Z");
        let mut record = TaskRecord {
            task_id: task_id.clone(),
            status: if terminal {
                crate::state::TaskStatus::Completed
            } else {
                crate::state::TaskStatus::Running
            },
            residency_state: crate::state::ResidencyState::PersistedOnly,
            parent_session_id: CHAOS_SESSION.to_string(),
            root_session_id: CHAOS_SESSION.to_string(),
            depth: 1,
            execution_mode: "in-process".to_string(),
            model: CHAOS_MODEL.to_string(),
            notify_on_terminal: false,
            created_at: timestamp.clone(),
            updated_at: timestamp,
            notification: crate::state::TaskNotification {
                run_epoch: (index % 3) as i64,
                notified_epoch: -1,
                notification_failed_epoch: None,
                liveness_notified_epoch: None,
            },
            name: Some(task_id.clone()),
            spawn_spec: Some(crate::state::TaskSpawnSpec::V1(crate::state::SpawnSpecV1 {
                cwd: state.harness.store.state_dir().to_string_lossy().into_owned(),
                prompt: format!("mass {index}"),
                instructions: None,
                member_scoped_tool_names: None,
                isolation: None,
            })),
            task_summary: None,
            description: None,
            agent_type: None,
            category: None,
            tool_allow: None,
            tool_deny: None,
            requested_model: None,
            fallback_models: None,
            fallback_attempts: None,
            resolved_model: None,
            owner: None,
            pending_steering: None,
            pid: None,
            host_pid: None,
            child_session_id: None,
            final_response: None,
            error_message: None,
            killed: None,
            run_stats: None,
            isolation: None,
            runner_kind: None,
            host_session: None,
        };
        if terminal {
            record.final_response = Some("already done".to_string());
        }
        let _ = state.harness.store.save(&record);
        super::observing_store::observe_store_write(&state.harness.store, &state.harness.observations, None, Some(&record));
        if terminal && index != count - 1 {
            state.harness.write_session(&task_id);
        }
        lock(&state.task_ids).push(task_id);
    }
    for engine in state.harness.engines.iter().take(2) {
        let _ = engine.lifecycle.reconcile_on_session_start(Some(CHAOS_SESSION));
        state.harness.observe_reconcile();
    }
    state.harness.observe_live_handles();

    let resident = state.harness.store.list().ok().and_then(|listed| {
        listed.records.into_iter().find(|record| {
            record.parent_session_id == CHAOS_SESSION
                && record.residency_state == crate::state::ResidencyState::Resident
                && record.status == crate::state::TaskStatus::Running
        })
    });
    let Some(resident) = resident else {
        return;
    };
    let owner = state
        .harness
        .engines
        .iter()
        .find(|engine| engine.manager.get_resident_handle(&resident.task_id).is_some());
    if let Some(owner) = owner {
        owner.manager.forget(&resident.task_id);
    }
    let before = state
        .harness
        .store
        .load(&resident.task_id)
        .ok()
        .flatten()
        .map(|record| record.notification.run_epoch);
    if let Some(owner) = owner {
        let _ = owner.lifecycle.reconcile_on_session_start(Some(CHAOS_SESSION));
        state.harness.observe_reconcile();
    }
    let after = state.harness.store.load(&resident.task_id).ok().flatten();
    observe_reclamation_postcondition(state, &resident.task_id, owner);
    if let Some(before) = before
        && after.as_ref().map(|record| record.notification.run_epoch) != Some(before + 1)
    {
        lock(&state.seam_breaches).push(format!(
            "reclaimed {} epoch did not advance exactly once",
            resident.task_id
        ));
    }

    let killed = state.harness.store.list().ok().and_then(|listed| {
        listed.records.into_iter().find(|record| {
            record.task_id != resident.task_id
                && record.residency_state == crate::state::ResidencyState::Resident
                && record.status == crate::state::TaskStatus::Running
        })
    });
    let Some(killed) = killed else {
        return;
    };
    let killed_owner = state
        .harness
        .engines
        .iter()
        .find(|engine| engine.manager.get_resident_handle(&killed.task_id).is_some());
    if let Some(killed_owner) = killed_owner {
        killed_owner.manager.forget(&killed.task_id);
    }
    let _ = state.harness.store.mutate(&killed.task_id, |fresh| {
        let mut next = fresh.clone();
        next.killed = Some(true);
        next
    });
    if let Some(killed_owner) = killed_owner {
        let _ = killed_owner.lifecycle.reconcile_on_session_start(Some(CHAOS_SESSION));
        state.harness.observe_reconcile();
    }
    if state
        .harness
        .store
        .load(&killed.task_id)
        .ok()
        .flatten()
        .map(|record| record.residency_state)
        != Some(crate::state::ResidencyState::Disposed)
    {
        let mut observations = super::observing_store::lock_observations(&state.harness.observations);
        observations
            .lifecycle_breaches
            .push(format!("killed orphan {} was not disposed", killed.task_id));
    }
}
