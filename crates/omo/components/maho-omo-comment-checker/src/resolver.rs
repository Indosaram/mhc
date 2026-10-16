use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use comment_checker_core::{
    CommentCheckerCacheDirInput, comment_checker_binary_name, comment_checker_cache_dir,
    is_cached_comment_checker_current,
};

use crate::constants::{COMMENT_CHECKER_CACHE_DIR_NAME, COMMENT_CHECKER_ENV_KEY};
use crate::utils::is_missing_module_value;

pub fn is_missing_module_error(value: &serde_json::Value) -> bool {
    is_missing_module_value(value)
}

pub fn resolve_senpi_comment_checker_binary(
    env: &BTreeMap<String, String>,
    package_binary: Option<&Path>,
) -> Option<PathBuf> {
    resolve_with_options_cache(
        env,
        Path::exists,
        || package_binary.map(Path::to_owned),
        |name| {
            std::env::split_paths(env.get("PATH")?)
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| p.join(name))
                .find(|p| {
                    std::fs::metadata(p).is_ok_and(|m| {
                        #[cfg(unix)]
                        {
                            m.permissions().mode() & 0o111 != 0
                        }
                        #[cfg(not(unix))]
                        {
                            m.is_file()
                        }
                    })
                })
        },
        if cfg!(windows) {
            "win32"
        } else if cfg!(target_os = "macos") {
            "darwin"
        } else {
            "linux"
        },
        None,
    )
}

pub fn default_comment_checker_cache_dir(
    platform: &str,
    env: Option<&BTreeMap<String, String>>,
) -> PathBuf {
    let empty_env = BTreeMap::new();
    let env = env.unwrap_or(&empty_env);
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    comment_checker_cache_dir(&CommentCheckerCacheDirInput {
        platform,
        env,
        homedir: &home,
        cache_dir_name: COMMENT_CHECKER_CACHE_DIR_NAME,
    })
}

pub fn resolve_with_options(
    env: &BTreeMap<String, String>,
    exists: impl Fn(&Path) -> bool,
    package_binary: impl Fn() -> Option<PathBuf>,
    path_lookup: impl Fn(&str) -> Option<PathBuf>,
    platform: &str,
) -> Option<PathBuf> {
    resolve_with_options_cache(env, exists, package_binary, path_lookup, platform, None)
}

pub fn resolve_with_options_cache(
    env: &BTreeMap<String, String>,
    exists: impl Fn(&Path) -> bool,
    package_binary: impl Fn() -> Option<PathBuf>,
    path_lookup: impl Fn(&str) -> Option<PathBuf>,
    platform: &str,
    cache_dir: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(path) = env
        .get(COMMENT_CHECKER_ENV_KEY)
        .map(|v| PathBuf::from(v.trim()))
        .filter(|p| p.is_absolute() && exists(p))
    {
        return Some(path);
    }
    if let Some(path) = package_binary().filter(|p| exists(p)) {
        return Some(path);
    }
    let binary_name = comment_checker_binary_name(platform);
    if let Some(path) = path_lookup(binary_name) {
        return Some(path);
    }

    let default_cache = default_comment_checker_cache_dir(platform, Some(env));
    let cache_dir = cache_dir.unwrap_or(&default_cache);
    let cached = cache_dir.join(binary_name);
    if exists(&cached) && is_cached_comment_checker_current(cache_dir, None) {
        Some(cached)
    } else {
        None
    }
}
