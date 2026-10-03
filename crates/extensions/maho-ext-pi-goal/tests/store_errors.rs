use maho_ext_pi_goal::goal::{errors::GoalStoreError, store::{parse_goal_file, read_goal}, types::GoalStoreRef};

#[test]
fn fractional_usage_is_written_without_rounding_then_rejected_at_read_boundary() {
    use maho_ext_pi_goal::goal::{store::*,types::*};
    let root=tempfile::tempdir().expect("fixture");
    let reference=GoalStoreRef{base_dir:root.path().into(),thread_id:"fraction".into()};
    create_goal_at(&reference,"numeric fixture",10,"id".into()).expect("create");
    account_goal_usage_at(&reference,&TokenUsageSnapshot{input:0.25,output:0.5,..Default::default()},1.9,GoalAccountingMode::Active,None,11).expect("account");
    let raw:serde_json::Value=serde_json::from_slice(&std::fs::read(goal_file_path(&reference)).expect("file")).expect("JSON");
    assert_eq!(raw["goal"]["tokensUsed"],serde_json::json!(0.75));
    assert_eq!(raw["goal"]["timeUsedSeconds"].as_f64(),Some(1.0));
    assert!(matches!(read_goal(&reference),Err(GoalStoreError::InvalidGoalStore(_))));
}

#[test]
fn nonfinite_usage_persists_null_fields_then_fails_source_read_validation() {
    use maho_ext_pi_goal::goal::{store::*,types::*};
    let root=tempfile::tempdir().expect("fixture");
    for (name,value) in [("nan",f64::NAN),("infinity",f64::INFINITY)] {
        let reference=GoalStoreRef{base_dir:root.path().into(),thread_id:name.into()};
        create_goal_at(&reference,"numeric fixture",10,"id".into()).expect("create");
        account_goal_usage_at(&reference,&TokenUsageSnapshot{input:value,..Default::default()},value,GoalAccountingMode::Active,None,11).expect("account");
        let raw:serde_json::Value=serde_json::from_slice(&std::fs::read(goal_file_path(&reference)).expect("file")).expect("JSON");
        assert!(raw["goal"]["tokensUsed"].is_null());assert!(raw["goal"]["timeUsedSeconds"].is_null());
        assert!(matches!(read_goal(&reference),Err(GoalStoreError::InvalidGoalStore(_))));
    }
}

#[test]
fn parse_errors_preserve_source_identities() {
    assert!(matches!(parse_goal_file("false"), Err(GoalStoreError::InvalidGoalStore(_))));
    assert!(matches!(parse_goal_file(r#"{"version":2,"goal":null}"#), Err(GoalStoreError::UnsupportedGoalStoreVersion(_))));
    assert!(matches!(parse_goal_file(r#"{"version":1}"#), Err(GoalStoreError::InvalidGoalStore(_))));
    assert!(matches!(parse_goal_file("{"), Err(GoalStoreError::Syntax(_))));
    assert_eq!(parse_goal_file("{").expect_err("invalid JSON").name(), "SyntaxError");
}

#[test]
fn read_distinguishes_missing_file_from_io_failure() {
    let root = tempfile::tempdir().expect("fixture directory");
    let reference = GoalStoreRef { base_dir: root.path().to_owned(), thread_id: "thread".into() };
    assert_eq!(read_goal(&reference).expect("absent store"), None);
    std::fs::create_dir(root.path().join("thread.json")).expect("directory at file path");
    assert!(matches!(read_goal(&reference), Err(GoalStoreError::Io(_))));
}

#[test]
fn mutations_preserve_missing_duplicate_corrupt_and_io_identities() {
    use maho_ext_pi_goal::goal::{store::*, types::*};
    let root = tempfile::tempdir().expect("fixture");
    let reference = GoalStoreRef { base_dir: root.path().join("goals"), thread_id: "thread".into() };
    assert!(matches!(update_goal_at(&reference, &GoalUpdate::default(), GoalUpdateSource::User, 10, "unused".into()), Err(GoalStoreError::GoalNotFound(_))));
    create_goal_at(&reference, "objective", 10, "id".into()).expect("goal");
    assert!(matches!(create_goal_at(&reference, "replacement", 11, "new".into()), Err(GoalStoreError::GoalAlreadyExists(_))));
    std::fs::write(goal_file_path(&reference), "{").expect("corrupt store");
    assert!(matches!(clear_goal(&reference), Err(GoalStoreError::Syntax(_))));
    let blocked = GoalStoreRef { base_dir: root.path().join("blocked"), thread_id: "thread".into() };
    std::fs::write(&blocked.base_dir, "not a directory").expect("blocked directory");
    assert!(matches!(write_goal(&blocked, None), Err(GoalStoreError::Io(_))));
}
