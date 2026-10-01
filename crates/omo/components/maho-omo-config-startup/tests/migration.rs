use maho_omo_config_startup::run_senpi_startup_migration;
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
