//! Next actions recommendations returned by status().

use crate::domain_types::{UlwLoopItem, UlwLoopPlan};
use crate::goal_status::{has_all_criteria_pass, is_final_run_completion_candidate};
use crate::plan_missing_recovery::create_goals_action;
use crate::surface::UlwLoopToolkitSurface;

pub fn status_next_actions(plan: &UlwLoopPlan, surface: Option<UlwLoopToolkitSurface>) -> Vec<String> {
    let s = surface.unwrap_or(UlwLoopToolkitSurface::LazyCodex);
    let mut actions = Vec::new();
    let active = plan.active_goal_id.as_deref().and_then(|id| plan.goals.iter().find(|g| g.id == id));

    if plan.goals.is_empty() {
        actions.push(format!("No goals yet: bootstrap with `{}`.", create_goals_action(&s)));
    } else if let Some(active_goal) = active {
        actions.extend(active_goal_actions(plan, active_goal, s));
    }

    for goal in plan.goals.iter().filter(|g| g.status == "review_blocked") {
        let msg = if s == UlwLoopToolkitSurface::OmoSenpi {
            format!(
                "{} is review_blocked: capture the reviewer verdict with `mhc ulw-loop record-review-blockers --goal-id {} --title \"<title>\" --objective \"<objective>\" --evidence \"<verdict>\" --codex-goal-json '<get_goal json>'`.",
                goal.id, goal.id
            )
        } else {
            format!(
                "{} is review_blocked: capture the reviewer verdict with `ulw-loop record-review-blockers --goal-id {} --title \"<title>\" --objective \"<objective>\" --evidence \"<verdict>\" --codex-goal-json '<get_goal json>'`.",
                goal.id, goal.id
            )
        };
        actions.push(msg);
    }

    if plan.evidence_layout_version != Some(2) || active.is_none() {
        actions.push("plan is evidence-layout v1; artifacts go under .omo/evidence/".to_string());
    }

    actions
}

fn active_goal_actions(plan: &UlwLoopPlan, active: &UlwLoopItem, surface: UlwLoopToolkitSurface) -> Vec<String> {
    let unresolved: Vec<_> = active.success_criteria.iter().filter(|c| c.status != "pass").collect();
    if !unresolved.is_empty() {
        let unresolved_ids: Vec<_> = unresolved.iter().map(|c| c.id.as_str()).collect();
        let msg = if surface == UlwLoopToolkitSurface::OmoSenpi {
            format!(
                "{} has {} unresolved criterion(s) ({}): record proof with `mhc ulw-loop record-evidence --goal-id {} --criterion-id <id> --status pass --evidence \"<observable proof>\"`.",
                active.id, unresolved.len(), unresolved_ids.join(", "), active.id
            )
        } else {
            format!(
                "{} has {} unresolved criterion(s) ({}): record proof with `ulw-loop record-evidence --goal-id {} --criterion-id <id> --status pass --evidence \"<observable proof>\"`.",
                active.id, unresolved.len(), unresolved_ids.join(", "), active.id
            )
        };
        return vec![msg];
    }

    if has_all_criteria_pass(active) && is_final_run_completion_candidate(plan, active) {
        let msg = if surface == UlwLoopToolkitSurface::OmoSenpi {
            format!(
                "{} passes every criterion and is the final story: complete the driver goal, then run `mhc ulw-loop checkpoint --goal-id {} --print-template --json` to build the final quality gate. Use the currentAttemptDir shown by `mhc ulw-loop status --json` for all quality-gate artifacts.",
                active.id, active.id
            )
        } else {
            format!(
                "{} passes every criterion and is the final story: update_goal complete, then checkpoint --print-template to build the final quality gate. Use status --json's currentAttemptDir for all quality-gate artifacts.",
                active.id
            )
        };
        return vec![msg];
    }

    let msg = if surface == UlwLoopToolkitSurface::OmoSenpi {
        format!(
            "{} passes every criterion: close it with `mhc ulw-loop checkpoint --goal-id {} --status complete --evidence \"<proof>\"`. Put quality-gate artifacts under the currentAttemptDir shown by `mhc ulw-loop status --json`.",
            active.id, active.id
        )
    } else {
        format!(
            "{} passes every criterion: close it with `ulw-loop checkpoint --goal-id {} --status complete --evidence \"<proof>\" --codex-goal-json '<get_goal json>'`. Put quality-gate artifacts under the currentAttemptDir shown by status --json.",
            active.id, active.id
        )
    };
    vec![msg]
}
