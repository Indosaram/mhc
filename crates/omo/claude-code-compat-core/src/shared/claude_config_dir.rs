use std::path::PathBuf;

pub fn get_claude_config_dir() -> PathBuf {
    if let Some(directory) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|s| !s.is_empty()) {
        return directory.into();
    }
    std::env::var_os("HOME")
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|s| !s.is_empty()))
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_default()
        .join(".claude")
}
