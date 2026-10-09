//! Validation batch membership and gating.

use std::collections::{BTreeMap, BTreeSet};
use serde_json::Value;

use crate::domain_types::{UlwLoopItem, UlwLoopLedgerEntry, UlwLoopPlan, UlwLoopQualityGate, UlwLoopValidationBatch};
use crate::goal_status::is_member_resolved;
use crate::runtime::UlwLoopError;

pub fn parse_validation_batches(
    input: Option<&str>,
    goals: &[UlwLoopItem],
) -> Result<Option<Vec<UlwLoopValidationBatch>>, UlwLoopError> {
    let Some(raw_input) = input else {
        return Ok(None);
    };
    let trimmed = raw_input.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let parsed: Value = if trimmed.starts_with('{') || trimmed.starts_with('[') {
        serde_json::from_str(trimmed).map_err(|_| {
            UlwLoopError::new("--validation-batch-json must be a JSON array.", "ULW_LOOP_VALIDATION_BATCH_INVALID")
        })?
    } else {
        let content = std::fs::read_to_string(trimmed).map_err(|_| {
            UlwLoopError::new(format!("Failed to read validation batch file {trimmed}"), "ULW_LOOP_VALIDATION_BATCH_INVALID")
        })?;
        serde_json::from_str(&content).map_err(|_| {
            UlwLoopError::new("--validation-batch-json must be a JSON array.", "ULW_LOOP_VALIDATION_BATCH_INVALID")
        })?
    };

    let Some(arr) = parsed.as_array() else {
        return Err(UlwLoopError::new("--validation-batch-json must be a JSON array.", "ULW_LOOP_VALIDATION_BATCH_INVALID"));
    };

    let mut batches = Vec::new();
    for item in arr {
        let Some(obj) = item.as_object() else {
            return Err(UlwLoopError::new("validation batch entries must be objects.", "ULW_LOOP_VALIDATION_BATCH_INVALID"));
        };
        let batch_id = obj.get("batchId").and_then(|v| v.as_str()).map(|s| s.trim()).filter(|s| !s.is_empty());
        let batch_id = match batch_id {
            Some(s) => s.to_string(),
            None => return Err(UlwLoopError::new("validation batch requires batchId.", "ULW_LOOP_VALIDATION_BATCH_INVALID")),
        };
        let final_goal_id = obj.get("finalGoalId").and_then(|v| v.as_str()).map(|s| s.trim()).filter(|s| !s.is_empty());
        let final_goal_id = match final_goal_id {
            Some(s) => s.to_string(),
            None => return Err(UlwLoopError::new("validation batch requires finalGoalId.", "ULW_LOOP_VALIDATION_BATCH_INVALID")),
        };
        let member_ids = obj.get("memberIds").and_then(|v| v.as_array());
        let member_ids: Vec<String> = match member_ids {
            Some(ids) => {
                let parsed: Vec<_> = ids.iter().filter_map(|v| v.as_str().map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).collect();
                if parsed.len() < 2 {
                    return Err(UlwLoopError::new("validation batch requires at least two memberIds.", "ULW_LOOP_VALIDATION_BATCH_INVALID"));
                }
                parsed
            }
            None => return Err(UlwLoopError::new("validation batch requires at least two memberIds.", "ULW_LOOP_VALIDATION_BATCH_INVALID")),
        };

        batches.push(UlwLoopValidationBatch {
            batch_id,
            member_ids,
            final_goal_id,
        });
    }

    validate_batches(&batches, goals)?;
    Ok(Some(batches))
}

fn validate_batches(batches: &[UlwLoopValidationBatch], goals: &[UlwLoopItem]) -> Result<(), UlwLoopError> {
    let goal_ids: BTreeSet<_> = goals.iter().map(|g| &g.id).collect();
    let mut batch_ids = BTreeSet::new();
    let mut members = BTreeSet::new();

    for batch in batches {
        if batch_ids.contains(&batch.batch_id) {
            return Err(UlwLoopError::new(format!("duplicate validation batch id: {}.", batch.batch_id), "ULW_LOOP_VALIDATION_BATCH_INVALID"));
        }
        batch_ids.insert(batch.batch_id.clone());

        let member_set: BTreeSet<_> = batch.member_ids.iter().collect();
        if member_set.len() != batch.member_ids.len() {
            return Err(UlwLoopError::new(format!("validation batch {} has duplicate memberIds.", batch.batch_id), "ULW_LOOP_VALIDATION_BATCH_INVALID"));
        }
        if !batch.member_ids.contains(&batch.final_goal_id) {
            return Err(UlwLoopError::new(format!("validation batch {} finalGoalId must be a member.", batch.batch_id), "ULW_LOOP_VALIDATION_BATCH_FINAL_NOT_MEMBER"));
        }
        for mid in &batch.member_ids {
            if !goal_ids.contains(mid) {
                return Err(UlwLoopError::new(format!("validation batch {} references unknown goal: {mid}.", batch.batch_id), "ULW_LOOP_VALIDATION_BATCH_MEMBER_UNKNOWN"));
            }
            if members.contains(mid) {
                return Err(UlwLoopError::new(format!("goal appears in multiple validation batches: {mid}."), "ULW_LOOP_VALIDATION_BATCH_OVERLAP"));
            }
            members.insert(mid.clone());
        }
    }
    Ok(())
}

pub fn update_batches_after_supersede(
    plan: &mut UlwLoopPlan,
    target_id: &str,
    replacement_ids: &[String],
) {
    if replacement_ids.is_empty() || plan.validation_batches.is_none() {
        return;
    }
    if let Some(batches) = &mut plan.validation_batches {
        for batch in batches.iter_mut() {
            if !batch.member_ids.iter().any(|id| id == target_id) {
                continue;
            }
            let mut new_members = Vec::new();
            for id in &batch.member_ids {
                if id == target_id {
                    new_members.extend(replacement_ids.iter().cloned());
                } else {
                    new_members.push(id.clone());
                }
            }
            let replacement_final = replacement_ids.last().cloned().unwrap_or_else(|| batch.final_goal_id.clone());
            let final_goal_id = if batch.final_goal_id == target_id {
                replacement_final
            } else {
                batch.final_goal_id.clone()
            };
            batch.member_ids = new_members;
            batch.final_goal_id = final_goal_id;
        }
    }
}

pub fn batch_update_ledger_entry(
    before: &UlwLoopPlan,
    after: &UlwLoopPlan,
    at: &str,
) -> Option<UlwLoopLedgerEntry> {
    let before_batches = serde_json::to_string(&before.validation_batches).unwrap_or_default();
    let after_batches = serde_json::to_string(&after.validation_batches).unwrap_or_default();
    if before_batches == after_batches {
        return None;
    }
    Some(UlwLoopLedgerEntry {
        at: at.to_string(),
        kind: "batch_updated".to_string(),
        before: before.validation_batches.as_ref().map(|v| serde_json::to_value(v).unwrap_or(Value::Null)),
        after: after.validation_batches.as_ref().map(|v| serde_json::to_value(v).unwrap_or(Value::Null)),
        message: Some("Validation batch membership updated after steering.".to_string()),
        ..Default::default()
    })
}

pub fn batch_of<'a>(plan: &'a UlwLoopPlan, goal_id: &str) -> Option<&'a UlwLoopValidationBatch> {
    plan.validation_batches
        .as_deref()
        .and_then(|batches| batches.iter().find(|b| b.member_ids.iter().any(|id| id == goal_id)))
}

pub fn batch_closed_by<'a>(plan: &'a UlwLoopPlan, goal_id: &str) -> Option<&'a UlwLoopValidationBatch> {
    let batch = batch_of(plan, goal_id)?;
    if batch.final_goal_id == goal_id {
        Some(batch)
    } else {
        None
    }
}

pub fn require_batch_final_ready(plan: &UlwLoopPlan, goal: &UlwLoopItem) -> Result<(), UlwLoopError> {
    let Some(batch) = batch_closed_by(plan, &goal.id) else {
        return Ok(());
    };
    let open: Vec<_> = batch
        .member_ids
        .iter()
        .filter(|id| *id != &goal.id && !member_resolved(plan, id))
        .cloned()
        .collect();
    if !open.is_empty() {
        let mut details = BTreeMap::new();
        details.insert("batchId".into(), serde_json::json!(batch.batch_id));
        details.insert("open".into(), serde_json::json!(open));
        return Err(UlwLoopError::with_details(
            "Validation batch has unresolved members.",
            "ULW_LOOP_VALIDATION_BATCH_OPEN",
            details,
        ));
    }
    Ok(())
}

pub fn require_all_validation_batches_closed(
    plan: &UlwLoopPlan,
    closing_goal_id: Option<&str>,
) -> Result<(), UlwLoopError> {
    let batches = plan.validation_batches.as_deref().unwrap_or(&[]);
    let mut open = Vec::new();
    for batch in batches {
        if batch
            .member_ids
            .iter()
            .any(|id| Some(id.as_str()) != closing_goal_id && !member_resolved(plan, id))
        {
            open.push(batch.batch_id.clone());
        }
    }
    if !open.is_empty() {
        let mut details = BTreeMap::new();
        details.insert("batchIds".into(), serde_json::json!(open));
        return Err(UlwLoopError::with_details(
            "Validation batches remain open.",
            "ULW_LOOP_VALIDATION_BATCH_OPEN",
            details,
        ));
    }
    Ok(())
}

pub fn require_batch_gate(
    plan: &UlwLoopPlan,
    goal: &UlwLoopItem,
    gate: &UlwLoopQualityGate,
) -> Result<(), UlwLoopError> {
    let Some(batch) = batch_closed_by(plan, &goal.id) else {
        return Ok(());
    };
    let members: Vec<_> = batch
        .member_ids
        .iter()
        .filter_map(|id| plan.goals.iter().find(|item| &item.id == id))
        .collect();
    let mut pending = Vec::new();
    for member in &members {
        for criterion in &member.success_criteria {
            if criterion.status != "pass" {
                pending.push(format!("{}:{}", member.id, criterion.id));
            }
        }
    }
    if !pending.is_empty() {
        let mut details = BTreeMap::new();
        details.insert("batchId".into(), serde_json::json!(batch.batch_id));
        details.insert("pending".into(), serde_json::json!(pending));
        return Err(UlwLoopError::with_details(
            "Validation batch criteria remain pending.",
            "ULW_LOOP_VALIDATION_BATCH_CRITERIA_PENDING",
            details,
        ));
    }

    let total_criteria: usize = members.iter().map(|m| m.success_criteria.len()).sum();
    let pass_count: usize = members
        .iter()
        .map(|m| m.success_criteria.iter().filter(|c| c.status == "pass").count())
        .sum();

    if gate.criteria_coverage.total_criteria != total_criteria
        || gate.criteria_coverage.pass_count != pass_count
    {
        let mut details = BTreeMap::new();
        details.insert("batchId".into(), serde_json::json!(batch.batch_id));
        details.insert(
            "expected".into(),
            serde_json::json!({ "totalCriteria": total_criteria, "passCount": pass_count }),
        );
        details.insert(
            "actual".into(),
            serde_json::json!({ "totalCriteria": gate.criteria_coverage.total_criteria, "passCount": gate.criteria_coverage.pass_count }),
        );
        return Err(UlwLoopError::with_details(
            "Validation batch gate coverage does not match member criteria.",
            "ULW_LOOP_VALIDATION_BATCH_GATE_MISMATCH",
            details,
        ));
    }

    Ok(())
}

fn member_resolved(plan: &UlwLoopPlan, goal_id: &str) -> bool {
    plan.goals
        .iter()
        .find(|c| c.id == goal_id)
        .is_some_and(|g| is_member_resolved(g, plan))
}
