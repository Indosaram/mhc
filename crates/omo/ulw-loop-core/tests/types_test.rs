use ulw_loop_core::constants::*;
use ulw_loop_core::runtime::{iso, UlwLoopError};
use std::sync::Arc;

#[test]
fn ulw_loop_dir_points_to_omo_workspace() {
    assert_eq!(ULW_LOOP_DIR, ".omo/ulw-loop");
}

#[test]
fn artifact_filenames_are_stable() {
    assert_eq!(ULW_LOOP_BRIEF, "brief.md");
    assert_eq!(ULW_LOOP_GOALS, "goals.json");
    assert_eq!(ULW_LOOP_LEDGER, "ledger.jsonl");
}

#[test]
fn steering_kinds_include_revise_criterion() {
    assert!(ULW_LOOP_STEERING_MUTATION_KINDS.contains(&"revise_criterion"));
}

#[test]
fn steering_kinds_total_seven() {
    assert_eq!(ULW_LOOP_STEERING_MUTATION_KINDS.len(), 7);
}

#[test]
fn criterion_user_models_include_adversarial() {
    assert_eq!(
        ULW_LOOP_SUCCESS_CRITERION_USER_MODELS,
        &["happy", "edge", "regression", "adversarial"]
    );
}

#[test]
fn criterion_statuses_expose_pending_pass_fail_blocked() {
    assert_eq!(
        ULW_LOOP_CRITERION_STATUSES,
        &["pending", "pass", "fail", "blocked"]
    );
}

#[test]
fn error_carries_code_and_message() {
    let err = UlwLoopError::new("bad", "TEST_CODE");

    let _: &dyn std::error::Error = &err;
    assert_eq!(err.code, "TEST_CODE");
    assert_eq!(err.message, "bad");
    assert_eq!(err.to_string(), "bad");
}

#[test]
fn error_accepts_optional_details() {
    let details = std::collections::BTreeMap::from([(
        "goalId".to_string(),
        serde_json::json!("G001"),
    )]);
    let cause: Arc<dyn std::error::Error + Send + Sync> = Arc::new(std::io::Error::other("upstream"));
    let err = UlwLoopError::with_cause_and_details("wrap", "WRAP", cause.clone(), details);

    assert!(Arc::ptr_eq(err.cause.as_ref().expect("cause"), &cause));
    assert_eq!(err.code, "WRAP");
    assert_eq!(err.details.as_ref().and_then(|d| d.get("goalId")), Some(&serde_json::json!("G001")));
}

#[test]
fn iso_returns_millisecond_utc_timestamp() {
    let value = iso();

    assert!(value.ends_with('Z'));
    assert_eq!(value.len(), 24);
    assert_eq!(&value[4..5], "-");
    assert_eq!(&value[7..8], "-");
    assert_eq!(&value[10..11], "T");
    assert_eq!(&value[13..14], ":");
    assert_eq!(&value[16..17], ":");
    assert_eq!(&value[19..20], ".");
}
