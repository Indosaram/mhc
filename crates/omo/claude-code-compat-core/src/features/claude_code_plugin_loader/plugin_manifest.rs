use super::types::PluginManifest;
use crate::shared::logger::log;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn find_plugin_manifest_path(install_path: &Path) -> Option<PathBuf> {
    [
        install_path.join(".claude-plugin/plugin.json"),
        install_path.join("plugin.json"),
    ]
    .into_iter()
    .find(|path| path.exists())
}

pub fn load_plugin_manifest(install_path: &Path) -> Option<PluginManifest> {
    let path = find_plugin_manifest_path(install_path)?;
    let manifest = read_manifest_from_path(&path);
    if manifest.is_none() {
        log(
            &format!("Failed to load plugin manifest from {}", path.display()),
            None,
        );
    }
    manifest
}

pub fn read_manifest_from_path(path: &Path) -> Option<PluginManifest> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(_) => return None,
    };
    serde_json::from_str(&content).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn claude_manifest_layout_wins_over_legacy_layout() {
        let root = tempfile::tempdir().expect("fixture directory");
        fs::create_dir(root.path().join(".claude-plugin")).expect("manifest directory");
        fs::write(
            root.path().join(".claude-plugin/plugin.json"),
            r#"{"name":"claude-layout","version":"2.0.0"}"#,
        )
        .expect("manifest");
        fs::write(
            root.path().join("plugin.json"),
            r#"{"name":"root-layout","version":"1.0.0"}"#,
        )
        .expect("legacy manifest");
        let manifest = load_plugin_manifest(root.path()).expect("loaded manifest");
        assert_eq!(manifest.name, "claude-layout");
        assert_eq!(manifest.version.as_deref(), Some("2.0.0"));
    }
}
