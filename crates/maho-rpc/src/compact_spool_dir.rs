//! senpi `modes/rpc/compact-spool-dir.ts`: ownership of the compact spool's epoch directory
//! (senpi#4272). The store creates the directory itself (0700, never adopting an existing one) and
//! writes a private marker into it; removal re-checks the directory and that marker without following
//! symlinks, so retirement never deletes a directory this store did not create and still own.
use std::io::ErrorKind;
use std::path::Path;

const MARKER: &str = ".compact-spool-owner";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactSpoolRefusal {
    PreExisting,
    MarkerWriteFailed,
    Symlink,
    NotDirectory,
    MarkerMissing,
    ForeignOwner,
    WideMode,
}

impl CompactSpoolRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreExisting => "pre_existing",
            Self::MarkerWriteFailed => "marker_write_failed",
            Self::Symlink => "symlink",
            Self::NotDirectory => "not_directory",
            Self::MarkerMissing => "marker_missing",
            Self::ForeignOwner => "foreign_owner",
            Self::WideMode => "wide_mode",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("compact spool directory refused ({}): {path}{detail}", .reason.as_str())]
pub struct CompactSpoolRefusedError {
    pub reason: CompactSpoolRefusal,
    pub path: String,
    detail: String,
}

impl CompactSpoolRefusedError {
    fn new(
        reason: CompactSpoolRefusal,
        path: &str,
        cause: Option<&std::io::Error>,
        cleanup_failure: Option<&std::io::Error>,
    ) -> Self {
        let mut detail = cause.map(|cause| format!(": {cause}")).unwrap_or_default();
        if let Some(failure) = cleanup_failure {
            detail.push_str(&format!("; removing the directory failed: {failure}"));
        }
        Self {
            reason,
            path: path.to_owned(),
            detail,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SpoolDirError {
    #[error(transparent)]
    Refused(#[from] CompactSpoolRefusedError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

async fn mkdir_private(path: &Path, recursive: bool) -> std::io::Result<()> {
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(recursive);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(path).await
}

/// Creates a fresh private epoch directory and returns the token its ownership marker holds.
pub async fn create_spool_dir(dir: &str) -> Result<String, SpoolDirError> {
    let path = Path::new(dir);
    if let Some(parent) = path.parent() {
        mkdir_private(parent, true).await?;
    }
    if let Err(cause) = mkdir_private(path, false).await {
        if cause.kind() == ErrorKind::AlreadyExists {
            return Err(CompactSpoolRefusedError::new(
                CompactSpoolRefusal::PreExisting,
                dir,
                None,
                None,
            )
            .into());
        }
        return Err(cause.into());
    }
    let token = uuid::Uuid::new_v4().to_string();
    let marker = path.join(MARKER).display().to_string();
    if let Err(cause) = crate::compact_storage_io::node_compact_storage_io()
        .write_file(marker, token.clone().into_bytes())
        .await
    {
        return Err(abandon_spool_dir(dir, cause).await.into());
    }
    Ok(token)
}

/// Removes a directory `create_spool_dir` just made but could not mark, so a later store creates it
/// fresh instead of refusing it as pre-existing. Only a marker this call's exclusive create made (any
/// failure but EEXIST) is unlinked, then the directory itself, which `rmdir` removes only while empty.
async fn abandon_spool_dir(dir: &str, cause: std::io::Error) -> CompactSpoolRefusedError {
    let path = Path::new(dir);
    let cleanup = async {
        if cause.kind() != ErrorKind::AlreadyExists {
            match tokio::fs::remove_file(path.join(MARKER)).await {
                Err(error) if error.kind() != ErrorKind::NotFound => return Err(error),
                _ => {}
            }
        }
        tokio::fs::remove_dir(path).await
    };
    let failure = cleanup.await.err();
    CompactSpoolRefusedError::new(
        CompactSpoolRefusal::MarkerWriteFailed,
        dir,
        Some(&cause),
        failure.as_ref(),
    )
}

/// Removes `dir` only while it is still the private, marked directory `create_spool_dir` made. A
/// directory that is already gone leaves nothing to remove, so retirement settles as a no-op.
pub async fn remove_spool_dir(dir: &str, token: &str) -> Result<(), SpoolDirError> {
    let owned = match tokio::fs::symlink_metadata(dir).await {
        Ok(owned) => owned,
        Err(cause) if cause.kind() == ErrorKind::NotFound => return Ok(()),
        Err(cause) => return Err(cause.into()),
    };
    if let Some(refusal) = refusal_for(dir, &owned, token).await {
        return Err(CompactSpoolRefusedError::new(refusal, dir, None, None).into());
    }
    match tokio::fs::remove_dir_all(dir).await {
        Err(cause) if cause.kind() != ErrorKind::NotFound => Err(cause.into()),
        _ => Ok(()),
    }
}

async fn refusal_for(
    dir: &str,
    owned: &std::fs::Metadata,
    token: &str,
) -> Option<CompactSpoolRefusal> {
    if owned.file_type().is_symlink() {
        return Some(CompactSpoolRefusal::Symlink);
    }
    if !owned.is_dir() {
        return Some(CompactSpoolRefusal::NotDirectory);
    }
    let marker_path = Path::new(dir).join(MARKER);
    let marker = tokio::fs::symlink_metadata(&marker_path).await.ok();
    let Some(marker) = marker.filter(std::fs::Metadata::is_file) else {
        return Some(CompactSpoolRefusal::MarkerMissing);
    };
    if tokio::fs::read_to_string(&marker_path)
        .await
        .ok()
        .as_deref()
        != Some(token)
    {
        return Some(CompactSpoolRefusal::MarkerMissing);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let uid = rustix::process::getuid().as_raw();
        if owned.uid() != uid || marker.uid() != uid {
            return Some(CompactSpoolRefusal::ForeignOwner);
        }
        if ((owned.mode() | marker.mode()) & 0o077) != 0 {
            return Some(CompactSpoolRefusal::WideMode);
        }
    }
    // Windows has no POSIX owner or mode bits; the marker token and the no-follow checks still apply.
    #[cfg(not(unix))]
    let _ = marker;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(error: SpoolDirError) -> Option<CompactSpoolRefusal> {
        match error {
            SpoolDirError::Refused(refused) => Some(refused.reason),
            SpoolDirError::Io(_) => None,
        }
    }

    #[tokio::test]
    async fn creates_private_marked_dir_and_never_adopts_an_existing_one() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path().join("recovery/epoch-1").display().to_string();
        let token = create_spool_dir(&dir).await.expect("create");
        assert_eq!(
            refusal(create_spool_dir(&dir).await.expect_err("second create")),
            Some(CompactSpoolRefusal::PreExisting)
        );
        assert_eq!(
            refusal(
                remove_spool_dir(&dir, "other")
                    .await
                    .expect_err("wrong token")
            ),
            Some(CompactSpoolRefusal::MarkerMissing)
        );
        remove_spool_dir(&dir, &token).await.expect("remove");
        assert!(!Path::new(&dir).exists());
        // Already gone settles as a no-op.
        remove_spool_dir(&dir, &token).await.expect("gone");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refuses_a_symlink_and_a_widened_dir() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path().join("epoch").display().to_string();
        let token = create_spool_dir(&dir).await.expect("create");
        let link = root.path().join("link").display().to_string();
        std::os::unix::fs::symlink(&dir, &link).expect("symlink");
        assert_eq!(
            refusal(remove_spool_dir(&link, &token).await.expect_err("symlink")),
            Some(CompactSpoolRefusal::Symlink)
        );
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert_eq!(
            refusal(remove_spool_dir(&dir, &token).await.expect_err("wide")),
            Some(CompactSpoolRefusal::WideMode)
        );
        assert!(Path::new(&dir).exists());
    }
}
