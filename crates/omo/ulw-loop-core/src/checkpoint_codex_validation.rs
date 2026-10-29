//! Codex goal validation during checkpoint.

use std::path::Path;
use serde_json::Value;

use crate::codex_goal_snapshot::{
    format_codex_goal_reconciliation, read_codex_goal_snapshot_input, reconcile_codex_goal_snapshot,
    ReconcileCodexGoalOptions,
};
use crate::domain_types::{UlwLoopItem, UlwLoopPlan};
use crate::driver_objective_ack::acknowledged_driver_objectives;
use crate::goal_status::{codex_goal_mode, compatible_codex_objectives, expected_codex_objective};
use crate::paths::UlwLoopScope;
use crate::runtime::UlwLoopError;

pub struct CheckpointCodexGoalValidation {
    pub raw: Option<Value>,
    pub next_actions: Vec<String>,
    pub warnings: Vec<String>,
    pub unacknowledged_objective: Option<String>,
}

pub fn validate_checkpoint_codex_goal(
    repo_root: impl AsRef<Path>,
    plan: &UlwLoopPlan,
    goal: &UlwLoopItem,
    raw_str: Option<&str>,
    evidence: &str,
    _scope: Option<&UlwLoopScope>,
) -> Result<CheckpointCodexGoalValidation, UlwLoopError> {
    let snapshot = read_codex_goal_snapshot_input(raw_str, repo_root)?;
    let expected = expected_codex_objective(plan, goal);
    let accepted = compatible_codex_objectives(plan);
    let acknowledged = acknowledged_driver_objectives(plan);

    let reconciliation = reconcile_codex_goal_snapshot(
        snapshot.as_ref(),
        &ReconcileCodexGoalOptions {
            expected_objective: &expected,
            accepted_objectives: Some(&accepted),
            acknowledged_objectives: Some(&acknowledged),
        },
    );

    if !reconciliation.ok {
        return Err(UlwLoopError::new(
            format_codex_goal_reconciliation(&reconciliation),
            "CODEX_GOAL_SNAPSHOT_ERROR",
        ));
    }

    Ok(CheckpointCodexGoalValidation {
        raw: snapshot.map(|s| s.raw),
        next_actions: reconciliation.next_actions,
        warnings: reconciliation.warnings,
        unacknowledged_objective: reconciliation.unacknowledged_objective,
    })
}
