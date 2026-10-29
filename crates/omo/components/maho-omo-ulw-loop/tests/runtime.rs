use maho_omo_ulw_loop::omo_command::run_omo_status;

#[tokio::test]
async fn in_process_status_queries_real_cwd() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let plan_dir = root.path().join(".omo/ulw-loop/session");
    std::fs::create_dir_all(&plan_dir)?;
    std::fs::write(
        plan_dir.join("goals.json"),
        r#"{"version":1,"revision":1,"goals":[{"id":"G001","title":"G1","objective":"O1","status":"pending","attempt":0,"createdAt":"","updatedAt":"","successCriteria":[]}],"goalsPath":".omo/ulw-loop/goals.json","ledgerPath":".omo/ulw-loop/ledger.jsonl","briefPath":".omo/ulw-loop/brief.md","createdAt":"","updatedAt":""}"#,
    )?;

    let result = run_omo_status(root.path(), Some("session")).await;
    assert_eq!(result.code, 0);
    let val: serde_json::Value = serde_json::from_str(&result.stdout)?;
    assert_eq!(val["ok"], true);
    assert_eq!(val["plan"]["goals"][0]["id"], "G001");
    assert_eq!(val["summary"]["total"], 1);
    assert_eq!(val["summary"]["pending"], 1);
    Ok(())
}

#[tokio::test]
async fn in_process_status_fails_on_empty_dir() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let result = run_omo_status(root.path(), Some("session")).await;
    assert_eq!(result.code, 1);
    let val: serde_json::Value = serde_json::from_str(&result.stdout)?;
    assert_eq!(val["ok"], false);
    assert_eq!(val["error"]["code"], "ULW_LOOP_PLAN_MISSING");
    Ok(())
}
