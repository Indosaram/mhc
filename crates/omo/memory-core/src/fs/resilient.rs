//! EINTR-resilient filesystem boundary: the memory stack's only path to `std::fs`.

use std::fs::{File, Metadata};
use std::path::{Path, PathBuf};

use super::rename_contention::{
    CONTENTION_DELAYS_MS, RenamePlatform, rename_with_contention_retry,
};
use super::retry::retry_on_eintr;
use super::write_all::{open_with_exclusive_policy, write_path_all};

/// Reads a whole file, retrying an interrupted read.
pub fn read(path: &Path) -> std::io::Result<Vec<u8>> {
    retry_on_eintr(|| std::fs::read(path))
}

/// Reads a whole file as UTF-8 text, retrying an interrupted read.
pub fn read_to_string(path: &Path) -> std::io::Result<String> {
    retry_on_eintr(|| std::fs::read_to_string(path))
}

/// Stats a path, retrying an interrupted call.
pub fn metadata(path: &Path) -> std::io::Result<Metadata> {
    retry_on_eintr(|| std::fs::metadata(path))
}

/// Metadata without following symlinks.
pub fn symlink_metadata(path: &Path) -> std::io::Result<Metadata> {
    retry_on_eintr(|| std::fs::symlink_metadata(path))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileSystemStatistics {
    pub filesystem_type: i64,
}

/// Filesystem type, used to reject remote filesystems for local coordination.
#[cfg(unix)]
pub fn statfs(path: &Path) -> std::io::Result<FileSystemStatistics> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    retry_on_eintr(|| {
        let mut info = std::mem::MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: the C string is terminated and info points to writable statfs storage;
        // initialization is read only after statfs reports success.
        #[allow(unsafe_code)]
        unsafe {
            if libc::statfs(path.as_ptr(), info.as_mut_ptr()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // `statfs.f_type` is i64 on Linux but u32 on macOS; the conversion is a no-op only on Linux.
            #[allow(clippy::useless_conversion)]
            let filesystem_type = i64::from(info.assume_init().f_type);
            Ok(FileSystemStatistics { filesystem_type })
        }
    })
}
#[cfg(not(unix))]
pub fn statfs(_path: &Path) -> std::io::Result<FileSystemStatistics> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "filesystem type probing unavailable",
    ))
}

/// True when the path exists; transient EINTR is retried rather than misread as missing.
pub fn exists(path: &Path) -> bool {
    metadata(path).is_ok()
}

/// Resolves a path to its canonical form, retrying an interrupted call.
pub fn canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    retry_on_eintr(|| std::fs::canonicalize(path))
}

/// Creates a directory tree, retrying an interrupted call.
pub fn create_dir_all(path: &Path) -> std::io::Result<()> {
    retry_on_eintr(|| std::fs::create_dir_all(path))
}

/// Removes a file, retrying an interrupted call.
pub fn remove_file(path: &Path) -> std::io::Result<()> {
    retry_on_eintr(|| std::fs::remove_file(path))
}

/// Removes a directory tree, retrying an interrupted call.
pub fn remove_dir_all(path: &Path) -> std::io::Result<()> {
    retry_on_eintr(|| std::fs::remove_dir_all(path))
}

/// Lists entry names in a directory, sorted.
pub fn read_dir_names(path: &Path) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in retry_on_eintr(|| std::fs::read_dir(path))? {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    Ok(names)
}

/// Lists subdirectory names, sorted; a missing directory yields an empty list.
pub fn read_dir_directories(path: &Path) -> std::io::Result<Vec<String>> {
    let entries = match retry_on_eintr(|| std::fs::read_dir(path)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        if retry_on_eintr(|| entry.file_type())?.is_dir() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Writes all of `data` to `path` (truncating), retrying interrupted writes.
pub fn write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    write_path_all(path, data, "w", false)
}

/// Appends all of `data` to `path`, retrying interrupted writes.
pub fn append(path: &Path, data: &[u8]) -> std::io::Result<()> {
    write_path_all(path, data, "a", false)
}

/// Opens an exclusive-create handle; exactly one attempt, since creation is ambiguous after EINTR.
pub fn create_exclusive(path: &Path) -> std::io::Result<File> {
    open_with_exclusive_policy(path, "wx")
}

/// Renames `from` onto `to` with the platform rename-contention policy.
pub fn rename(from: &Path, to: &Path) -> std::io::Result<()> {
    rename_with_contention_retry(
        &mut |source, target| std::fs::rename(source, target),
        from,
        to,
        RenamePlatform::current(),
        &CONTENTION_DELAYS_MS,
        &mut sleep_ms,
    )
}

/// Closes a handle; `close(2)` state is unspecified after EINTR, so it maps to success.
pub fn close_sync(handle: File) {
    drop(handle);
}

/// Blocks the current thread for `ms` milliseconds.
pub fn sleep_ms(ms: u64) {
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

#[cfg(test)]
#[path = "resilient_tests.rs"]
mod tests;
