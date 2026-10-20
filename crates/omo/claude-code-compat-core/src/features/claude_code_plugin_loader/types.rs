use std::{collections::BTreeMap, path::{Path, PathBuf}};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginScope { #[default] User, Project, Local, Managed }

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PluginInstallation {
    pub scope: PluginScope,
    pub install_path: PathBuf,
    pub version: String,
    pub installed_at: String,
    pub last_updated: String,
    pub git_commit_sha: Option<String>,
    pub is_local: Option<bool>,
    pub project_path: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PluginManifest {
    pub name: String,
    pub version: Option<String>,
    #[serde(flatten)]
    pub fields: BTreeMap<String, Value>,
}

pub type HooksConfig = Value;
pub type LspServerConfig = Value;
pub type PluginHookConfig = Value;
pub type PluginHooksConfig = Value;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InstalledPluginsV1 {
    pub version: u32,
    pub plugins: BTreeMap<String, PluginInstallation>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InstalledPluginsV2 {
    pub version: u32,
    pub plugins: BTreeMap<String, Vec<PluginInstallation>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InstalledPluginsDatabase {
    V1(InstalledPluginsV1),
    V2(InstalledPluginsV2),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LoadedPlugin {
    pub name: String,
    pub version: String,
    pub scope: PluginScope,
    pub install_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<PluginManifest>,
    pub plugin_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commands_dir: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agents_dir: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills_dir: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hooks_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lsp_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PluginLoadResult { pub plugins: Vec<LoadedPlugin>, pub errors: Vec<PluginLoadError> }

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PluginLoadError { pub plugin_key: String, pub install_path: PathBuf, pub error: String }

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ClaudeSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled_plugins: Option<BTreeMap<String, bool>>,
    #[serde(flatten)]
    pub fields: BTreeMap<String, Value>,
}

pub type PluginManifestLoader = fn(&Path) -> Option<PluginManifest>;

#[derive(Debug, Clone, Default)]
pub struct PluginLoaderOptions {
    pub cwd: Option<PathBuf>,
    pub claude_config_dir: Option<PathBuf>,
    pub plugins_home_override: Option<PathBuf>,
    pub load_plugin_manifest_override: Option<PluginManifestLoader>,
    pub enabled_plugins_override: Option<BTreeMap<String, bool>>,
    pub anthropic_provider: Option<String>,
}
