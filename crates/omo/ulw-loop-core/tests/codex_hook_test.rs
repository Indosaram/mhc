use std::io::Cursor;

use serde_json::{json, Value};
use ulw_loop_core::codex_hook::*;
use ulw_loop_core::domain_types::*;
use ulw_loop_core::plan_io::write_plan;
use ulw_loop_core::paths::{ulw_loop_dir, ulw_loop_ledger_path};
use ulw_loop_core::paths::UlwLoopScope;

fn bootstrap_plan_repo() -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("tempdir");
    let scope = UlwLoopScope::new("s1");
    std::fs::create_dir_all(ulw_loop_dir(repo.path(), Some(&scope))).expect("ulw dir");
    write_plan(repo.path(), UlwLoopPlan {
        version: 1,
        revision: None,
        brief: None,
        ledger_reset_revision: None,
        evidence_layout_version: None,
        created_at: "2026-05-23T00:00:00.000Z".into(),
        updated_at: "2026-05-23T00:00:00.000Z".into(),
        brief_path: ".omo/ulw-loop/brief.md".into(),
        goals_path: ".omo/ulw-loop/goals.json".into(),
        ledger_path: ".omo/ulw-loop/ledger.jsonl".into(),
        codex_goal_mode: None,
        codex_objective: None,
        codex_objective_aliases: None,
        acknowledged_driver_objectives: None,
        aggregate_completion: None,
        active_goal_id: None,
        validation_batches: None,
        goals: vec![UlwLoopItem {
            id: "G001".into(), title: "Build hook".into(), objective: "Apply safe steering directives from Codex hooks.".into(), status: "pending".into(),
            success_criteria: vec![], attempt: 0, created_at: "2026-05-23T00:00:00.000Z".into(), updated_at: "2026-05-23T00:00:00.000Z".into(),
            started_at: None, completed_at: None, failed_at: None, review_blocked_at: None, evidence: None, failure_reason: None, steering_status: None,
            superseded_by: None, supersedes: None, blocked_reason: None, blocker_signature: None, blocker_occurrence_count: None,
            required_external_decision: None, non_retriable: None, steering_evidence: None, steering_rationale: None,
        }],
    }, Some(&scope)).expect("plan");
    repo
}

fn steering_prompt(marker: &str) -> String {
    format!(r#"{marker}: {{"kind":"annotate_ledger","source":"user_prompt_submit","evidence":"x","rationale":"y"}}"#)
}

fn payload(prompt: &str, cwd: &str) -> UserPromptSubmitPayload {
    UserPromptSubmitPayload { cwd: cwd.into(), hook_event_name: "UserPromptSubmit".into(), model: None, permission_mode: None, prompt: prompt.into(), session_id: "s1".into(), transcript_path: None, turn_id: None }
}

fn pre_tool_payload(tool_name: &str, tool_input: Value) -> PreToolUsePayload {
    PreToolUsePayload { cwd: "/repo".into(), hook_event_name: "PreToolUse".into(), model: "gpt-5.5".into(), permission_mode: "default".into(), session_id: "s1".into(), tool_input, tool_name: tool_name.into(), tool_use_id: "call-1".into(), transcript_path: None, turn_id: "turn-1".into() }
}

#[test]
fn parse_valid_json_payload() -> Result<(), Box<dyn std::error::Error>> {
    let raw = include_str!("fixtures/user-prompt-submit.json");
    let parsed = parse_user_prompt_submit_payload(raw).expect("payload");
    assert_eq!(parsed.hook_event_name, "UserPromptSubmit");
    assert!(parsed.prompt.contains("OMO_ULW_LOOP_STEER"));
    Ok(())
}

#[test]
fn parse_empty_input_returns_none() { assert!(parse_user_prompt_submit_payload("").is_none()); }

#[test]
fn parse_invalid_json_returns_none() { assert!(parse_user_prompt_submit_payload("{bad").is_none()); }

#[test]
fn parse_missing_hook_event_returns_none() {
    assert!(parse_user_prompt_submit_payload(r#"{"cwd":"/repo","prompt":"x","session_id":"s1"}"#).is_none());
}

#[test]
fn steering_omo_marker_without_plan_is_silent() {
    let out = apply_user_prompt_ulw_loop_steering(&payload("OMO_ULW_LOOP_STEER: {bad", "/tmp"), &Default::default());
    assert_eq!(out, "");
}

#[test]
fn steering_normal_prompt_is_silent() {
    assert_eq!(apply_user_prompt_ulw_loop_steering(&payload("just a normal user message", "/tmp"), &Default::default()), "");
}

#[test]
fn steering_standalone_ulw_is_silent() {
    assert_eq!(apply_user_prompt_ulw_loop_steering(&payload("ulw this change", "/tmp"), &Default::default()), "");
}

#[test]
fn steering_non_user_prompt_event_is_silent() {
    let mut input = payload("OMO_ULW_LOOP_STEER: {bad", "/tmp");
    input.hook_event_name = "PostToolUse".into();
    assert_eq!(apply_user_prompt_ulw_loop_steering(&input, &Default::default()), "");
}

#[test]
fn steering_malformed_directive_with_ultrawork_enabled_does_not_fall_through() {
    let options = UserPromptSubmitHookOptions { include_ultrawork_directive: true, ..Default::default() };
    assert_eq!(apply_user_prompt_ulw_loop_steering(&payload("OMO_ULW_LOOP_STEER: {bad", "/tmp"), &options), "");
}

#[test]
fn steering_missing_plan_returns_empty() {
    let out = apply_user_prompt_ulw_loop_steering(&payload("OMO_ULW_LOOP_STEER: {\"kind\":\"annotate_ledger\",\"source\":\"user_prompt_submit\",\"evidence\":\"x\",\"rationale\":\"y\"}", "/tmp"), &Default::default());
    assert_eq!(out, "");
}

#[test]
fn steering_cli_empty_stdin_writes_nothing() {
    let mut out = Vec::new();
    run_ulw_loop_hook_cli(Cursor::new(""), &mut out, &Default::default());
    assert!(out.is_empty());
}

#[test]
fn budget_guard_token_budget_denies_with_unlimited_warning() {
    let out = apply_pre_tool_use_goal_budget_guard(&pre_tool_payload("create_goal", json!({"objective":"Ship the feature","token_budget":5000})));
    let parsed: Value = serde_json::from_str(&out).expect("json");
    assert_eq!(parsed["hookSpecificOutput"]["permissionDecision"], "deny");
    assert!(parsed["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap().contains("token_budget"));
    assert!(parsed["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap().contains("unlimited"));
    assert!(parsed["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap().contains("update_goal"));
}

#[test]
fn budget_guard_status_denies_with_objective_only_warning() {
    let out = apply_pre_tool_use_goal_budget_guard(&pre_tool_payload("create_goal", json!({"objective":"Ship the feature","status":"active"})));
    let parsed: Value = serde_json::from_str(&out).expect("json");
    assert_eq!(parsed["hookSpecificOutput"]["permissionDecision"], "deny");
    assert!(parsed["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap().contains("update_goal"));
}

#[test]
fn budget_guard_unknown_field_denies() {
    let out = apply_pre_tool_use_goal_budget_guard(&pre_tool_payload("create_goal", json!({"objective":"Ship the feature","statusText":"active"})));
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap()["hookSpecificOutput"]["permissionDecision"], "deny");
}

#[test]
fn budget_guard_objective_only_is_silent() {
    assert_eq!(apply_pre_tool_use_goal_budget_guard(&pre_tool_payload("create_goal", json!({"objective":"Ship the feature"}))), "");
}

#[test]
fn budget_guard_neighboring_tool_is_silent() {
    assert_eq!(apply_pre_tool_use_goal_budget_guard(&pre_tool_payload("update_goal", json!({"status":"complete","token_budget":5000,"statusText":"done"}))), "");
}

#[test]
fn pre_tool_cli_budgeted_create_goal_writes_blocking_json() {
    let input = serde_json::to_string(&pre_tool_payload("create_goal", json!({"objective":"Ship","token_budget":1}))).unwrap();
    let mut out = Vec::new();
    run_pre_tool_use_goal_budget_guard_cli(Cursor::new(input), &mut out);
    let parsed: Value = serde_json::from_slice(&out).expect("json");
    assert_eq!(parsed["hookSpecificOutput"]["permissionDecision"], "deny");
    assert!(parsed["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap().contains("unlimited"));
}

#[test]
fn steering_marker_variants_are_recognized() {
    let repo = bootstrap_plan_repo();
    for marker in ["omo ulw-loop steer", "omo-agent-toolkit ulw-loop steer"] {
        let out = apply_user_prompt_ulw_loop_steering(&payload(&steering_prompt(marker), &repo.path().to_string_lossy()), &Default::default());
        assert!(out.contains("annotate_ledger"));
    }
}

#[test]
fn steering_writes_session_scoped_ledger() {
    let repo = bootstrap_plan_repo();
    let out = apply_user_prompt_ulw_loop_steering(&payload(&steering_prompt("OMO_ULW_LOOP_STEER"), &repo.path().to_string_lossy()), &Default::default());
    assert!(out.contains("accepted"));
    let scope = UlwLoopScope::new("s1");
    let ledger = std::fs::read_to_string(ulw_loop_ledger_path(repo.path(), Some(&scope))).expect("ledger");
    assert!(ledger.contains("steering_accepted"));
}

#[test]
fn steering_omo_marker_succeeds() {
    let repo = bootstrap_plan_repo();
    let out = apply_user_prompt_ulw_loop_steering(&payload(&steering_prompt("OMO_ULW_LOOP_STEER"), &repo.path().to_string_lossy()), &Default::default());
    assert!(out.contains("accepted"));
}

#[test]
fn steering_cli_success_writes_audit() {
    let repo = bootstrap_plan_repo();
    let input = serde_json::to_string(&payload(&steering_prompt("OMO_ULW_LOOP_STEER"), &repo.path().to_string_lossy())).unwrap();
    let mut out = Vec::new();
    run_ulw_loop_hook_cli(Cursor::new(input), &mut out, &Default::default());
    assert!(!out.is_empty());
}

