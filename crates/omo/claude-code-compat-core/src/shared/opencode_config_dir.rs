pub use super::opencode_config_dir_types::*;
use super::plugin_identity::CONFIG_BASENAME;
use std::{
    env,
    path::{Component, Path, PathBuf},
};

pub const TAURI_APP_IDENTIFIER: &str = "ai.opencode.desktop";
pub const TAURI_APP_IDENTIFIER_DEV: &str = "ai.opencode.desktop.dev";

pub fn is_dev_build(version: Option<&str>) -> bool {
    version.is_some_and(|v| v.contains("-dev") || v.contains(".dev"))
}

fn env_trimmed(key: &str) -> Option<String> {
    #[cfg(test)]
    let value = crate::test_support::env_var(key);
    #[cfg(not(test))]
    let value = env::var(key);
    value
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn is_wsl_environment() -> bool {
    cfg!(target_os = "linux")
        && (env_trimmed("WSL_DISTRO_NAME").is_some() || env_trimmed("WSL_INTEROP").is_some())
}

fn windows_user(path: &str) -> Option<String> {
    let normalized = path.replace('\\', "/");
    let parts: Vec<_> = normalized.split('/').collect();
    let user_index = if parts
        .first()
        .is_some_and(|p| p.len() == 2 && p.ends_with(':'))
    {
        2
    } else if parts.len() > 4
        && parts[0].is_empty()
        && parts[1].eq_ignore_ascii_case("mnt")
        && parts[2].len() == 1
    {
        4
    } else {
        return None;
    };
    if !parts.get(user_index - 1)?.eq_ignore_ascii_case("users") {
        return None;
    }
    parts
        .get(user_index)
        .filter(|s| !s.is_empty())
        .map(|s| (*s).to_owned())
}

pub(crate) fn resolve_config_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir().unwrap_or_default().join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str())
            }
        }
    }
    if is_wsl_environment() && path.is_absolute() {
        return normalized;
    }
    std::fs::canonicalize(&normalized).unwrap_or(normalized)
}

fn get_tauri_config_dir(identifier: &str) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_default();
    if cfg!(target_os = "macos") {
        return home.join("Library/Application Support").join(identifier);
    }
    if cfg!(windows) {
        return env_trimmed("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Roaming"))
            .join(identifier);
    }
    env_trimmed("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"))
        .join(identifier)
}

fn get_cli_default_config_dir() -> PathBuf {
    let xdg = env_trimmed("XDG_CONFIG_HOME");
    let config = match xdg {
        Some(xdg) if is_wsl_environment() && windows_user(&xdg).is_some() => {
            let home = env_trimmed("HOME")
                .filter(|p| p.starts_with('/') && windows_user(p).is_none())
                .or_else(|| {
                    ["USER", "LOGNAME", "SUDO_USER"]
                        .into_iter()
                        .find_map(env_trimmed)
                        .or_else(|| windows_user(&xdg))
                        .map(|user| format!("/home/{user}"))
                })
                .unwrap_or_else(|| "/home".to_owned());
            PathBuf::from(home).join(".config")
        }
        Some(xdg) => PathBuf::from(xdg),
        None => dirs::home_dir().unwrap_or_default().join(".config"),
    };
    resolve_config_path(&config.join("opencode"))
}

fn get_cli_custom_config_dir() -> Option<PathBuf> {
    env_trimmed("OPENCODE_CONFIG_DIR").map(|p| resolve_config_path(Path::new(&p)))
}

pub fn get_open_code_config_dirs(options: &OpenCodeConfigDirOptions) -> Vec<PathBuf> {
    if options.binary == OpenCodeBinaryType::OpencodeDesktop {
        return vec![get_open_code_config_dir(options)];
    }
    let mut result: Vec<_> = get_cli_custom_config_dir().into_iter().collect();
    let default = get_cli_default_config_dir();
    if !result.contains(&default) {
        result.push(default);
    }
    result
}

pub fn get_open_code_config_dir(options: &OpenCodeConfigDirOptions) -> PathBuf {
    let legacy = get_cli_custom_config_dir().unwrap_or_else(get_cli_default_config_dir);
    if options.binary == OpenCodeBinaryType::Opencode {
        return legacy;
    }
    if options.check_existing && has_config(&legacy) {
        return legacy;
    }
    let identifier = if is_dev_build(options.version.as_deref()) {
        TAURI_APP_IDENTIFIER_DEV
    } else {
        TAURI_APP_IDENTIFIER
    };
    resolve_config_path(&get_tauri_config_dir(identifier))
}

fn has_config(dir: &Path) -> bool {
    dir.join("opencode.json").exists() || dir.join("opencode.jsonc").exists()
}

pub fn get_open_code_config_paths(options: &OpenCodeConfigDirOptions) -> OpenCodeConfigPaths {
    let config_dir = get_open_code_config_dir(options);
    OpenCodeConfigPaths {
        config_json: config_dir.join("opencode.json"),
        config_jsonc: config_dir.join("opencode.jsonc"),
        package_json: config_dir.join("package.json"),
        omo_config: config_dir.join(format!("{CONFIG_BASENAME}.json")),
        config_dir,
    }
}

pub fn detect_existing_config_dir(
    binary: OpenCodeBinaryType,
    version: Option<&str>,
) -> Option<PathBuf> {
    let mut locations: Vec<_> = get_cli_custom_config_dir().into_iter().collect();
    if binary == OpenCodeBinaryType::OpencodeDesktop {
        locations.push(get_tauri_config_dir(if is_dev_build(version) {
            TAURI_APP_IDENTIFIER_DEV
        } else {
            TAURI_APP_IDENTIFIER
        }));
        if is_dev_build(version) {
            locations.push(get_tauri_config_dir(TAURI_APP_IDENTIFIER));
        }
    }
    locations.push(get_cli_custom_config_dir().unwrap_or_else(get_cli_default_config_dir));
    locations.into_iter().find(|dir| has_config(dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn development_versions_and_windows_home_roots_are_recognized() {
        // Given platform-independent version and root strings.
        // When classifying them, then only development versions and user roots match.
        assert!(is_dev_build(Some("1.0-dev")));
        assert!(is_dev_build(Some("1.0.dev.1")));
        assert!(!is_dev_build(None));
        assert_eq!(
            windows_user("C:\\Users\\alice\\.config"),
            Some("alice".into())
        );
        assert_eq!(
            windows_user("/mnt/c/Users/alice/.config"),
            Some("alice".into())
        );
        assert_eq!(windows_user("/home/alice/.config"), None);
    }
}
