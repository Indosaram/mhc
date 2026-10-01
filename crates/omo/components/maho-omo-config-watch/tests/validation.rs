use maho_omo_config_watch::validate::*;
#[test] fn baseline_errors_do_not_reject_unrelated_changes()->Result<(),std::io::Error> {let root=tempfile::tempdir()?;let config=root.path().join(".omo/omo.jsonc");std::fs::create_dir_all(config.parent().expect("parent"))?;std::fs::write(&config,"{")?;let env=std::collections::BTreeMap::from([("HOME".into(),root.path().to_string_lossy().into_owned())]);let mut validator=OmoConfigValidator::new(root.path().to_string_lossy().into_owned(),env);assert!(matches!(validator.validate(&[root.path().join("other")]),ConfigWatchValidation::Ok));Ok(())}
#[test]
fn rejects_parse_error_and_stays_sticky_until_repair()->Result<(),std::io::Error> {
    let root=tempfile::tempdir()?;let home=root.path().join("home");let project=home.join("project");let config=project.join(".omo/omo.jsonc");std::fs::create_dir_all(config.parent().expect("test path"))?;std::fs::write(&config,"{}")?;
    let env=std::collections::BTreeMap::from([("HOME".into(),home.to_string_lossy().into_owned())]);
    let mut validator=OmoConfigValidator::new(project.to_string_lossy().into_owned(),env);
    std::fs::write(&config,"{\"task\":")?;
    assert!(matches!(validator.validate(std::slice::from_ref(&config)),ConfigWatchValidation::Rejected{..}));
    assert!(matches!(validator.validate(&[project.join("other")]),ConfigWatchValidation::Rejected{..}));
    std::fs::write(&config,"{}")?;
    assert!(matches!(validator.validate(&[config]),ConfigWatchValidation::Ok));Ok(())
}
