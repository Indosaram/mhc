pub mod support;
use maho_omo_ulw_loop::footer_status::*;
#[test] fn all_four_frames_publish_then_clear()->Result<(),Box<dyn std::error::Error>> {let root=tempfile::tempdir()?;let path=root.path().join("goal.json");std::fs::write(&path,r#"{"version":1,"goal":{"status":"active"}}"#)?;let ui=std::sync::Arc::new(support::TestUi::default());let mut footer=FooterStatus::default();footer.sync(Some(FooterRuntime{ui:ui.clone(),goal_paths:vec![path]}),true);for _ in 0..3{footer.tick();}footer.dispose();let calls=ui.0.lock().expect("status");assert_eq!(&calls[..4],&ULW_LOOP_FOOTER_FRAMES.map(|s|Some(s.to_string())));assert_eq!(calls[4],None);Ok(())}
#[tokio::test] async fn shared_timer_starts_and_disposes_without_waiting()->Result<(),Box<dyn std::error::Error>> {let root=tempfile::tempdir()?;let path=root.path().join("goal.json");std::fs::write(&path,r#"{"version":1,"goal":{"status":"active"}}"#)?;let mut ctx=support::context();ctx.has_ui=true;ctx.goal_store_file=Some(path);let footer=std::sync::Arc::new(std::sync::Mutex::new(FooterStatus::default()));sync_shared(&footer,&ctx,true);assert!(footer.lock().expect("footer").running);footer.lock().expect("footer").dispose();assert!(!footer.lock().expect("footer").running);Ok(())}
#[test] fn active_goal_starts_and_dispose_stops()->Result<(),Box<dyn std::error::Error>> { let root=tempfile::tempdir()?;let path=root.path().join("goal.json");std::fs::write(&path,r#"{"version":1,"goal":{"status":"active"}}"#)?;let mut footer=FooterStatus::default();footer.sync(Some(FooterRuntime{ui:support::context().ui,goal_paths:vec![path]}),true);assert!(footer.running);footer.tick();assert!(footer.running);footer.dispose();assert!(!footer.running);Ok(()) }
#[test] fn completed_goal_stops_on_tick()->Result<(),Box<dyn std::error::Error>> { let root=tempfile::tempdir()?;let path=root.path().join("goal.json");std::fs::write(&path,r#"{"version":1,"goal":{"status":"active"}}"#)?;let mut footer=FooterStatus::default();footer.sync(Some(FooterRuntime{ui:support::context().ui,goal_paths:vec![path.clone()]}),true);std::fs::write(path,r#"{"version":1,"goal":{"status":"complete"}}"#)?;footer.tick();assert!(!footer.running);Ok(()) }
#[test] fn missing_goal_never_starts() { let mut footer=FooterStatus::default();footer.sync(Some(FooterRuntime{ui:support::context().ui,goal_paths:vec![]}),true);assert!(!footer.running); }
#[test] fn first_valid_goal_has_priority()->Result<(),Box<dyn std::error::Error>> { let root=tempfile::tempdir()?;let first=root.path().join("first");let second=root.path().join("second");std::fs::write(&first,r#"{"version":1,"goal":{"status":"complete"}}"#)?;std::fs::write(&second,r#"{"version":1,"goal":{"status":"active"}}"#)?;assert!(!goal_active(&FooterRuntime{ui:support::context().ui,goal_paths:vec![first,second]}));Ok(()) }

#[test]
fn headless_context_does_not_publish_or_start_footer() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("goal.json");
    std::fs::write(&path, r#"{"version":1,"goal":{"status":"active"}}"#)?;
    let ui = std::sync::Arc::new(support::TestUi::default());
    let mut ctx = support::context();
    ctx.ui = ui.clone();
    ctx.goal_store_file = Some(path);
    ctx.has_ui = false;
    let mut footer = FooterStatus::default();
    footer.sync_context(&ctx, true);
    assert!(!footer.running);
    assert!(ui.0.lock().expect("status").is_empty());
    Ok(())
}

#[tokio::test]
async fn session_scoped_goal_path_resolves_from_cwd() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let goal_dir = root.path().join(".omo/goal");
    std::fs::create_dir_all(&goal_dir)?;
    std::fs::write(goal_dir.join("session.json"), r#"{"version":1,"goal":{"status":"active"}}"#)?;
    let mut ctx = support::context();
    ctx.has_ui = true;
    ctx.cwd = root.path().into();
    ctx.goal_store_file = None;
    let footer = std::sync::Arc::new(std::sync::Mutex::new(FooterStatus::default()));
    sync_shared(&footer, &ctx, true);
    assert!(footer.lock().expect("footer").running);
    footer.lock().expect("footer").dispose();
    Ok(())
}
