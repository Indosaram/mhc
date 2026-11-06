use ulw_loop_core::cli::main_cli;
use ulw_loop_core::cli_output::ULW_LOOP_HELP;

#[test]
fn bare_status_routes_to_ulw_loop_surface() {
    assert!(ULW_LOOP_HELP.contains("status"));
}
#[test]
fn legacy_ulw_loop_status_routes_to_same_surface() {
    assert_eq!(main_cli(&["ulw-loop".into(), "help".into()]), 0);
}
#[test]
fn unscoped_status_is_rejected_by_scope_policy() {
    assert_eq!(
        main_cli(&["ulw-loop".into(), "status".into(), "--json".into()]),
        1
    );
}
#[test]
fn top_level_help_prints_usage() {
    assert_eq!(main_cli(&["help".into()]), 0);
    assert!(ULW_LOOP_HELP.contains("Usage:"));
}
#[test]
fn unknown_command_fails() {
    assert_eq!(main_cli(&["frobnicate".into()]), 1);
}
