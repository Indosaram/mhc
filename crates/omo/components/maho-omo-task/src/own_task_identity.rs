//! `own-task-identity.ts`: the one hosted task id this process may claim as its own.
//!
//! A hosted child session's own task id is read from the session context its spawner attached
//! (`task_id` plus `state_dir = <task store>/children/<task_id>`). Task ids are only unique within
//! one store, so the id is trusted only when the whole resolved `state_dir` EQUALS
//! `<this engine's store>/children/<task_id>`; anything else (no context, a foreign store, a
//! lookalike path, a mismatched id, a path that cannot be resolved) answers `None`.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;

const CHILDREN_DIR: &str = "children";

/// `ownHostedTaskId`: `None` unless the context's `state_dir` is exactly this store's child folder.
pub fn own_hosted_task_id(context: Option<&Value>, engine_state_dir: &Path) -> Option<String> {
    let context = context?;
    let task_id = context.get("task_id").and_then(Value::as_str)?;
    if !is_single_path_segment(task_id) {
        return None;
    }
    let child_state_dir = context.get("state_dir").and_then(Value::as_str)?;
    if child_state_dir.is_empty() || engine_state_dir.as_os_str().is_empty() {
        return None;
    }
    let store = canonical(engine_state_dir)?;
    let child = canonical(Path::new(child_state_dir))?;
    (child == store.join(CHILDREN_DIR).join(task_id)).then(|| task_id.to_string())
}

/// A task id is one folder name, never a path step: otherwise `join` would normalize it elsewhere.
fn is_single_path_segment(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && Path::new(value).file_name().is_some_and(|name| name == value)
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// The real path of `path`, resolved through its nearest existing ancestor, so the same store spelled
/// through a symlink compares equal even before the child's folder exists. A step that exists but
/// cannot be resolved (a dangling symlink, an unreadable folder, a file used as a folder) answers
/// `None`, so its target is never taken on trust. It never panics: the caller runs in the shutdown loop.
fn canonical(path: &Path) -> Option<PathBuf> {
    let mut missing: Vec<String> = Vec::new();
    let mut current = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    loop {
        match std::fs::symlink_metadata(&current) {
            Ok(_) => {
                let mut resolved = std::fs::canonicalize(&current).ok()?;
                for name in missing.iter().rev() {
                    resolved.push(name);
                }
                return Some(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = current.parent()?.to_path_buf();
                if parent == current {
                    return None;
                }
                let name = current.file_name()?.to_string_lossy().into_owned();
                missing.push(name);
                current = parent;
            }
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store(dir: &Path) -> PathBuf {
        let store = dir.join("task-state");
        std::fs::create_dir_all(&store).expect("store dir");
        store
    }

    #[test]
    fn an_exact_child_state_dir_is_trusted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store(dir.path());
        let child = store.join("children").join("st_1");
        let context = json!({"task_id": "st_1", "state_dir": child.to_string_lossy()});
        assert_eq!(
            own_hosted_task_id(Some(&context), &store).as_deref(),
            Some("st_1")
        );
    }

    #[test]
    fn a_missing_children_folder_is_still_an_exact_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store(dir.path());
        let child = store.join("children").join("st_2");
        let context = json!({"task_id": "st_2", "state_dir": child.to_string_lossy()});
        assert_eq!(
            own_hosted_task_id(Some(&context), &store).as_deref(),
            Some("st_2")
        );
    }

    #[test]
    fn a_foreign_store_or_mismatched_id_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store(dir.path());
        let foreign = dir.path().join("other").join("children").join("st_1");
        assert_eq!(
            own_hosted_task_id(
                Some(&json!({"task_id": "st_1", "state_dir": foreign.to_string_lossy()})),
                &store
            ),
            None
        );
        let child = store.join("children").join("st_9");
        assert_eq!(
            own_hosted_task_id(
                Some(&json!({"task_id": "st_1", "state_dir": child.to_string_lossy()})),
                &store
            ),
            None
        );
    }

    #[test]
    fn a_path_step_or_missing_context_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store(dir.path());
        assert_eq!(own_hosted_task_id(None, &store), None);
        assert_eq!(
            own_hosted_task_id(
                Some(&json!({"task_id": "../escape", "state_dir": store.to_string_lossy()})),
                &store
            ),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_is_refused_and_a_live_one_resolves() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store(dir.path());
        let dangling = dir.path().join("dangling");
        std::os::unix::fs::symlink(dir.path().join("nowhere"), &dangling).expect("symlink");
        assert_eq!(
            own_hosted_task_id(
                Some(&json!({"task_id": "st_1", "state_dir": dangling.to_string_lossy()})),
                &store
            ),
            None
        );
        let link = dir.path().join("store-link");
        std::os::unix::fs::symlink(&store, &link).expect("symlink");
        let child = link.join("children").join("st_3");
        assert_eq!(
            own_hosted_task_id(
                Some(&json!({"task_id": "st_3", "state_dir": child.to_string_lossy()})),
                &store
            )
            .as_deref(),
            Some("st_3")
        );
    }
}
