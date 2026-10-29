//! Port of `__adversarial__/chaos-lifecycle.test.ts`.
#![cfg(test)]

use super::chaos_drive::{LifecycleMutation, run_lifecycle_mutation_probe, run_lifecycle_probe};
use super::chaos_engine::ActionCounts;

const LIFECYCLE_INVARIANTS: [u8; 8] = [6, 7, 8, 9, 10, 11, 12, 13];

#[test]
fn given_every_new_lifecycle_action_when_deterministic_interleavings_run_then_all_lifecycle_invariants_hold() {
    let report = run_lifecycle_probe();
    assert_eq!(
        report.action_counts,
        ActionCounts {
            suspend_session: 1,
            resume_session: 1,
            crash_mid_suspend: 1,
            sibling_reconcile_race: 1,
            mass_revive_at_cap: 1,
        }
    );
    assert!(report.violations.is_empty());
    assert_eq!(LIFECYCLE_INVARIANTS.len(), 8);
}

#[test]
fn given_the_double_revive_regression_when_the_invariant_witness_runs_then_invariant_6_fails() {
    let report = run_lifecycle_mutation_probe(LifecycleMutation::DoubleRevive);
    assert!(report.violations.iter().any(|violation| violation.invariant == 6));
}

#[test]
fn given_the_lose_recoverable_regression_when_the_invariant_witness_runs_then_invariant_7_fails() {
    let report = run_lifecycle_mutation_probe(LifecycleMutation::LoseRecoverable);
    assert!(report.violations.iter().any(|violation| violation.invariant == 7));
}

#[test]
fn given_the_suspension_bumps_epoch_regression_when_the_invariant_witness_runs_then_invariant_8_fails() {
    let report = run_lifecycle_mutation_probe(LifecycleMutation::SuspensionBumpsEpoch);
    assert!(report.violations.iter().any(|violation| violation.invariant == 8));
}

#[test]
fn given_the_spawn_before_orphan_kill_regression_when_the_invariant_witness_runs_then_invariant_9_fails() {
    let report = run_lifecycle_mutation_probe(LifecycleMutation::SpawnBeforeOrphanKill);
    assert!(report.violations.iter().any(|violation| violation.invariant == 9));
}

#[test]
fn given_the_revive_forbidden_regression_when_the_invariant_witness_runs_then_invariant_10_fails() {
    let report = run_lifecycle_mutation_probe(LifecycleMutation::ReviveForbidden);
    assert!(report.violations.iter().any(|violation| violation.invariant == 10));
}

#[test]
fn given_the_over_admit_regression_when_the_invariant_witness_runs_then_invariant_11_fails() {
    let report = run_lifecycle_mutation_probe(LifecycleMutation::OverAdmit);
    assert!(report.violations.iter().any(|violation| violation.invariant == 11));
}

#[test]
fn given_the_capacity_gate_reclamation_regression_when_the_invariant_witness_runs_then_invariant_12_fails() {
    let report = run_lifecycle_mutation_probe(LifecycleMutation::CapacityGateReclamation);
    assert!(report.violations.iter().any(|violation| violation.invariant == 12));
    let detail = report
        .violations
        .iter()
        .find(|violation| violation.invariant == 12)
        .map(|violation| violation.detail.as_str());
    assert!(detail.is_some_and(|detail| detail.contains("remained ownerless after capacity-gated reconcile")));
}

#[test]
fn given_the_relaunch_terminal_without_session_regression_when_the_invariant_witness_runs_then_invariant_13_fails() {
    let report = run_lifecycle_mutation_probe(LifecycleMutation::RelaunchTerminalWithoutSession);
    assert!(report.violations.iter().any(|violation| violation.invariant == 13));
}
