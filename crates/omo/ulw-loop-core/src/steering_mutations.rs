//! Steering mutations for ulw-loop goals.

use serde_json::Value;

use crate::domain_types::{UlwLoopItem, UlwLoopPlan, UlwLoopSuccessCriterion};
use crate::plan_goal_factory::{seed_default_success_criteria, GoalSeedOptions};
use crate::steering_types::{UlwLoopSteeringChildGoal, UlwLoopSteeringProposal};
use crate::surface::UlwLoopToolkitSurface;
use crate::validation_batch::update_batches_after_supersede;

fn read_field<'a>(val: &'a Value, key: &str) -> Option<&'a Value> {
    val.as_object().and_then(|obj| obj.get(key))
}

fn text_field(val: &Value, key: &str) -> Option<String> {
    read_field(val, key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn after_obj(val: &Value) -> Option<&Value> {
    read_field(val, "after").filter(|v| v.is_object())
}

fn revised_str(val: &Value, direct: &str, nested: &str) -> Option<String> {
    text_field(val, direct).or_else(|| {
        let nested_obj = after_obj(val).unwrap_or(val);
        text_field(nested_obj, nested)
    })
}

fn targets(proposal: &UlwLoopSteeringProposal) -> Vec<String> {
    if let Some(list) = &proposal.target_goal_ids {
        if !list.is_empty() {
            return list.clone();
        }
    }
    if let Some(single) = &proposal.target_goal_id {
        if !single.is_empty() {
            return vec![single.clone()];
        }
    }
    if let Some(single) = &proposal.goal_id {
        if !single.is_empty() {
            return vec![single.clone()];
        }
    }
    if let Some(obj) = &proposal.raw_proposal {
        if let Some(gid) = text_field(obj, "goalId") {
            return vec![gid];
        }
    }
    Vec::new()
}

fn extract_children(proposal: &UlwLoopSteeringProposal) -> Vec<UlwLoopSteeringChildGoal> {
    if let Some(direct) = &proposal.child_goals {
        if !direct.is_empty() {
            return direct.clone();
        }
    }
    if let Some(val) = &proposal.raw_proposal {
        let nested = after_obj(val);
        let from_after = nested.and_then(|n| read_field(n, "children"));
        if let Some(arr) = from_after.and_then(|v| v.as_array()) {
            let mut res = Vec::new();
            for item in arr {
                if let (Some(title), Some(objective)) = (text_field(item, "title"), text_field(item, "objective")) {
                    res.push(UlwLoopSteeringChildGoal { title, objective });
                }
            }
            return res;
        }
    }
    Vec::new()
}

fn next_id(plan: &UlwLoopPlan, offset: usize) -> String {
    let mut max = 0usize;
    for item in &plan.goals {
        if let Some(rest) = item.id.strip_prefix('G') {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(num) = digits.parse::<usize>() {
                max = max.max(num);
            }
        }
    }
    format!("G{:03}", max + offset)
}

pub fn make_goal(
    plan: &UlwLoopPlan,
    child_goal: &UlwLoopSteeringChildGoal,
    evidence: &str,
    now: &str,
    offset: usize,
    surface: Option<UlwLoopToolkitSurface>,
) -> UlwLoopItem {
    let id = next_id(plan, offset);
    let digits: String = id.strip_prefix('G').unwrap_or("").chars().take_while(|c| c.is_ascii_digit()).collect();
    let goal_index = digits.parse::<usize>().map(|n| n.saturating_sub(1)).unwrap_or_else(|_| plan.goals.len() + offset - 1);
    let seed_opts = GoalSeedOptions {
        goal_id: Some(id.clone()),
        surface,
        ..Default::default()
    };
    let criteria = seed_default_success_criteria(goal_index, &child_goal.objective, Some(&seed_opts));
    UlwLoopItem {
        id,
        title: child_goal.title.clone(),
        objective: child_goal.objective.clone(),
        status: "pending".to_string(),
        success_criteria: criteria,
        attempt: 0,
        created_at: now.to_string(),
        updated_at: now.to_string(),
        started_at: None,
        completed_at: None,
        failed_at: None,
        review_blocked_at: None,
        evidence: Some(evidence.to_string()),
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
    }
}

pub fn revise_wording(plan: &mut UlwLoopPlan, proposal: &UlwLoopSteeringProposal, now: &str) {
    let target_ids = targets(proposal);
    let Some(first_target) = target_ids.first() else { return; };
    let Some(target) = plan.goals.iter_mut().find(|g| g.id == *first_target) else { return; };

    let raw = proposal.raw_proposal.as_ref().unwrap_or(&Value::Null);
    let title = proposal
        .revised_title
        .clone()
        .or_else(|| revised_str(raw, "revisedTitle", "title"));
    let objective = proposal
        .revised_objective
        .clone()
        .or_else(|| revised_str(raw, "revisedObjective", "objective"));

    if let Some(t) = title {
        target.title = t;
    }
    if let Some(o) = objective {
        target.objective = o;
    }
    target.steering_evidence = Some(proposal.evidence.clone());
    target.steering_rationale = Some(proposal.rationale.clone());
    target.updated_at = now.to_string();
}

pub fn split_or_block(
    plan: &mut UlwLoopPlan,
    proposal: &UlwLoopSteeringProposal,
    now: &str,
    surface: Option<UlwLoopToolkitSurface>,
) {
    let target_ids = targets(proposal);
    let Some(first_target) = target_ids.first() else { return; };
    let Some(target_idx) = plan.goals.iter().position(|g| g.id == *first_target) else { return; };

    let child_goals = extract_children(proposal);
    let replacements: Vec<UlwLoopItem> = child_goals
        .iter()
        .enumerate()
        .map(|(i, c)| make_goal(plan, c, &proposal.evidence, now, i + 1, surface))
        .collect();

    let target = &mut plan.goals[target_idx];
    target.steering_evidence = Some(proposal.evidence.clone());
    target.steering_rationale = Some(proposal.rationale.clone());
    target.updated_at = now.to_string();

    let target_id = target.id.clone();
    if replacements.is_empty() {
        target.status = "blocked".to_string();
        target.steering_status = Some("blocked".to_string());
        target.blocked_reason = proposal
            .blocked_reason
            .clone()
            .or_else(|| Some(proposal.rationale.clone()));
    } else {
        let rep_ids: Vec<String> = replacements.iter().map(|r| r.id.clone()).collect();
        target.steering_status = Some("superseded".to_string());
        target.superseded_by = Some(rep_ids.clone());

        let mut replacements = replacements;
        for r in &mut replacements {
            r.supersedes = Some(vec![target_id.clone()]);
        }

        // Insert replacements right after target
        let insert_idx = target_idx + 1;
        for (i, r) in replacements.into_iter().enumerate() {
            plan.goals.insert(insert_idx + i, r);
        }

        update_batches_after_supersede(plan, &target_id, &rep_ids);
    }

    if plan.active_goal_id.as_deref() == Some(&target_id) {
        plan.active_goal_id = None;
    }
}

pub fn revise_criterion(plan: &mut UlwLoopPlan, proposal: &UlwLoopSteeringProposal, now: &str) {
    let target_ids = targets(proposal);
    let Some(first_target) = target_ids.first() else { return; };
    let Some(target) = plan.goals.iter_mut().find(|g| g.id == *first_target) else { return; };

    let Some(crit_id) = &proposal.criterion_id else { return; };
    let Some(crit) = target.success_criteria.iter_mut().find(|c| c.id == *crit_id) else { return; };

    let raw = proposal.raw_proposal.as_ref().unwrap_or(&Value::Null);
    if let Some(sc) = proposal.scenario.clone().or_else(|| text_field(raw, "scenario")) {
        crit.scenario = sc;
    }
    if let Some(ee) = proposal.expected_evidence.clone().or_else(|| text_field(raw, "expectedEvidence")) {
        crit.expected_evidence = ee;
    }
    if let Some(m) = proposal.user_model.clone().or_else(|| text_field(raw, "userModel")) {
        crit.user_model = m;
    }
    target.updated_at = now.to_string();
}
