use std::collections::BTreeMap;
use maho_omo_config_watch::paths::*;
#[test] fn nested_targets_and_user_creation_are_root_anchored()->Result<(),Box<dyn std::error::Error>> {
 let root=tempfile::tempdir()?;let home=root.path().join("home");let work=home.join("work");let project=work.join("project");let cwd=project.join("child");std::fs::create_dir_all(&cwd)?;
 for dir in [&work,&project,&cwd] {std::fs::create_dir(dir.join(".omo"))?;std::fs::write(dir.join(".omo/omo.jsonc"),"{}")?;}
 let env=BTreeMap::from([("SENPI_CODING_AGENT_DIR".into(),root.path().join("agent").to_string_lossy().into_owned())]);let r=resolve_omo_config_watch_target_resolution(&cwd,&home,&env);
 assert!(r.user_config_creation_watched);assert_eq!(r.user_config_creation_discovery,"watched");
 for dir in [&work,&project,&cwd] {let target=r.targets.iter().find(|t|t.path==dir.join(".omo")).expect("config target");assert_eq!(target.filter_globs,OMO_CONFIG_FILE_FILTER_GLOBS);}
 let user=r.targets.iter().find(|t|t.path==home&&t.filter_globs==USER_OMO_CONFIG_DIRECTORY_FILTER_GLOBS).expect("user creation");assert!(user.filter_globs.iter().all(|g|g.starts_with('/')));Ok(())
}
#[test] fn explicit_agent_under_home_excludes_covering_root()->Result<(),Box<dyn std::error::Error>> {let root=tempfile::tempdir()?;let home=root.path().join("home");let cwd=home.join("work/project");std::fs::create_dir_all(&cwd)?;let env=BTreeMap::from([("SENPI_CODING_AGENT_DIR".into(),home.join("custom-agent").to_string_lossy().into_owned())]);let r=resolve_omo_config_watch_target_resolution(&cwd,&home,&env);assert!(!r.targets.iter().any(|t|t.path==home));assert!(r.targets.iter().any(|t|t.path==home.join("work")));Ok(())}
#[test] fn symlink_project_directory_not_watched()->Result<(),Box<dyn std::error::Error>> {let root=tempfile::tempdir()?;let home=root.path().join("home");let cwd=home.join("project");let outside=root.path().join("outside");std::fs::create_dir_all(&cwd)?;std::fs::create_dir(&outside)?;std::fs::write(outside.join("omo.jsonc"),"{}")?;std::os::unix::fs::symlink(&outside,cwd.join(".omo"))?;let r=resolve_omo_config_watch_target_resolution(&cwd,&home,&BTreeMap::new());assert!(!r.targets.iter().any(|t|t.path==cwd.join(".omo")));assert!(r.targets.iter().any(|t|t.path==cwd));Ok(())}
#[test] fn creation_target_contains_repair_filters()->Result<(),Box<dyn std::error::Error>> {let root=tempfile::tempdir()?;let home=root.path().join("home");let cwd=home.join("project");std::fs::create_dir_all(&cwd)?;let r=resolve_omo_config_watch_target_resolution(&cwd,&home,&BTreeMap::new());let t=r.targets.iter().find(|t|t.path==cwd).expect("creation target");assert_eq!(t.kind,"dir");assert_eq!(t.filter_globs,OMO_CONFIG_DIRECTORY_FILTER_GLOBS);assert_eq!(r.user_config_creation_discovery,"reload_required");Ok(())}
#[test] fn existing_project_directory_watched()->Result<(),Box<dyn std::error::Error>> {let root=tempfile::tempdir()?;let home=root.path().join("home");let cwd=home.join("project");std::fs::create_dir_all(cwd.join(".omo"))?;let r=resolve_omo_config_watch_target_resolution(&cwd,&home,&BTreeMap::new());assert!(r.targets.iter().any(|t|t.path==cwd.join(".omo")));assert!(r.targets.iter().any(|t|t.path==cwd));assert!(!r.targets.iter().any(|t|t.path==home));Ok(())}
#[test] fn user_creation_reports_reload_required()->Result<(),Box<dyn std::error::Error>> {let root=tempfile::tempdir()?;let home=root.path().join("home");let cwd=home.join("project");std::fs::create_dir_all(&cwd)?;assert!(!resolve_omo_config_watch_target_resolution(&cwd,&home,&BTreeMap::new()).user_config_creation_watched);Ok(())}
#[test] fn existing_user_config_watched()->Result<(),Box<dyn std::error::Error>> {let root=tempfile::tempdir()?;let home=root.path().join("home");let cwd=home.join("project");std::fs::create_dir_all(&cwd)?;std::fs::create_dir(home.join(".maho"))?;let mut env=BTreeMap::new();env.insert("SENPI_CODING_AGENT_DIR".into(),root.path().join("agent").to_string_lossy().into_owned());let r=resolve_omo_config_watch_target_resolution(&cwd,&home,&env);assert!(r.user_config_creation_watched);assert!(r.targets.iter().any(|t|t.path==home.join(".maho")));Ok(())}

#[test]
fn default_agent_protected_paths_are_never_covered() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let cwd = home.join("work/project/child");
    std::fs::create_dir_all(&cwd)?;
    let resolution = resolve_omo_config_watch_target_resolution(&cwd, &home, &BTreeMap::new());
    for target in &resolution.targets {
        for protected in [home.join(".maho/agent/auth.json"), home.join(".maho/agent/sessions"), home.join(".maho/agent/logs")] {
            assert!(!protected.starts_with(&target.path));
            assert!(!target.path.starts_with(protected));
        }
    }
    assert_eq!(resolution.targets.iter().filter(|t| t.filter_globs == OMO_CONFIG_DIRECTORY_FILTER_GLOBS)
        .map(|t| t.path.clone()).collect::<Vec<_>>(), vec![cwd, home.join("work/project"), home.join("work")]);
    Ok(())
}

#[test]
fn runtime_subtrees_do_not_expand_config_filters() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let cwd = home.join("project");
    let config = cwd.join(".omo");
    std::fs::create_dir_all(config.join("senpi-task/children/child"))?;
    std::fs::write(config.join("omo.jsonc"), "{}")?;
    std::fs::write(config.join("senpi-task/children/child/transcript.jsonl"), "{}\n")?;
    let resolution = resolve_omo_config_watch_target_resolution(&cwd, &home, &BTreeMap::new());
    let target = resolution.targets.iter().find(|target| target.path == config).expect("config target");
    assert_eq!(target.filter_globs, ["/omo.jsonc", "/omo.json"]);
    Ok(())
}
