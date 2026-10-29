//! Goal status and completion predicates.

use crate::domain_types::{UlwLoopItem, UlwLoopPlan, UlwLoopSuccessCriterion};
use crate::paths::{ulw_loop_goals_relative_path, ulw_loop_ledger_relative_path, UlwLoopScope};

pub const ULW_LOOP_AGGREGATE_CODEX_OBJECTIVE: &str = "Complete the durable ulw-loop plan in .omo/ulw-loop/goals.json, including later accepted/appended stories, under the original brief constraints; use .omo/ulw-loop/ledger.jsonl as the audit trail.";

pub fn aggregate_codex_objective_for_scope(scope: Option<&UlwLoopScope>) -> String {
    format!(
        "Complete the durable ulw-loop plan in {}, including later accepted/appended stories, under the original brief constraints; use {} as the audit trail.",
        ulw_loop_goals_relative_path(scope),
        ulw_loop_ledger_relative_path(scope)
    )
}

pub fn codex_goal_mode(plan: &UlwLoopPlan) -> &str {
    plan.codex_goal_mode.as_deref().unwrap_or("per_story")
}

pub fn is_resolved_status(status: &str) -> bool {
    status == "complete"
}

pub fn is_member_resolved(goal: &UlwLoopItem, plan: &UlwLoopPlan) -> bool {
    is_resolved_status(&goal.status) || is_superseded_resolved(goal, plan)
}

fn is_superseded_resolved(goal: &UlwLoopItem, plan: &UlwLoopPlan) -> bool {
    if goal.steering_status.as_deref() != Some("superseded") {
        return false;
    }
    let replacements = goal.superseded_by.as_deref().unwrap_or(&[]);
    if replacements.is_empty() {
        return false;
    }
    replacements.iter().all(|id| {
        let replacement = plan.goals.iter().find(|candidate| candidate.id == *id);
        replacement.is_some_and(|r| is_resolved_status(&r.status))
    })
}

fn is_completion_blocking(goal: &UlwLoopItem, plan: &UlwLoopPlan) -> bool {
    if goal.steering_status.as_deref() == Some("superseded") {
        return !is_superseded_resolved(goal, plan);
    }
    if goal.steering_status.as_deref() == Some("blocked") {
        return true;
    }
    !is_resolved_status(&goal.status)
}

fn is_completion_blocking_for_final_candidate(
    candidate: &UlwLoopItem,
    final_candidate: &UlwLoopItem,
    plan: &UlwLoopPlan,
) -> bool {
    if candidate.id == final_candidate.id {
        return false;
    }
    if candidate.steering_status.as_deref() == Some("superseded") {
        let replacements = candidate.superseded_by.as_deref().unwrap_or(&[]);
        if replacements.is_empty() {
            return true;
        }
        return !replacements.iter().all(|id| {
            if id == &final_candidate.id {
                return true;
            }
            let replacement = plan.goals.iter().find(|goal| goal.id == *id);
            replacement.is_some_and(|r| is_resolved_status(&r.status))
        });
    }
    is_completion_blocking(candidate, plan)
}

pub fn is_ulw_loop_done(plan: &UlwLoopPlan) -> bool {
    if plan.aggregate_completion.as_ref().is_some_and(|c| c.status == "complete") {
        return true;
    }
    plan.goals.iter().all(|goal| !is_completion_blocking(goal, plan))
}

pub fn is_final_run_completion_candidate(plan: &UlwLoopPlan, goal: &UlwLoopItem) -> bool {
    is_completion_blocking(goal, plan)
        && plan.goals.iter().all(|candidate| {
            !is_completion_blocking_for_final_candidate(candidate, goal, plan)
        })
}

pub fn aggregate_codex_objective(plan: &UlwLoopPlan) -> String {
    plan.codex_objective.clone().unwrap_or_else(|| aggregate_codex_objective_for_scope(None))
}

pub fn expected_codex_objective(plan: &UlwLoopPlan, goal: &UlwLoopItem) -> String {
    if codex_goal_mode(plan) == "aggregate" {
        aggregate_codex_objective(plan)
    } else {
        goal.objective.clone()
    }
}

pub fn compatible_codex_objectives(plan: &UlwLoopPlan) -> Vec<String> {
    let mut out = vec![aggregate_codex_objective(plan)];
    if let Some(aliases) = &plan.codex_objective_aliases {
        out.extend(aliases.iter().cloned());
    }
    out
}

pub fn has_all_criteria_pass(goal: &UlwLoopItem) -> bool {
    !goal.success_criteria.is_empty()
        && goal.success_criteria.iter().all(|c| c.status == "pass")
}

pub fn is_essential_criterion(criterion: &UlwLoopSuccessCriterion) -> bool {
    criterion.essential.unwrap_or(true)
}

pub fn essential_criteria_of(goal: &UlwLoopItem) -> Vec<&UlwLoopSuccessCriterion> {
    let explicit: Vec<_> = goal.success_criteria.iter().filter(|c| is_essential_criterion(c)).collect();
    if !explicit.is_empty() {
        return explicit;
    }
    if let Some(happy) = goal.success_criteria.iter().find(|c| c.user_model == "happy") {
        vec![happy]
    } else {
        Vec::new()
    }
}

pub fn has_essential_criteria_pass(goal: &UlwLoopItem) -> bool {
    let criteria = essential_criteria_of(goal);
    !criteria.is_empty() && criteria.iter().all(|c| c.status == "pass")
}

pub fn first_unresolved_criterion(goal: &UlwLoopItem) -> Option<&UlwLoopSuccessCriterion> {
    goal.success_criteria.iter().find(|c| c.status != "pass")
}
