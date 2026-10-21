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

impl SessionLease {
    pub fn acquire(root: &Path, version: &str) -> io::Result<Self> {
        let file = open_lock(&version_lock_path(root, version))?;
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
#[cfg(test)]
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
}
