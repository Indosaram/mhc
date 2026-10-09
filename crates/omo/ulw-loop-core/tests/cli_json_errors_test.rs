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
    assert_ne!(payload.to_string().contains("Usage:"), true);
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
