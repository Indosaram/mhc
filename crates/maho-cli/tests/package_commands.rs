use maho_cli::package_manager_cli::*;
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
