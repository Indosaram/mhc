//! `isolation/baseline-store.ts`: the on-disk baseline a later process reads back.
//!
//! The baseline is the ONLY way a later process can tell the child's work from the parent's: a host
//! that dies mid-run leaves no memory, so the salvage pass reads it back off disk.

use std::path::{Path, PathBuf};

use isolation_core::WorktreeBaseline;

/// `isolationArtifactsDir`: `<stateDir>/isolation/<taskId>`.
pub fn isolation_artifacts_dir(state_dir: &Path, task_id: &str) -> PathBuf {
    state_dir.join("isolation").join(task_id)
}

/// `baselinePath`: `<stateDir>/isolation/<taskId>/baseline.json`.
pub fn baseline_path(state_dir: &Path, task_id: &str) -> PathBuf {
    isolation_artifacts_dir(state_dir, task_id).join("baseline.json")
}

/// `writeBaseline`: persist the baseline as JSON, creating the artifacts directory.
pub fn write_baseline(
    state_dir: &Path,
    task_id: &str,
    baseline: &WorktreeBaseline,
) -> std::io::Result<PathBuf> {
    let path = baseline_path(state_dir, task_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string(baseline).map_err(std::io::Error::other)?;
    std::fs::write(&path, text)?;
    Ok(path)
}

/// `readBaseline`: the persisted baseline, or `None` when it is absent. A present-but-unparsable
/// file is an error, matching the TypeScript rethrow of every failure except `ENOENT`.
pub fn read_baseline(state_dir: &Path, task_id: &str) -> std::io::Result<Option<WorktreeBaseline>> {
    let path = baseline_path(state_dir, task_id);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(std::io::Error::other)
}
