use ulw_loop_core::command_types::CreateGoalsOptions;
use ulw_loop_core::plan_crud::create_ulw_loop_plan;
use ulw_loop_core::plan_io::read_ulw_loop_plan;

fn create_options(brief: &str, batch: impl Into<String>) -> CreateGoalsOptions {
    CreateGoalsOptions {
        brief: brief.into(),
        goals: None,
        codex_goal_mode: None,
        force: None,
        validation_batches_json: Some(batch.into()),
    }
}

#[test]
fn test_valid_validation_batch_is_stored_on_plan() {
    let repo = tempfile::tempdir().expect("tempdir");
    let plan = create_ulw_loop_plan(
        repo.path(),
        &create_options(
            "- Goal alpha\n- Goal beta\n- Goal gamma",
            r#"[{"batchId":"VB001","memberIds":["G001-goal-alpha","G002-goal-beta"],"finalGoalId":"G002-goal-beta"}]"#,
        ),
        None,
        None,
    )
    .expect("create plan");

    assert_eq!(plan.validation_batches.expect("batches")[0].batch_id, "VB001");
}

#[test]
fn test_unknown_member_id_rejects_with_member_unknown_code() {
    let repo = tempfile::tempdir().expect("tempdir");
    let error = create_ulw_loop_plan(
        repo.path(),
        &create_options(
            "- Goal alpha\n- Goal beta",
            r#"[{"batchId":"VB001","memberIds":["G001-goal-alpha","missing"],"finalGoalId":"G001-goal-alpha"}]"#,
        ),
        None,
        None,
    )
    .expect_err("unknown member should fail");

    assert_eq!(error.code, "ULW_LOOP_VALIDATION_BATCH_MEMBER_UNKNOWN");
}

#[test]
fn test_final_goal_outside_members_rejects_with_final_not_member_code() {
    let repo = tempfile::tempdir().expect("tempdir");
    let error = create_ulw_loop_plan(
        repo.path(),
        &create_options(
            "- Goal alpha\n- Goal beta\n- Goal gamma",
            r#"[{"batchId":"VB001","memberIds":["G001-goal-alpha","G002-goal-beta"],"finalGoalId":"G003-goal-gamma"}]"#,
        ),
        None,
        None,
    )
    .expect_err("final outside members should fail");

    assert_eq!(error.code, "ULW_LOOP_VALIDATION_BATCH_FINAL_NOT_MEMBER");
}

#[test]
fn test_overlapping_validation_batches_reject_with_overlap_code() {
    let repo = tempfile::tempdir().expect("tempdir");
    let error = create_ulw_loop_plan(
        repo.path(),
        &create_options(
            "- Goal alpha\n- Goal beta\n- Goal gamma",
            r#"[{"batchId":"VB001","memberIds":["G001-goal-alpha","G002-goal-beta"],"finalGoalId":"G002-goal-beta"},{"batchId":"VB002","memberIds":["G002-goal-beta","G003-goal-gamma"],"finalGoalId":"G003-goal-gamma"}]"#,
        ),
        None,
        None,
    )
    .expect_err("overlap should fail");

    assert_eq!(error.code, "ULW_LOOP_VALIDATION_BATCH_OVERLAP");
}

#[test]
fn test_duplicate_members_retain_structural_invalid_code() {
    let repo = tempfile::tempdir().expect("tempdir");
    let error = create_ulw_loop_plan(
        repo.path(),
        &create_options(
            "- Goal alpha\n- Goal beta",
            r#"[{"batchId":"VB001","memberIds":["G001-goal-alpha","G001-goal-alpha"],"finalGoalId":"G001-goal-alpha"}]"#,
        ),
        None,
        None,
    )
    .expect_err("duplicate members should fail");

    assert_eq!(error.code, "ULW_LOOP_VALIDATION_BATCH_INVALID");
}

#[test]
fn test_validation_batch_file_is_read_from_disk() {
    let repo = tempfile::tempdir().expect("tempdir");
    let file = repo.path().join("batches.json");
    std::fs::write(&file, r#"[{"batchId":"VB001","memberIds":["G001-goal-alpha","G002-goal-beta"],"finalGoalId":"G002-goal-beta"}]"#).expect("write batches");
    create_ulw_loop_plan(
        repo.path(),
        &create_options("- Goal alpha\n- Goal beta", file.to_string_lossy()),
        None,
        None,
    )
    .expect("create plan");

    let stored = read_ulw_loop_plan(repo.path(), None).expect("read plan");
    assert_eq!(stored.validation_batches.expect("batches")[0].batch_id, "VB001");
}
