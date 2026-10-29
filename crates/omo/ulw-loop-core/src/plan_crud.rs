//! Plan lifecycle operations: create, add-goal, start-next, and summarize.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::command_types::{AddGoalOptions, CreateUlwLoopOptions, StartNextOptions};
use crate::domain_types::{UlwLoopItem, UlwLoopLedgerEntry, UlwLoopPlan, UlwLoopSuccessCriterion};
use crate::goal_status::{aggregate_codex_objective_for_scope, is_ulw_loop_done};
use crate::paths::{
    ulw_loop_brief_relative_path, ulw_loop_goals_relative_path, ulw_loop_ledger_relative_path,
    UlwLoopScope,
};
use crate::plan_commit::{commit, CommitMutation};
use crate::plan_goal_factory::{append_goal_to_plan, derive_goal_candidates, make_goal, GoalSeedOptions};
use crate::plan_io::{plan_exists, read_ulw_loop_plan, with_ulw_loop_mutation_lock};
use crate::runtime::{iso, UlwLoopError};
use crate::surface::UlwLoopToolkitSurface;
use crate::validation_batch::parse_validation_batches;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UlwLoopPlanSummaryCriteria {
    pub total: usize,
    pub pass: usize,
    pub pending: usize,
    pub fail: usize,
    pub blocked: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UlwLoopPlanSummary {
    pub total: usize,
    pub pending: usize,
    pub in_progress: usize,
    pub complete: usize,
    pub failed: usize,
    pub blocked: usize,
    pub review_blocked: usize,
    pub needs_user_decision: usize,
    pub superseded: usize,
    pub criteria: UlwLoopPlanSummaryCriteria,
}

fn is_schedule_eligible(goal: &UlwLoopItem) -> bool {
    goal.steering_status.as_deref() != Some("superseded")
        && goal.steering_status.as_deref() != Some("blocked")
}

pub fn clear_goal_blocker_fields(goal: &mut UlwLoopItem) {
    goal.blocked_reason = None;
    goal.blocker_signature = None;
    goal.blocker_occurrence_count = None;
    goal.required_external_decision = None;
    goal.non_retriable = None;
    goal.failed_at = None;
    goal.failure_reason = None;
}

pub fn create_ulw_loop_plan(
    repo_root: impl AsRef<Path>,
    args: &CreateUlwLoopOptions,
    scope: Option<&UlwLoopScope>,
    surface: Option<UlwLoopToolkitSurface>,
) -> Result<UlwLoopPlan, UlwLoopError> {
    let s = surface.unwrap_or(UlwLoopToolkitSurface::LazyCodex);
    with_ulw_loop_mutation_lock(&repo_root, scope, None, || {
        let mut existing: Option<UlwLoopPlan> = None;
        if plan_exists(&repo_root, scope) {
            match read_ulw_loop_plan(&repo_root, scope) {
                Ok(p) => existing = Some(p),
                Err(err) => {
                    if !args.force.unwrap_or(false) {
                        return Err(err);
                    }
                }
            }
        }

        if !args.force.unwrap_or(false) {
            if let Some(p) = existing.as_ref() {
                if is_ulw_loop_done(p) {
                    return Err(completed_plan_exists_error(scope, s));
                }
                return Err(UlwLoopError::new(
                    format!(
                        "Refusing to overwrite existing {}; pass --force to recreate it.",
                        ulw_loop_goals_relative_path(scope)
                    ),
                    "ULW_LOOP_PLAN_EXISTS",
                ));
            }
        }

        let now = iso();
        let candidates = derive_goal_candidates(&args.brief);
        let mut goals = Vec::new();
        for (index, (title, objective)) in candidates.into_iter().enumerate() {
            let seed_opts = GoalSeedOptions {
                surface: Some(s),
                ..Default::default()
            };
            goals.push(make_goal(&title, &objective, index, &now, Some(&seed_opts))?);
        }

        let brief_content = if args.brief.ends_with('\n') {
            args.brief.clone()
        } else {
            format!("{}\n", args.brief)
        };

        let mut plan = UlwLoopPlan {
            version: 1,
            revision: existing.as_ref().and_then(|p| p.revision),
            ledger_reset_revision: existing.as_ref().and_then(|p| p.revision).map(|r| r + 1),
            brief: Some(brief_content),
            evidence_layout_version: Some(2),
            created_at: now.clone(),
            updated_at: now.clone(),
            brief_path: ulw_loop_brief_relative_path(scope),
            goals_path: ulw_loop_goals_relative_path(scope),
            ledger_path: ulw_loop_ledger_relative_path(scope),
            codex_goal_mode: Some(args.codex_goal_mode.clone().unwrap_or_else(|| "aggregate".into())),
            codex_objective: None,
            codex_objective_aliases: None,
            acknowledged_driver_objectives: None,
            aggregate_completion: None,
            active_goal_id: None,
            validation_batches: None,
            goals,
        };

        let validation_batches = parse_validation_batches(args.validation_batches_json.as_deref(), &plan.goals)?;
        if let Some(vb) = validation_batches {
            plan.validation_batches = Some(vb);
        }

        if plan.codex_goal_mode.as_deref() == Some("aggregate") {
            plan.codex_objective = Some(aggregate_codex_objective_for_scope(scope));
        }

        let mut entries = vec![UlwLoopLedgerEntry {
            at: now,
            kind: "plan_created".to_string(),
            message: Some(format!("{} goal(s) created", plan.goals.len())),
            ..Default::default()
        }];

        commit(
            &repo_root,
            scope,
            CommitMutation {
                plan: &mut plan,
                entries: &mut entries,
            },
        )?;

        Ok(plan)
    })
}

fn completed_plan_exists_error(scope: Option<&UlwLoopScope>, surface: UlwLoopToolkitSurface) -> UlwLoopError {
    let msg = if surface == UlwLoopToolkitSurface::OmoSenpi {
        format!(
            "Existing ulw-loop aggregate is already complete at {}. Start a new run under a fresh session id (a new session) or run `mhc ulw-loop create-goals --brief \"<brief>\" --force --json` to recreate.",
            ulw_loop_goals_relative_path(scope)
        )
    } else {
        format!(
            "Existing ulw-loop aggregate is already complete at {}. Start a new run with `ulw-loop create-goals --session-id <new-id> ...` to isolate fresh state. Use --force only when you intentionally want to overwrite the completed evidence.",
            ulw_loop_goals_relative_path(scope)
        )
    };
    UlwLoopError::new(msg, "ULW_LOOP_PLAN_EXISTS_COMPLETE")
}

pub fn add_ulw_loop_goal(
    repo_root: impl AsRef<Path>,
    args: &crate::command_types::AddGoalOptions,
    scope: Option<&UlwLoopScope>,
    surface: Option<UlwLoopToolkitSurface>,
) -> Result<AddGoalResult, UlwLoopError> {
    let s = surface.unwrap_or(UlwLoopToolkitSurface::LazyCodex);
    with_ulw_loop_mutation_lock(&repo_root, scope, None, || {
        let mut plan = read_ulw_loop_plan(&repo_root, scope)?;
        let now = iso();
        let seed_opts = GoalSeedOptions {
            surface: Some(s),
            success_criteria: args.success_criteria.as_ref().map(|criteria| {
                criteria
                    .iter()
                    .map(|criterion| {
                        let mut value = serde_json::json!({
                            "scenario": criterion.scenario,
                            "expectedEvidence": criterion.expected_evidence,
                        });
                        if let Some(user_model) = criterion.user_model.as_deref() {
                            value["userModel"] = serde_json::json!(user_model);
                        }
                        if let Some(essential) = criterion.essential {
                            value["essential"] = serde_json::json!(essential);
                        }
                        value
                    })
                    .collect()
            }),
            ..Default::default()
        };
        let goal = append_goal_to_plan(&mut plan, &args.title, &args.objective, &now, Some(&seed_opts))?;
        let mut entries = vec![UlwLoopLedgerEntry {
            at: now,
            kind: "goal_added".to_string(),
            goal_id: Some(goal.id.clone()),
            status: Some(goal.status.clone()),
            message: Some(goal.title.clone()),
            ..Default::default()
        }];
        commit(
            &repo_root,
            scope,
            CommitMutation {
                plan: &mut plan,
                entries: &mut entries,
            },
        )?;
        Ok(AddGoalResult { plan, goal })
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StartNextResult {
    Started {
        plan: UlwLoopPlan,
        goal: UlwLoopItem,
        resumed: bool,
    },
    Done {
        plan: UlwLoopPlan,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AddGoalResult {
    pub plan: UlwLoopPlan,
    pub goal: UlwLoopItem,
}

pub fn start_next_ulw_loop(
    repo_root: impl AsRef<Path>,
    args: Option<&StartNextOptions>,
    scope: Option<&UlwLoopScope>,
) -> Result<StartNextResult, UlwLoopError> {
    with_ulw_loop_mutation_lock(&repo_root, scope, None, || {
        let mut plan = read_ulw_loop_plan(&repo_root, scope)?;
        let now = iso();
        if plan.aggregate_completion.as_ref().is_some_and(|c| c.status == "complete") {
            return Ok(StartNextResult::Done { plan });
        }

        if let Some(existing) = plan.goals.iter().find(|g| g.status == "in_progress" && is_schedule_eligible(g)) {
            let goal = existing.clone();
            return Ok(StartNextResult::Started {
                plan,
                goal,
                resumed: true,
            });
        }

        let mut entries = Vec::new();
        let mut target_goal_idx = plan.goals.iter().position(|g| g.status == "pending" && is_schedule_eligible(g));

        if target_goal_idx.is_none() && args.and_then(|a| a.retry_failed).unwrap_or(false) {
            target_goal_idx = plan.goals.iter().position(|g| g.status == "failed" && !g.non_retriable.unwrap_or(false) && is_schedule_eligible(g));
            if let Some(idx) = target_goal_idx {
                let g = &plan.goals[idx];
                entries.push(UlwLoopLedgerEntry {
                    at: now.clone(),
                    kind: "goal_retried".to_string(),
                    goal_id: Some(g.id.clone()),
                    status: Some("pending".to_string()),
                    message: g.failure_reason.clone(),
                    ..Default::default()
                });
            }
        }

        let Some(idx) = target_goal_idx else {
            return Ok(StartNextResult::Done { plan });
        };

        let next = &mut plan.goals[idx];
        next.status = "in_progress".to_string();
        next.attempt += 1;
        next.started_at = Some(now.clone());
        clear_goal_blocker_fields(next);
        next.updated_at = now.clone();

        let goal_id = next.id.clone();
        let attempt = next.attempt;
        let goal_status = next.status.clone();
        let cloned_next = next.clone();

        plan.active_goal_id = Some(goal_id.clone());
        plan.updated_at = now.clone();

        entries.push(UlwLoopLedgerEntry {
            at: now,
            kind: "goal_started".to_string(),
            goal_id: Some(goal_id),
            status: Some(goal_status),
            message: Some(format!("Attempt {attempt}")),
            ..Default::default()
        });

        commit(
            &repo_root,
            scope,
            CommitMutation {
                plan: &mut plan,
                entries: &mut entries,
            },
        )?;

        Ok(StartNextResult::Started {
            plan,
            goal: cloned_next,
            resumed: false,
        })
    })
}

pub fn summarize_ulw_loop_plan(plan: &UlwLoopPlan) -> UlwLoopPlanSummary {
    let count_status = |status: &str| -> usize {
        plan.goals.iter().filter(|g| g.status == status).count()
    };
    let count_criteria = |status: &str| -> usize {
        plan.goals
            .iter()
            .map(|g| g.success_criteria.iter().filter(|c| c.status == status).count())
            .sum()
    };

    UlwLoopPlanSummary {
        total: plan.goals.len(),
        pending: count_status("pending"),
        in_progress: count_status("in_progress"),
        complete: count_status("complete"),
        failed: count_status("failed"),
        blocked: count_status("blocked"),
        review_blocked: count_status("review_blocked"),
        needs_user_decision: count_status("needs_user_decision"),
        superseded: plan
            .goals
            .iter()
            .filter(|g| g.steering_status.as_deref() == Some("superseded"))
            .count(),
        criteria: UlwLoopPlanSummaryCriteria {
            total: plan.goals.iter().map(|g| g.success_criteria.len()).sum(),
            pass: count_criteria("pass"),
            pending: count_criteria("pending"),
            fail: count_criteria("fail"),
            blocked: count_criteria("blocked"),
        },
    }
}
