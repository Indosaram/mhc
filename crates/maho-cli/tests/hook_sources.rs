use std::path::PathBuf;

use maho_cli::cli::args::Args;
use maho_cli::cli::hook_sources::{build_loaded_hook_sources, hooks_value, merge_paths, settings_hook_paths, with_runtime_hook_paths, HOOKS_SETTING_KEY};
use maho_cli::cli::host_runtime::CliRuntimeConfiguration;
use maho_core::settings_manager::{Settings, SettingsManager};

fn config(parsed: &Args, hooks: Vec<String>, additional: Vec<PathBuf>) -> CliRuntimeConfiguration {
    let mut config = CliRuntimeConfiguration::from_parsed(parsed, "/tmp/project", "/tmp/agent", maho_core::project_trust::AppMode::Print);
    config.hooks = hooks;
    config.additional_hook_paths = additional;
    config
}

#[test]
fn hook_paths_resolve_against_the_launch_cwd() {
    let parsed = Args::default();
    let config = config(&parsed, vec!["hooks.json".to_owned(), "/abs/hooks.json".to_owned()], Vec::new());
    assert_eq!(config.resolved_hook_paths(), vec![PathBuf::from("/tmp/project/hooks.json"), PathBuf::from("/abs/hooks.json")]);
}

#[test]
fn merge_paths_dedupes_and_preserves_order() {
    let merged = merge_paths(
        vec![PathBuf::from("/a"), PathBuf::from("/b")],
        vec![PathBuf::from("/b"), PathBuf::from("/c")],
    );
    assert_eq!(merged, vec![PathBuf::from("/a"), PathBuf::from("/b"), PathBuf::from("/c")]);
}

#[test]
fn runtime_hook_paths_append_without_duplicates() {
    let parsed = Args::default();
    let config = config(&parsed, vec!["hooks.json".to_owned()], Vec::new());
    let manager = SettingsManager::create("/tmp/project", "/tmp/agent", "/tmp/home", false);
    let sources = build_loaded_hook_sources(&config, &manager);
    assert!(sources.runtime_hook_source_paths.is_empty());
    let extended = with_runtime_hook_paths(sources, vec![PathBuf::from("/tmp/project/hooks.json"), PathBuf::from("/r")]);
    assert_eq!(extended.runtime_hook_source_paths, vec![PathBuf::from("/tmp/project/hooks.json"), PathBuf::from("/r")]);
}

#[test]
fn hook_sources_use_the_real_agent_dir_and_default_paths() {
    let parsed = Args::default();
    let config = config(&parsed, Vec::new(), Vec::new());
    let manager = SettingsManager::create("/tmp/project", "/tmp/agent", "/tmp/home", false);
    let sources = build_loaded_hook_sources(&config, &manager);
    assert_eq!(sources.agent_dir, PathBuf::from("/tmp/agent"));
    assert_eq!(sources.cwd, PathBuf::from("/tmp/project"));
    assert_eq!(sources.global_hooks_path, PathBuf::from("/tmp/agent/hooks.json"));
    assert_eq!(sources.project_hooks_path, PathBuf::from("/tmp/project").join(maho_core::config::config_dir_name()).join("hooks.json"));
    assert!(sources.global_hook_source_paths.is_empty());
    assert!(sources.project_hook_source_paths.is_empty());
    assert!(sources.pre_session_hook_source_paths.is_empty());
}

#[test]
fn settings_hook_paths_read_strings_and_objects_per_scope() {
    let mut global = Settings::new();
    global.insert(HOOKS_SETTING_KEY.to_owned(), serde_json::json!(["/g/hooks.json", {"path": "/g/extra.json"}]));
    assert_eq!(settings_hook_paths(&global), vec![PathBuf::from("/g/hooks.json"), PathBuf::from("/g/extra.json")]);
    assert!(hooks_value(&global).is_some());

    let mut project = Settings::new();
    project.insert(HOOKS_SETTING_KEY.to_owned(), serde_json::json!("/p/hooks.json"));
    assert_eq!(settings_hook_paths(&project), vec![PathBuf::from("/p/hooks.json")]);

    let empty = Settings::new();
    assert!(settings_hook_paths(&empty).is_empty());
    assert!(hooks_value(&empty).is_none());
}

#[test]
fn cli_and_additional_hook_paths_merge_into_the_pre_session_scope() {
    let parsed = Args::default();
    let config = config(&parsed, vec!["cli-hooks.json".to_owned()], vec![PathBuf::from("/tmp/project/additional.json")]);
    let manager = SettingsManager::create("/tmp/project", "/tmp/agent", "/tmp/home", false);
    let sources = build_loaded_hook_sources(&config, &manager);
    assert_eq!(
        sources.pre_session_hook_source_paths,
        vec![PathBuf::from("/tmp/project/cli-hooks.json"), PathBuf::from("/tmp/project/additional.json")]
    );
}
