//! Port of senpi `terminal/monitor-state-dir.ts`: the per-session persistence dir for the
//! terminal lease, manifest and per-monitor state dirs.
use std::path::{Path, PathBuf};

/// `terminalStateDir`: `None` when the context carries no durable absolute session dir or runs a
/// one-shot `print`/`json` turn, which has no later generation to restore into.
pub fn terminal_state_dir(ctx: Option<&maho_ext_api::ExtensionContext>) -> Option<PathBuf> {
    let ctx = ctx?;
    if matches!(
        ctx.mode,
        maho_ext_api::ExtensionMode::Print | maho_ext_api::ExtensionMode::Json
    ) {
        return None;
    }
    terminal_state_dir_for(
        maho_ext_api::SessionManager::get_session_dir(ctx.session_manager.as_ref()).as_deref(),
    )
}

/// The path half of `terminalStateDir`, for callers that already hold the session dir.
pub fn terminal_state_dir_for(session_dir: Option<&Path>) -> Option<PathBuf> {
    let session_dir = session_dir?;
    if session_dir.as_os_str().is_empty() || !session_dir.is_absolute() {
        return None;
    }
    Some(session_dir.join("extensions").join("terminal"))
}

pub fn monitor_state_dir(terminal_dir: &Path, monitor_id: &str) -> PathBuf {
    terminal_dir.join("state").join(monitor_id)
}

pub async fn ensure_monitor_state_dir(
    terminal_dir: &Path,
    monitor_id: &str,
) -> std::io::Result<PathBuf> {
    ensure_monitor_state_dir_sync(terminal_dir, monitor_id)
}

/// The synchronous body of `ensure_monitor_state_dir`, for the spawn paths that run under the
/// manager's std mutex.
pub fn ensure_monitor_state_dir_sync(
    terminal_dir: &Path,
    monitor_id: &str,
) -> std::io::Result<PathBuf> {
    let dir = monitor_state_dir(terminal_dir, monitor_id);
    // The crate's tokio has no `fs` feature; a single mkdir is not worth a blocking-pool hop.
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(&dir)?;
    Ok(dir)
}

pub async fn remove_monitor_state_dir(
    terminal_dir: &Path,
    monitor_id: &str,
) -> std::io::Result<()> {
    remove_monitor_state_dir_sync(terminal_dir, monitor_id)
}

/// The synchronous body of `remove_monitor_state_dir`, for the monitor-ended sink, which runs
/// inside the registry's settle path.
pub fn remove_monitor_state_dir_sync(terminal_dir: &Path, monitor_id: &str) -> std::io::Result<()> {
    match std::fs::remove_dir_all(monitor_state_dir(terminal_dir, monitor_id)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_dir_requires_an_absolute_session_dir() {
        assert_eq!(terminal_state_dir_for(None), None);
        assert_eq!(terminal_state_dir_for(Some(Path::new(""))), None);
        assert_eq!(terminal_state_dir_for(Some(Path::new("relative"))), None);
        let root = std::env::temp_dir();
        assert_eq!(
            terminal_state_dir_for(Some(&root)),
            Some(root.join("extensions").join("terminal"))
        );
    }

    #[tokio::test]
    async fn ensure_and_remove_monitor_state_dir() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let created = ensure_monitor_state_dir(dir.path(), "mon_1").await?;
        assert_eq!(created, dir.path().join("state").join("mon_1"));
        assert!(created.is_dir());
        remove_monitor_state_dir(dir.path(), "mon_1").await?;
        assert!(!created.exists());
        remove_monitor_state_dir(dir.path(), "mon_1").await
    }
}
