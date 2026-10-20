use std::path::{Path, PathBuf};

pub fn get_plugins_base_dir() -> PathBuf {
    std::env::var_os("CLAUDE_PLUGINS_HOME").filter(|value| !value.is_empty()).map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".claude/plugins"))
}

pub fn get_installed_plugins_path(plugins_base_dir: Option<&Path>) -> PathBuf {
    plugins_base_dir.map(Path::to_path_buf).unwrap_or_else(get_plugins_base_dir).join("installed_plugins.json")
}

pub fn get_claude_settings_path() -> PathBuf {
    std::env::var_os("CLAUDE_SETTINGS_PATH").filter(|value| !value.is_empty()).map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".claude/settings.json"))
}
