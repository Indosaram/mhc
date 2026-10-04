use maho_cli::package_manager_cli::*;
#[tokio::test]
async fn resource_execution_persists_install_and_removal_in_callers_settings() {
    use maho_core::{package_manager::{DefaultPackageManager, PackageManagerOptions}, settings_manager::SettingsManager};
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("project"); let agent = root.path().join("agent");
    let resource = root.path().join("resource");
    std::fs::create_dir_all(&cwd).unwrap(); std::fs::create_dir_all(&resource).unwrap();
    let source = resource.to_string_lossy().into_owned();
    let cwd = cwd.to_string_lossy(); let agent = agent.to_string_lossy();
    let mut settings = SettingsManager::create(&cwd, &agent, root.path().to_str().unwrap(), true);
    {
        let mut manager = DefaultPackageManager::new(PackageManagerOptions { cwd: &cwd, agent_dir: &agent, settings_manager: &mut settings });
        let install = parse_package_command(&args(&["install", &source, "--local"])).unwrap();
        execute_package_command(&install, &mut manager).await.unwrap();
        assert_eq!(manager.list_configured_packages().unwrap().len(), 1);
    }
    assert_eq!(settings.get_project()["packages"].as_array().unwrap().len(), 1);
    {
        let mut manager = DefaultPackageManager::new(PackageManagerOptions { cwd: &cwd, agent_dir: &agent, settings_manager: &mut settings });
        let remove = parse_package_command(&args(&["remove", &source, "--local"])).unwrap();
        execute_package_command(&remove, &mut manager).await.unwrap();
        assert!(execute_package_command(&remove, &mut manager).await.is_err());
    }
    assert!(settings.get_project()["packages"].as_array().unwrap().is_empty());
}
#[test]
fn config_route_preserves_help_priority_scope_and_last_trust_override() {
    let options = parse_config_command(&args(&["config", "--local", "--approve", "--no-approve"])).unwrap().unwrap();
    assert!(options.local); assert_eq!(options.project_trust_override, Some(false));
    assert!(parse_config_command(&args(&["config", "--invalid", "--help"])).unwrap().unwrap().help);
    assert!(parse_config_command(&args(&["config", "--invalid"])).is_err());
    assert!(parse_config_command(&args(&["config", "unexpected"])).is_err());
}
fn args(values: &[&str]) -> Vec<String> { values.iter().map(|value| (*value).to_owned()).collect() }
#[test] fn remove_alias_accepts_project_scope() {
    let command = parse_package_command(&args(&["uninstall", "--local", "npm:resource", "-a"])).unwrap();
    assert!(command.command == PackageCommand::Remove && command.local);
    assert_eq!(command.project_trust_override, Some(true));
}
#[test] fn list_rejects_local_flag() {
    assert!(parse_package_command(&args(&["list", "-l"])).unwrap().invalid_option.is_some());
}
#[test] fn resource_update_rejects_conflicting_targets() {
    assert!(parse_package_command(&args(&["update", "--extension", "resource", "other"])).unwrap().conflicting_options.is_some());
    assert!(parse_package_command(&args(&["update", "--extension"])).unwrap().missing_option_value.is_some());
    assert!(parse_package_command(&args(&["update", "--extension", "one", "--extension", "two"])).unwrap().conflicting_options.is_some());
}
#[test] fn update_target_precedence_preserves_positional_source() {
    let parsed = parse_package_command(&args(&["update", "--extension", "resource"])).unwrap();
    assert_eq!(parsed.source, None);
    assert_eq!(parsed.update_target, Some(UpdateTarget::Extensions { source: Some("resource".to_owned()) }));
    let parsed = parse_package_command(&args(&["update", "self", "--extensions"])).unwrap();
    assert!(parsed.conflicting_options.is_none());
    assert_eq!(parsed.update_target, Some(UpdateTarget::All));
    assert!(parse_package_command(&args(&["update"])).unwrap().show_extensions_skipped_note);
}
#[test] fn update_conflicts_keep_first_error_and_models_target() {
    let parsed = parse_package_command(&args(&["update", "--all", "--models", "--self", "--force"])).unwrap();
    assert_eq!(parsed.update_target, Some(UpdateTarget::Models));
    assert!(parsed.conflicting_options.unwrap().starts_with("--all"));
    assert!(parsed.force);
    assert!(parse_package_command(&args(&["install", "--force"])).unwrap().invalid_option.is_some());
}
