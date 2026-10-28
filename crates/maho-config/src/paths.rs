//! Where configuration files live (spec §2).

use crate::env::{Env, forward_slashes, home_dir_with};
use crate::fs::ConfigFs;
use std::path::{Path, PathBuf};

/// Maximum number of directories visited by the ancestor walk.
pub const MAX_WALK_DEPTH: usize = 256;

/// Which layer a candidate belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    User,
    Project,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::User => "user",
            Scope::Project => "project",
        }
    }
}

/// A configuration file candidate in precedence order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    pub scope: Scope,
}

/// The user candidate inside `<home>/.omo` (read even when it is a symlink).
pub fn user_candidate(home: &Path, fs: &dyn ConfigFs) -> PathBuf {
    let dir = home.join(".omo");
    let jsonc = dir.join("omo.jsonc");
    let json = dir.join("omo.json");
    if !fs.exists(&jsonc) && fs.exists(&json) {
        json
    } else {
        jsonc
    }
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        forward_slashes(&a.to_string_lossy()).to_lowercase()
            == forward_slashes(&b.to_string_lossy()).to_lowercase()
    } else {
        a == b
    }
}

fn is_boundary(
    dir: &Path,
    boundaries: &[PathBuf],
    real_boundaries: &[PathBuf],
    fs: &dyn ConfigFs,
) -> bool {
    if boundaries.iter().any(|boundary| paths_equal(dir, boundary)) {
        return true;
    }
    match fs.realpath(dir) {
        Ok(real) => real_boundaries
            .iter()
            .any(|boundary| paths_equal(&real, boundary)),
        Err(_) => false,
    }
}

/// The project configuration file of `dir`, applying the symlink protections of §2.3.
pub fn project_file_in(dir: &Path, fs: &dyn ConfigFs) -> Option<PathBuf> {
    let omo = dir.join(".omo");
    if fs.is_symlink(&omo) || !fs.is_dir(&omo) {
        return None;
    }
    let jsonc = omo.join("omo.jsonc");
    let json = omo.join("omo.json");
    if fs.is_symlink(&jsonc) {
        return fs.is_regular_file(&json).then_some(json);
    }
    if fs.exists(&jsonc) {
        return Some(jsonc);
    }
    if fs.is_symlink(&json) || !fs.exists(&json) {
        return None;
    }
    Some(json)
}

/// Project configuration files from `cwd` upwards, ordered farthest ancestor first.
pub fn project_paths_farthest_first(
    cwd: &Path,
    home: &Path,
    fs: &dyn ConfigFs,
    account_home: Option<&Path>,
) -> Vec<PathBuf> {
    let mut boundaries = vec![home.to_path_buf()];
    if let Some(account) = account_home {
        boundaries.push(account.to_path_buf());
    }
    let real_boundaries: Vec<PathBuf> = boundaries
        .iter()
        .filter_map(|b| fs.realpath(b).ok())
        .collect();
    let start = maho_support::normalize_lexically(cwd);
    let mut found = Vec::new();
    let mut current = Some(start.as_path());
    let mut visited = 0;
    while let Some(dir) = current {
        if visited >= MAX_WALK_DEPTH || is_boundary(dir, &boundaries, &real_boundaries, fs) {
            break;
        }
        visited += 1;
        if let Some(file) = project_file_in(dir, fs) {
            found.push(file);
        }
        current = dir.parent();
    }
    found.reverse();
    found
}

/// Every candidate in precedence order: the user candidate, then project files farthest first.
pub fn resolve_paths(
    cwd: &Path,
    env: &Env,
    fs: &dyn ConfigFs,
    account_home: Option<&Path>,
) -> Vec<Candidate> {
    let home = home_dir_with(env, cwd);
    let mut out = vec![Candidate {
        path: user_candidate(&home, fs),
        scope: Scope::User,
    }];
    out.extend(
        project_paths_farthest_first(cwd, &home, fs, account_home)
            .into_iter()
            .map(|path| Candidate {
                path,
                scope: Scope::Project,
            }),
    );
    out
}
