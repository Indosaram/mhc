use maho_omo_config_startup::run_senpi_startup_migration;
#[test] fn second_caller_is_locked_at_journal_boundary()->Result<(),Box<dyn std::error::Error>> {use std::cell::RefCell;let root=tempfile::tempdir()?;let home=root.path().join("home");let project=home.join("project");std::fs::create_dir_all(&project)?;std::fs::create_dir_all(home.join(".maho"))?;std::fs::write(home.join(".maho/config.jsonc"),r#"{"codegraph":{"daemon":false}}"#)?;let cwd=project.to_string_lossy().into_owned();let home=home.to_string_lossy().into_owned();let env=std::collections::BTreeMap::from([("HOME".into(),home.clone())]);let raced=RefCell::new(None);let result=maho_omo_config_startup::run_senpi_startup_migration_with_options(&cwd,&env,&home,maho_omo_config_startup::SenpiStartupMigrationOptions{on_boundary:Some(Box::new(|boundary| {if boundary==omo_config_core::MigrationBoundary::JournalWritten && raced.borrow().is_none(){*raced.borrow_mut()=Some(run_senpi_startup_migration(&cwd,&env,&home));}Ok(())})),..Default::default()});assert!(result.error.is_none());let second=raced.into_inner().expect("boundary invoked");assert_eq!(second.error.as_deref(),Some("Configuration migration is already running"));assert!(second.results.is_empty());Ok(())}
#[test] fn both_legacy_groups_migrate_into_unified_config()->Result<(),Box<dyn std::error::Error>> {let root=tempfile::tempdir()?;let home=root.path().join("home");let project=home.join("project");std::fs::create_dir_all(&project)?;std::fs::create_dir_all(home.join(".config/opencode"))?;std::fs::create_dir_all(home.join(".maho"))?;std::fs::write(home.join(".config/opencode/oh-my-openagent.jsonc"),r#"{"agents":{"finder":{"model":"provider/finder"}}}"#)?;std::fs::write(home.join(".maho/config.jsonc"),r#"{"codegraph":{"daemon":false}}"#)?;let env=std::collections::BTreeMap::from([("HOME".into(),home.to_string_lossy().into_owned())]);let result=run_senpi_startup_migration(&project.to_string_lossy(),&env,&home.to_string_lossy());assert!(result.error.is_none(),"{:?}",result.error);assert!(!result.migrated_from.is_empty());assert!(home.join(".maho/omo.jsonc").exists());Ok(())}
#[test]
fn missing_home_reports_error() {
    let result=run_senpi_startup_migration("/tmp/project",&Default::default(),"");
    assert!(result.error.is_some()); assert!(result.results.is_empty());
}
#[test]
fn empty_isolated_home_has_no_migrations() -> Result<(),std::io::Error> {
    let root=tempfile::tempdir()?; let home=root.path().join("home"); let project=home.join("project"); std::fs::create_dir_all(&project)?;
    let home=home.to_string_lossy().into_owned();
    let env=std::collections::BTreeMap::from([("HOME".into(),home.clone())]);
    let result=run_senpi_startup_migration(&project.to_string_lossy(),&env,&home);
    assert!(result.error.is_none()); assert!(result.migrated_from.is_empty()); Ok(())
}
