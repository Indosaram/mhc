use crate::types::{
    ConfigMigrationDiscoveryFileSystem, ConfigMigrationDiscoveryOptions, DiscoveryFsError,
    PathOperations, Platform, StdDiscoveryFileSystem,
};

pub const CONFIG_FILE_NAMES: [&str; 4] = [
    "oh-my-openagent.jsonc",
    "oh-my-openagent.json",
    "oh-my-opencode.jsonc",
    "oh-my-opencode.json",
];

static STD_DISCOVERY_FILE_SYSTEM: StdDiscoveryFileSystem = StdDiscoveryFileSystem;

pub(crate) fn discovery_file_system<'a>(
    options: &ConfigMigrationDiscoveryOptions<'a>,
) -> &'a dyn ConfigMigrationDiscoveryFileSystem {
    options.file_system.unwrap_or(&STD_DISCOVERY_FILE_SYSTEM)
}

pub(crate) fn uses_windows_path_semantics(options: &ConfigMigrationDiscoveryOptions<'_>) -> bool {
    options.platform == Some(Platform::Win32) || (options.file_system.is_none() && cfg!(windows))
}

pub(crate) fn host_path_operations(
    options: &ConfigMigrationDiscoveryOptions<'_>,
) -> PathOperations {
    if uses_windows_path_semantics(options) {
        PathOperations::Win32
    } else {
        options.path_operations
    }
}

fn comparison_path(path: String, options: &ConfigMigrationDiscoveryOptions<'_>) -> String {
    if uses_windows_path_semantics(options) {
        path.replace('\\', "/").to_lowercase()
    } else {
        path
    }
}

pub(crate) fn canonical_path(
    path: &str,
    options: &ConfigMigrationDiscoveryOptions<'_>,
) -> Result<String, DiscoveryFsError> {
    let normalized = options.path_operations.normalize(path);
    let resolved =
        if uses_windows_path_semantics(options) && PathOperations::Win32.is_absolute(&normalized) {
            normalized
        } else {
            options.path_operations.resolve(&[&normalized])
        };
    match discovery_file_system(options).realpath(&resolved) {
        Ok(real) => Ok(real),
        Err(error) if error.is_missing() => Ok(resolved),
        Err(error) => Err(error),
    }
}

pub(crate) fn path_key(
    path: &str,
    options: &ConfigMigrationDiscoveryOptions<'_>,
) -> Result<String, DiscoveryFsError> {
    Ok(comparison_path(canonical_path(path, options)?, options))
}

pub(crate) fn config_paths(
    directory: &str,
    options: &ConfigMigrationDiscoveryOptions<'_>,
) -> Vec<String> {
    CONFIG_FILE_NAMES
        .iter()
        .map(|file_name| options.path_operations.join(&[directory, file_name]))
        .filter(|path| discovery_file_system(options).exists(path))
        .collect()
}

pub(crate) fn profile_directories(
    root: &str,
    options: &ConfigMigrationDiscoveryOptions<'_>,
) -> Result<Vec<String>, DiscoveryFsError> {
    let directory = options.path_operations.join(&[root, "profiles"]);
    match discovery_file_system(options).read_dir(&directory) {
        Ok(entries) => Ok(entries),
        Err(error) if error.is_missing() => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

fn is_within(parent: &str, child: &str, options: &ConfigMigrationDiscoveryOptions<'_>) -> bool {
    let path_operations = host_path_operations(options);
    let relative = path_operations.relative(parent, child);
    relative.is_empty() || (!relative.starts_with("..") && !path_operations.is_absolute(&relative))
}

pub(crate) fn project_directories(
    options: &ConfigMigrationDiscoveryOptions<'_>,
) -> Result<Vec<String>, DiscoveryFsError> {
    let mut directories = Vec::new();
    let home_dir = canonical_path(options.home_dir, options)?;
    let home_key = path_key(&home_dir, options)?;
    let mut current = canonical_path(options.cwd, options)?;
    let stop_at_home = is_within(&home_dir, &current, options);
    loop {
        directories.push(current.clone());
        if stop_at_home && path_key(&current, options)? == home_key {
            break;
        }
        let parent = host_path_operations(options).dirname(&current);
        if parent == current {
            break;
        }
        current = parent;
    }
    Ok(directories)
}
