use std::collections::HashSet;

use crate::discovery_paths::{
    canonical_path, host_path_operations, path_key, uses_windows_path_semantics,
};
use crate::types::{ConfigMigrationDiscoveryOptions, DiscoveryFsError, Platform};

const TAURI_IDENTIFIERS: [&str; 2] = ["ai.opencode.desktop", "ai.opencode.desktop.dev"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfigRoot {
    pub active_profile: Option<String>,
    pub path: String,
    pub precedence: i64,
}

fn environment_value(options: &ConfigMigrationDiscoveryOptions<'_>, key: &str) -> Option<String> {
    options
        .environment
        .get(key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn is_windows_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/');
    let forward = path.replace('\\', "/").to_lowercase();
    let mount = forward
        .strip_prefix("/mnt/")
        .and_then(|rest| {
            rest.as_bytes()
                .first()
                .copied()
                .filter(u8::is_ascii_alphabetic)
                .map(|_| &rest[1..])
        })
        .is_some_and(|rest| rest.starts_with("/users/"));
    drive || mount
}

fn is_wsl(options: &ConfigMigrationDiscoveryOptions<'_>) -> bool {
    options.platform == Some(Platform::Linux)
        && (environment_value(options, "WSL_DISTRO_NAME").is_some()
            || environment_value(options, "WSL_INTEROP").is_some())
}

fn config_base_directory(options: &ConfigMigrationDiscoveryOptions<'_>) -> String {
    match environment_value(options, "XDG_CONFIG_HOME") {
        Some(configured) if !(is_wsl(options) && is_windows_path(&configured)) => configured,
        _ => options.path_operations.join(&[options.home_dir, ".config"]),
    }
}

fn default_root(options: &ConfigMigrationDiscoveryOptions<'_>) -> String {
    options
        .path_operations
        .join(&[&config_base_directory(options), "opencode"])
}

fn default_tauri_roots(options: &ConfigMigrationDiscoveryOptions<'_>) -> Vec<String> {
    if let Some(directories) = options.tauri_config_dirs {
        return directories.to_vec();
    }
    let base = if options.platform == Some(Platform::Darwin) {
        options
            .path_operations
            .join(&[options.home_dir, "Library", "Application Support"])
    } else {
        config_base_directory(options)
    };
    TAURI_IDENTIFIERS
        .iter()
        .map(|identifier| options.path_operations.join(&[&base, identifier]))
        .collect()
}

fn windows_roots(options: &ConfigMigrationDiscoveryOptions<'_>) -> Vec<String> {
    if !uses_windows_path_semantics(options) {
        return Vec::new();
    }
    let path_operations = host_path_operations(options);
    let app_data = environment_value(options, "APPDATA")
        .unwrap_or_else(|| path_operations.join(&[options.home_dir, "AppData", "Roaming"]));
    std::iter::once("opencode")
        .chain(TAURI_IDENTIFIERS)
        .map(|name| path_operations.join(&[&app_data, name]))
        .collect()
}

fn active_profile_root(path: &str, options: &ConfigMigrationDiscoveryOptions<'_>) -> ConfigRoot {
    let path_operations = options.path_operations;
    let normalized = path_operations.normalize(path);
    let profiles_directory = path_operations.dirname(&normalized);
    if path_operations.basename(&profiles_directory).to_lowercase() != "profiles" {
        return ConfigRoot {
            active_profile: None,
            path: normalized,
            precedence: 0,
        };
    }
    ConfigRoot {
        active_profile: Some(path_operations.basename(&normalized)),
        path: path_operations.dirname(&profiles_directory),
        precedence: 0,
    }
}

pub(crate) fn config_roots(
    options: &ConfigMigrationDiscoveryOptions<'_>,
) -> Result<Vec<ConfigRoot>, DiscoveryFsError> {
    let plain = |path: String, precedence: i64| ConfigRoot {
        active_profile: None,
        path,
        precedence,
    };
    let mut candidates: Vec<ConfigRoot> = Vec::new();
    if let Some(custom) = environment_value(options, "OPENCODE_CONFIG_DIR") {
        candidates.push(active_profile_root(&custom, options));
    }
    candidates.push(plain(default_root(options), 1));
    candidates.extend(
        default_tauri_roots(options)
            .into_iter()
            .zip(2..)
            .map(|(path, precedence)| plain(path, precedence)),
    );
    candidates.extend(
        windows_roots(options)
            .into_iter()
            .zip(4..)
            .map(|(path, precedence)| plain(path, precedence)),
    );

    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for candidate in candidates {
        if !seen.insert(path_key(&candidate.path, options)?) {
            continue;
        }
        let path = canonical_path(&candidate.path, options)?;
        result.push(ConfigRoot { path, ..candidate });
    }
    Ok(result)
}
