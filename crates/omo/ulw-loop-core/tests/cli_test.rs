use ulw_loop_core::cli_commands::*;
use ulw_loop_core::cli_output::*;

#[test]
fn test_ulw_loop_help() {
    assert!(ULW_LOOP_HELP.contains("create-goals"));
    assert!(ULW_LOOP_HELP.contains("complete-goals"));
    assert!(ULW_LOOP_HELP.contains("status"));
    assert!(ULW_LOOP_HELP.contains("checkpoint"));
    assert!(ULW_LOOP_HELP.contains("steer"));
    assert!(ULW_LOOP_HELP.contains("record-evidence"));
    assert!(ULW_LOOP_HELP.contains("criteria"));
    assert!(ULW_LOOP_HELP.contains("add-goal"));
    assert!(ULW_LOOP_HELP.contains("record-review-blockers"));
}

#[test]
fn test_subcommand_help() {
    let help = subcommand_help("status");
    assert!(help.contains("ulw-loop status"));
}

#[test]
fn test_is_ulw_loop_subcommand() {
    assert!(is_ulw_loop_subcommand("create-goals"));
    assert!(is_ulw_loop_subcommand("status"));
    assert!(is_ulw_loop_subcommand("checkpoint"));
    assert!(!is_ulw_loop_subcommand("unknown-cmd"));
}

#[test]
fn test_normalize_codex_goal_mode() {
    assert_eq!(normalize_codex_goal_mode(None).unwrap(), "aggregate");
    assert_eq!(normalize_codex_goal_mode(Some("aggregate")).unwrap(), "aggregate");
    assert_eq!(normalize_codex_goal_mode(Some("per_story")).unwrap(), "per_story");
    assert!(normalize_codex_goal_mode(Some("invalid")).is_err());
}
