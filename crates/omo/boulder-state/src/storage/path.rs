//! State-file and plan-path resolution (`storage/path.ts`).

use std::path::{Component, Path, PathBuf};

use crate::constants::{BOULDER_DIR, BOULDER_FILE};
use crate::records::{BoulderState, BoulderWorkState};

/// `<directory>/.omo/boulder.json`.
pub fn get_boulder_file_path(directory: &Path) -> PathBuf {
    directory.join(BOULDER_DIR).join(BOULDER_FILE)
}

/// Absolute path of the state's active plan, preferring the copy inside its worktree
/// when the plan lives under `directory` and that copy exists.
pub fn resolve_boulder_plan_path(directory: &Path, state: &BoulderState) -> PathBuf {
    resolve_plan_path(
        directory,
        state.active_plan().unwrap_or_default(),
        state.worktree_path(),
    )
}

/// [`resolve_boulder_plan_path`] for one work.
pub fn resolve_boulder_plan_path_for_work(directory: &Path, work: &BoulderWorkState) -> PathBuf {
    resolve_plan_path(
        directory,
        work.active_plan().unwrap_or_default(),
        work.worktree_path(),
    )
}

fn resolve_plan_path(directory: &Path, active_plan: &str, worktree_path: Option<&str>) -> PathBuf {
    let absolute_plan_path = resolve_tracked_path(directory, Path::new(active_plan));
    let Some(worktree_path) = worktree_path.map(str::trim).filter(|path| !path.is_empty()) else {
        return absolute_plan_path;
    };

    let absolute_directory = resolve_tracked_path(directory, Path::new(""));
    let Some(relative_plan_path) = relative(&absolute_directory, &absolute_plan_path) else {
        return absolute_plan_path;
    };
    let relative_text = relative_plan_path.to_string_lossy();
    if relative_text.is_empty() || relative_text.starts_with("..") {
        return absolute_plan_path;
    }

    let absolute_worktree_path = resolve_tracked_path(directory, Path::new(worktree_path));
    let worktree_plan_path = normalize(&absolute_worktree_path.join(relative_plan_path));
    if worktree_plan_path.exists() {
        worktree_plan_path
    } else {
        absolute_plan_path
    }
}

/// Node's `path.resolve(base, tracked)`: absolute and lexically normalized.
fn resolve_tracked_path(base: &Path, tracked: &Path) -> PathBuf {
    if tracked.is_absolute() {
        return normalize(tracked);
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    normalize(&cwd.join(base).join(tracked))
}

fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                let at_root = matches!(
                    normalized.components().next_back(),
                    None | Some(Component::RootDir | Component::Prefix(_))
                );
                if !at_root {
                    normalized.pop();
                }
            }
        }
    }
    normalized
}

/// Node's `path.relative(from, to)` for two normalized absolute paths; `None` when they
/// share no root (the `isAbsolute(relative)` case on Windows).
fn relative(from: &Path, to: &Path) -> Option<PathBuf> {
    let from_parts: Vec<Component<'_>> = from.components().collect();
    let to_parts: Vec<Component<'_>> = to.components().collect();
    let common = from_parts
        .iter()
        .zip(&to_parts)
        .take_while(|(left, right)| left == right)
        .count();
    if common == 0 {
        return None;
    }
    let mut result = PathBuf::new();
    for _ in common..from_parts.len() {
        result.push("..");
    }
    for part in &to_parts[common..] {
        result.push(part);
    }
    Some(result)
}
