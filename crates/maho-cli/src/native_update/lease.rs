use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

/// Shared lock on `R/versions/<v>.lock`, held by a running payload session for its lifetime.
pub struct SessionLease {
    _file: File,
}

/// Exclusive lock on `R/.update.lock`, held only by an updater; sessions never take it.
pub struct UpdateLease {
    _file: File,
}

pub(super) fn version_lock_path(root: &Path, version: &str) -> PathBuf {
    root.join("versions").join(format!("{version}.lock"))
}

fn open_lock(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)
}

fn lock_error(error: TryLockError, busy: &str) -> io::Error {
    match error {
        TryLockError::WouldBlock => io::Error::new(io::ErrorKind::WouldBlock, busy),
        TryLockError::Error(error) => error,
    }
}

/// Opens an existing lock file without write access. A shared lock needs only a readable handle
/// (`flock` on Unix, `LockFileEx` on Windows), so a session whose view of the install root is
/// read-only (the memory reflection/facts child under `bwrap --ro-bind / /`, a read-only mount)
/// still takes the lease its parent created and keeps the version protected from cleanup.
fn open_existing_lock_read_only(path: &Path, write_error: io::Error) -> io::Result<File> {
    match write_error.kind() {
        io::ErrorKind::ReadOnlyFilesystem | io::ErrorKind::PermissionDenied => {
            OpenOptions::new().read(true).open(path).map_err(|read_error| {
                if read_error.kind() == io::ErrorKind::NotFound { write_error } else { read_error }
            })
        }
        _ => Err(write_error),
    }
}

impl SessionLease {
    pub fn acquire(root: &Path, version: &str) -> io::Result<Self> {
        let path = version_lock_path(root, version);
        let file = match open_lock(&path) {
            Ok(file) => file,
            Err(error) => open_existing_lock_read_only(&path, error)?,
        };
        file.try_lock_shared()
            .map_err(|error| lock_error(error, "version lock is held exclusively by a cleanup"))?;
        Ok(Self { _file: file })
    }
}

impl UpdateLease {
    pub fn acquire(root: &Path) -> io::Result<Self> {
        let file = open_lock(&root.join(".update.lock"))?;
        file.try_lock().map_err(|error| lock_error(error, "another update is running"))?;
        Ok(Self { _file: file })
    }
}

/// Non-blocking exclusive lock on a version's lock file; `None` while any session holds it.
pub(super) fn try_exclusive_version(root: &Path, version: &str) -> io::Result<Option<File>> {
    let file = open_lock(&version_lock_path(root, version))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_share_a_version_lock_and_updaters_exclude_each_other() -> io::Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("versions"))?;
        let first = SessionLease::acquire(root.path(), "1.0.0")?;
        let second = SessionLease::acquire(root.path(), "1.0.0")?;
        assert!(try_exclusive_version(root.path(), "1.0.0")?.is_none());
        let update = UpdateLease::acquire(root.path())?;
        assert!(matches!(UpdateLease::acquire(root.path()), Err(e) if e.kind() == io::ErrorKind::WouldBlock));
        drop(update);
        drop((first, second));
        assert!(try_exclusive_version(root.path(), "1.0.0")?.is_some());
        let _update = UpdateLease::acquire(root.path())?;
        Ok(())
    }

    /// A session that cannot write the install root still shares the existing version lock, and that
    /// lock still blocks cleanup.
    #[cfg(unix)]
    #[test]
    fn read_only_session_shares_the_existing_version_lock() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir()?;
        let versions = root.path().join("versions");
        std::fs::create_dir(&versions)?;
        drop(SessionLease::acquire(root.path(), "1.0.0")?);
        let lock = version_lock_path(root.path(), "1.0.0");
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o444))?;
        std::fs::set_permissions(&versions, std::fs::Permissions::from_mode(0o555))?;
        let writable = OpenOptions::new().write(true).open(&lock);
        let session = SessionLease::acquire(root.path(), "1.0.0");
        let missing = SessionLease::acquire(root.path(), "2.0.0");
        std::fs::set_permissions(&versions, std::fs::Permissions::from_mode(0o755))?;
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644))?;
        if writable.is_ok() {
            // Running as root: permissions do not restrict access, so the read-only path cannot occur.
            return Ok(());
        }
        let session = session?;
        assert!(matches!(missing, Err(e) if e.kind() == io::ErrorKind::PermissionDenied));
        assert!(try_exclusive_version(root.path(), "1.0.0")?.is_none(), "read-only lease defers cleanup");
        drop(session);
        assert!(try_exclusive_version(root.path(), "1.0.0")?.is_some());
        Ok(())
    }
}
