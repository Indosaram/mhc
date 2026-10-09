use maho_omo_ulw_loop::omo_command::run_omo_status;

#[tokio::test]
async fn missing_plan_returns_failure() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let result = run_omo_status(root.path()).await;
    assert_eq!(result.code, 1);
    let val: serde_json::Value = serde_json::from_str(&result.stdout)?;
    assert_eq!(val["ok"], false);
    assert_eq!(val["error"]["code"], "ULW_LOOP_PLAN_MISSING");
    Ok(())
}

#[tokio::test]
async fn valid_plan_returns_success() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let plan_dir = root.path().join(".omo/ulw-loop");
    std::fs::create_dir_all(&plan_dir)?;
    std::fs::write(
        plan_dir.join("goals.json"),
        r#"{"version":1,"revision":1,"goals":[],"goalsPath":".omo/ulw-loop/goals.json","ledgerPath":".omo/ulw-loop/ledger.jsonl","briefPath":".omo/ulw-loop/brief.md","createdAt":"","updatedAt":""}"#,
    )?;
    let result = run_omo_status(root.path()).await;
    assert_eq!(result.code, 0);
    let val: serde_json::Value = serde_json::from_str(&result.stdout)?;
    assert_eq!(val["ok"], true);
    assert_eq!(val["plan"]["version"], 1);
    Ok(())
}

#[tokio::test]
async fn corrupt_plan_returns_failure() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let plan_dir = root.path().join(".omo/ulw-loop");
    std::fs::create_dir_all(&plan_dir)?;
    std::fs::write(plan_dir.join("goals.json"), "invalid json content")?;
    let result = run_omo_status(root.path()).await;
    assert_eq!(result.code, 1);
    let val: serde_json::Value = serde_json::from_str(&result.stdout)?;
    assert_eq!(val["ok"], false);
    assert_eq!(val["error"]["code"], "ULW_LOOP_PLAN_MISSING");
    Ok(())
}
