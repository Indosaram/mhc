mod support;
use std::{collections::BTreeMap,sync::Arc};
use maho_ext_api::*;
use maho_omo_config_startup::{ConfigStartupComponent,SenpiStartupMigrationResult};
#[tokio::test] async fn migration_and_config_diagnostics_report_once()->Result<(),Box<dyn std::error::Error>> {
 let root=tempfile::tempdir()?;let home=root.path().join("home");std::fs::create_dir(&home)?;let path=root.path().to_string_lossy().into_owned();let env=BTreeMap::from([("HOME".into(),home.to_string_lossy().into_owned())]);
 let mut api=ExtensionApi::new(LoadedExtension::new("startup",root.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
 ConfigStartupComponent{run_migration:Some(Arc::new(|_|SenpiStartupMigrationResult{migrated_from:vec!["/legacy".into()],results:vec![omo_config_core::MigrationRunResult{diagnostics:vec!["conflict".into()],journal_resumed:false,preview:None,status:omo_config_core::MigrationStatus::Migrated}],..Default::default()})),load_config:Some(Arc::new(move |_| {let mut config=maho_omo_config_resolution::load_senpi_omo_config(omo_config_core::LoadOmoConfigOptions{cwd:Some(path.clone()),env:Some(env.clone()),..Default::default()});config.diagnostics.push(maho_omo_config_resolution::SenpiConfigDiagnostic::Config(omo_config_core::OmoConfigDiagnostic{kind:"parse",path:"/config".into(),message:"diagnostic".into(),issue_paths:vec![]}));config}))}.register(&mut api);
 let notices=Arc::new(std::sync::Mutex::new(Vec::new()));let mut ctx=support::context();ctx.has_ui=true;ctx.ui=Arc::new(support::TestUi(notices.clone()));
 for _ in 0..2 {let mut event=ExtensionEvent::SessionStart(SessionStartEvent{reason:SessionReason::Startup,initial_model_provenance:None,previous_session_file:None});api.registered.handlers[&EventKind::SessionStart][0](&mut event,&ctx).await?;}
 assert_eq!(notices.lock().expect("notices").len(),3);Ok(())
}
#[tokio::test] async fn migration_notice_reports_once_to_ui()->Result<(),Box<dyn std::error::Error>> {
 let root=tempfile::tempdir()?;let path=root.path().to_string_lossy().into_owned();let env=BTreeMap::from([("HOME".into(),path.clone())]);
 let mut api=ExtensionApi::new(LoadedExtension::new("startup",root.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
 ConfigStartupComponent{run_migration:Some(Arc::new(|_|SenpiStartupMigrationResult{error:Some("failure".into()),..Default::default()})),load_config:Some(Arc::new(move |_|maho_omo_config_resolution::load_senpi_omo_config(omo_config_core::LoadOmoConfigOptions{cwd:Some(path.clone()),env:Some(env.clone()),..Default::default()})))}.register(&mut api);
 let notices=Arc::new(std::sync::Mutex::new(Vec::new()));let mut ctx=support::context();ctx.has_ui=true;ctx.ui=Arc::new(support::TestUi(notices.clone()));
 for _ in 0..2 {let mut event=ExtensionEvent::SessionStart(SessionStartEvent{reason:SessionReason::Startup,initial_model_provenance:None,previous_session_file:None});api.registered.handlers[&EventKind::SessionStart][0](&mut event,&ctx).await?;}
 assert_eq!(notices.lock().expect("notices").len(),1);Ok(())
}

#[tokio::test]
async fn real_conflict_diagnostic_reaches_ui_once() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let project = home.join("project");
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(home.join(".config/opencode"))?;
    std::fs::create_dir_all(home.join(".maho"))?;
    std::fs::write(home.join(".config/opencode/oh-my-openagent.jsonc"), r#"{"agents":{"finder":{"model":"provider/legacy"}}}"#)?;
    std::fs::write(home.join(".maho/omo.jsonc"), r#"{"[opencode]":{"agents":{"finder":{"model":"provider/kept"}}}}"#)?;
    let env = BTreeMap::from([("HOME".into(), home.to_string_lossy().into_owned())]);
    let migration_env = env.clone();
    let migration_home = home.to_string_lossy().into_owned();
    let mut api = ExtensionApi::new(LoadedExtension::new("startup", project, SourceInfo::default()),
        ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    ConfigStartupComponent {
        run_migration: Some(Arc::new(move |cwd| maho_omo_config_startup::run_senpi_startup_migration(cwd, &migration_env, &migration_home))),
        load_config: Some(Arc::new(move |cwd| maho_omo_config_resolution::load_senpi_omo_config(omo_config_core::LoadOmoConfigOptions {
            cwd: Some(cwd.into()), env: Some(env.clone()), ..Default::default()
        }))),
    }.register(&mut api);
    let notices = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut ctx = support::context(); ctx.has_ui = true;
    ctx.ui = Arc::new(support::TestUi(notices.clone()));
    for _ in 0..2 {
        let mut event = ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup,
            initial_model_provenance: None, previous_session_file: None });
        api.registered.handlers[&EventKind::SessionStart][0](&mut event, &ctx).await?;
    }
    assert_eq!(notices.lock().expect("notices").len(), 2);
    Ok(())
}

#[tokio::test]
async fn registered_session_start_reports_an_unserved_devin_selector_once() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let mut api = ExtensionApi::new(LoadedExtension::new("startup", root.path().into(), SourceInfo::default()),
        ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    ConfigStartupComponent {
        run_migration: Some(Arc::new(|_| SenpiStartupMigrationResult::default())),
        load_config: Some(Arc::new(|_| maho_omo_config_resolution::SenpiOmoConfigResult {
            loaded: omo_config_core::LoadOmoConfigResult {
                config: serde_json::Map::new(), diagnostics: Vec::new(), layers: Vec::new(),
                profile: None, sources: Vec::new(),
            },
            config: serde_json::json!({ "categories": { "quick": { "model": "devin/swe-2-low" } } }),
            diagnostics: Vec::new(),
        })),
    }.register(&mut api);
    let notices = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut ctx = support::context(); ctx.has_ui = true;
    ctx.ui = Arc::new(support::TestUi(notices.clone()));
    for _ in 0..2 {
        let mut event = ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup,
            initial_model_provenance: None, previous_session_file: None });
        api.registered.handlers[&EventKind::SessionStart][0](&mut event, &ctx).await?;
    }
    let reported = notices.lock().expect("notices");
    assert_eq!(reported.len(), 1, "the Devin clause reports exactly once across repeated SessionStart");
    assert!(reported[0].contains("devin/swe-2-low"));
    assert!(reported[0].contains("categories.quick.model"));
    Ok(())
}

#[tokio::test]
async fn registered_session_start_stays_silent_for_a_served_devin_lane() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let mut api = ExtensionApi::new(LoadedExtension::new("startup", root.path().into(), SourceInfo::default()),
        ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    ConfigStartupComponent {
        run_migration: Some(Arc::new(|_| SenpiStartupMigrationResult::default())),
        load_config: Some(Arc::new(|_| maho_omo_config_resolution::SenpiOmoConfigResult {
            loaded: omo_config_core::LoadOmoConfigResult {
                config: serde_json::Map::new(), diagnostics: Vec::new(), layers: Vec::new(),
                profile: None, sources: Vec::new(),
            },
            config: serde_json::json!({ "categories": { "quick": { "model": "devin/swe-2-medium" } } }),
            diagnostics: Vec::new(),
        })),
    }.register(&mut api);
    let notices = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut ctx = support::context(); ctx.has_ui = true;
    ctx.ui = Arc::new(support::TestUi(notices.clone()));
    let mut event = ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup,
        initial_model_provenance: None, previous_session_file: None });
    api.registered.handlers[&EventKind::SessionStart][0](&mut event, &ctx).await?;
    assert!(notices.lock().expect("notices").is_empty(), "a served SWE-2 lane is not a boundary violation");
    Ok(())
}
