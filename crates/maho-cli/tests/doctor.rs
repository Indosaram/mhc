use std::collections::BTreeMap;

use maho_cli::cli::doctor::doctor_lines;

#[test]
fn invalid_config_is_reported_without_values() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().to_string_lossy().into_owned();
    std::fs::create_dir_all(root.path().join(".maho")).expect("mkdir");
    std::fs::write(root.path().join(".maho/omo.jsonc"), r#"{"task":{"host_engine_policy":"sometimes"}}"#).expect("write");
    let env = BTreeMap::from([(String::from("HOME"), home.clone())]);
    let lines = doctor_lines(root.path(), &env, &format!("{home}/.maho/agent"));
    assert!(lines.iter().any(|line| line == "WARN config: ~/.maho/omo.jsonc: task.host_engine_policy ignored (invalid value)"));
    assert!(lines.iter().all(|line| !line.contains("sometimes")));
}

#[test]
fn malformed_config_is_reported_as_not_loaded() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().to_string_lossy().into_owned();
    std::fs::create_dir_all(root.path().join(".maho")).expect("mkdir");
    std::fs::write(root.path().join(".maho/omo.jsonc"), r#"{"task":{"#).expect("write");
    let env = BTreeMap::from([(String::from("HOME"), home.clone())]);
    let lines = doctor_lines(root.path(), &env, &format!("{home}/.maho/agent"));
    assert!(lines.iter().any(|line| line.contains("not loaded (JSONC parse error)")));
}

#[test]
fn untrusted_project_resources_fail() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().to_string_lossy().into_owned();
    std::fs::create_dir_all(root.path().join(".maho")).expect("mkdir");
    std::fs::write(root.path().join(".maho/settings.json"), "{}").expect("write");
    let env = BTreeMap::from([(String::from("HOME"), home.clone())]);
    let lines = doctor_lines(root.path(), &env, &format!("{home}/.maho/agent"));
    assert!(lines.iter().any(|line| line == "WARN project resources: trust decision is not recorded"));
}

#[test]
fn category_report_is_warning_when_no_models_are_available() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().to_string_lossy().into_owned();
    let env = BTreeMap::from([(String::from("HOME"), home.clone())]);
    let lines = doctor_lines(root.path(), &env, &format!("{home}/.maho/agent"));
    assert!(lines.iter().any(|line| line.starts_with("WARN task categories:")));
}

