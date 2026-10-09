use std::collections::BTreeMap;
use ulw_loop_core::paths::{
    resolve_ulw_loop_session_id_from_env, ulw_loop_attempt_evidence_dir, ulw_loop_dir,
    ulw_loop_goals_path, UlwLoopScope,
};

#[test]
fn create_scope_requires_session_id() {
    assert_eq!(resolve_ulw_loop_session_id_from_env(&BTreeMap::new()), None);
}

#[test]
fn legacy_root_plan_is_not_selected_by_session_resolution() {
    let mut env = BTreeMap::new();
    env.insert("PI_SESSION_ID".into(), "other-session".into());
    assert_eq!(resolve_ulw_loop_session_id_from_env(&env), Some("other-session".into()));
    assert!(ulw_loop_goals_path("/repo", Some(&UlwLoopScope::new("other-session"))).ends_with("other-session/goals.json"));
}

#[test]
fn session_scope_help_contract_uses_session_flag_and_env_keys() {
    let mut env = BTreeMap::new();
    env.insert("PI_SESSION_ID".into(), "env-session".into());
    assert_eq!(resolve_ulw_loop_session_id_from_env(&env), Some("env-session".into()));
}

#[test]
fn unscoped_root_directory_remains_available_for_help() {
    assert!(ulw_loop_dir("/repo", None).ends_with(".omo/ulw-loop"));
}

#[test]
fn status_scope_resolves_to_session_directory() {
    let scope = UlwLoopScope::new("env-session");
    assert!(ulw_loop_dir("/repo", Some(&scope)).ends_with(".omo/ulw-loop/env-session"));
}

#[test]
fn attempt_dir_rejects_missing_scope() {
    let error = ulw_loop_attempt_evidence_dir("G001", 1, None).unwrap_err();
    assert_eq!(error.code, "ULW_LOOP_SESSION_SCOPE_REQUIRED");
    let error = ulw_loop_attempt_evidence_dir("G001", 1, Some(&UlwLoopScope::new("  "))).unwrap_err();
    assert_eq!(error.code, "ULW_LOOP_SESSION_SCOPE_REQUIRED");
}

#[test]
fn attempt_dir_lives_under_session_scope() {
    assert_eq!(
        ulw_loop_attempt_evidence_dir("G001", 2, Some(&UlwLoopScope::new("s1"))).unwrap(),
        ".omo/evidence/ulw/s1/G001/a2"
    );
}
