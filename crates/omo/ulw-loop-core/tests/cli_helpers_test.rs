use std::fs;

use serde_json::json;
use tempfile::tempdir;
use ulw_loop_core::cli_arg_parser::{
    has_flag, parse_goal_arg, parse_record_evidence_args, positional_text, read_json_input,
    read_repeated, read_value,
};
use ulw_loop_core::cli_output::{normalize_codex_goal_mode, render_status, ULW_LOOP_HELP};
use ulw_loop_core::domain_types::{UlwLoopItem, UlwLoopPlan, UlwLoopSuccessCriterion};

const NOW: &str = "2026-05-23T00:00:00.000Z";

fn criterion(id: &str, status: &str) -> UlwLoopSuccessCriterion {
    UlwLoopSuccessCriterion {
        id: id.into(),
        scenario: "happy path returns 200".into(),
        user_model: "happy".into(),
        expected_evidence: "HTTP 200".into(),
        essential: None,
        captured_evidence: (status == "pass").then(|| format!("{id} passed")),
        status: status.into(),
        captured_at: None,
        notes: None,
        artifacts: None,
    }
}

fn goal(id: &str, criteria: Vec<UlwLoopSuccessCriterion>) -> UlwLoopItem {
    UlwLoopItem {
        id: id.into(),
        title: "Auth endpoint".into(),
        objective: "Build JWT auth".into(),
        status: "in_progress".into(),
        success_criteria: criteria,
        attempt: 1,
        created_at: NOW.into(),
        updated_at: NOW.into(),
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

fn plan(goals: Vec<UlwLoopItem>) -> UlwLoopPlan {
    UlwLoopPlan {
        version: 1,
        revision: None,
        brief: None,
        ledger_reset_revision: None,
        evidence_layout_version: None,
        created_at: NOW.into(),
        updated_at: NOW.into(),
        brief_path: ".omo/ulw-loop/brief.md".into(),
        goals_path: ".omo/ulw-loop/goals.json".into(),
        ledger_path: ".omo/ulw-loop/ledger.jsonl".into(),
        codex_goal_mode: None,
        codex_objective: None,
        codex_objective_aliases: None,
        acknowledged_driver_objectives: None,
        aggregate_completion: None,
        active_goal_id: Some("G001".into()),
        validation_batches: None,
        goals,
    }
}

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn returns_true_for_present_flag() {
    assert!(has_flag(&args(&["status", "--json"]), "--json"));
}

#[test]
fn returns_false_otherwise() {
    assert!(!has_flag(&args(&["status"]), "--json"));
}

#[test]
fn returns_value_after_flag() {
    assert_eq!(read_value(&args(&["criteria", "--goal-id", "G001"]), "--goal-id"), Some("G001".into()));
}

#[test]
fn returns_none_when_absent() {
    assert_eq!(read_value(&args(&["criteria"]), "--goal-id"), None);
}

#[test]
fn returns_none_when_flag_has_no_following_value() {
    assert_eq!(read_value(&args(&["criteria", "--goal-id"]), "--goal-id"), None);
}

#[test]
fn collects_all_occurrences() {
    assert_eq!(
        read_repeated(&args(&["create-goals", "--goal", "A", "--goal=B"]), "--goal"),
        vec!["A", "B"]
    );
}

#[test]
fn returns_value_of_goal_id_or_goal() {
    assert_eq!(parse_goal_arg(&args(&["criteria", "--goal", "G002"])), Some("G002".into()));
    assert_eq!(parse_goal_arg(&args(&["criteria", "--goal-id", "G001"])), Some("G001".into()));
}

#[test]
fn returns_joined_positional_args_after_subcommand() {
    assert_eq!(
        positional_text(&args(&["create-goals", "Build", "auth", "--json", "--brief", "ignored"])),
        "Build auth"
    );
}

#[test]
fn keeps_help_as_brief_text_when_it_is_not_the_subcommand() {
    assert_eq!(positional_text(&args(&["help", "center", "redesign"])), "help center redesign");
}

#[test]
fn parses_inline_json_when_value_looks_like_json() {
    assert_eq!(read_json_input(Some(r#"{"ok":true}"#)).unwrap(), Some(json!({"ok": true})));
}

#[test]
fn reads_from_file_path() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("input.json");
    fs::write(&file, r#"{"fromFile":true}"#).unwrap();
    assert_eq!(read_json_input(file.to_str()).unwrap(), Some(json!({"fromFile": true})));
}

#[test]
fn returns_none_when_value_is_undefined() {
    assert_eq!(read_json_input(None).unwrap(), None);
}

#[test]
fn parses_goal_criterion_status_and_evidence() {
    let parsed = parse_record_evidence_args(&args(&[
        "record-evidence", "--goal-id", "G001", "--criterion-id", "C001", "--status", "pass", "--evidence",
        "curl 200",
    ]))
    .unwrap();
    assert_eq!(parsed.goal_id, "G001");
    assert_eq!(parsed.criterion_id, "C001");
    assert_eq!(parsed.status, "pass");
    assert_eq!(parsed.evidence, "curl 200");
}

#[test]
fn throws_when_goal_id_missing() {
    let result = parse_record_evidence_args(&args(&[
        "record-evidence", "--criterion-id", "C001", "--status", "pass", "--evidence", "x",
    ]));
    match result {
        Ok(_) => panic!("expected missing goal id error"),
        Err(error) => assert_eq!(error.code, "ULW_LOOP_GOAL_ID_REQUIRED"),
    }
}

#[test]
fn throws_when_status_is_not_pass_fail_or_blocked() {
    let result = parse_record_evidence_args(&args(&[
        "record-evidence", "--goal-id", "G001", "--criterion-id", "C001", "--status", "skip", "--evidence", "x",
    ]));
    match result {
        Ok(_) => panic!("expected invalid status error"),
        Err(error) => assert_eq!(error.code, "ULW_LOOP_EVIDENCE_STATUS_INVALID"),
    }
}

#[test]
fn includes_optional_notes_when_present() {
    let parsed = parse_record_evidence_args(&args(&[
        "record-evidence", "--goal-id", "G001", "--criterion-id", "C001", "--status", "blocked", "--evidence",
        "auth missing", "--notes", "waiting",
    ]))
    .unwrap();
    assert_eq!(parsed.notes.as_deref(), Some("waiting"));
}

#[test]
fn help_mentions_cli_and_every_subcommand() {
    for command in [
        "mhc ulw-loop", "create-goals", "complete-goals", "status", "checkpoint", "steer",
        "record-evidence", "criteria", "add-goal", "record-review-blockers",
    ] {
        assert!(ULW_LOOP_HELP.contains(command), "missing {command}");
    }
}

#[test]
fn shows_criteria_pass_total_per_goal() {
    let output = render_status(&plan(vec![goal(
        "G001",
        vec![criterion("C001", "pass"), criterion("C002", "pass"), criterion("C003", "pending")],
    )]));
    assert!(output.contains("criteria: 2/3"));
}

#[test]
fn shows_aggregate_counts() {
    let output = render_status(&plan(vec![
        goal("G001", vec![criterion("C001", "pass"), criterion("C002", "pass"), criterion("C003", "pending")]),
        goal("G002", vec![criterion("C001", "pass")]),
    ]));
    assert!(output.contains("total goals: 2"));
    assert!(output.contains("criteria: 3/4 pass"));
}

#[test]
fn returns_aggregate_when_mode_is_undefined() {
    assert_eq!(normalize_codex_goal_mode(None).unwrap(), "aggregate");
}

#[test]
fn returns_explicit_mode_when_valid() {
    assert_eq!(normalize_codex_goal_mode(Some("per_story")).unwrap(), "per_story");
}

#[test]
fn throws_when_mode_is_invalid() {
    let error = normalize_codex_goal_mode(Some("per-story")).unwrap_err();
    assert_eq!(error.code, "ULW_LOOP_CODEX_GOAL_MODE_INVALID");
}
