use std::path::PathBuf;

use maho_cli::cli::startup_ui::{DistributionMetadata, should_run_first_time_setup_with};
use maho_core::settings_manager::{Settings, SettingsManager};

const OFFICIAL: DistributionMetadata<'static> = DistributionMetadata {
    package_name: "@earendil-works/pi-coding-agent",
    app_name: "pi",
    config_dir_name: ".pi",
};

fn settings() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    (dir, path)
}

#[test]
fn returns_true_when_experimental_default_agent_dir_and_no_settings_json() {
    let (_dir, path) = settings();
    assert!(should_run_first_time_setup_with(
        &OFFICIAL, true, None, &path
    ));
}

#[test]
fn returns_false_when_experimental_features_are_disabled() {
    let (_dir, path) = settings();
    assert!(!should_run_first_time_setup_with(
        &OFFICIAL, false, None, &path
    ));
}

#[test]
fn returns_false_when_a_custom_agent_dir_is_set() {
    let (dir, path) = settings();
    assert!(!should_run_first_time_setup_with(
        &OFFICIAL,
        true,
        Some(&dir.path().to_string_lossy()),
        &path,
    ));
}

#[test]
fn returns_false_when_settings_json_already_exists() {
    let (_dir, path) = settings();
    std::fs::write(&path, "{}").expect("write settings");
    assert!(!should_run_first_time_setup_with(
        &OFFICIAL, true, None, &path
    ));
}

#[test]
fn a_rebranded_distribution_never_runs_first_time_setup() {
    let (_dir, path) = settings();
    let maho = DistributionMetadata {
        package_name: "",
        app_name: "maho",
        config_dir_name: ".omo",
    };
    assert!(!should_run_first_time_setup_with(&maho, true, None, &path));
}

#[test]
fn analytics_defaults_to_disabled_with_no_tracking_identifier() {
    let manager = SettingsManager::in_memory(Settings::new(), true);
    assert!(!manager.get_enable_analytics());
    assert_eq!(manager.get_tracking_id(), None);
}

#[test]
fn analytics_opt_in_generates_a_tracking_identifier() {
    let mut manager = SettingsManager::in_memory(Settings::new(), true);
    manager.set_enable_analytics(true).expect("opt in");
    assert!(manager.get_enable_analytics());
    let id = manager.get_tracking_id().expect("tracking id");
    assert_eq!(id.len(), 36);
    assert!(
        id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
        "{id}"
    );
}

#[test]
fn analytics_opt_out_does_not_generate_a_tracking_identifier() {
    let mut manager = SettingsManager::in_memory(Settings::new(), true);
    manager.set_enable_analytics(false).expect("opt out");
    assert!(!manager.get_enable_analytics());
    assert_eq!(manager.get_tracking_id(), None);
}

#[test]
fn analytics_toggling_keeps_the_tracking_identifier() {
    let mut manager = SettingsManager::in_memory(Settings::new(), true);
    manager.set_enable_analytics(true).expect("opt in");
    let id = manager.get_tracking_id();
    manager.set_enable_analytics(false).expect("opt out");
    manager.set_enable_analytics(true).expect("opt in again");
    assert_eq!(manager.get_tracking_id(), id);
}
