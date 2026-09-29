use std::collections::HashSet;

use crate::internal::posix_path::{
    account_home_dir, posix_dirname, posix_join, posix_resolve, to_posix_path,
};
use crate::loader::types::{
    OmoConfigEnv, OmoConfigReadFileSystem, SCOPE_PROJECT, SCOPE_USER, StdReadFileSystem,
};

pub const MAX_PROJECT_CONFIG_DIRECTORY_DEPTH: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OmoConfigPathCandidate {
    pub path: String,
    pub scope: &'static str,
}

pub struct ResolveOmoConfigPathsOptions<'a> {
    pub cwd: String,
    pub env: Option<OmoConfigEnv>,
    pub file_system: Option<&'a dyn OmoConfigReadFileSystem>,
    pub platform: Option<String>,
}

pub fn process_env() -> OmoConfigEnv {
    std::env::vars().collect()
}

pub fn resolve_home_dir(env: &OmoConfigEnv) -> String {
    let home_dir = env
        .get("HOME")
        .or_else(|| env.get("USERPROFILE"))
        .cloned()
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|path| to_posix_path(&path.to_string_lossy()))
                .unwrap_or_default()
        });
    if home_dir.starts_with('/') {
        posix_resolve(&[home_dir.as_str()])
    } else {
        to_posix_path(&posix_resolve(&[home_dir.as_str()]))
    }
}

pub fn resolve_user_omo_config_directory(env: &OmoConfigEnv) -> String {
    posix_join(&[resolve_home_dir(env).as_str(), ".maho"])
}

pub fn resolve_user_omo_config_path(env: &OmoConfigEnv) -> String {
    posix_join(&[resolve_user_omo_config_directory(env).as_str(), "omo.jsonc"])
}

fn detect_user_omo_json_path(
    env: &OmoConfigEnv,
    file_system: &dyn OmoConfigReadFileSystem,
) -> String {
    let config_dir = resolve_user_omo_config_directory(env);
    let jsonc_path = posix_join(&[config_dir.as_str(), "omo.jsonc"]);
    if file_system.exists(&jsonc_path) {
        return jsonc_path;
    }
    let json_path = posix_join(&[config_dir.as_str(), "omo.json"]);
    if file_system.exists(&json_path) {
        json_path
    } else {
        jsonc_path
    }
}

fn is_symlinked_project_path(path: &str, file_system: &dyn OmoConfigReadFileSystem) -> bool {
    if !file_system.exists(path) {
        return false;
    }
    file_system.is_symbolic_link(path).unwrap_or(false)
}

fn is_loadable_project_config_file(path: &str, file_system: &dyn OmoConfigReadFileSystem) -> bool {
    file_system.exists(path) && !is_symlinked_project_path(path, file_system)
}

fn detect_omo_json_path(dir: &str, file_system: &dyn OmoConfigReadFileSystem) -> Option<String> {
    let omo_dir = posix_join(&[dir, ".omo"]);
    if is_symlinked_project_path(&omo_dir, file_system) {
        return None;
    }
    let jsonc_path = posix_join(&[omo_dir.as_str(), "omo.jsonc"]);
    if is_loadable_project_config_file(&jsonc_path, file_system) {
        return Some(jsonc_path);
    }
    let json_path = posix_join(&[omo_dir.as_str(), "omo.json"]);
    if is_loadable_project_config_file(&json_path, file_system) {
        Some(json_path)
    } else {
        None
    }
}

fn realpath_or_self(path: &str, file_system: &dyn OmoConfigReadFileSystem) -> String {
    file_system
        .realpath(path)
        .unwrap_or_else(|| path.to_string())
}

pub fn find_project_config_paths_farthest_first(
    cwd: &str,
    home_dir: &str,
    file_system: &dyn OmoConfigReadFileSystem,
    account_home: Option<&str>,
) -> Vec<String> {
    let start_dir = posix_resolve(&[cwd]);
    let account_home = account_home.unwrap_or(home_dir);
    let mut boundary_dirs: Vec<String> = vec![posix_resolve(&[home_dir])];
    let account_boundary = posix_resolve(&[account_home]);
    if !boundary_dirs.contains(&account_boundary) {
        boundary_dirs.push(account_boundary);
    }
    let real_boundary_dirs: HashSet<String> = boundary_dirs
        .iter()
        .map(|path| realpath_or_self(path, file_system))
        .collect();

    let mut nearest_first: Vec<String> = Vec::new();
    let mut current_dir = start_dir;

    for _ in 0..MAX_PROJECT_CONFIG_DIRECTORY_DEPTH {
        let is_home_dir = boundary_dirs
            .iter()
            .any(|boundary| boundary == &current_dir)
            || real_boundary_dirs.contains(&realpath_or_self(&current_dir, file_system));
        let config_path = if is_home_dir {
            None
        } else {
            detect_omo_json_path(&current_dir, file_system)
        };
        if let Some(config_path) = config_path {
            nearest_first.push(config_path);
        }
        if is_home_dir {
            break;
        }
        let parent_dir = posix_dirname(&current_dir);
        if parent_dir == current_dir {
            break;
        }
        current_dir = parent_dir;
    }

    nearest_first.reverse();
    nearest_first
}

pub fn resolve_omo_config_paths(
    options: &ResolveOmoConfigPathsOptions<'_>,
) -> Vec<OmoConfigPathCandidate> {
    let default_file_system = StdReadFileSystem;
    let file_system: &dyn OmoConfigReadFileSystem =
        options.file_system.unwrap_or(&default_file_system);
    let default_env = process_env();
    let env = options.env.as_ref().unwrap_or(&default_env);
    let user_path = detect_user_omo_json_path(env, file_system);
    let account_home = account_home_dir();
    let project_paths = find_project_config_paths_farthest_first(
        &options.cwd,
        &resolve_home_dir(env),
        file_system,
        Some(account_home.as_str()),
    );
    let mut candidates = vec![OmoConfigPathCandidate {
        path: user_path,
        scope: SCOPE_USER,
    }];
    candidates.extend(
        project_paths
            .into_iter()
            .map(|path| OmoConfigPathCandidate {
                path,
                scope: SCOPE_PROJECT,
            }),
    );
    candidates
}
