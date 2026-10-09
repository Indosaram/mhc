use ulw_loop_core::domain_types::*;
use ulw_loop_core::plan_io::*;

fn sample_plan() -> UlwLoopPlan {
    UlwLoopPlan {
        version: 1,
        revision: Some(1),
        brief: Some("sample brief".into()),
        ledger_reset_revision: None,
        evidence_layout_version: Some(2),
        goals_path: ".omo/ulw-loop/goals.json".into(),
        ledger_path: ".omo/ulw-loop/ledger.jsonl".into(),
        brief_path: ".omo/ulw-loop/brief.md".into(),
        created_at: "2026-05-23T00:00:00Z".into(),
        updated_at: "2026-05-23T00:00:00Z".into(),
        active_goal_id: None,
        goals: vec![],
        codex_objective: None,
        codex_objective_aliases: None,
        acknowledged_driver_objectives: None,
        aggregate_completion: None,
        validation_batches: None,
        codex_goal_mode: None,
    }
}

#[test]
fn test_write_and_read_plan() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let plan = sample_plan();
    write_plan(tmp.path(), plan.clone(), None)?;

    let read_back = read_ulw_loop_plan(tmp.path(), None)?;
    assert_eq!(read_back.brief, plan.brief);
    assert_eq!(read_back.version, plan.version);
    Ok(())
}

#[test]
fn test_read_nonexistent_plan_fails() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(read_ulw_loop_plan(tmp.path(), None).is_err());
}
