use maho_omo_ulw_loop::omo_command::run_omo_status;

#[tokio::test]
async fn missing_plan_returns_failure() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let result = run_omo_status(root.path(), Some("session")).await;
    assert_eq!(result.code, 1);
    let val: serde_json::Value = serde_json::from_str(&result.stdout)?;
    assert_eq!(val["ok"], false);
    assert_eq!(val["error"]["code"], "ULW_LOOP_PLAN_MISSING");
    Ok(())
}

#[tokio::test]
async fn valid_plan_returns_success() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let plan_dir = root.path().join(".omo/ulw-loop/session");
    std::fs::create_dir_all(&plan_dir)?;
    std::fs::write(
        plan_dir.join("goals.json"),
        r#"{"version":1,"revision":1,"goals":[],"goalsPath":".omo/ulw-loop/goals.json","ledgerPath":".omo/ulw-loop/ledger.jsonl","briefPath":".omo/ulw-loop/brief.md","createdAt":"","updatedAt":""}"#,
    )?;
    let result = run_omo_status(root.path(), Some("session")).await;
    assert_eq!(result.code, 0);
    let val: serde_json::Value = serde_json::from_str(&result.stdout)?;
    assert_eq!(val["ok"], true);
    assert_eq!(val["plan"]["version"], 1);
    Ok(())
}

#[tokio::test]
async fn corrupt_plan_returns_failure() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let plan_dir = root.path().join(".omo/ulw-loop/session");
    std::fs::create_dir_all(&plan_dir)?;
    std::fs::write(plan_dir.join("goals.json"), "invalid json content")?;
    let result = run_omo_status(root.path(), Some("session")).await;
    assert_eq!(result.code, 1);
    let val: serde_json::Value = serde_json::from_str(&result.stdout)?;
    assert_eq!(val["ok"], false);
    // A goals.json that exists is never a missing plan (plan-io.ts:83-85).
    assert_eq!(val["error"]["code"], "ULW_LOOP_PLAN_INVALID");
    Ok(())
}

#[tokio::test]
async fn shared_root_plan_is_never_read_for_a_session() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let shared = root.path().join(".omo/ulw-loop");
    std::fs::create_dir_all(&shared)?;
    std::fs::write(
        shared.join("goals.json"),
        r#"{"version":1,"revision":1,"goals":[{"id":"G001","title":"G1","objective":"O1","status":"pending","attempt":0,"createdAt":"","updatedAt":"","successCriteria":[]}],"goalsPath":".omo/ulw-loop/goals.json","ledgerPath":".omo/ulw-loop/ledger.jsonl","briefPath":".omo/ulw-loop/brief.md","createdAt":"","updatedAt":""}"#,
    )?;
    let scoped: serde_json::Value = serde_json::from_str(&run_omo_status(root.path(), Some("other-session")).await.stdout)?;
    assert_eq!(scoped["error"]["code"], "ULW_LOOP_PLAN_MISSING");
    let unscoped: serde_json::Value = serde_json::from_str(&run_omo_status(root.path(), None).await.stdout)?;
    assert_eq!(unscoped["error"]["code"], "ULW_LOOP_SESSION_SCOPE_REQUIRED");
    Ok(())
}
