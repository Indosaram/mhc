//! In-process serialization queue for Git worktree mutations.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::errors::GitError;

static WORKTREE_LOCKS: Mutex<Option<HashMap<PathBuf, Arc<Mutex<()>>>>> = Mutex::new(None);

fn get_worktree_lock(dir: &Path) -> Arc<Mutex<()>> {
    let mut guard = WORKTREE_LOCKS.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    let key = dir.to_path_buf();
    map.entry(key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Executes a worktree mutation holding an in-process per-directory lock.
pub fn with_serialized_git_worktree_mutation<T, F>(dir: &Path, mut mutate: F) -> Result<T, GitError>
where
    F: FnMut() -> Result<T, GitError>,
{
    let lock = get_worktree_lock(dir);
    let _guard = lock.lock().unwrap();
    mutate()
}
