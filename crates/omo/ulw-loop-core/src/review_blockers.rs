//! Final review blocker recording.

use std::path::Path;
use serde::{Deserialize, Serialize};

use crate::codex_goal_snapshot::{
    format_codex_goal_reconciliation, read_codex_goal_snapshot_input, reconcile_codex_goal_snapshot,
    ReconcileCodexGoalOptions,
};
use crate::command_types::RecordFinalReviewBlockersOptions;
use crate::domain_types::{UlwLoopItem, UlwLoopLedgerEntry, UlwLoopPlan};
use crate::driver_objective_ack::{acknowledge_driver_objective, acknowledged_driver_objectives};
use crate::goal_status::{compatible_codex_objectives, expected_codex_objective, is_final_run_completion_candidate};
use crate::paths::UlwLoopScope;
use crate::plan_commit::{commit, CommitMutation};
use crate::plan_crud::clear_goal_blocker_fields;
use crate::plan_goal_factory::{seed_default_success_criteria, GoalSeedOptions};
use crate::plan_io::{read_ulw_loop_plan, with_ulw_loop_mutation_lock};
use crate::runtime::{iso, UlwLoopError};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordFinalReviewBlockersResult {
    pub plan: UlwLoopPlan,
    pub blocked_goal: UlwLoopItem,
    pub new_goal: UlwLoopItem,
    pub ledger_entries: Vec<UlwLoopLedgerEntry>,
    pub next_actions: Vec<String>,
    pub warnings: Vec<String>,
}

fn next_goal_id(plan: &UlwLoopPlan) -> String {
    let mut max = 0u32;
    for g in &plan.goals {
        if let Some(rest) = g.id.strip_prefix('G') {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(num) = digits.parse::<u32>() {
                max = max.max(num);
            }
        }
    }
    format!("G{:03}", max + 1)
}

fn append_blocker_goal(
    plan: &mut UlwLoopPlan,
    args: &RecordFinalReviewBlockersOptions,
    now: &str,
) -> UlwLoopItem {
    let index = plan.goals.len();
    let id = next_goal_id(plan);
    let seed_opts = GoalSeedOptions {
        goal_id: Some(id.clone()),
        ..Default::default()
    };
    let criteria = seed_default_success_criteria(index, &args.objective, Some(&seed_opts));
    let goal = UlwLoopItem {
        id,
        title: args.title.clone(),
        objective: args.objective.clone(),
        status: "pending".to_string(),
        success_criteria: criteria,
        attempt: 0,
        created_at: now.to_string(),
        updated_at: now.to_string(),
        started_at: None,
        completed_at: None,
        failed_at: None,
        review_blocked_at: None,
        evidence: None,
        failure_reason: None,
        steering_status: None,
        superseded_by: None,
        supersedes: None,
        blocked_reason: None,
        blocker_signature: None,
        blocker_occurrence_count: None,
        required_external_decision: None,
        non_retriable: None,
        steering_evidence: None,
        steering_rationale: None,
    };
    plan.goals.push(goal.clone());
    goal
}

pub fn record_final_review_blockers(
    repo_root: impl AsRef<Path>,
    args: &RecordFinalReviewBlockersOptions,
    scope: Option<&UlwLoopScope>,
) -> Result<RecordFinalReviewBlockersResult, UlwLoopError> {
    with_ulw_loop_mutation_lock(&repo_root, scope, None, || {
        let mut plan = read_ulw_loop_plan(&repo_root, scope)?;
        let goal_idx = plan
            .goals
            .iter()
            .position(|g| g.id == args.goal_id)
            .ok_or_else(|| {
                UlwLoopError::new(
                    format!("Unknown ulw-loop id: {}", args.goal_id),
                    "ulw_loop_goal_not_found",
                )
            })?;

        let goal = &plan.goals[goal_idx];
        if goal.status != "in_progress" {
            return Err(UlwLoopError::new(
                format!("{} is {}.", goal.id, goal.status),
                "ulw_loop_goal_not_in_progress",
            ));
        }
        if !is_final_run_completion_candidate(&plan, goal) {
            return Err(UlwLoopError::new(
                format!("{} is not final.", goal.id),
                "ulw_loop_not_final_story",
            ));
        }

        let snapshot = read_codex_goal_snapshot_input(args.codex_goal_json.as_deref(), &repo_root)?;
        let accepted_objs = compatible_codex_objectives(&plan);
        let acked_objs = acknowledged_driver_objectives(&plan);
        let exp_obj = expected_codex_objective(&plan, goal);

        let reconciliation = reconcile_codex_goal_snapshot(
            snapshot.as_ref(),
            &ReconcileCodexGoalOptions {
                expected_objective: &exp_obj,
                accepted_objectives: Some(&accepted_objs),
                acknowledged_objectives: Some(&acked_objs),
            },
        );

        if !reconciliation.ok {
            return Err(UlwLoopError::new(
                format_codex_goal_reconciliation(&reconciliation),
                "CODEX_GOAL_SNAPSHOT_ERROR",
            ));
        }

        acknowledge_driver_objective(&mut plan, reconciliation.unacknowledged_objective.as_deref());

        let now = iso();
        let target_goal = &mut plan.goals[goal_idx];
        clear_goal_blocker_fields(target_goal);
        target_goal.status = "review_blocked".to_string();
        target_goal.review_blocked_at = Some(now.clone());
        target_goal.evidence = Some(args.evidence.clone());
        target_goal.updated_at = now.clone();

        if plan.active_goal_id.as_deref() == Some(&args.goal_id) {
            plan.active_goal_id = None;
        }

        let blocked_goal = target_goal.clone();
        let new_goal = append_blocker_goal(&mut plan, args, &now);
        plan.updated_at = now.clone();

        let codex_goal = snapshot.as_ref().map(|s| s.raw.clone());
        let blocked_entry = UlwLoopLedgerEntry {
            at: now.clone(),
            kind: "goal_review_blocked".to_string(),
            goal_id: Some(blocked_goal.id.clone()),
            status: Some(blocked_goal.status.clone()),
            evidence: Some(args.evidence.clone()),
            codex_goal: codex_goal.clone(),
            ..Default::default()
        };
        let added_entry = UlwLoopLedgerEntry {
            at: now.clone(),
            kind: "goal_added".to_string(),
            goal_id: Some(new_goal.id.clone()),
            status: Some(new_goal.status.clone()),
            evidence: Some(args.evidence.clone()),
            message: Some(new_goal.title.clone()),
            ..Default::default()
        };
        let summary_entry = UlwLoopLedgerEntry {
            at: now,
            kind: "blocker_recorded".to_string(),
            goal_id: Some(blocked_goal.id.clone()),
            status: Some(blocked_goal.status.clone()),
            evidence: Some(args.evidence.clone()),
            codex_goal,
            message: Some(format!("Review blockers recorded; appended {}.", new_goal.id)),
            ..Default::default()
        };

        let mut entries = vec![blocked_entry, added_entry, summary_entry];
        commit(
            &repo_root,
            scope,
            CommitMutation {
                plan: &mut plan,
                entries: &mut entries,
            },
        )?;

        Ok(RecordFinalReviewBlockersResult {
            plan,
            blocked_goal,
            new_goal,
            ledger_entries: entries,
            next_actions: reconciliation.next_actions,
            warnings: reconciliation.warnings,
        })
    })
}
