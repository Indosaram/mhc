//! Evidence recording and criteria pass assertion.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::domain_types::{UlwLoopItem, UlwLoopLedgerEntry, UlwLoopPlan, UlwLoopSuccessCriterion};
use crate::evidence_artifacts::resolve_evidence_artifacts;
use crate::goal_status::{essential_criteria_of, has_all_criteria_pass, has_essential_criteria_pass};
use crate::paths::UlwLoopScope;
use crate::plan_commit::{commit, CommitMutation};
use crate::plan_io::{read_ulw_loop_plan, with_ulw_loop_mutation_lock};
use crate::runtime::{iso, UlwLoopError};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordEvidenceResult {
    pub plan: UlwLoopPlan,
    pub goal: UlwLoopItem,
    pub criterion: UlwLoopSuccessCriterion,
    #[serde(rename = "ledgerEntry")]
    pub ledger_entry: UlwLoopLedgerEntry,
}

pub type RecordEvidenceOptions = RecordEvidenceArgs;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordEvidenceArgs {
    pub goal_id: String,
    pub criterion_id: String,
    pub status: String,
    pub evidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CriteriaSummary {
    pub total_criteria: usize,
    pub pass_count: usize,
    pub pending_count: usize,
    pub fail_count: usize,
    pub blocked_count: usize,
    pub goals_with_unresolved_criteria: Vec<String>,
}

pub fn criteria_summary(plan: &UlwLoopPlan) -> CriteriaSummary {
    let mut summary = CriteriaSummary {
        total_criteria: 0,
        pass_count: 0,
        pending_count: 0,
        fail_count: 0,
        blocked_count: 0,
        goals_with_unresolved_criteria: Vec::new(),
    };
    for goal in &plan.goals {
        let mut unresolved = false;
        for criterion in &goal.success_criteria {
            summary.total_criteria += 1;
            match criterion.status.as_str() {
                "pass" => summary.pass_count += 1,
                "pending" => { summary.pending_count += 1; unresolved = true; }
                "fail" => { summary.fail_count += 1; unresolved = true; }
                "blocked" => { summary.blocked_count += 1; unresolved = true; }
                _ => unresolved = true,
            }
        }
        if unresolved {
            summary.goals_with_unresolved_criteria.push(goal.id.clone());
        }
    }
    summary
}

fn ledger_kind(status: &str) -> Result<&'static str, UlwLoopError> {
    match status {
        "pass" => Ok("evidence_captured"),
        "fail" => Ok("criterion_failed"),
        "blocked" => Ok("criterion_blocked"),
        _ => {
            let mut details = BTreeMap::new();
            details.insert("status".into(), serde_json::json!(status));
            Err(UlwLoopError::with_details(
                "Invalid criterion status.",
                "ULW_LOOP_CRITERION_STATUS_INVALID",
                details,
            ))
        }
    }
}

pub fn record_evidence(
    repo_root: impl AsRef<Path>,
    args: &RecordEvidenceArgs,
    scope: Option<&UlwLoopScope>,
) -> Result<RecordEvidenceResult, UlwLoopError> {
    with_ulw_loop_mutation_lock(&repo_root, scope, None, || {
        let mut plan = read_ulw_loop_plan(&repo_root, scope)?;
        let goal_idx = plan
            .goals
            .iter()
            .position(|g| g.id == args.goal_id)
            .ok_or_else(|| {
                let mut details = BTreeMap::new();
                details.insert("goalId".into(), serde_json::json!(args.goal_id));
                UlwLoopError::with_details(
                    format!("UlwLoop goal not found: {}.", args.goal_id),
                    "ULW_LOOP_GOAL_NOT_FOUND",
                    details,
                )
            })?;

        let crit_idx = plan.goals[goal_idx]
            .success_criteria
            .iter()
            .position(|c| c.id == args.criterion_id)
            .ok_or_else(|| {
                let mut details = BTreeMap::new();
                details.insert("goalId".into(), serde_json::json!(args.goal_id));
                details.insert("criterionId".into(), serde_json::json!(args.criterion_id));
                UlwLoopError::with_details(
                    format!("Success criterion not found: {}.", args.criterion_id),
                    "ULW_LOOP_CRITERION_NOT_FOUND",
                    details,
                )
            })?;

        let trimmed_evidence = args.evidence.trim();
        if trimmed_evidence.is_empty() {
            return Err(UlwLoopError::new(
                "Evidence must be a non-empty string.",
                "ULW_LOOP_EVIDENCE_REQUIRED",
            ));
        }

        let artifacts = resolve_evidence_artifacts(&repo_root, args.artifacts.as_deref())?;
        let kind = ledger_kind(&args.status)?;
        let captured_at = iso();

        let prev_status = plan.goals[goal_idx].success_criteria[crit_idx].status.clone();
        {
            let criterion = &mut plan.goals[goal_idx].success_criteria[crit_idx];
            criterion.status = args.status.clone();
            criterion.captured_evidence = Some(trimmed_evidence.to_string());
            criterion.captured_at = Some(captured_at.clone());
            if let Some(notes) = &args.notes {
                criterion.notes = Some(notes.clone());
            }
            criterion.artifacts = artifacts.clone();
        }

        let goal = &mut plan.goals[goal_idx];
        goal.updated_at = captured_at.clone();
        plan.updated_at = captured_at.clone();

        let cloned_crit = goal.success_criteria[crit_idx].clone();
        let cloned_goal = goal.clone();

        let ledger_entry = UlwLoopLedgerEntry {
            at: captured_at.clone(),
            kind: kind.to_string(),
            goal_id: Some(cloned_goal.id.clone()),
            criterion_id: Some(cloned_crit.id.clone()),
            criterion_status: Some(args.status.clone()),
            evidence: Some(trimmed_evidence.to_string()),
            captured_evidence: Some(trimmed_evidence.to_string()),
            artifacts,
            before: Some(serde_json::json!({ "status": prev_status })),
            after: Some(serde_json::json!({
                "goalId": cloned_goal.id,
                "criterionId": cloned_crit.id,
                "status": args.status,
                "evidence": trimmed_evidence,
                "capturedAt": captured_at,
                "prevStatus": prev_status,
            })),
            ..Default::default()
        };

        let mut entries = vec![ledger_entry.clone()];
        commit(
            &repo_root,
            scope,
            CommitMutation {
                plan: &mut plan,
                entries: &mut entries,
            },
        )?;

        Ok(RecordEvidenceResult { plan, goal: cloned_goal, criterion: cloned_crit, ledger_entry })
    })
}

pub fn mark_criteria_pending_reset_for_goal(
    repo_root: impl AsRef<Path>,
    goal_id: &str,
    scope: Option<&UlwLoopScope>,
) -> Result<(UlwLoopPlan, usize), UlwLoopError> {
    with_ulw_loop_mutation_lock(&repo_root, scope, None, || {
        let mut plan = read_ulw_loop_plan(&repo_root, scope)?;
        let goal = plan
            .goals
            .iter_mut()
            .find(|g| g.id == goal_id)
            .ok_or_else(|| {
                let mut details = BTreeMap::new();
                details.insert("goalId".into(), serde_json::json!(goal_id));
                UlwLoopError::with_details(
                    format!("UlwLoop goal not found: {goal_id}."),
                    "ULW_LOOP_GOAL_NOT_FOUND",
                    details,
                )
            })?;

        let now = iso();
        let before: Vec<_> = goal
            .success_criteria
            .iter()
            .map(|c| {
                serde_json::json!({
                    "id": c.id,
                    "status": c.status,
                    "capturedEvidence": c.captured_evidence,
                    "capturedAt": c.captured_at,
                })
            })
            .collect();

        let count = goal.success_criteria.len();
        for c in &mut goal.success_criteria {
            c.status = "pending".to_string();
            c.captured_evidence = None;
            c.captured_at = None;
            c.notes = None;
            c.artifacts = None;
        }

        goal.updated_at = now.clone();
        plan.updated_at = now.clone();

        let mut entries = vec![UlwLoopLedgerEntry {
            at: now,
            kind: "criteria_revised".to_string(),
            goal_id: Some(goal_id.to_string()),
            message: Some(format!("Reset {count} criteria to pending.")),
            before: Some(serde_json::json!(before)),
            after: Some(serde_json::json!({ "resetCount": count })),
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

        Ok((plan, count))
    })
}

pub fn unresolved_criteria_of(goal: &UlwLoopItem) -> Vec<&UlwLoopSuccessCriterion> {
    goal.success_criteria.iter().filter(|c| c.status != "pass").collect()
}

pub fn unresolved_essential_criteria_of(goal: &UlwLoopItem) -> Vec<&UlwLoopSuccessCriterion> {
    let essential = essential_criteria_of(goal);
    let essential_ids: std::collections::BTreeSet<_> = essential.iter().map(|c| &c.id).collect();
    goal.success_criteria
        .iter()
        .filter(|c| essential_ids.contains(&c.id) && c.status != "pass")
        .collect()
}

pub fn require_all_criteria_pass(goal: &UlwLoopItem) -> Result<(), UlwLoopError> {
    if has_all_criteria_pass(goal) {
        return Ok(());
    }
    let unresolved: Vec<_> = unresolved_criteria_of(goal)
        .into_iter()
        .map(|c| serde_json::json!({ "id": c.id, "status": c.status }))
        .collect();
    let mut details = BTreeMap::new();
    details.insert("goalId".into(), serde_json::json!(goal.id));
    details.insert("unresolved".into(), serde_json::json!(unresolved));
    Err(UlwLoopError::with_details(
        format!("Goal {} has unresolved success criteria.", goal.id),
        "ulw_loop_criteria_not_all_pass",
        details,
    ))
}

pub fn require_all_plan_criteria_pass(plan: &UlwLoopPlan) -> Result<(), UlwLoopError> {
    let unresolved: Vec<_> = plan
        .goals
        .iter()
        .flat_map(|g| {
            unresolved_criteria_of(g).into_iter().map(|c| {
                serde_json::json!({ "goalId": g.id, "id": c.id, "status": c.status })
            })
        })
        .collect();
    if unresolved.is_empty() {
        return Ok(());
    }
    let mut details = BTreeMap::new();
    details.insert("unresolved".into(), serde_json::json!(unresolved));
    Err(UlwLoopError::with_details(
        "Ulw-loop aggregate has unresolved success criteria.",
        "ulw_loop_criteria_not_all_pass",
        details,
    ))
}

pub fn require_essential_criteria_pass(goal: &UlwLoopItem) -> Result<(), UlwLoopError> {
    if has_essential_criteria_pass(goal) {
        return Ok(());
    }
    let unresolved: Vec<_> = unresolved_essential_criteria_of(goal)
        .into_iter()
        .map(|c| serde_json::json!({ "id": c.id, "status": c.status }))
        .collect();
    let mut details = BTreeMap::new();
    details.insert("goalId".into(), serde_json::json!(goal.id));
    details.insert("unresolved".into(), serde_json::json!(unresolved));
    Err(UlwLoopError::with_details(
        format!("Goal {} has unresolved essential success criteria.", goal.id),
        "ulw_loop_criteria_not_all_pass",
        details,
    ))
}
