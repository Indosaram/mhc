use std::fs;

use ulw_loop_core::checkpoint::{checkpoint_ulw_loop, CheckpointUlwLoopArgs};
use ulw_loop_core::domain_types::*;
use ulw_loop_core::goal_status::aggregate_codex_objective_for_scope;
use ulw_loop_core::plan_io::{read_ulw_loop_plan, write_plan};

fn criterion(id: &str, status: &str) -> UlwLoopSuccessCriterion {
    UlwLoopSuccessCriterion {
        id: id.into(),
        scenario: format!("{id} scenario"),
        user_model: "happy".into(),
        expected_evidence: format!("{id} proof"),
        essential: Some(true),
        captured_evidence: (status == "pass").then(|| format!("{id} passed")),
        status: status.into(),
        captured_at: None,
        notes: None,
        artifacts: None,
    }
}

fn goal(id: &str, status: &str, criteria: Vec<UlwLoopSuccessCriterion>) -> UlwLoopItem {
    UlwLoopItem {
        id: id.into(),
        title: format!("Goal {id}"),
        objective: format!("Objective {id}"),
        status: status.into(),
        success_criteria: criteria,
        attempt: 1,
        created_at: "2026-05-23T00:00:00Z".into(),
        updated_at: "2026-05-23T00:00:00Z".into(),
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
    }
}

fn plan(goals: Vec<UlwLoopItem>, members: Vec<&str>) -> UlwLoopPlan {
    let active_goal_id = goals.iter().find(|g| g.status == "in_progress").map(|g| g.id.clone());
    UlwLoopPlan {
        version: 1,
        revision: Some(1),
        brief: Some("brief".into()),
        ledger_reset_revision: None,
        evidence_layout_version: None,
        created_at: "2026-05-23T00:00:00Z".into(),
        updated_at: "2026-05-23T00:00:00Z".into(),
        brief_path: ".omo/ulw-loop/brief.md".into(),
        goals_path: ".omo/ulw-loop/goals.json".into(),
        ledger_path: ".omo/ulw-loop/ledger.jsonl".into(),
        codex_goal_mode: Some("aggregate".into()),
        codex_objective: Some(aggregate_codex_objective_for_scope(None)),
        codex_objective_aliases: None,
        acknowledged_driver_objectives: None,
        aggregate_completion: None,
        active_goal_id,
        validation_batches: Some(vec![UlwLoopValidationBatch {
            batch_id: "VB001".into(),
            member_ids: members.into_iter().map(str::to_string).collect(),
            final_goal_id: "G002".into(),
        }]),
        goals,
    }
}

fn repo_with(seed: UlwLoopPlan) -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(repo.path().join(".omo/ulw-loop")).expect("loop directory");
    let scope = ulw_loop_core::paths::UlwLoopScope { session_id: Some("s1".into()), ..Default::default() };
    write_plan(repo.path(), seed, Some(&scope)).expect("write plan");
    repo
}

fn snapshot(status: &str) -> String {
    serde_json::json!({"goal":{"objective":aggregate_codex_objective_for_scope(None),"status":status}}).to_string()
}

fn expect_code(repo: &tempfile::TempDir, args: CheckpointUlwLoopArgs, code: &str) {
    let scope = ulw_loop_core::paths::UlwLoopScope { session_id: Some("s1".into()), ..Default::default() };
    let error = checkpoint_ulw_loop(repo.path(), &args, Some(&scope), None).expect_err("expected error");
    assert_eq!(error.code, code);
}

fn gate_json(repo: &tempfile::TempDir, total: usize, pass: usize) -> String {
    let qa = repo.path().join(".omo/evidence/ulw/s1/G002/a1");
    fs::create_dir_all(&qa).expect("qa directory");
    fs::write(qa.join("code-review.md"), "approved\n").expect("code review");
    fs::write(qa.join("gate-review.md"), "approved\n").expect("gate review");
    fs::write(qa.join("cli-pass.txt"), "passed\n").expect("cli evidence");
    serde_json::json!({
        "codeReview":{"by":"lazycodex-code-reviewer","recommendation":"APPROVE","codeQualityStatus":"CLEAR","reportPath":".omo/evidence/ulw/s1/G002/a1/code-review.md","evidence":"approved","blockers":[]},
        "manualQa":{"by":"lazycodex-qa-executor","status":"passed","evidence":"passed","surfaceEvidence":[{"id":"surface","criterionRef":"C001","surface":"cli","invocation":"checkpoint","verdict":"passed","artifactRefs":["cli"]}],"adversarialCases":[{"id":"adv","criterionRef":"C002","scenario":"bad gate","expectedBehavior":"reject","verdict":"passed","artifactRefs":["cli"]}],"artifactRefs":[{"id":"cli","kind":"cli-transcript","description":"cli","path":".omo/evidence/ulw/s1/G002/a1/cli-pass.txt"}]},
        "gateReview":{"by":"lazycodex-gate-reviewer","recommendation":"APPROVE","reportPath":".omo/evidence/ulw/s1/G002/a1/gate-review.md","evidence":"approved","blockers":[]},
        "iteration":{"fullRerun":true,"status":"passed","rerunCommands":["checkpoint"],"evidence":"passed"},
        "criteriaCoverage":{"totalCriteria":total,"passCount":pass,"originalIntent":"intent","desiredOutcome":"outcome","userOutcomeReview":"review","adversarialClassesCovered":["missing_artifact"]}
    }).to_string()
}

#[test]
fn rejects_open_batch_member_when_completing_final_goal() {
    let repo = repo_with(plan(vec![goal("G001", "pending", vec![criterion("C001", "pass")]), goal("G002", "in_progress", vec![criterion("C001", "pass")]), goal("G003", "pending", vec![])], vec!["G001", "G002"]));
    expect_code(&repo, CheckpointUlwLoopArgs { goal_id: "G002".into(), status: "complete".into(), evidence: "batch final implementation done".into(), codex_goal_json: Some(snapshot("active")), quality_gate_json: Some("{}".into()) }, "ULW_LOOP_VALIDATION_BATCH_OPEN");
}

#[test]
fn requires_batch_gate_when_members_complete() {
    let repo = repo_with(plan(vec![goal("G001", "complete", vec![criterion("C001", "pass")]), goal("G002", "in_progress", vec![criterion("C001", "pass")]), goal("G003", "pending", vec![])], vec!["G001", "G002"]));
    expect_code(&repo, CheckpointUlwLoopArgs { goal_id: "G002".into(), status: "complete".into(), evidence: "batch final implementation done".into(), codex_goal_json: Some(snapshot("active")), quality_gate_json: None }, "ULW_LOOP_VALIDATION_BATCH_GATE_REQUIRED");
}

#[test]
fn rejects_pending_member_criteria_with_gate() {
    let repo = repo_with(plan(vec![goal("G001", "complete", vec![criterion("C001", "pass"), criterion("C002", "pending")]), goal("G002", "in_progress", vec![criterion("C001", "pass"), criterion("C002", "pass")]), goal("G003", "pending", vec![])], vec!["G001", "G002"]));
    expect_code(&repo, CheckpointUlwLoopArgs { goal_id: "G002".into(), status: "complete".into(), evidence: "batch final implementation done".into(), codex_goal_json: Some(snapshot("active")), quality_gate_json: Some(gate_json(&repo, 4, 4)) }, "ULW_LOOP_VALIDATION_BATCH_CRITERIA_PENDING");
}

#[test]
fn rejects_mismatched_batch_gate_coverage() {
    let repo = repo_with(plan(vec![goal("G001", "complete", vec![criterion("C001", "pass"), criterion("C002", "pass")]), goal("G002", "in_progress", vec![criterion("C001", "pass"), criterion("C002", "pass")]), goal("G003", "pending", vec![])], vec!["G001", "G002"]));
    expect_code(&repo, CheckpointUlwLoopArgs { goal_id: "G002".into(), status: "complete".into(), evidence: "batch final implementation done".into(), codex_goal_json: Some(snapshot("active")), quality_gate_json: Some(gate_json(&repo, 3, 3)) }, "ULW_LOOP_VALIDATION_BATCH_GATE_MISMATCH");
}

#[test]
fn closes_batch_and_aggregate_when_final_goal_is_both() {
    let repo = repo_with(plan(vec![goal("G001", "complete", vec![criterion("C001", "pass")]), goal("G002", "complete", vec![criterion("C001", "pass")]), goal("G003", "in_progress", vec![criterion("C001", "pass")])], vec!["G002", "G003"]));
    let scope = ulw_loop_core::paths::UlwLoopScope { session_id: Some("s1".into()), ..Default::default() };
    let result = checkpoint_ulw_loop(repo.path(), &CheckpointUlwLoopArgs { goal_id: "G003".into(), status: "complete".into(), evidence: "batch final is also aggregate final".into(), codex_goal_json: Some(snapshot("complete")), quality_gate_json: Some(gate_json(&repo, 2, 2)) }, Some(&scope), None).expect("checkpoint");
    assert_eq!(result.aggregate_completion.expect("aggregate").status, "complete");
    assert_eq!(read_ulw_loop_plan(repo.path(), Some(&scope)).expect("read plan").goals.iter().find(|g| g.id == "G003").expect("G003").status, "complete");
}

#[test]
fn appends_batch_closed_when_final_member_completes() {
    let repo = repo_with(plan(vec![goal("G001", "complete", vec![criterion("C001", "pass")]), goal("G002", "in_progress", vec![criterion("C001", "pass")]), goal("G003", "pending", vec![])], vec!["G001", "G002"]));
    let scope = ulw_loop_core::paths::UlwLoopScope { session_id: Some("s1".into()), ..Default::default() };
    let result = checkpoint_ulw_loop(repo.path(), &CheckpointUlwLoopArgs { goal_id: "G002".into(), status: "complete".into(), evidence: "batch final implementation done".into(), codex_goal_json: Some(snapshot("active")), quality_gate_json: Some(gate_json(&repo, 2, 2)) }, Some(&scope), None).expect("checkpoint");
    assert_eq!(result.goal.status, "complete");
    let ledger = fs::read_to_string(repo.path().join(".omo/ulw-loop/s1/ledger.jsonl")).expect("ledger");
    assert!(ledger.contains("batch_closed"));
}

