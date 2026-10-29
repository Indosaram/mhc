//! Test fixtures for the team messaging layer (port of `__fixtures__/messaging-fakes.ts`).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use tempfile::TempDir;

use crate::store::StateDirConfig;

static CLEANUP_ROOTS: Mutex<Vec<TempDir>> = Mutex::new(Vec::new());

pub(crate) fn cleanup_messaging_tmp() {
    let roots: Vec<TempDir> = CLEANUP_ROOTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .drain(..)
        .collect();
    drop(roots);
}

pub(crate) fn temp_project_dir() -> PathBuf {
    let directory = tempfile::Builder::new()
        .prefix("senpi-team-messaging-")
        .tempdir()
        .expect("create temp project dir");
    let path = directory.path().to_path_buf();
    CLEANUP_ROOTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(directory);
    path
}

pub(crate) fn state_dir_config(project_dir: &Path) -> StateDirConfig {
    StateDirConfig {
        project_dir: project_dir.to_string_lossy().into_owned().into(),
        task_state_dir: None,
    }
}
