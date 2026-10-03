//! Port of senpi `core/resource-loader.ts` `buildLoadedHookSources` and the CLI hook inputs.
//!
//! The native `ExtensionContextActions::get_loaded_hook_sources` is owned by maho-core and currently
//! returns empty path vectors with `agent_dir = cwd`; this module builds the real record the CLI
//! owns and hands it to the session once the core exposes the seam (routed in
//! `.omo/evidence/task-38-hook-source-routing.md`). Nothing here is synthesized: every path comes
//! from a CLI argument, a settings value, or the agent/cwd directories.

use std::path::PathBuf;

use maho_core::settings_manager::{Settings, SettingsManager};
use maho_ext_api::LoadedHookSources;

use super::host_runtime::CliRuntimeConfiguration;

/// The settings key whose value holds inline hook definitions (senpi `settings.hooks`).
pub const HOOKS_SETTING_KEY: &str = "hooks";

pub fn hooks_value(settings: &Settings) -> Option<serde_json::Value> {
    settings.get(HOOKS_SETTING_KEY).filter(|value| !value.is_null()).cloned()
}

/// Path entries declared by a settings `hooks` value: a string, or objects with a `path`.
pub fn settings_hook_paths(settings: &Settings) -> Vec<PathBuf> {
    let Some(value) = settings.get(HOOKS_SETTING_KEY) else { return Vec::new() };
    let mut paths = Vec::new();
    match value {
        serde_json::Value::Array(entries) => {
            for entry in entries {
                match entry {
                    serde_json::Value::String(path) => paths.push(PathBuf::from(path)),
                    serde_json::Value::Object(object) => {
                        if let Some(path) = object.get("path").and_then(serde_json::Value::as_str) {
                            paths.push(PathBuf::from(path));
                        }
                    }
                    _ => {}
                }
            }
        }
        serde_json::Value::String(path) => paths.push(PathBuf::from(path)),
        _ => {}
    }
    paths
}

/// Merge two path lists, preserving order and dropping duplicates (senpi `mergePaths`).
pub fn merge_paths(first: Vec<PathBuf>, second: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut merged: Vec<PathBuf> = Vec::with_capacity(first.len() + second.len());
    for path in first.into_iter().chain(second) {
        if !merged.contains(&path) {
            merged.push(path);
        }
    }
    merged
}

/// senpi `buildLoadedHookSources`: the real record for this launch, from the CLI configuration and
/// the loaded settings.
pub fn build_loaded_hook_sources(config: &CliRuntimeConfiguration, settings: &SettingsManager) -> LoadedHookSources {
    let cwd = PathBuf::from(&config.cwd);
    let agent_dir = PathBuf::from(&config.agent_dir);
    let global_settings = settings.get_global();
    let project_settings = settings.get_project();
    let pre_session = merge_paths(config.resolved_hook_paths(), config.additional_hook_paths.clone());
    LoadedHookSources {
        global_hooks_path: agent_dir.join("hooks.json"),
        project_hooks_path: cwd.join(maho_core::config::config_dir_name()).join("hooks.json"),
        agent_dir,
        cwd,
        global_settings_hooks: hooks_value(global_settings),
        project_settings_hooks: hooks_value(project_settings),
        global_hook_source_paths: settings_hook_paths(global_settings),
        project_hook_source_paths: settings_hook_paths(project_settings),
        pre_session_hook_source_paths: pre_session,
        runtime_hook_source_paths: Vec::new(),
    }
}

/// senpi `extendResources` for hooks: merge runtime-added hook paths into the record.
pub fn with_runtime_hook_paths(mut sources: LoadedHookSources, added: Vec<PathBuf>) -> LoadedHookSources {
    sources.runtime_hook_source_paths = merge_paths(sources.runtime_hook_source_paths, added);
    sources
}
