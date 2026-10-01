mod support;
use std::{collections::BTreeMap,sync::Arc};
use maho_ext_api::*;
use maho_omo_config_startup::{ConfigStartupComponent,SenpiStartupMigrationResult};
#[tokio::test] async fn migration_notice_reports_once_to_ui()->Result<(),Box<dyn std::error::Error>> {
 let root=tempfile::tempdir()?;let path=root.path().to_string_lossy().into_owned();let env=BTreeMap::from([("HOME".into(),path.clone())]);
 let mut api=ExtensionApi::new(LoadedExtension::new("startup",root.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
 ConfigStartupComponent{run_migration:Some(Arc::new(|_|SenpiStartupMigrationResult{error:Some("failure".into()),..Default::default()})),load_config:Some(Arc::new(move |_|maho_omo_config_resolution::load_senpi_omo_config(omo_config_core::LoadOmoConfigOptions{cwd:Some(path.clone()),env:Some(env.clone()),..Default::default()})))}.register(&mut api);
 let mut ctx=support::context();ctx.has_ui=true;
 for _ in 0..2 {let mut event=ExtensionEvent::SessionStart(SessionStartEvent{reason:SessionReason::Startup,initial_model_provenance:None,previous_session_file:None});api.registered.handlers[&EventKind::SessionStart][0](&mut event,&ctx).await?;}
 assert_eq!(support::NOTICES.lock().expect("notices").len(),1);Ok(())
}
