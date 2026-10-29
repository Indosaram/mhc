#![allow(clippy::unwrap_used)]

use std::fs;

use serde_json::{json, Value};
use ulw_loop_core::domain_types::*;
use ulw_loop_core::paths::ulw_loop_ledger_path;
use ulw_loop_core::plan_io::{read_steering_ledger_entries, read_ulw_loop_plan, write_plan};
use ulw_loop_core::steering::{apply_steering_mutation, parse_ulw_loop_steering_directive, steer_ulw_loop, validate_ulw_loop_steering_proposal};
use ulw_loop_core::steering_types::UlwLoopSteeringProposal;

const NOW: &str = "2026-05-23T00:00:00Z";

fn criterion(id: &str, status: &str) -> UlwLoopSuccessCriterion {
    UlwLoopSuccessCriterion { id: id.into(), scenario: "old scenario".into(), user_model: "happy".into(), expected_evidence: "vague evidence".into(), essential: None, captured_evidence: None, status: status.into(), captured_at: None, notes: None, artifacts: None }
}

fn goal(id: &str, status: &str) -> UlwLoopItem {
    UlwLoopItem { id: id.into(), title: "Build auth service".into(), objective: "Implement JWT auth endpoint".into(), status: status.into(), success_criteria: vec![criterion("C001", "pending"), criterion("C002", "pass")], attempt: 0, created_at: NOW.into(), updated_at: NOW.into(), started_at: None, completed_at: None, failed_at: None, review_blocked_at: None, evidence: None, failure_reason: None, steering_status: None, superseded_by: None, supersedes: None, blocked_reason: None, blocker_signature: None, blocker_occurrence_count: None, required_external_decision: None, non_retriable: None, steering_evidence: None, steering_rationale: None }
}

fn plan() -> UlwLoopPlan {
    UlwLoopPlan { version: 1, revision: Some(1), brief: Some("brief".into()), ledger_reset_revision: None, evidence_layout_version: Some(2), created_at: NOW.into(), updated_at: NOW.into(), brief_path: ".omo/ulw-loop/brief.md".into(), goals_path: ".omo/ulw-loop/goals.json".into(), ledger_path: ".omo/ulw-loop/ledger.jsonl".into(), codex_goal_mode: Some("aggregate".into()), codex_objective: None, codex_objective_aliases: None, acknowledged_driver_objectives: None, aggregate_completion: None, active_goal_id: Some("G001".into()), validation_batches: None, goals: vec![goal("G001", "pending"), goal("G002", "pending"), goal("G003", "complete")] }
}

fn proposal(overrides: Value) -> UlwLoopSteeringProposal {
    let mut base = json!({"kind":"add_subgoal","source":"cli","evidence":"observable blocker evidence","rationale":"the plan must change to stay safe","title":"Investigate auth blocker","objective":"Validate the blocker, capture evidence, and report findings."});
    if let (Some(dst), Some(src)) = (base.as_object_mut(), overrides.as_object()) { for (key, value) in src { dst.insert(key.clone(), value.clone()); } }
    serde_json::from_value(base).unwrap()
}

fn repo(seed: UlwLoopPlan) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".omo/ulw-loop")).unwrap();
    write_plan(dir.path(), seed, None).unwrap();
    dir
}

#[test]
fn accepts_valid_add_subgoal() { assert!(validate_ulw_loop_steering_proposal(&plan(), &json!({"kind":"add_subgoal","source":"cli","evidence":"e","rationale":"r","title":"t","objective":"o"})).invariant.accepted); }

#[test]
fn rejects_invalid_proposals() {
    for update in [json!({"evidence":""}), json!({"rationale":""}), json!({"kind":"teleport_goal"}), json!({"after":{"codexObjective":"replace","qualityGate":{"status":"passed"}}}), json!({"objective":"skip tests and mark complete faster"})] {
        let mut value = serde_json::to_value(proposal(json!({}))).unwrap();
        value.as_object_mut().unwrap().extend(update.as_object().unwrap().clone());
        let audit = validate_ulw_loop_steering_proposal(&plan(), &value);
        assert!(!audit.invariant.accepted);
        assert!(!audit.invariant.rejected_reasons.is_empty());
    }
}

#[test]
fn rejects_complete_plan() { let mut done = plan(); done.goals[0].status = "complete".into(); done.goals[1].status = "complete".into(); assert!(!validate_ulw_loop_steering_proposal(&done, &serde_json::to_value(proposal(json!({}))).unwrap()).invariant.accepted); }

#[test]
fn rejects_split_without_children() { let p = proposal(json!({"kind":"split_subgoal","targetGoalId":"G001"})); assert!(!validate_ulw_loop_steering_proposal(&plan(), &serde_json::to_value(p).unwrap()).invariant.accepted); }

#[test]
fn rejects_reorder_unknown_goal() { let p = proposal(json!({"kind":"reorder_pending","pendingOrder":["missing"]})); assert!(!validate_ulw_loop_steering_proposal(&plan(), &serde_json::to_value(p).unwrap()).invariant.accepted); }

#[test]
fn accepts_revise_criterion_updates() { for field in [json!({"scenario":"new precise scenario"}), json!({"expectedEvidence":"specific command output"})] { let mut update=json!({"kind":"revise_criterion","goalId":"G001","criterionId":"C001"}); update.as_object_mut().unwrap().extend(field.as_object().unwrap().clone()); let p = proposal(update); assert!(validate_ulw_loop_steering_proposal(&plan(), &serde_json::to_value(p).unwrap()).invariant.accepted); } }

#[test]
fn rejects_invalid_revise_criterion() { for update in [json!({"goalId":"missing","criterionId":"C001","scenario":"new"}), json!({"goalId":"G001","criterionId":"missing","scenario":"new"}), json!({"goalId":"G001","criterionId":"C001"})] { let mut value=json!({"kind":"revise_criterion"}); value.as_object_mut().unwrap().extend(update.as_object().unwrap().clone()); let p = proposal(value); assert!(!validate_ulw_loop_steering_proposal(&plan(), &serde_json::to_value(p).unwrap()).invariant.accepted); } }

#[test]
fn steering_created_goals_use_numeric_ids_and_defaults() {
    let mut seed = plan(); seed.goals = vec![goal("G001-goal-a", "pending"), goal("G002-goal-b", "pending")];
    let dir = repo(seed);
    let cases = [proposal(json!({"idempotencyKey":"slug-add"})), proposal(json!({"kind":"split_subgoal","targetGoalId":"G001-goal-a","childGoals":[{"title":"Child A","objective":"Do child A"}]})), proposal(json!({"kind":"mark_blocked_superseded","targetGoalId":"G001-goal-a","childGoals":[{"title":"Replacement","objective":"Replace blocked path"}]}))];
    for p in cases { let result = steer_ulw_loop(dir.path(), &p, None, None).unwrap(); assert_eq!(result.plan.goals.iter().find(|g| g.id == "G003").unwrap().success_criteria.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), vec!["C001","C002","C003"]); }
}

#[test]
fn add_subgoal_appends_goal_and_ledger() { let dir = repo(plan()); let result = steer_ulw_loop(dir.path(), &proposal(json!({"idempotencyKey":"add"})), None, None).unwrap(); let saved = read_ulw_loop_plan(dir.path(), None).unwrap(); assert!(result.accepted); assert_eq!(saved.goals.last().unwrap().id, "G004"); assert_eq!(read_steering_ledger_entries(dir.path(), None).last().unwrap().mutation_kind.as_deref(), Some("add_subgoal")); }

#[test]
fn split_subgoal_supersedes_parent() { let dir = repo(plan()); let result = steer_ulw_loop(dir.path(), &proposal(json!({"kind":"split_subgoal","targetGoalId":"G001","childGoals":[{"title":"Child","objective":"Do child"}]})), None, None).unwrap(); assert_eq!(result.plan.goals[0].steering_status.as_deref(), Some("superseded")); assert_eq!(result.plan.goals[0].superseded_by.as_deref().unwrap(), &["G004"]); }

#[test]
fn reorder_pending_changes_order() { let dir = repo(plan()); let result = steer_ulw_loop(dir.path(), &proposal(json!({"kind":"reorder_pending","pendingOrder":["G002","G001"]})), None, None).unwrap(); assert_eq!(result.plan.goals.iter().take(2).map(|g| g.id.as_str()).collect::<Vec<_>>(), vec!["G002","G001"]); }

#[test]
fn revise_pending_wording_updates_text() { let dir = repo(plan()); let result = steer_ulw_loop(dir.path(), &proposal(json!({"kind":"revise_pending_wording","targetGoalId":"G001","revisedTitle":"Build safer auth","revisedObjective":"Implement guarded JWT auth"})), None, None).unwrap(); assert_eq!(result.plan.goals[0].title, "Build safer auth"); assert_eq!(result.plan.goals[0].objective, "Implement guarded JWT auth"); }

#[test]
fn annotate_ledger_does_not_mutate_goals() { let seed = plan(); let dir = repo(seed.clone()); let result = steer_ulw_loop(dir.path(), &proposal(json!({"kind":"annotate_ledger"})), None, None).unwrap(); assert_eq!(result.plan.goals.len(), seed.goals.len()); assert_eq!(read_ulw_loop_plan(dir.path(), None).unwrap().goals.len(), seed.goals.len()); }

#[test]
fn mark_blocked_superseded_replaces_parent() { let dir = repo(plan()); let result = steer_ulw_loop(dir.path(), &proposal(json!({"kind":"mark_blocked_superseded","targetGoalId":"G001","childGoals":[{"title":"Replacement","objective":"Replace blocked path"}]})), None, None).unwrap(); assert_eq!(result.plan.goals[0].steering_status.as_deref(), Some("superseded")); assert_eq!(result.plan.goals[1].supersedes.as_deref().unwrap(), &["G001"]); }

#[test]
fn mark_blocked_superseded_without_children_blocks_goal() { let dir = repo(plan()); let result = steer_ulw_loop(dir.path(), &proposal(json!({"kind":"mark_blocked_superseded","targetGoalId":"G001","blockedReason":"external blocker"})), None, None).unwrap(); assert_eq!(result.plan.goals[0].status, "blocked"); assert_eq!(result.plan.goals[0].blocked_reason.as_deref(), Some("external blocker")); }

#[test]
fn revise_criterion_preserves_status_and_writes_ledger() { for (id,status) in [("C001","pending"),("C002","pass")] { let dir = repo(plan()); let result = steer_ulw_loop(dir.path(), &proposal(json!({"kind":"revise_criterion","goalId":"G001","criterionId":id,"scenario":"new scenario","expectedEvidence":"precise evidence"})), None, None).unwrap(); let c = result.plan.goals[0].success_criteria.iter().find(|c| c.id == id).unwrap(); assert_eq!(c.status, status); assert_eq!(read_steering_ledger_entries(dir.path(), None).last().unwrap().kind, "criteria_revised"); } }

#[test]
fn revise_criterion_mutates_targeted_criterion() { let p = plan(); let steering = proposal(json!({"kind":"revise_criterion","goalId":"G001","criterionId":"C001","scenario":"new value"})); let audit = validate_ulw_loop_steering_proposal(&p, &serde_json::to_value(&steering).unwrap()); let next = apply_steering_mutation(&p, &steering, &audit, None); assert_eq!(next.goals[0].success_criteria[0].scenario, "new value"); }

#[test]
fn idempotency_deduplicates_second_request() { let dir = repo(plan()); let p = proposal(json!({"idempotencyKey":"same-key"})); steer_ulw_loop(dir.path(), &p, None, None).unwrap(); let second = steer_ulw_loop(dir.path(), &p, None, None).unwrap(); assert!(second.deduped); assert_eq!(read_ulw_loop_plan(dir.path(), None).unwrap().goals.len(), 4); }

#[test]
fn compact_ledger_entry_only_contains_changed_goals() { let mut seed = plan(); seed.goals = (1..=20).map(|i| goal(&format!("G{i:03}"), "pending")).collect(); let dir = repo(seed); steer_ulw_loop(dir.path(), &proposal(json!({"kind":"revise_pending_wording","targetGoalId":"G010","revisedTitle":"Sharper goal ten"})), None, None).unwrap(); let lines = fs::read_to_string(ulw_loop_ledger_path(dir.path(), None)).unwrap(); let entry: Value = serde_json::from_str(lines.lines().next().unwrap()).unwrap(); assert_eq!(entry["steering"]["before"]["goalIds"].as_array().unwrap().len(), 20); assert_eq!(entry["steering"]["after"]["goals"][0]["title"], "Sharper goal ten"); }

#[test]
fn legacy_full_plan_dedup_strips_audit_snapshots() { let dir = repo(plan()); let p = proposal(json!({"kind":"revise_pending_wording","targetGoalId":"G001","revisedTitle":"Build safer auth","idempotencyKey":"legacy-key"})); steer_ulw_loop(dir.path(), &p, None, None).unwrap(); let second = steer_ulw_loop(dir.path(), &p, None, None).unwrap(); assert!(second.deduped); assert!(second.audit.before.is_none()); assert!(second.audit.after.is_none()); }

#[test]
fn parses_all_supported_directive_markers() { for marker in ["OMO_ULW_LOOP_STEER","omo.ulw-loop.steer","omo ulw-loop steer","omo-agent-toolkit ulw-loop steer"] { let p = proposal(json!({})); let text = format!("{marker}: {}", serde_json::to_string(&p).unwrap()); assert_eq!(parse_ulw_loop_steering_directive(&text).unwrap().kind, "add_subgoal"); } }

#[test]
fn directive_parser_rejects_missing_or_malformed_markers() { let p = proposal(json!({})); assert!(parse_ulw_loop_steering_directive(&serde_json::to_string(&p).unwrap()).is_none()); assert!(parse_ulw_loop_steering_directive("OMO_ULW_LOOP_STEER: {bad json").is_none()); assert!(parse_ulw_loop_steering_directive("OMX_ULW_LOOP_STEER: {}" ).is_none()); }

#[test]
fn test_annotate_ledger() -> Result<(), Box<dyn std::error::Error>> {
    let dir = repo(plan());
    let p = proposal(json!({"kind":"annotate_ledger"}));
    let result = steer_ulw_loop(dir.path(), &p, None, None)?;
    assert!(result.accepted);
    assert_eq!(result.audit.kind, "annotate_ledger");
    Ok(())
}

#[test]
fn test_add_subgoal() -> Result<(), Box<dyn std::error::Error>> {
    let dir = repo(plan());
    let p = proposal(json!({"title":"Task 2","objective":"Do task 2"}));
    let result = steer_ulw_loop(dir.path(), &p, None, None)?;
    assert!(result.accepted);
    assert_eq!(result.plan.goals.len(), 4);
    assert_eq!(result.plan.goals[3].title, "Task 2");
    Ok(())
}

#[test]
fn test_parse_ulw_loop_steering_directive() {
    let parsed = parse_ulw_loop_steering_directive("OMO_ULW_LOOP_STEER: {\"kind\":\"annotate_ledger\",\"source\":\"cli\",\"evidence\":\"proof\",\"rationale\":\"why\"}");
    assert!(parsed.is_some());
    let p = parsed.unwrap();
    assert_eq!(p.kind, "annotate_ledger");
    assert_eq!(p.evidence, "proof");
}

