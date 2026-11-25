//! Port of senpi `packages/coding-agent/src/modes/rpc/loopless-runtime-settings.ts`.
//!
//! Because `CliRuntimeConfiguration` and `Args` live in `maho-cli` (downstream of `maho-rpc`),
//! this module defines `LooplessCliConfiguration` as the crate-boundary data adapter.

use std::collections::{BTreeMap, HashSet};

use maho_core::agent_session_services::{
    AgentSessionRuntimeDiagnostic, AgentSessionRuntimeDiagnosticType,
};
use maho_core::config::home_dir;
use maho_core::paths::{PathInputOptions, is_local_path, resolve_path};
use maho_core::project_trust::{
    AppMode, DefaultProjectTrust, ProjectTrustContext, ResolveProjectTrustedOptions,
    preset_trusts_project, resolve_project_trusted,
};
use maho_core::runtime::active_tools::{ToolSelection, resolve_tool_selection};
use maho_core::session_extension_flags::session_extension_flag_values;
use maho_core::settings_manager::SettingsManager;
use maho_core::tool_dispatch::tool_dispatcher::ToolRuntimeKind;
use maho_core::trust_manager::{
    ProjectTrustStore, get_saved_project_trust, has_trust_requiring_project_resources,
};
use maho_ext_api::FlagValue;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::loopless_runtime::LooplessRuntimeProfile;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum LooplessFlagValue {
    Boolean(bool),
    String(String),
}

impl From<LooplessFlagValue> for FlagValue {
    fn from(value: LooplessFlagValue) -> Self {
        match value {
            LooplessFlagValue::Boolean(b) => Self::Boolean(b),
            LooplessFlagValue::String(s) => Self::String(s),
        }
    }
}

impl From<FlagValue> for LooplessFlagValue {
    fn from(value: FlagValue) -> Self {
        match value {
            FlagValue::Boolean(b) => Self::Boolean(b),
            FlagValue::String(s) => Self::String(s),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LooplessToolRuntimeKind {
    #[default]
    InProcess,
    Child,
}

impl From<LooplessToolRuntimeKind> for ToolRuntimeKind {
    fn from(kind: LooplessToolRuntimeKind) -> Self {
        match kind {
            LooplessToolRuntimeKind::InProcess => Self::InProcess,
            LooplessToolRuntimeKind::Child => Self::Child,
        }
    }
}

impl From<ToolRuntimeKind> for LooplessToolRuntimeKind {
    fn from(kind: ToolRuntimeKind) -> Self {
        match kind {
            ToolRuntimeKind::InProcess => Self::InProcess,
            ToolRuntimeKind::Child => Self::Child,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LooplessAppMode {
    Interactive,
    #[default]
    Print,
    Json,
    Rpc,
}

impl From<LooplessAppMode> for AppMode {
    fn from(mode: LooplessAppMode) -> Self {
        match mode {
            LooplessAppMode::Interactive => Self::Interactive,
            LooplessAppMode::Print => Self::Print,
            LooplessAppMode::Json => Self::Json,
            LooplessAppMode::Rpc => Self::Rpc,
        }
    }
}

impl From<AppMode> for LooplessAppMode {
    fn from(mode: AppMode) -> Self {
        match mode {
            AppMode::Interactive => Self::Interactive,
            AppMode::Print => Self::Print,
            AppMode::Json => Self::Json,
            AppMode::Rpc => Self::Rpc,
        }
    }
}

/// Serializable adapter for `maho_cli::cli::host_runtime::CliRuntimeConfiguration` + CLI tool
/// flags.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LooplessCliConfiguration {
    pub cwd: String,
    pub agent_dir: String,
    #[serde(default)]
    pub app_mode: LooplessAppMode,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub prompt_templates: Vec<String>,
    #[serde(default)]
    pub themes: Vec<String>,
    #[serde(default)]
    pub no_extensions: bool,
    #[serde(default)]
    pub no_skills: bool,
    #[serde(default)]
    pub no_prompt_templates: bool,
    #[serde(default)]
    pub no_themes: bool,
    #[serde(default)]
    pub no_context_files: bool,
    #[serde(default)]
    pub project_trust_override: Option<bool>,
    #[serde(default)]
    pub unknown_flags: BTreeMap<String, LooplessFlagValue>,
    #[serde(default)]
    pub tool_runtime: Option<LooplessToolRuntimeKind>,
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    #[serde(default)]
    pub no_tools: bool,
    #[serde(default)]
    pub no_builtin_tools: bool,
    #[serde(default)]
    pub exclude_tools: Option<Vec<String>>,
}

impl LooplessCliConfiguration {
    pub fn flag_map(&self) -> BTreeMap<String, FlagValue> {
        self.unknown_flags
            .iter()
            .map(|(k, v)| (k.clone(), v.clone().into()))
            .collect()
    }
}

pub struct LooplessRuntimeSettings {
    pub settings_manager: SettingsManager,
    pub flag_values: BTreeMap<String, FlagValue>,
    pub should_resolve_project_trust: bool,
}

struct HeadlessProjectTrustContext;

impl ProjectTrustContext for HeadlessProjectTrustContext {
    fn has_ui(&self) -> bool {
        false
    }

    fn select(&self, _title: &str, _options: &[String]) -> Option<String> {
        None
    }
}

/// The project trust a session opened from this configuration would get, decided without a UI: a
/// runtime without a loop has no one to ask, exactly like an RPC worker.
pub async fn resolve_loopless_settings(
    configuration: &LooplessCliConfiguration,
    startup_settings_manager: &SettingsManager,
    profile: &LooplessRuntimeProfile,
    diagnostics: &mut Vec<AgentSessionRuntimeDiagnostic>,
) -> LooplessRuntimeSettings {
    let cwd = &profile.cwd;
    let trust_store = ProjectTrustStore::new(&configuration.agent_dir);
    let host_flags = configuration.flag_map();
    let flag_values = session_extension_flag_values(&host_flags, profile.launch_profile.as_ref());
    let has_trust_requiring_resources = has_trust_requiring_project_resources(cwd, None);
    let saved_project_trust = get_saved_project_trust(&trust_store, cwd)
        .await
        .ok()
        .flatten();
    let global_preset = startup_settings_manager
        .get_global()
        .get("permissionPreset");
    let full_access_trust =
        preset_trusts_project(flag_values.get("permission-preset"), global_preset)
            && saved_project_trust != Some(false);
    let should_resolve_project_trust = configuration.project_trust_override.is_none()
        && !full_access_trust
        && has_trust_requiring_resources;

    let mut project_trusted = if should_resolve_project_trust {
        false
    } else {
        configuration.project_trust_override.unwrap_or_else(|| {
            full_access_trust || !has_trust_requiring_resources || saved_project_trust == Some(true)
        })
    };

    if should_resolve_project_trust {
        let default_project_trust = startup_settings_manager
            .get_global()
            .get("defaultProjectTrust")
            .and_then(Value::as_str)
            .and_then(DefaultProjectTrust::parse);
        let ctx = HeadlessProjectTrustContext;
        let trust_diags = std::sync::Mutex::new(Vec::new());
        let on_error = |message: &str| {
            if let Ok(mut guard) = trust_diags.lock() {
                guard.push(AgentSessionRuntimeDiagnostic {
                    kind: AgentSessionRuntimeDiagnosticType::Warning,
                    message: message.to_owned(),
                });
            }
        };
        project_trusted = resolve_project_trusted(ResolveProjectTrustedOptions {
            cwd,
            trust_store: &trust_store,
            trust_override: configuration.project_trust_override,
            default_project_trust,
            emit_project_trust: None,
            project_trust_context: &ctx,
            on_extension_error: Some(&on_error),
            on_trust_store_error: Some(&on_error),
        })
        .await;
        if let Ok(mut guard) = trust_diags.into_inner() {
            diagnostics.append(&mut guard);
        }
    }

    let trust_from_full_access = full_access_trust
        && has_trust_requiring_resources
        && configuration.project_trust_override.is_none()
        && saved_project_trust != Some(true);

    let mut settings_manager =
        SettingsManager::create(cwd, &configuration.agent_dir, &home_dir(), project_trusted);
    settings_manager.set_trust_from_full_access(trust_from_full_access);
    settings_manager.set_project_trust_locked(configuration.project_trust_override.is_some());

    LooplessRuntimeSettings {
        settings_manager,
        flag_values,
        should_resolve_project_trust,
    }
}

pub fn resolve_cli_paths(cwd: &str, paths: &[String]) -> Vec<String> {
    let opts = PathInputOptions::default();
    paths
        .iter()
        .map(|value| {
            if is_local_path(value) {
                resolve_path(value, cwd, &opts)
            } else {
                value.clone()
            }
        })
        .collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LooplessToolFilters {
    pub default_tool_names: Option<HashSet<String>>,
    pub allowed_tool_names: Option<HashSet<String>>,
    pub excluded_tool_names: Option<HashSet<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LooplessToolSelection {
    /// The registry filters, as a session applies them.
    pub filters: LooplessToolFilters,
    /// The active set a session would request for the same flags and settings.
    pub initial_active_tool_names: Vec<String>,
}

/// The CLI's tool filters (`--tools`, `--no-tools`, `--no-builtin-tools`, `--exclude-tools`,
/// settings `defaultTools`), resolved by `resolve_tool_selection`, so a loopless runtime registers
/// exactly the tools a session registers for the same flags and settings.
pub fn loopless_tool_selection(
    configuration: &LooplessCliConfiguration,
    settings_manager: &SettingsManager,
) -> LooplessToolSelection {
    let configured_defaults: Option<Vec<String>> = settings_manager
        .get_value("defaultTools")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        });
    let resolved = resolve_tool_selection(
        &ToolSelection {
            tools: configuration.tools.clone(),
            no_all_tools: configuration.no_tools,
            has_no_tools: configuration.no_tools || configuration.no_builtin_tools,
            exclude_tools: configuration.exclude_tools.clone(),
        },
        configured_defaults.as_deref(),
    );
    LooplessToolSelection {
        filters: LooplessToolFilters {
            default_tool_names: resolved.default_tool_names.map(|v| v.into_iter().collect()),
            allowed_tool_names: resolved.allowed_tool_names.map(|v| v.into_iter().collect()),
            excluded_tool_names: resolved
                .excluded_tool_names
                .map(|v| v.into_iter().collect()),
        },
        initial_active_tool_names: resolved.initial_active_tool_names,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_selection_maps_flags_and_exclusions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = SettingsManager::create(
            &dir.path().to_string_lossy(),
            &dir.path().to_string_lossy(),
            &dir.path().to_string_lossy(),
            false,
        );
        let cfg = LooplessCliConfiguration {
            tools: Some(vec!["read".into(), "bash".into(), "edit".into()]),
            exclude_tools: Some(vec!["bash".into()]),
            ..Default::default()
        };
        let sel = loopless_tool_selection(&cfg, &settings);
        assert_eq!(
            sel.filters.allowed_tool_names,
            Some(HashSet::from(["read".into(), "bash".into(), "edit".into()]))
        );
        assert_eq!(sel.initial_active_tool_names, vec!["read", "edit"]);
    }

    #[test]
    fn resolve_cli_paths_resolves_local_and_preserves_non_local() {
        let resolved = resolve_cli_paths("/workspace", &["./ext.ts".into(), "npm:pkg".into()]);
        assert_eq!(resolved[0], "/workspace/ext.ts");
        assert_eq!(resolved[1], "npm:pkg");
    }
}
