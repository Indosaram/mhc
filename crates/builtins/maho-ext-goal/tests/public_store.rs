use maho_ext_goal::{GoalStoreRef,GoalStatus,GoalUpdate,GoalUpdateSource,create_goal,read_goal,update_goal,clear_goal,goal_store_ref};

#[tokio::test]
async fn canonical_public_store_preserves_budget_tristate_and_seconds() {
    let temp=tempfile::tempdir().unwrap();
    let session=maho_core::session_manager::SessionManager::create("/workspace",Some(temp.path().to_str().unwrap()),None);
    let reference:GoalStoreRef=goal_store_ref(&session,"/workspace");
    assert_eq!(reference.base_dir,temp.path().join("extensions/goal"));
    assert!(read_goal(&reference).unwrap().is_none());
    for value in [-1.0,0.5,f64::INFINITY,9_007_199_254_740_992.0] { assert!(create_goal(&reference,"invalid",Some(value),123).await.is_err()); assert!(read_goal(&reference).unwrap().is_none()); }
    let created=create_goal(&reference,"work",Some(10.0),123).await.unwrap();
    assert_eq!(created.created_at,123); assert_eq!(created.token_budget,Some(10));
    let preserved=update_goal(&reference,&GoalUpdate::default(),GoalUpdateSource::User,124).await.unwrap();
    assert_eq!(preserved.token_budget,Some(10));
    let cleared=update_goal(&reference,&GoalUpdate { token_budget:Some(None),..Default::default() },GoalUpdateSource::User,125).await.unwrap();
    assert!(cleared.token_budget.is_none());
    let numbered=update_goal(&reference,&GoalUpdate { token_budget:Some(Some(20.0)),..Default::default() },GoalUpdateSource::User,126).await.unwrap();
    assert_eq!(numbered.token_budget,Some(20));
    for value in [-1.0,0.5,f64::INFINITY,9_007_199_254_740_992.0] {
        assert!(update_goal(&reference,&GoalUpdate { token_budget:Some(Some(value)),..Default::default() },GoalUpdateSource::User,127).await.is_err());
    }
    assert_eq!(read_goal(&reference).unwrap(),Some(numbered));
    let completed=update_goal(&reference,&GoalUpdate { status:Some(GoalStatus::Complete),..Default::default() },GoalUpdateSource::Model,128).await.unwrap();
    assert_eq!(completed.completed_at,Some(128));
    assert!(clear_goal(&reference).await.unwrap()); assert!(!clear_goal(&reference).await.unwrap());
    let envelope:serde_json::Value=serde_json::from_str(&std::fs::read_to_string(maho_ext_goal::goal_file_path(&reference)).unwrap()).unwrap();
    assert_eq!(envelope,serde_json::json!({"version":1,"goal":null}));
}
