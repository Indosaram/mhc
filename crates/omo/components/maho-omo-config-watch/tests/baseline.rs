use std::collections::BTreeMap;
use maho_omo_config_watch::validate::{ConfigWatchValidation, OmoConfigValidator};

#[test]
fn valid_change_advances_baseline() -> Result<(), std::io::Error> {
    let root = tempfile::tempdir()?;
    let project = root.path().join("project");
    let config = project.join(".omo/omo.jsonc");
    std::fs::create_dir_all(config.parent().expect("parent"))?;
    std::fs::write(&config, r#"{"task":{"default_concurrency":3}}"#)?;
    let env = BTreeMap::from([("HOME".into(), root.path().to_string_lossy().into_owned())]);
    let mut validator = OmoConfigValidator::new(project.to_string_lossy().into_owned(), env);
    std::fs::write(&config, r#"{"task":{"default_concurrency":4}}"#)?;
    assert!(matches!(validator.validate(std::slice::from_ref(&config)), ConfigWatchValidation::Ok));
    assert!(matches!(validator.validate(&[]), ConfigWatchValidation::Ok));
    Ok(())
}

#[test]
fn invalid_ancestor_does_not_reject_valid_nearer_change() -> Result<(), std::io::Error> {
    let root = tempfile::tempdir()?;
    let work = root.path().join("work");
    let project = work.join("project");
    let far = work.join(".omo/omo.jsonc");
    let near = project.join(".omo/omo.jsonc");
    std::fs::create_dir_all(far.parent().expect("parent"))?;
    std::fs::create_dir_all(near.parent().expect("parent"))?;
    std::fs::write(&far, r#"{"task":{"default_concurrency":"three"}}"#)?;
    std::fs::write(&near, r#"{"task":{"default_concurrency":3}}"#)?;
    let env = BTreeMap::from([("HOME".into(), root.path().to_string_lossy().into_owned())]);
    let loaded = maho_omo_config_resolution::load_senpi_omo_config(omo_config_core::LoadOmoConfigOptions {
        cwd: Some(project.to_string_lossy().into_owned()), env: Some(env.clone()), ..Default::default()
    });
    assert!(!loaded.diagnostics.is_empty(), "ancestor fixture must produce a real diagnostic");
    let mut validator = OmoConfigValidator::new(project.to_string_lossy().into_owned(), env);
    std::fs::write(&near, r#"{"task":{"default_concurrency":4}}"#)?;
    assert!(matches!(validator.validate(&[near]), ConfigWatchValidation::Ok));
    Ok(())
}

#[test]
fn unreadable_changed_config_returns_rejection() -> Result<(), std::io::Error> {
    let root = tempfile::tempdir()?;
    let project = root.path().join("project");
    let config = project.join(".omo/omo.jsonc");
    std::fs::create_dir_all(config.parent().expect("parent"))?;
    std::fs::write(&config, "{}")?;
    let env = BTreeMap::from([("HOME".into(), root.path().to_string_lossy().into_owned())]);
    let mut validator = OmoConfigValidator::new(project.to_string_lossy().into_owned(), env);
    std::fs::remove_file(&config)?;
    std::fs::create_dir(&config)?;
    let ConfigWatchValidation::Rejected { errors } = validator.validate(&[config]) else { panic!("read rejection") };
    assert!(!errors.is_empty());
    Ok(())
}
