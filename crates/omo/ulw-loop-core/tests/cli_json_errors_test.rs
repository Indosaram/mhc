use serde_json::Value;
use tempfile::tempdir;
use ulw_loop_core::cli_arg_parser::parse_codex_goal_json;
use ulw_loop_core::cli_output::json_error;
use ulw_loop_core::cli_subcommands::status;
use ulw_loop_core::runtime::UlwLoopError;

#[test]
fn no_plan_status_json_error_is_stdout_contract() {
    let dir = tempdir().unwrap();
    let error = status(dir.path(), true, Some(&ulw_loop_core::paths::UlwLoopScope::new("json-errors"))).unwrap_err();
    let payload = json_error(&error);
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "ULW_LOOP_PLAN_MISSING");
    assert!(payload["error"]["message"].as_str().unwrap().contains("No ulw-loop plan"));
    assert!(payload["error"].get("details").is_none() || payload["error"]["details"].is_object());
    assert!(payload["error"]["message"].as_str().unwrap().contains("Recovery:"));
}

fn status_plan() -> ulw_loop_core::domain_types::UlwLoopPlan {
    serde_json::from_value(serde_json::json!({"version":1,"createdAt":"2026-05-23T00:00:00Z","updatedAt":"2026-05-23T00:00:00Z","briefPath":"brief","goalsPath":"goals","ledgerPath":"ledger","goals":[]})).expect("valid plan fixture")
}

#[test]
fn status_reads_plan_for_session_id_with_slash() {
    let dir = tempdir().unwrap();
    let scope = ulw_loop_core::paths::UlwLoopScope::new("team/run-1");
    ulw_loop_core::plan_io::write_plan(dir.path(), status_plan(), Some(&scope)).unwrap();
    assert_eq!(status(dir.path(), true, Some(&scope)).unwrap(), 0);
}

#[test]
fn status_reads_plan_present_only_as_revision_log() {
    let dir = tempdir().unwrap();
    let scope = ulw_loop_core::paths::UlwLoopScope::new("revisions-only");
    ulw_loop_core::plan_io::write_plan(dir.path(), status_plan(), Some(&scope)).unwrap();
    std::fs::remove_file(ulw_loop_core::paths::ulw_loop_goals_path(dir.path(), Some(&scope))).unwrap();
    assert_eq!(status(dir.path(), true, Some(&scope)).unwrap(), 0);
}

#[test]
fn no_plan_complete_goals_json_error_is_typed() {
    let error = UlwLoopError::new("No ulw-loop plan at .omo/ulw-loop/session/goals.json.", "ULW_LOOP_PLAN_MISSING");
    let payload = json_error(&error);
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "ULW_LOOP_PLAN_MISSING");
}

#[test]
fn unknown_subcommand_json_error_is_not_help_text() {
    let error = UlwLoopError::new("Unknown ulw-loop subcommand: wat.", "ULW_LOOP_SUBCOMMAND_UNKNOWN");
    let payload = json_error(&error);
    assert_eq!(payload["ok"], false);
    assert!(!payload.to_string().contains("Usage:"));
    assert!(payload["error"]["code"].is_string());
}

#[test]
fn invalid_codex_snapshot_token_surfaces_stable_typed_error() {
    let error = parse_codex_goal_json(Some("not-json-or-a-file")).unwrap_err();
    let payload = json_error(&error);
    assert_eq!(payload["error"]["code"], "ULW_LOOP_CODEX_GOAL_JSON_INVALID");
}

#[test]
fn malformed_required_flag_surfaces_code_and_details() {
    let mut details = std::collections::BTreeMap::new();
    details.insert("flag".into(), Value::String("--goal-id".into()));
    let error = UlwLoopError::with_details("Missing --goal-id.", "ULW_LOOP_ARGUMENT_MISSING", details);
    let payload = json_error(&error);
    assert_eq!(payload["error"]["code"], "ULW_LOOP_ARGUMENT_MISSING");
    assert_eq!(payload["error"]["details"]["flag"], "--goal-id");
}
