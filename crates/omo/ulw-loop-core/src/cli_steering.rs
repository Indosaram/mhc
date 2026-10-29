//! Steering CLI parsing and execution.

use serde_json::Value;

use crate::cli_arg_parser::{parse_goal_arg, read_json_input, read_value};
use crate::cli_output::{print_json, print_status};
use crate::runtime::UlwLoopError;
use crate::steering_batch::{steer_ulw_loop_batch, SteerUlwLoopBatchResult};
use crate::steering_types::{
    SteerUlwLoopResult, UlwLoopSteeringChildGoal, UlwLoopSteeringMutationKind,
    UlwLoopSteeringProposal, UlwLoopSteeringSource,
};

pub const STEERING_KIND_HELP: &str = "Allowed --kind values: add_subgoal, split_subgoal, reorder_pending, revise_pending_wording, revise_criterion, annotate_ledger, mark_blocked_superseded\n\
Kind-specific required flags:\n\
  add_subgoal: --title, --objective, --evidence, --rationale\n\
  split_subgoal: --goal-id, --children, --evidence, --rationale\n\
  reorder_pending: --order, --evidence, --rationale\n\
  revise_pending_wording: --goal-id, --title or --objective, --evidence, --rationale\n\
  revise_criterion: --goal-id, --criterion-id, one of --scenario/--expected-evidence/--user-model, --evidence, --rationale\n\
  annotate_ledger: --evidence, --rationale\n\
  mark_blocked_superseded: --goal-id, optional --replacements, --evidence, --rationale\n\
Example: mhc ulw-loop steer --kind annotate_ledger --evidence \"observed behavior\" --rationale \"why this changes the plan\" --json";

pub fn parse_steering_kind(argv: &[String]) -> Result<String, UlwLoopError> {
    let val = read_value(argv, "--kind");
    let kinds = [
        "add_subgoal", "split_subgoal", "reorder_pending", "revise_pending_wording",
        "revise_criterion", "annotate_ledger", "mark_blocked_superseded",
    ];
    match val {
        Some(k) if kinds.contains(&k.as_str()) => Ok(k),
        Some(k) => Err(UlwLoopError::new(
            format!("Invalid --kind: {k}.\n\n{STEERING_KIND_HELP}"),
            "ULW_LOOP_STEERING_KIND_INVALID",
        )),
        None => Err(UlwLoopError::new(
            format!("Missing --kind.\n\n{STEERING_KIND_HELP}"),
            "ULW_LOOP_STEERING_KIND_REQUIRED",
        )),
    }
}

pub fn parse_steering_source(argv: &[String]) -> Result<String, UlwLoopError> {
    let val = read_value(argv, "--source");
    let sources = ["user_prompt_submit", "finding", "cli"];
    match val {
        Some(s) if sources.contains(&s.as_str()) => Ok(s),
        Some(s) => Err(UlwLoopError::new(
            format!("Invalid --source: {s}."),
            "ULW_LOOP_STEERING_SOURCE_INVALID",
        )),
        None => Ok("cli".to_string()),
    }
}

fn required(argv: &[String], flag: &str) -> Result<String, UlwLoopError> {
    if let Some(v) = read_value(argv, flag) {
        let trimmed = v.trim().to_string();
        if !trimmed.is_empty() {
            return Ok(trimmed);
        }
    }
    Err(UlwLoopError::new(format!("Missing {flag}."), "ULW_LOOP_STEERING_FIELD_REQUIRED"))
}

fn required_goal(argv: &[String]) -> Result<String, UlwLoopError> {
    if let Some(v) = parse_goal_arg(argv) {
        let trimmed = v.trim().to_string();
        if !trimmed.is_empty() {
            return Ok(trimmed);
        }
    }
    Err(UlwLoopError::new("Missing --goal-id.", "ULW_LOOP_GOAL_ID_REQUIRED"))
}

fn children_from_val(val: Option<Value>, flag: &str) -> Result<Vec<UlwLoopSteeringChildGoal>, UlwLoopError> {
    let Some(v) = val else { return Ok(Vec::new()); };
    let Some(arr) = v.as_array() else {
        return Err(UlwLoopError::new(format!("{flag} must be a JSON array."), "ULW_LOOP_STEERING_JSON_ARRAY_REQUIRED"));
    };
    let mut res = Vec::new();
    for item in arr {
        let title = item.get("title").and_then(|t| t.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let objective = item.get("objective").and_then(|t| t.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        if let (Some(title), Some(objective)) = (title, objective) {
            res.push(UlwLoopSteeringChildGoal { title, objective });
        } else {
            return Err(UlwLoopError::new(format!("{flag} entries require title/objective."), "ULW_LOOP_STEERING_CHILD_INVALID"));
        }
    }
    Ok(res)
}

fn string_array_from_val(val: Option<Value>, flag: &str) -> Result<Vec<String>, UlwLoopError> {
    let Some(v) = val else { return Ok(Vec::new()); };
    let Some(arr) = v.as_array() else {
        return Err(UlwLoopError::new(format!("{flag} must be a JSON array."), "ULW_LOOP_STEERING_JSON_ARRAY_REQUIRED"));
    };
    let mut res = Vec::new();
    for item in arr {
        if let Some(s) = item.as_str() {
            res.push(s.trim().to_string());
        } else {
            return Err(UlwLoopError::new(format!("{flag} entries must be strings."), "ULW_LOOP_STEERING_STRING_ARRAY_REQUIRED"));
        }
    }
    Ok(res)
}

pub fn parse_steering_proposal(argv: &[String]) -> Result<UlwLoopSteeringProposal, UlwLoopError> {
    let kind = parse_steering_kind(argv)?;
    let source = parse_steering_source(argv)?;
    let evidence = required(argv, "--evidence")?;
    let rationale = required(argv, "--rationale")?;
    let idempotency_key = read_value(argv, "--idempotency-key");

    let mut proposal = UlwLoopSteeringProposal {
        kind: kind.clone(),
        source,
        evidence,
        rationale,
        idempotency_key,
        target_goal_id: None,
        target_goal_ids: None,
        criterion_id: None,
        title: None,
        objective: None,
        child_goals: None,
        revised_title: None,
        revised_objective: None,
        pending_order: None,
        blocked_reason: None,
        scenario: None,
        expected_evidence: None,
        user_model: None,
        directive_text: None,
        prompt_signature: None,
        raw_proposal: None,
        goal_id: None,
        after: None,
    };

    match kind.as_str() {
        "add_subgoal" => {
            proposal.title = Some(required(argv, "--title")?);
            proposal.objective = Some(required(argv, "--objective")?);
        }
        "split_subgoal" => {
            let gid = required_goal(argv)?;
            proposal.target_goal_id = Some(gid.clone());
            proposal.target_goal_ids = Some(vec![gid]);
            let children_raw = required(argv, "--children")?;
            let val = read_json_input(Some(&children_raw))?;
            proposal.child_goals = Some(children_from_val(val, "--children")?);
        }
        "reorder_pending" => {
            let order_raw = required(argv, "--order")?;
            let val = read_json_input(Some(&order_raw))?;
            proposal.pending_order = Some(string_array_from_val(val, "--order")?);
        }
        "revise_pending_wording" => {
            let gid = required_goal(argv)?;
            proposal.target_goal_id = Some(gid.clone());
            proposal.target_goal_ids = Some(vec![gid]);
            let title = read_value(argv, "--title");
            let objective = read_value(argv, "--objective");
            if title.is_none() && objective.is_none() {
                return Err(UlwLoopError::new("revise_pending_wording requires --title or --objective.", "ULW_LOOP_STEERING_UPDATE_REQUIRED"));
            }
            proposal.revised_title = title;
            proposal.revised_objective = objective;
        }
        "revise_criterion" => {
            let gid = required_goal(argv)?;
            proposal.target_goal_id = Some(gid.clone());
            proposal.target_goal_ids = Some(vec![gid]);
            proposal.criterion_id = Some(required(argv, "--criterion-id")?);
            let scenario = read_value(argv, "--scenario");
            let expected_evidence = read_value(argv, "--expected-evidence");
            let user_model = read_value(argv, "--user-model");
            if scenario.is_none() && expected_evidence.is_none() && user_model.is_none() {
                return Err(UlwLoopError::new("revise_criterion requires scenario, expected-evidence, or user-model.", "ULW_LOOP_STEERING_UPDATE_REQUIRED"));
            }
            proposal.scenario = scenario;
            proposal.expected_evidence = expected_evidence;
            proposal.user_model = user_model;
        }
        "annotate_ledger" => {}
        "mark_blocked_superseded" => {
            let gid = required_goal(argv)?;
            proposal.target_goal_id = Some(gid.clone());
            proposal.target_goal_ids = Some(vec![gid]);
            if let Some(rep_raw) = read_value(argv, "--replacements") {
                let val = read_json_input(Some(&rep_raw))?;
                proposal.child_goals = Some(children_from_val(val, "--replacements")?);
            }
        }
        _ => return Err(UlwLoopError::new(format!("Unsupported steering kind: {kind}."), "ULW_LOOP_STEERING_KIND_UNSUPPORTED")),
    }

    Ok(proposal)
}

pub fn parse_steering_proposals(argv: &[String]) -> Result<Vec<UlwLoopSteeringProposal>, UlwLoopError> {
    let input = read_value(argv, "--proposals-json");
    let Some(input_val) = input else {
        return Ok(vec![parse_steering_proposal(argv)?]);
    };

    if read_value(argv, "--kind").is_some() {
        return Err(UlwLoopError::new("--kind and --proposals-json are mutually exclusive.", "ULW_LOOP_STEERING_BATCH_CONFLICT"));
    }

    let raw = read_json_input(Some(&input_val))?;
    let Some(arr) = raw.and_then(|v| v.as_array().cloned()) else {
        return Err(UlwLoopError::new("--proposals-json must be a non-empty JSON array.", "ULW_LOOP_STEERING_BATCH_ARRAY_REQUIRED"));
    };
    if arr.is_empty() {
        return Err(UlwLoopError::new("--proposals-json must be a non-empty JSON array.", "ULW_LOOP_STEERING_BATCH_ARRAY_REQUIRED"));
    }

    let mut proposals = Vec::new();
    for item in arr {
        let p: UlwLoopSteeringProposal = serde_json::from_value(item).map_err(|e| {
            UlwLoopError::new(format!("Invalid batch steering proposal: {e}"), "ULW_LOOP_STEERING_BATCH_ITEM_INVALID")
        })?;
        proposals.push(p);
    }
    Ok(proposals)
}

pub fn normalize_steering_proposal(
    mut proposal: UlwLoopSteeringProposal,
) -> Result<UlwLoopSteeringProposal, UlwLoopError> {
    fn trim(value: &mut Option<String>) {
        if let Some(value) = value {
            *value = value.trim().to_string();
        }
    }

    trim(&mut proposal.goal_id);
    trim(&mut proposal.target_goal_id);
    trim(&mut proposal.criterion_id);
    trim(&mut proposal.title);
    trim(&mut proposal.objective);
    trim(&mut proposal.revised_title);
    trim(&mut proposal.revised_objective);
    trim(&mut proposal.blocked_reason);
    trim(&mut proposal.scenario);
    trim(&mut proposal.expected_evidence);
    trim(&mut proposal.user_model);
    trim(&mut proposal.directive_text);
    trim(&mut proposal.prompt_signature);
    trim(&mut proposal.idempotency_key);
    proposal.evidence = proposal.evidence.trim().to_string();
    proposal.rationale = proposal.rationale.trim().to_string();
    if proposal.evidence.is_empty() {
        return Err(UlwLoopError::new(
            "Evidence must not be empty.",
            "ULW_LOOP_STEERING_EVIDENCE_REQUIRED",
        ));
    }
    Ok(proposal)
}

pub fn print_steer_result(result: &SteerUlwLoopResult, json: bool) {
    if json {
        let out = serde_json::json!({
            "ok": result.accepted,
            "accepted": result.accepted,
            "rejectedReasons": result.rejected_reasons,
            "deduped": result.deduped,
            "audit": result.audit,
            "plan": result.plan,
        });
        print_json(&out);
        return;
    }
    let outcome = if result.deduped { "deduped" } else if result.accepted { "accepted" } else { "rejected" };
    println!("ulw-loop steer: {outcome} {}", result.audit.kind);
    if !result.rejected_reasons.is_empty() {
        println!("rejected: {}", result.rejected_reasons.join("; "));
    }
    if let Some(key) = &result.audit.idempotency_key {
        println!("idempotency-key: {key}");
    }
    print_status(&result.plan);
}

pub fn print_steer_batch_result(result: &SteerUlwLoopBatchResult, json: bool) {
    if json {
        let out = serde_json::json!({
            "ok": result.accepted,
            "accepted": result.accepted,
            "rejectedReasons": result.rejected_reasons,
            "results": result.results,
            "plan": result.plan,
        });
        print_json(&out);
        return;
    }
    println!("ulw-loop steer batch: {} {} proposal(s)", if result.accepted { "accepted" } else { "rejected" }, result.results.len());
    if !result.rejected_reasons.is_empty() {
        println!("rejected: {}", result.rejected_reasons.join("; "));
    }
    print_status(&result.plan);
}
