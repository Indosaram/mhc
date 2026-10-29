use std::collections::BTreeMap;
use std::path::PathBuf;

use ulw_loop_core::cli_commands::{is_ulw_loop_subcommand, ULW_LOOP_SUBCOMMANDS};
use ulw_loop_core::cli_output::{subcommand_help, ULW_LOOP_HELP};
use ulw_loop_core::paths::{resolve_ulw_loop_session_id_from_env, ulw_loop_dir};

#[test]
fn package_declares_native_ulw_loop_surface() {
    assert!(ULW_LOOP_HELP.contains("ulw-loop"));
    assert!(ULW_LOOP_HELP.contains("create-goals"));
}

#[test]
fn package_exposes_expected_help_command() {
    assert!(is_ulw_loop_subcommand("help"));
    assert!(ULW_LOOP_SUBCOMMANDS.contains(&"help"));
}

#[test]
fn package_exposes_create_goals_command() {
    assert!(is_ulw_loop_subcommand("create-goals"));
    assert!(subcommand_help("create-goals").contains("create-goals"));
}

#[test]
fn package_exposes_status_command() {
    assert!(is_ulw_loop_subcommand("status"));
    assert!(subcommand_help("status").contains("status"));
}

#[test]
fn package_exposes_complete_goals_command() {
    assert!(is_ulw_loop_subcommand("complete-goals"));
    assert!(subcommand_help("complete-goals").contains("complete-goals"));
}

#[test]
fn package_exposes_checkpoint_command() {
    assert!(is_ulw_loop_subcommand("checkpoint"));
    assert!(subcommand_help("checkpoint").contains("checkpoint"));
}

#[test]
fn package_exposes_steer_command() {
    assert!(is_ulw_loop_subcommand("steer"));
    assert!(subcommand_help("steer").contains("steer"));
}

#[test]
fn package_exposes_add_goal_command() {
    assert!(is_ulw_loop_subcommand("add-goal"));
    assert!(subcommand_help("add-goal").contains("add-goal"));
}

#[test]
fn package_exposes_criteria_command() {
    assert!(is_ulw_loop_subcommand("criteria"));
    assert!(subcommand_help("criteria").contains("criteria"));
}

#[test]
fn package_exposes_record_evidence_command() {
    assert!(is_ulw_loop_subcommand("record-evidence"));
    assert!(subcommand_help("record-evidence").contains("record-evidence"));
}

#[test]
fn package_exposes_review_blockers_command() {
    assert!(is_ulw_loop_subcommand("record-review-blockers"));
    assert!(subcommand_help("record-review-blockers").contains("record-review-blockers"));
}

#[test]
fn package_requires_session_scope_for_state() {
    assert!(ULW_LOOP_HELP.contains("Every state subcommand needs a session scope"));
    assert!(ULW_LOOP_HELP.contains("PI_SESSION_ID"));
}

#[test]
fn package_scopes_state_under_session_directory() {
    assert_eq!(ulw_loop_dir("/repo", Some(&ulw_loop_core::paths::UlwLoopScope::new("session"))), PathBuf::from("/repo/.omo/ulw-loop/session"));
}

#[test]
fn package_resolves_pi_session_environment() {
    let mut env = BTreeMap::new();
    env.insert("PI_SESSION_ID".into(), "session-package".into());
    assert_eq!(resolve_ulw_loop_session_id_from_env(&env), Some("session-package".into()));
}

