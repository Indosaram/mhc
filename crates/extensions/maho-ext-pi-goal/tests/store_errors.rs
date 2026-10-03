use maho_ext_pi_goal::goal::{errors::GoalStoreError, store::{parse_goal_file, read_goal}, types::GoalStoreRef};

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
