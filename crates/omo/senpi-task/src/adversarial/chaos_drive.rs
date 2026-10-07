//! Rust port of `__adversarial__/chaos-drive.ts`: `drain`, one fully-isolated chaos iteration
//! (`run_iteration`), the dedicated lifecycle probe (`run_lifecycle_probe`), and the eight
//! deterministic lifecycle-mutation regressions (`run_lifecycle_mutation_probe`).
//!
//! The TS source is test-only harness code, so the module is compiled only for tests.
#![cfg(test)]

use crate::manager::types::{ManagedRunner, ManagedStartSpec};
use crate::state::{ResidencyState, TaskNotification, TaskRecord, TaskSpawnSpec, TaskStatus};

use super::chaos_actions::{ChaosState, apply_random_action};
use super::chaos_engine::ActionCounts;
use super::chaos_harness::{ChaosHarness, ChaosHarnessOptions, CHAOS_SESSION, build_harness};
use super::chaos_invariants::{Violation, collect_invariant_violations};
use super::chaos_lifecycle_actions::{
    crash_mid_suspend, mass_revive_at_cap, observe_reclamation_postcondition, resume_session,
    sibling_reconcile_race, suspend_session,
};
use super::observing_store::is_terminal_status;
use super::prng::RandomSource;

fn handle_count(harness: &ChaosHarness) -> usize {
    harness.runner.all_handles().len()
}

/// `drain`: forces every live and queued task to a terminal outcome so the slot/queue invariant
/// can be checked against a fully quiesced system. Settling every handle each round releases
/// occupied slots (including revived and late-launched children) and lets the queue drain
/// deterministically.
fn drain(state: &ChaosState) {
    let cap = state.task_ids().len() * 8 + 80;
    for _round in 0..cap {
        let before = handle_count(&state.harness);
        for engine in &state.harness.engines {
            for handle in engine.in_process_runner.all_handles() {
                handle.complete("drain");
            }
            for handle in engine.process_runner.all_handles() {
                handle.complete("drain");
            }
        }
        state.harness.flush_outcomes();
        state.harness.waiters.advance();
        for engine in &state.harness.engines {
            let _ = engine.lifecycle.admit_resident(CHAOS_SESSION);
            let _ = engine.lifecycle.reconcile_on_session_start(Some(CHAOS_SESSION));
        }
        state.harness.observe_reconcile();
        let _ = state.harness.notifier.flush_buffered(&crate::completion::FlushInput {
            session_id: CHAOS_SESSION.to_string(),
            replaced: false,
        });
        let _ = state.harness.notifier.reconcile_failed_notifications(crate::completion::ReconcileUnnotifiedNotificationsInput {
            session_id: CHAOS_SESSION,
            parent_state: crate::completion::ParentState::Idle,
        });
        state.harness.observe_reconcile();
        let pending_retries = state.harness.retry_scheduler.pending_count();
        if pending_retries > 0 {
            let index = state.rng_int(0, pending_retries as i64 - 1) as usize;
            state.harness.retry_scheduler.run(index);
            state.harness.observe_reconcile();
        }
        let Ok(listed) = state.harness.store.list() else {
            break;
        };
        let all_terminal = listed.records.iter().all(|record| is_terminal_status(record.status));
        let any_pending = listed.records.iter().any(|record| record.status == TaskStatus::Pending);
        let grew = handle_count(&state.harness) > before;
        let notifications_settled = state.harness.retry_scheduler.pending_count() == 0
            && state.harness.notifier.buffered_count(CHAOS_SESSION) == 0;
        if all_terminal && !any_pending && !grew && notifications_settled {
            break;
        }
    }
    state.harness.waiters.abort_all();
}

/// `IterationReport`: one seeded chaos iteration's outcome. `steps` and `tasks` mirror the TS
/// report fields; the bench asserts only `violations`, as the TS test does.
#[allow(dead_code)]
pub struct IterationReport {
    pub steps: u32,
    pub tasks: usize,
    pub violations: Vec<Violation>,
}

fn make_state(seed: u32, concurrency: usize, residency_max: usize, max_tasks: u32) -> ChaosState {
    let rng = RandomSource::new(seed);
    ChaosState::new(
        build_harness(ChaosHarnessOptions {
            concurrency,
            residency_max,
            max_depth: 3,
        }),
        rng,
        max_tasks,
    )
}

/// `runIteration`: runs one fully-isolated chaos iteration for `seed` and returns any invariant
/// violations. The harness (temp state dir) is always disposed via `Drop`, pass or fail.
pub fn run_iteration(seed: u32) -> IterationReport {
    // Each iteration builds a fresh harness in a fresh temp dir, but a freed allocation can be
    // reused for the next iteration's handles; drop any barrier entries a prior iteration left so
    // the new handles cannot inherit stale watcher state.
    crate::manager::outcome::test_barrier::clear();
    let mut rng = RandomSource::new(seed);
    let concurrency = rng.int(1, 4) as usize;
    let residency_max = rng.int(1, 4) as usize;
    let max_tasks = rng.int(3, 8) as u32;
    let state = make_state(seed, concurrency, residency_max, max_tasks);
    let steps = rng.int(12, 30) as u32;
    for _ in 0..steps {
        apply_random_action(&state);
    }
    drain(&state);
    let violations = collect_invariant_violations(&state);
    let tasks = state.task_ids().len();
    IterationReport { steps, tasks, violations }
}

/// `LifecycleProbeReport`: the action-count snapshot and violations one lifecycle probe run
/// produced.
pub struct LifecycleProbeReport {
    pub action_counts: ActionCounts,
    pub violations: Vec<Violation>,
}

fn start_probe_task(state: &ChaosState, mode: &'static str, prompt: &str) -> String {
    let execution_mode = if mode == "process" {
        crate::manager::execution_mode::ExecutionMode::Process
    } else {
        crate::manager::execution_mode::ExecutionMode::InProcess
    };
    let result = state.harness.manager.start(&crate::manager::types::ManagerStartSpec {
        prompt: prompt.to_string(),
        parent_session_id: CHAOS_SESSION.to_string(),
        root_session_id: Some(CHAOS_SESSION.to_string()),
        depth: 1,
        category: Some("quick".to_string()),
        model: Some(state.harness.model.clone()),
        execution_mode: Some(execution_mode),
        run_in_background: true,
        ..crate::manager::types::ManagerStartSpec::default()
    });
    let crate::manager::types::StartResult::Started(started) = result else {
        panic!("probe start failed");
    };
    state.push_task_id(started.task_id.clone());
    state.mark_background(&started.task_id, true);
    started.task_id
}

/// `runLifecycleProbe`: a fixed, non-randomized scenario that suspends, resumes, crashes and mass
/// revives at a small deterministic seed, then drains and checks invariants.
pub fn run_lifecycle_probe() -> LifecycleProbeReport {
    let state = make_state(0x0021_ca05, 8, 3, 20);
    start_probe_task(&state, "in-process", "suspend and resume");
    start_probe_task(&state, "process", "crash during suspend");
    suspend_session(&state);
    resume_session(&state, CHAOS_SESSION);
    start_probe_task(&state, "process", "retained pid");
    crash_mid_suspend(&state);
    sibling_reconcile_race(&state);
    mass_revive_at_cap(&state);
    drain(&state);
    LifecycleProbeReport {
        action_counts: *state.harness.lifecycle_observations.action_counts.lock().unwrap_or_else(|error| error.into_inner()),
        violations: collect_invariant_violations(&state),
    }
}

/// `LifecycleMutation`: the eight deterministic mutant scenarios `run_lifecycle_mutation_probe`
/// drives, each expected to trip at least one invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleMutation {
    DoubleRevive,
    LoseRecoverable,
    SuspensionBumpsEpoch,
    SpawnBeforeOrphanKill,
    ReviveForbidden,
    OverAdmit,
    CapacityGateReclamation,
    RelaunchTerminalWithoutSession,
}

impl LifecycleMutation {
    /// The eight scenarios in TS `MUTATIONS` order; the lifecycle test drives them individually.
    #[allow(dead_code)]
    pub const ALL: [LifecycleMutation; 8] = [
        LifecycleMutation::DoubleRevive,
        LifecycleMutation::LoseRecoverable,
        LifecycleMutation::SuspensionBumpsEpoch,
        LifecycleMutation::SpawnBeforeOrphanKill,
        LifecycleMutation::ReviveForbidden,
        LifecycleMutation::OverAdmit,
        LifecycleMutation::CapacityGateReclamation,
        LifecycleMutation::RelaunchTerminalWithoutSession,
    ];

    /// The TS mutation label (`MUTATIONS` first column), kept for parity with the TS surface.
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            LifecycleMutation::DoubleRevive => "double_revive",
            LifecycleMutation::LoseRecoverable => "lose_recoverable",
            LifecycleMutation::SuspensionBumpsEpoch => "suspension_bumps_epoch",
            LifecycleMutation::SpawnBeforeOrphanKill => "spawn_before_orphan_kill",
            LifecycleMutation::ReviveForbidden => "revive_forbidden",
            LifecycleMutation::OverAdmit => "over_admit",
            LifecycleMutation::CapacityGateReclamation => "capacity_gate_reclamation",
            LifecycleMutation::RelaunchTerminalWithoutSession => "relaunch_terminal_without_session",
        }
    }
}

fn seed_mutation_record(state: &ChaosState, task_id: &str, overrides: impl FnOnce(&mut TaskRecord)) {
    let mut record = TaskRecord {
        task_id: task_id.to_string(),
        name: Some(task_id.to_string()),
        parent_session_id: CHAOS_SESSION.to_string(),
        root_session_id: CHAOS_SESSION.to_string(),
        depth: 1,
        execution_mode: "in-process".to_string(),
        model: state.harness.model.clone(),
        status: TaskStatus::Running,
        residency_state: ResidencyState::PersistedOnly,
        created_at: "2027-01-01T00:00:00.000Z".to_string(),
        updated_at: "2027-01-01T00:00:00.000Z".to_string(),
        notify_on_terminal: false,
        notification: TaskNotification {
            run_epoch: 4,
            notified_epoch: -1,
            notification_failed_epoch: None,
            liveness_notified_epoch: None,
        },
        spawn_spec: Some(TaskSpawnSpec::V1(crate::state::SpawnSpecV1 {
            cwd: state.harness.store.state_dir().to_string_lossy().into_owned(),
            prompt: "mutation".to_string(),
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
    overrides(&mut record);
    let _ = state.harness.store.save(&record);
    super::observing_store::observe_store_write(&state.harness.store, &state.harness.observations, None, Some(&record));
    state.push_task_id(task_id.to_string());
}

fn managed_spec(state: &ChaosState, task_id: &str, prompt: &str) -> ManagedStartSpec {
    ManagedStartSpec {
        task_id: task_id.to_string(),
        cwd: state.harness.store.state_dir().to_string_lossy().into_owned(),
        state_dir: state.harness.store.state_dir().to_string_lossy().into_owned(),
        prompt: prompt.to_string(),
        depth: 1,
        parent_session_id: CHAOS_SESSION.to_string(),
        root_session_id: CHAOS_SESSION.to_string(),
        ..ManagedStartSpec::default()
    }
}

/// `runLifecycleMutationProbe`: runs a single deterministic scenario that deliberately violates
/// one lifecycle invariant, then reports which violations the checks caught.
pub fn run_lifecycle_mutation_probe(mutation: LifecycleMutation) -> LifecycleProbeReport {
    let state = make_state(0x0002_1bad, 4, 1, 8);
    let task_id = "st_0000bad0";
    match mutation {
        LifecycleMutation::DoubleRevive => {
            let started = start_probe_task(&state, "in-process", "double claim");
            let first = &state.harness.engines[0];
            let second = &state.harness.engines[1];
            let _ = state.harness.store.mutate(&started, |fresh| {
                let mut next = fresh.clone();
                next.host_pid = Some(second.host_pid);
                next
            });
            let _ = first;
            let record = state.harness.store.load(&started).ok().flatten().expect("mutation record missing");
            let handle = second
                .in_process_runner
                .start(&managed_spec(&state, &started, "double claim"))
                .expect("second runner start");
            let _ = second.manager.reattach(&record, handle);
            state.harness.observe_live_handles();
        }
        LifecycleMutation::LoseRecoverable => {
            seed_mutation_record(&state, task_id, |_| {});
            if let Some(record) = state.harness.store.load(task_id).ok().flatten() {
                let previous = record.clone();
                let mut next = record;
                next.status = TaskStatus::Lost;
                next.error_message = Some("mutant".to_string());
                let _ = state.harness.store.replace(&next);
                super::observing_store::observe_store_replace_with_write(
                    &state.harness.store,
                    &state.harness.observations,
                    &previous,
                    &next,
                );
            }
        }
        LifecycleMutation::SuspensionBumpsEpoch => {
            seed_mutation_record(&state, task_id, |record| {
                record.residency_state = ResidencyState::Resident;
            });
            if let Some(record) = state.harness.store.load(task_id).ok().flatten() {
                let previous = record.clone();
                let mut next = record;
                next.residency_state = ResidencyState::PersistedOnly;
                next.notification.run_epoch = 5;
                let _ = state.harness.store.replace(&next);
                super::observing_store::observe_store_replace_with_write(
                    &state.harness.store,
                    &state.harness.observations,
                    &previous,
                    &next,
                );
            }
        }
        LifecycleMutation::SpawnBeforeOrphanKill => {
            let pid = state.harness.processes.spawn();
            seed_mutation_record(&state, task_id, |record| {
                record.execution_mode = "process".to_string();
                record.residency_state = ResidencyState::Resident;
                record.pid = Some(pid);
            });
            let _ = state.harness.engines[0]
                .process_runner
                .start(&managed_spec(&state, task_id, "mutant"));
        }
        LifecycleMutation::ReviveForbidden => {
            seed_mutation_record(&state, task_id, |record| {
                record.status = TaskStatus::Cancelled;
                record.residency_state = ResidencyState::Disposed;
            });
            let _ = state.harness.store.mutate(task_id, |fresh| {
                let mut next = fresh.clone();
                next.status = TaskStatus::Running;
                next.residency_state = ResidencyState::Resident;
                next
            });
            super::observing_store::observe_store_write(
                &state.harness.store,
                &state.harness.observations,
                None,
                state.harness.store.load(task_id).ok().flatten().as_ref(),
            );
        }
        LifecycleMutation::OverAdmit => {
            seed_mutation_record(&state, "st_0000bad1", |record| {
                record.residency_state = ResidencyState::Resident;
            });
            seed_mutation_record(&state, "st_0000bad2", |record| {
                record.residency_state = ResidencyState::Resident;
            });
        }
        LifecycleMutation::CapacityGateReclamation => {
            let owner = &state.harness.engines[0];
            seed_mutation_record(&state, task_id, |record| {
                record.residency_state = ResidencyState::Resident;
                record.host_pid = Some(owner.host_pid);
            });
            // Mutant: route resident-orphan reclamation through ordinary admission. Because the
            // orphan itself consumes the only slot, admission rejects and the buggy reconcile
            // path returns without running reclaim_orphaned_resident.
            let admission = owner.lifecycle.admit_resident(CHAOS_SESSION);
            if !matches!(admission, Ok(crate::lifecycle::AdmissionResult::Rejected(_))) {
                panic!("capacity-gate mutation did not reach a full cap");
            }
            observe_reclamation_postcondition(&state, task_id, Some(owner));
        }
        LifecycleMutation::RelaunchTerminalWithoutSession => {
            seed_mutation_record(&state, task_id, |record| {
                record.status = TaskStatus::Completed;
                record.final_response = Some("done".to_string());
            });
            let _ = state.harness.engines[0]
                .in_process_runner
                .start(&managed_spec(&state, task_id, "mutant"));
        }
    }
    LifecycleProbeReport {
        action_counts: *state.harness.lifecycle_observations.action_counts.lock().unwrap_or_else(|error| error.into_inner()),
        violations: collect_invariant_violations(&state),
    }
}
