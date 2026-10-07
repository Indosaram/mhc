use maho_omo_config_watch::validate::*;
#[test] fn merged_team_diagnostic_rejected_and_preexisting_baselined()->Result<(),std::io::Error> {let root=tempfile::tempdir()?;let work=root.path().join("work");let project=work.join("project");let cwd=project.join("child");for dir in [&work,&project,&cwd] {std::fs::create_dir_all(dir.join(".omo"))?;}let far=work.join(".omo/omo.jsonc");let near=project.join(".omo/omo.jsonc");let changed=cwd.join(".omo/omo.jsonc");std::fs::write(&far,r#"{"teams":{"alpha":{"members":[{"name":"one","kind":"category","category":"quick","prompt":"go"}]}}}"#)?;std::fs::write(&near,"{}")?;let env=std::collections::BTreeMap::from([("HOME".into(),root.path().to_string_lossy().into_owned())]);let mut validator=OmoConfigValidator::new(cwd.to_string_lossy().into_owned(),env.clone());std::fs::write(&near,r#"{"teams":{"alpha":{"members":[{"name":"one","kind":"category","category":"quick","prompt":"go"},{"name":"two","kind":"category","category":"quick","prompt":"go"}]}}}"#)?;assert!(matches!(validator.validate(&[near]),ConfigWatchValidation::Rejected{..}));let mut baseline=OmoConfigValidator::new(cwd.to_string_lossy().into_owned(),env);std::fs::write(&changed,r#"{"task":{"default_concurrency":4}}"#)?;assert!(matches!(baseline.validate(&[changed]),ConfigWatchValidation::Ok));Ok(())}
#[test] fn accepted_non_attributable_error_becomes_baseline()->Result<(),std::io::Error> {let root=tempfile::tempdir()?;let work=root.path().join("work");let project=work.join("project");std::fs::create_dir_all(work.join(".omo"))?;std::fs::create_dir_all(project.join(".omo"))?;let far=work.join(".omo/omo.jsonc");let near=project.join(".omo/omo.jsonc");std::fs::write(&far,"{}")?;std::fs::write(&near,"{}")?;let env=std::collections::BTreeMap::from([("HOME".into(),root.path().to_string_lossy().into_owned())]);let mut validator=OmoConfigValidator::new(project.to_string_lossy().into_owned(),env);std::fs::write(&far,r#"{"task":{"default_concurrency":"three"}}"#)?;for value in [4,5] {std::fs::write(&near,format!("{{\"task\":{{\"default_concurrency\":{value}}}}}"))?;assert!(matches!(validator.validate(std::slice::from_ref(&near)),ConfigWatchValidation::Ok));}Ok(())}
#[test] fn directory_changes_and_sibling_deletion_attributed()->Result<(),std::io::Error> {let root=tempfile::tempdir()?;let project=root.path().join("project");std::fs::create_dir(&project)?;let env=std::collections::BTreeMap::from([("HOME".into(),root.path().to_string_lossy().into_owned())]);let mut validator=OmoConfigValidator::new(project.to_string_lossy().into_owned(),env);let dir=project.join(".omo");std::fs::create_dir(&dir)?;let jsonc=dir.join("omo.jsonc");let json=dir.join("omo.json");std::fs::write(&jsonc,"{")?;assert!(matches!(validator.validate(std::slice::from_ref(&dir)),ConfigWatchValidation::Rejected{..}));std::fs::write(&jsonc,"{}")?;assert!(matches!(validator.validate(std::slice::from_ref(&jsonc)),ConfigWatchValidation::Ok));std::fs::write(&json,"{")?;std::fs::remove_file(&jsonc)?;assert!(matches!(validator.validate(&[jsonc]),ConfigWatchValidation::Rejected{..}));Ok(())}
#[test] fn catalog_cycle_and_unreadable_file_rejected()->Result<(),std::io::Error> {let root=tempfile::tempdir()?;let project=root.path().join("project");let config=project.join(".omo/omo.jsonc");std::fs::create_dir_all(config.parent().expect("parent"))?;std::fs::write(&config,"{}")?;let env=std::collections::BTreeMap::from([("HOME".into(),root.path().to_string_lossy().into_owned())]);let mut validator=OmoConfigValidator::new(project.to_string_lossy().into_owned(),env);std::fs::write(&config,r#"{"models":{"loop":{"model":"loop"}},"[senpi]":{"categories":{"quick":{"model":"loop"}}}}"#)?;assert!(matches!(validator.validate(std::slice::from_ref(&config)),ConfigWatchValidation::Rejected{..}));std::fs::remove_file(&config)?;std::fs::create_dir(&config)?;assert!(matches!(validator.validate(&[config]),ConfigWatchValidation::Rejected{..}));Ok(())}
#[test] fn schema_error_rejected_and_valid_change_accepted()->Result<(),std::io::Error> {let root=tempfile::tempdir()?;let config=root.path().join(".omo/omo.jsonc");std::fs::create_dir_all(config.parent().expect("parent"))?;std::fs::write(&config,"{\"task\":{\"default_concurrency\":3}}")?;let env=std::collections::BTreeMap::from([("HOME".into(),root.path().parent().expect("home boundary").to_string_lossy().into_owned())]);let mut validator=OmoConfigValidator::new(root.path().to_string_lossy().into_owned(),env);std::fs::write(&config,"{\"task\":{\"default_concurrency\":\"three\"}}")?;assert!(matches!(validator.validate(std::slice::from_ref(&config)),ConfigWatchValidation::Rejected{..}));std::fs::write(&config,"{\"task\":{\"default_concurrency\":4}}")?;assert!(matches!(validator.validate(&[config]),ConfigWatchValidation::Ok));Ok(())}
#[test] fn missing_optional_user_config_accepted()->Result<(),std::io::Error> {let root=tempfile::tempdir()?;let env=std::collections::BTreeMap::from([("HOME".into(),root.path().parent().expect("home boundary").to_string_lossy().into_owned())]);let mut validator=OmoConfigValidator::new(root.path().to_string_lossy().into_owned(),env);assert!(matches!(validator.validate(&[root.path().join(".maho/omo.jsonc")]),ConfigWatchValidation::Ok));Ok(())}
#[test] fn baseline_errors_do_not_reject_unrelated_changes()->Result<(),std::io::Error> {let root=tempfile::tempdir()?;let config=root.path().join(".omo/omo.jsonc");std::fs::create_dir_all(config.parent().expect("parent"))?;std::fs::write(&config,"{")?;let env=std::collections::BTreeMap::from([("HOME".into(),root.path().parent().expect("home boundary").to_string_lossy().into_owned())]);let mut validator=OmoConfigValidator::new(root.path().to_string_lossy().into_owned(),env);assert!(matches!(validator.validate(&[root.path().join("other")]),ConfigWatchValidation::Ok));Ok(())}
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

#[test]
fn deprecated_key_notices_never_reject_a_reload() -> Result<(), std::io::Error> {
    use std::sync::{Arc, Mutex};
    let path = "/home/test/project/.omo/omo.jsonc";
    let notice = |message: &str, issue_path: &str| {
        maho_omo_config_resolution::SenpiConfigDiagnostic::Config(
            omo_config_core::OmoConfigDiagnostic {
                kind: "deprecated-keys",
                path: path.into(),
                message: message.into(),
                issue_paths: vec![issue_path.into()],
            },
        )
    };
    let diagnostics = Arc::new(Mutex::new(vec![notice("Deprecated harness block", "[senpi]")]));
    let current = diagnostics.clone();
    let env = std::collections::BTreeMap::from([("HOME".into(), "/home/test".into())]);
    let mut validator = OmoConfigValidator::with_loader(
        "/home/test/project".into(),
        env,
        Box::new(move |_, _| {
            current
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }),
    );
    let changed = [std::path::PathBuf::from(path)];
    assert!(matches!(
        validator.validate(&changed),
        ConfigWatchValidation::Ok
    ));
    *diagnostics
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = vec![notice(
        "Deprecated harness block renamed",
        "[native]",
    )];
    assert!(matches!(
        validator.validate(&changed),
        ConfigWatchValidation::Ok
    ));
    *diagnostics
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = vec![
        notice("Deprecated category name", "categories.deep"),
        maho_omo_config_resolution::SenpiConfigDiagnostic::Config(
            omo_config_core::OmoConfigDiagnostic {
                kind: "validation",
                path: path.into(),
                message: "Invalid omo config".into(),
                issue_paths: vec!["task.default_concurrency".into()],
            },
        ),
    ];
    let ConfigWatchValidation::Rejected { errors } = validator.validate(&changed) else {
        panic!("expected the schema diagnostic to reject");
    };
    assert_eq!(errors, vec!["Invalid omo config".to_string()]);
    Ok(())
}
