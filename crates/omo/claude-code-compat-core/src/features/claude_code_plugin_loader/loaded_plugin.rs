use std::path::Path;
use super::{plugin_key::derive_plugin_name_from_key, types::*};

fn resolve_version(installation: &str, manifest: Option<&str>) -> String {
    let trimmed = installation.trim();
    if !trimmed.is_empty() && trimmed != "unknown" { return installation.to_owned(); }
    if let Some(version) = manifest.filter(|version| !version.trim().is_empty()) { return version.to_owned(); }
    if !trimmed.is_empty() { installation.to_owned() } else { "unknown".to_owned() }
}

pub fn create_loaded_plugin(plugin_key: &str, installation: &PluginInstallation, install_path: &Path, manifest: Option<PluginManifest>) -> LoadedPlugin {
    let name = manifest.as_ref().map(|manifest| &manifest.name).filter(|name| !name.is_empty()).cloned().unwrap_or_else(|| derive_plugin_name_from_key(plugin_key));
    let version = resolve_version(&installation.version, manifest.as_ref().and_then(|manifest| manifest.version.as_deref()));
    let component = |relative: &str| { let path = install_path.join(relative); path.exists().then_some(path) };
    LoadedPlugin {
        name, version, scope: installation.scope, install_path: install_path.to_path_buf(), plugin_key: plugin_key.to_owned(), manifest,
        commands_dir: component("commands"), agents_dir: component("agents"), skills_dir: component("skills"),
        hooks_path: component("hooks/hooks.json"), mcp_path: component(".mcp.json"), lsp_path: None,
    }
}
