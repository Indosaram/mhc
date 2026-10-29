use std::fs;

use serde_json::json;
use ulw_loop_core::codex_goal_snapshot::*;

fn options<'a>(expected_objective: &'a str) -> ReconcileCodexGoalOptions<'a> {
    ReconcileCodexGoalOptions {
        expected_objective,
        accepted_objectives: None,
        acknowledged_objectives: None,
    }
}

#[test]
fn parse_returns_available_snapshot_from_goal_json() {
    let snapshot = parse_codex_goal_snapshot(&json!({"goal": {"objective": "X", "status": "active"}}));
    assert!(snapshot.available);
    assert_eq!(snapshot.objective.as_deref(), Some("X"));
    assert_eq!(snapshot.status, CodexGoalSnapshotStatus::Active);
}

#[test]
fn parse_reads_title_bearing_ulw_loop_status_goal() {
    let snapshot = parse_codex_goal_snapshot(&json!({
        "id": "G002-goal-2-senpi-repo-audit-determine-wh",
        "title": "Audit the Senpi repository",
        "status": "in_progress",
        "successCriteria": []
    }));
    assert!(snapshot.available);
    assert_eq!(snapshot.objective.as_deref(), Some("Audit the Senpi repository"));
    assert_eq!(snapshot.status, CodexGoalSnapshotStatus::Active);
}

#[test]
fn parse_ignores_remaining_token_budget_fields() {
    let snapshot = parse_codex_goal_snapshot(&json!({"goal": {"objective": "X", "status": "active"}, "remainingTokens": 123}));
    assert_eq!(snapshot.raw.get("remainingTokens"), Some(&json!(123)));
    assert!(snapshot.objective.is_some());
}

#[test]
fn parse_returns_unavailable_from_null() {
    let snapshot = parse_codex_goal_snapshot(&serde_json::Value::Null);
    assert!(!snapshot.available);
}

#[test]
fn parse_returns_unavailable_from_malformed_payload() {
    let snapshot = parse_codex_goal_snapshot(&json!({"wrong": "shape"}));
    assert!(!snapshot.available);
    assert_eq!(snapshot.status, CodexGoalSnapshotStatus::Unknown);
}

#[test]
fn read_parses_inline_json_string() -> Result<(), Box<dyn std::error::Error>> {
    let snapshot = read_codex_goal_snapshot_input(Some(r#"{"goal":{"objective":"X","status":"active"}}"#), ".")?
        .expect("snapshot");
    assert!(snapshot.available);
    assert_eq!(snapshot.objective.as_deref(), Some("X"));
    Ok(())
}

#[test]
fn read_reads_file_path() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("snap.json");
    fs::write(&path, r#"{"goal":{"objective":"X","status":"complete"}}"#)?;
    let snapshot = read_codex_goal_snapshot_input(Some(path.to_str().expect("utf8 path")), ".")?.expect("snapshot");
    assert!(snapshot.available);
    assert_eq!(snapshot.status, CodexGoalSnapshotStatus::Complete);
    Ok(())
}

#[test]
fn read_reads_sample_fixture_path() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex-goal-snapshot.json");
    let snapshot = read_codex_goal_snapshot_input(Some(path.to_str().expect("utf8 path")), ".")?.expect("snapshot");
    assert!(snapshot.available);
    assert_eq!(snapshot.objective.as_deref(), Some("Complete the durable ulw-loop plan"));
    Ok(())
}

#[test]
fn read_rejects_non_json_nonexistent_path() {
    let error = read_codex_goal_snapshot_input(Some("not json and not a path"), ".").expect_err("invalid input");
    assert_eq!(error.code, "CODEX_GOAL_SNAPSHOT_ERROR");
}

#[test]
fn reconcile_preserves_objective_whitespace_in_warning() {
    let expected = "  exact   objective\nwith spacing  ";
    let snapshot = parse_codex_goal_snapshot(&json!({"goal": {"objective": "different", "status": "active"}}));
    let result = reconcile_codex_goal_snapshot(Some(&snapshot), &options(expected));
    assert!(result.warnings[0].contains(&format!("expected \"{expected}\"")));
}

#[test]
fn reconcile_returns_ok_when_snapshot_matches() {
    let snapshot = parse_codex_goal_snapshot(&json!({"goal": {"objective": "X", "status": "active"}}));
    let result = reconcile_codex_goal_snapshot(Some(&snapshot), &options("X"));
    assert!(result.ok);
    assert!(result.errors.is_empty());
}

#[test]
fn reconcile_warns_when_objective_mismatches() {
    let snapshot = parse_codex_goal_snapshot(&json!({"goal": {"objective": "X", "status": "active"}}));
    let result = reconcile_codex_goal_snapshot(Some(&snapshot), &options("Y"));
    assert!(result.ok);
    assert!(result.errors.is_empty());
    assert!(result.warnings.join(" ").contains("driver_objective_differs"));
    assert!(result.next_actions.is_empty());
    assert_eq!(result.unacknowledged_objective.as_deref(), Some("X"));
}

#[test]
fn reconcile_silences_acknowledged_differing_objective() {
    let snapshot = parse_codex_goal_snapshot(&json!({"goal": {"objective": " X  \n", "status": "active"}}));
    let acknowledged = ["X".to_string()];
    let result = reconcile_codex_goal_snapshot(Some(&snapshot), &ReconcileCodexGoalOptions {
        acknowledged_objectives: Some(&acknowledged),
        ..options("Y")
    });
    assert!(result.warnings.is_empty());
    assert!(result.next_actions.is_empty());
    assert!(result.unacknowledged_objective.is_none());
}

#[test]
fn reconcile_missing_snapshot_requests_create_goal() {
    let result = reconcile_codex_goal_snapshot(None, &options("Y"));
    assert!(result.next_actions.join(" ").contains("create_goal"));
    assert!(result.warnings.is_empty());
}

#[test]
fn reconcile_completed_or_limited_driver_advises_next_action() {
    let completed = parse_codex_goal_snapshot(&json!({"goal": {"objective": "Y", "status": "complete"}}));
    let limited = parse_codex_goal_snapshot(&json!({"goal": {"objective": "Y", "status": "usage_limited"}}));
    let completed_result = reconcile_codex_goal_snapshot(Some(&completed), &options("Y"));
    let limited_result = reconcile_codex_goal_snapshot(Some(&limited), &options("Y"));
    assert!(completed_result.next_actions.join(" ").contains("create_goal"));
    assert!(limited_result.next_actions.join(" ").contains("/goal resume"));
    assert!(completed_result.warnings.is_empty());
    assert!(limited_result.warnings.is_empty());
}

#[test]
fn reconcile_accepts_snapshot_without_objective_as_advisory() {
    let snapshot = parse_codex_goal_snapshot(&json!({"goal": {"status": "active"}}));
    let result = reconcile_codex_goal_snapshot(Some(&snapshot), &options("X"));
    assert!(result.ok);
    assert!(result.errors.is_empty());
}

#[test]
fn reconcile_accepts_limited_driver_statuses() {
    let snapshot = parse_codex_goal_snapshot(&json!({"goal": {"objective": "X", "status": "budget_limited"}}));
    let result = reconcile_codex_goal_snapshot(Some(&snapshot), &options("X"));
    assert!(result.ok);
    assert!(result.errors.is_empty());
    assert!(result.next_actions.join(" ").contains("/goal resume"));
    assert!(result.warnings.is_empty());
}

#[test]
fn format_renders_reconciliation_messages() {
    let snapshot = parse_codex_goal_snapshot(&json!({"goal": {"objective": "X", "status": "active"}}));
    let result = reconcile_codex_goal_snapshot(Some(&snapshot), &options("Y"));
    let formatted = format_codex_goal_reconciliation(&result);
    assert!(formatted.to_ascii_lowercase().contains("objective") || formatted.to_ascii_lowercase().contains("status"));
}
