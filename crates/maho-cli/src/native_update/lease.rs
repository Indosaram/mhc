use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

pub struct SessionLease {
    _file: File,
}

pub struct UpdateLease {
    _file: File,
}

pub(super) fn lock_path(installation: &Path) -> io::Result<PathBuf> {
    let parent = installation.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "installation has no parent")
    })?;
    let name = installation.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "installation has no name")
    })?;
    let mut filename = std::ffi::OsString::from(".");
    filename.push(name);
    filename.push(".update.lock");
    Ok(parent.join(filename))
}

fn open_lock(installation: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path(installation)?)
}

fn lock_error(error: TryLockError) -> io::Error {
    match error {
        TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            "this installation is in use; close other mhc sessions before updating",
        ),
        TryLockError::Error(error) => error,
    }
}

impl SessionLease {
    pub fn acquire(installation: &Path) -> io::Result<Self> {
        let file = open_lock(installation)?;
        file.try_lock_shared().map_err(lock_error)?;
        Ok(Self { _file: file })
    }
}

impl UpdateLease {
    pub fn acquire(installation: &Path) -> io::Result<Self> {
        let file = open_lock(installation)?;
        file.try_lock().map_err(lock_error)?;
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_share_access_but_exclude_replacement() -> io::Result<()> {
        let root = tempfile::tempdir()?;
        let installation = root.path().join("mhc");
        std::fs::create_dir(&installation)?;
        let first = SessionLease::acquire(&installation)?;
        let second = SessionLease::acquire(&installation)?;
        assert!(matches!(UpdateLease::acquire(&installation), Err(e) if e.kind() == io::ErrorKind::WouldBlock));
        drop(first);
        assert!(UpdateLease::acquire(&installation).is_err());
        drop(second);
        let update = UpdateLease::acquire(&installation)?;
        assert!(matches!(SessionLease::acquire(&installation), Err(e) if e.kind() == io::ErrorKind::WouldBlock));
        assert!(UpdateLease::acquire(&installation).is_err());
        drop(update);
        let _session = SessionLease::acquire(&installation)?;
        Ok(())
    }

    #[test]
    fn lock_identity_survives_installation_rename() -> io::Result<()> {
        let root = tempfile::tempdir()?;
        let installation = root.path().join("mhc");
        std::fs::create_dir(&installation)?;
        let update = UpdateLease::acquire(&installation)?;
        std::fs::rename(&installation, root.path().join("backup"))?;
        std::fs::create_dir(&installation)?;
        assert!(SessionLease::acquire(&installation).is_err());
        drop(update);
        assert!(lock_path(&installation)?.is_file());
        let _session = SessionLease::acquire(&installation)?;
        Ok(())
    }
}
