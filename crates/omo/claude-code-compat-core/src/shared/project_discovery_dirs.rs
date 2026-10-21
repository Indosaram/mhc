use super::opencode_config_dir::resolve_config_path;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
    sync::{LazyLock, Mutex},
};

static WORKTREE_PATH_CACHE: LazyLock<Mutex<BTreeMap<PathBuf, Option<PathBuf>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

fn find_git_marker_ancestor(directory: &Path) -> Option<PathBuf> {
    resolve_config_path(directory)
        .ancestors()
        .find(|p| p.join(".git").exists())
        .map(Path::to_path_buf)
}

fn detect_worktree_path(directory: &Path) -> Option<PathBuf> {
    let key = resolve_config_path(directory);
    let mut cache = WORKTREE_PATH_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(path) = cache.get(&key) {
        return path.clone();
    }
    let path = find_git_marker_ancestor(&key).or_else(|| {
        let output = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(&key)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8(output.stdout).ok()?;
        Some(resolve_config_path(Path::new(text.trim())))
    });
    cache.insert(key, path.clone());
    path
}

pub fn find_project_opencode_command_dirs(
    start_directory: &Path,
    stop_directory: Option<&Path>,
) -> Vec<PathBuf> {
    let stop = stop_directory
        .map(resolve_config_path)
        .or_else(|| detect_worktree_path(start_directory));
    let mut result = Vec::new();
    let start = resolve_config_path(start_directory);
    for ancestor in start.ancestors() {
        for alias in ["commands", "command"] {
            let candidate = ancestor.join(".opencode").join(alias);
            if candidate.exists() {
                let candidate = resolve_config_path(&candidate);
                if !result.contains(&candidate) {
                    result.push(candidate);
                }
            }
        }
        if stop.as_deref() == Some(ancestor) {
            break;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nearest_aliases_are_discovered_without_crossing_worktree_root() {
        // Given ancestor commands outside and inside a worktree.
        let root = tempfile::tempdir().expect("fixture");
        let project = root.path().join("repo");
        let child = project.join("packages/app");
        for path in [
            root.path().join(".opencode/commands"),
            project.join(".git"),
            project.join(".opencode/commands"),
            child.join(".opencode/command"),
        ] {
            std::fs::create_dir_all(path).expect("fixture directory");
        }
        // When discovering from the nested app.
        let result = find_project_opencode_command_dirs(&child, None);
        // Then nearest aliases come first, and the outer root is excluded.
        assert_eq!(
            result,
            vec![
                std::fs::canonicalize(child.join(".opencode/command")).expect("canonical child alias"),
                std::fs::canonicalize(project.join(".opencode/commands")).expect("canonical project alias")
            ]
        );
    }
}
