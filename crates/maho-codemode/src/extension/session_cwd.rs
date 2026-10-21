use std::{
    fs::Metadata,
    io,
    path::{Path, PathBuf},
};

#[derive(Debug, thiserror::Error)]
#[error("The session working directory {cwd} is unavailable ({reason}). Eval cells run in the session's project directory; reopen the session on an existing directory.", cwd = cwd.display())]
pub struct CodemodeSessionCwdUnavailableError {
    pub cwd: PathBuf,
    pub reason: &'static str,
}

/// Check before and after startup, without blocking the async executor on stat.
pub async fn assert_session_cwd_available(cwd: &Path) -> io::Result<()> {
    check_metadata(cwd, tokio::fs::metadata(cwd).await)
}

/// Warm reuse checks every cell without a filesystem thread-pool round trip.
pub fn assert_session_cwd_available_sync(cwd: &Path) -> io::Result<()> {
    check_metadata(cwd, std::fs::metadata(cwd))
}

fn check_metadata(cwd: &Path, metadata: io::Result<Metadata>) -> io::Result<()> {
    let (kind, reason) = match metadata {
        Ok(metadata) if metadata.is_dir() => return Ok(()),
        Ok(_) => (io::ErrorKind::NotADirectory, "not a directory"),
        Err(error) => match error.kind() {
            io::ErrorKind::NotFound => (error.kind(), "ENOENT"),
            io::ErrorKind::NotADirectory => (error.kind(), "ENOTDIR"),
            _ => return Err(error),
        },
    };
    Err(io::Error::new(
        kind,
        CodemodeSessionCwdUnavailableError {
            cwd: cwd.into(),
            reason,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrelated_stat_failure_preserves_original_error() {
        // Given a filesystem failure that reopening the session cannot repair.
        let error = io::Error::from(io::ErrorKind::PermissionDenied);
        // When classifying the stat result.
        let result = check_metadata(Path::new("project"), Err(error));
        // Then the original error is not relabeled as an unavailable cwd.
        let error = result.expect_err("stat failure");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.get_ref().is_none());
    }
}
