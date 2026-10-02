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
