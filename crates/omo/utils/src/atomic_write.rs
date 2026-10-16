//! Atomic file replacement: write `<path>.tmp`, fsync, rename.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenamePlatform {
    Windows,
    Other,
}

impl RenamePlatform {
    pub const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

pub type FsyncFn<'a> = &'a dyn Fn(&fs::File) -> io::Result<()>;

#[derive(Default)]
pub struct AtomicWriteOptions<'a> {
    /// Defaults to the running platform.
    pub platform: Option<RenamePlatform>,
    /// Replacement fsync (tests inject failures here).
    pub fsync: Option<FsyncFn<'a>>,
}

fn is_tolerated_fsync_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported | io::ErrorKind::InvalidInput
    ) || error.raw_os_error().is_some_and(|code| {
        [libc::EPERM, libc::EACCES, libc::ENOTSUP, libc::EINVAL].contains(&code)
    })
}

pub fn write_file_atomically(path: impl AsRef<Path>, content: &str) -> io::Result<()> {
    write_file_atomically_with(path, content, &AtomicWriteOptions::default())
}

pub fn write_file_atomically_with(
    path: impl AsRef<Path>,
    content: &str,
    options: &AtomicWriteOptions<'_>,
) -> io::Result<()> {
    let path = path.as_ref();
    let temp_path = PathBuf::from(format!(
        "{}.{}.{}.tmp",
        path.display(),
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        file.write_all(content.as_bytes())?;
        let synced = match options.fsync {
            Some(fsync) => fsync(&file),
            None => file.sync_all(),
        };
        if let Err(error) = synced
            && !is_tolerated_fsync_error(&error)
        {
            return Err(error);
        }
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    let rename_result = match fs::rename(&temp_path, path) {
        Ok(()) => Ok(()),
        Err(error)
            if options.platform.unwrap_or(RenamePlatform::current()) == RenamePlatform::Windows
                && error.kind() == io::ErrorKind::PermissionDenied =>
        {
            fs::remove_file(path)?;
            fs::rename(&temp_path, path)
        }
        Err(error) => Err(error),
    };
    if let Err(error) = rename_result {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    if options.platform.unwrap_or(RenamePlatform::current()) == RenamePlatform::Windows {
        return Ok(());
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let synced = OpenOptions::new()
        .read(true)
        .open(parent)
        .and_then(|directory| match options.fsync {
            Some(fsync) => fsync(&directory),
            None => directory.sync_all(),
        });
    if let Err(error) = synced && !is_tolerated_fsync_error(&error) {
        return Err(error);
    }
    Ok(())
}
