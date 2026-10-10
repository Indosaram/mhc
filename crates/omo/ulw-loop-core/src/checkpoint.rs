//! Goal checkpointing and completion validation.

use std::fs;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::checkpoint_codex_validation::validate_checkpoint_codex_goal;
use crate::checkpoint_reconciliation::combine_checkpoint_validation_errors;
use crate::command_types::CheckpointOptions;
use crate::domain_types::{UlwLoopAggregateCompletion, UlwLoopItem, UlwLoopLedgerEntry, UlwLoopPlan, UlwLoopQualityGate};
use crate::driver_objective_ack::acknowledge_driver_objective;
use crate::evidence::{require_all_criteria_pass, require_all_plan_criteria_pass, require_essential_criteria_pass};
use crate::goal_status::{codex_goal_mode, is_final_run_completion_candidate};
use crate::paths::{ulw_loop_attempt_evidence_dir, UlwLoopScope};
use crate::plan_commit::{commit, CommitMutation};
use crate::plan_crud::clear_goal_blocker_fields;
use crate::plan_io::{read_ulw_loop_plan, with_ulw_loop_mutation_lock};
use crate::quality_gate::{classify_external_authorization_blocker, same_blocker_occurrences, validate_quality_gate, ValidateQualityGateOptions};
use crate::quality_gate_artifacts::RealQualityGateFs;
use crate::runtime::{iso, UlwLoopError};
use crate::surface::{resolve_toolkit_surface, UlwLoopToolkitSurface};
use crate::validation_batch::{batch_closed_by, require_all_validation_batches_closed, require_batch_final_ready, require_batch_gate};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckpointUlwLoopArgs {
    pub goal_id: String,
    pub status: String,
    pub evidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_goal_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_gate_json: Option<String>,
}

pub struct CheckpointUlwLoopDependencies {
    pub surface: Option<UlwLoopToolkitSurface>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckpointUlwLoopResult {
    pub plan: UlwLoopPlan,
    pub goal: UlwLoopItem,
    pub ledger_entry: UlwLoopLedgerEntry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate_completion: Option<UlwLoopAggregateCompletion>,
    pub next_actions: Vec<String>,
    pub warnings: Vec<String>,
}

fn read_json_input(raw: Option<&str>, repo_root: &Path) -> Result<Option<Value>, UlwLoopError> {
    let Some(raw_str) = raw else {
        return Ok(None);
    };
    let trimmed = raw_str.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
        return Ok(Some(parsed));
    }

    let path = if Path::new(trimmed).is_absolute() {
        PathBuf::from(trimmed)
    } else {
        repo_root.join(trimmed)
    };

    if !path.exists() {
        return Err(UlwLoopError::new(
            "Quality gate JSON is neither valid JSON nor a readable path.",
            "ulw_loop_json_input_invalid",
        ));
    }

    let content = fs::read_to_string(&path).map_err(|e| {
        UlwLoopError::new(
            format!("Quality gate path does not contain valid JSON: {e}."),
            "ulw_loop_json_input_invalid",
        )
    })?;

    let parsed: Value = serde_json::from_str(&content).map_err(|e| {
        UlwLoopError::new(
            format!("Quality gate path does not contain valid JSON: {e}."),
            "ulw_loop_json_input_invalid",
        )
    })?;

    Ok(Some(parsed))
}

fn apply_blocked_or_failed(
    plan: &mut UlwLoopPlan,
    goal_idx: usize,
    status: &str,
    evidence: &str,
    now: &str,
) {
    let signature = classify_external_authorization_blocker(evidence);
    let occurrences = if let Some(sig) = &signature {
        same_blocker_occurrences(plan, sig) + 1
    } else {
        0
    };
    let needs_decision = signature.is_some() && occurrences >= 3;
    let goal_id = plan.goals[goal_idx].id.clone();
    let goal = &mut plan.goals[goal_idx];
    goal.status = if needs_decision {
        "needs_user_decision".to_string()
    } else {
        status.to_string()
    };
    goal.updated_at = now.to_string();

    if status == "failed" || needs_decision {
        goal.failed_at = Some(now.to_string());
        goal.failure_reason = Some(evidence.to_string());
    }
    if status == "blocked" || needs_decision {
        goal.blocked_reason = Some(evidence.to_string());
    }
    if let Some(sig) = signature {
        goal.blocker_signature = Some(sig.clone());
        goal.blocker_occurrence_count = Some(occurrences as u32);
        goal.required_external_decision = Some(format!("Resolve external authorization: {sig}"));
    }
    if needs_decision {
        goal.non_retriable = Some(true);
    }
    if plan.active_goal_id.as_deref() == Some(&goal_id) {
        plan.active_goal_id = None;
    }
}

fn ledger_kind(
    status: &str,
    goal: &UlwLoopItem,
    aggregate_completion: Option<&UlwLoopAggregateCompletion>,
) -> &'static str {
    if aggregate_completion.is_some() {
        return "aggregate_completed";
    }
    if status == "complete" {
        return "goal_completed";
    }
    if goal.status == "needs_user_decision" {
        return "goal_needs_user_decision";
    }
    if status == "blocked" {
        "goal_blocked"
    } else {
        "goal_failed"
    }
}

pub fn checkpoint_ulw_loop(
    repo_root: impl AsRef<Path>,
    args: &CheckpointUlwLoopArgs,
    scope: Option<&UlwLoopScope>,
    dependencies: Option<&CheckpointUlwLoopDependencies>,
) -> Result<CheckpointUlwLoopResult, UlwLoopError> {
    let root = repo_root.as_ref();
    with_ulw_loop_mutation_lock(root, scope, None, || {
        let mut plan = read_ulw_loop_plan(root, scope)?;
        let goal_idx = plan
            .goals
            .iter()
            .position(|g| g.id == args.goal_id)
            .ok_or_else(|| {
                UlwLoopError::new(
                    format!("Unknown ulw-loop id: {}.", args.goal_id),
                    "ulw_loop_goal_not_found",
                )
            })?;

        let trimmed_evidence = args.evidence.trim();
        if trimmed_evidence.is_empty() {
            return Err(UlwLoopError::new(
                "Evidence must be a non-empty string.",
                "ulw_loop_evidence_required",
            ));
        }

        let now = iso();
        let mut aggregate_completion = None;
        let mut quality_gate = None;
        let mut codex_goal = None;
        let mut next_actions = Vec::new();
        let mut warnings = Vec::new();

        if args.status == "complete" {
            let is_aggregate = codex_goal_mode(&plan) == "aggregate";
            let final_candidate = is_final_run_completion_candidate(&plan, &plan.goals[goal_idx]);
            let closes_batch = batch_closed_by(&plan, &plan.goals[goal_idx].id).is_some();

            if closes_batch {
                require_batch_final_ready(&plan, &plan.goals[goal_idx])?;
            }
            if closes_batch && args.quality_gate_json.is_none() {
                return Err(UlwLoopError::new(
                    "Validation batch final checkpoint requires --quality-gate-json.",
                    "ULW_LOOP_VALIDATION_BATCH_GATE_REQUIRED",
                ));
            }

            if final_candidate {
                require_all_criteria_pass(&plan.goals[goal_idx])?;
                require_all_plan_criteria_pass(&plan)?;
                require_all_validation_batches_closed(&plan, Some(&plan.goals[goal_idx].id))?;
            } else if is_aggregate {
                require_essential_criteria_pass(&plan.goals[goal_idx])?;
            } else {
                require_all_criteria_pass(&plan.goals[goal_idx])?;
            }

            let mut codex_validation_error = None;
            match validate_checkpoint_codex_goal(
                root,
                &plan,
                &plan.goals[goal_idx],
                args.codex_goal_json.as_deref(),
                trimmed_evidence,
                scope,
            ) {
                Ok(v) => {
                    codex_goal = v.raw;
                    next_actions = v.next_actions;
                    warnings = v.warnings;
                    acknowledge_driver_objective(&mut plan, v.unacknowledged_objective.as_deref());
                }
                Err(err) => {
                    codex_validation_error = Some(err);
                }
            }

            if final_candidate {
                aggregate_completion = Some(UlwLoopAggregateCompletion {
                    status: "complete".to_string(),
                    completed_at: now.clone(),
                    evidence: trimmed_evidence.to_string(),
                    codex_goal: codex_goal.clone(),
                });
            }

            if final_candidate || aggregate_completion.is_some() || closes_batch {
                let gate_input = read_json_input(args.quality_gate_json.as_deref(), root)?;
                let gate_val = gate_input.unwrap_or(Value::Null);
                let attempt_dir = if plan.evidence_layout_version == Some(2) {
                    Some(ulw_loop_attempt_evidence_dir(&plan.goals[goal_idx].id, plan.goals[goal_idx].attempt, scope)?)
                } else {
                    None
                };
                let real_fs = RealQualityGateFs;
                let gate_opts = ValidateQualityGateOptions {
                    repo_root: Some(root),
                    fs: Some(&real_fs),
                    current_attempt_dir: attempt_dir.as_deref(),
                    reviewer_surface: dependencies.and_then(|d| d.surface).or_else(|| Some(resolve_toolkit_surface())),
                };

                match validate_quality_gate(&gate_val, Some(&gate_opts)) {
                    Ok(qg) => {
                        require_batch_gate(&plan, &plan.goals[goal_idx], &qg)?;
                        quality_gate = Some(qg);
                    }
                    Err(gate_err) => {
                        if let Some(codex_err) = codex_validation_error {
                            return Err(combine_checkpoint_validation_errors(&codex_err, &gate_err));
                        }
                        return Err(gate_err);
                    }
                }
            }

            if let Some(codex_err) = codex_validation_error {
                return Err(codex_err);
            }

            let goal = &mut plan.goals[goal_idx];
            goal.status = "complete".to_string();
            goal.completed_at = Some(now.clone());
            goal.evidence = Some(trimmed_evidence.to_string());
            goal.failed_at = None;
            goal.failure_reason = None;
            clear_goal_blocker_fields(goal);
            if plan.active_goal_id.as_deref() == Some(&goal.id) {
                plan.active_goal_id = None;
            }
        } else {
            let status = args.status.as_str();
            apply_blocked_or_failed(&mut plan, goal_idx, status, trimmed_evidence, &now);
        }

        let goal = &mut plan.goals[goal_idx];
        goal.updated_at = now.clone();
        let cloned_goal = goal.clone();

        if let Some(agg) = &aggregate_completion {
            plan.aggregate_completion = Some(agg.clone());
            next_actions.push("aggregate complete — now update_goal({status:\"complete\"})".to_string());
        }
        plan.updated_at = now.clone();

        let kind = ledger_kind(&args.status, &cloned_goal, aggregate_completion.as_ref());
        let watch = quality_gate.as_ref().is_some_and(|qg| {
            qg.surface == "lazycodex"
                && qg
                    .code_review
                    .as_ref()
                    .is_some_and(|cr| cr.code_quality_status == "WATCH")
        });

        let ledger_evidence = if watch {
            let code_ev = quality_gate
                .as_ref()
                .and_then(|qg| qg.code_review.as_ref())
                .map(|cr| cr.evidence.as_str())
                .unwrap_or("");
            format!("{trimmed_evidence} | codeQuality=WATCH: {code_ev}")
        } else {
            trimmed_evidence.to_string()
        };

        let ledger_entry = UlwLoopLedgerEntry {
            at: now.clone(),
            kind: kind.to_string(),
            goal_id: Some(cloned_goal.id.clone()),
            status: Some(cloned_goal.status.clone()),
            evidence: Some(ledger_evidence),
            codex_goal: codex_goal.clone(),
            quality_gate: quality_gate.map(|qg| serde_json::to_value(qg).unwrap_or(Value::Null)),
            blocker_signature: cloned_goal.blocker_signature.clone(),
            blocker_occurrence_count: cloned_goal.blocker_occurrence_count,
            required_external_decision: cloned_goal.required_external_decision.clone(),
            ..Default::default()
        };

        let mut entries = vec![ledger_entry.clone()];
        let closed_batch = if args.status == "complete" {
            batch_closed_by(&plan, &cloned_goal.id).cloned()
        } else {
            None
        };
        if let Some(batch) = closed_batch {
            entries.push(UlwLoopLedgerEntry {
                at: now,
                kind: "batch_closed".to_string(),
                goal_id: Some(cloned_goal.id.clone()),
                message: Some(batch.batch_id),
                ..Default::default()
            });
        }

        commit(
            root,
            scope,
            CommitMutation {
                plan: &mut plan,
                entries: &mut entries,
            },
        )?;

        Ok(CheckpointUlwLoopResult {
            plan,
            goal: cloned_goal,
            ledger_entry,
            aggregate_completion,
            next_actions,
            warnings,
        })
    })
}
