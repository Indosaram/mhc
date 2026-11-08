//! Crash-safe file replacement.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A durable step of [`write_atomically_with`], reported to the test hook before it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtomicStep {
    CreateTemp,
    Write,
    FsyncFile,
    Rename,
    FsyncDir,
}

/// Hook called before each step; returning an error makes the step fail with that error.
pub type AtomicHook<'a> = &'a dyn Fn(AtomicStep) -> io::Result<()>;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temp path next to `path`, unique per process and call: `<name>.<pid>.<counter>.tmp`.
pub fn unique_temp_path(path: &Path) -> PathBuf {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    path.with_file_name(format!("{name}.{}.{counter}.tmp", std::process::id()))
}

/// Replaces `path` with `bytes` so that the target holds either the old or the new content,
/// never a partial write.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomically_with(path, bytes, &|_| Ok(()))
}

/// [`write_atomically`] with a hook that can inject failures (tests).
pub fn write_atomically_with(path: &Path, bytes: &[u8], hook: AtomicHook<'_>) -> io::Result<()> {
    let temp = unique_temp_path(path);
    let result = write_temp_and_rename(path, &temp, bytes, hook);
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn write_temp_and_rename(
    path: &Path,
    temp: &Path,
    bytes: &[u8],
    hook: AtomicHook<'_>,
) -> io::Result<()> {
    hook(AtomicStep::CreateTemp)?;
    let mut file = OpenOptions::new().write(true).create_new(true).open(temp)?;
    hook(AtomicStep::Write)?;
    file.write_all(bytes)?;
    tolerate_fsync(hook(AtomicStep::FsyncFile).and_then(|()| file.sync_all()))?;
    drop(file);
    hook(AtomicStep::Rename)?;
    rename_over(temp, path)?;
    #[cfg(not(windows))]
    {
        if let Some(parent) = path.parent() {
            let parent = if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            };
            tolerate_fsync(hook(AtomicStep::FsyncDir).and_then(|()| sync_dir(parent)))?;
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Replace `to` with `from`.
///
/// `std::fs::rename` already maps to `MoveFileExW(..., MOVEFILE_REPLACE_EXISTING)` on Windows, so
/// the only recoverable failures are transient sharing violations and delete-pending states. Those
/// are retried; the destination is never deleted first, because a failed rename after the delete
/// would leave no file at all.
fn rename_over(from: &Path, to: &Path) -> io::Result<()> {
    let attempts = if cfg!(windows) { 5 } else { 1 };
    let mut attempt = 0;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) => {
                attempt += 1;
                let retryable = cfg!(windows)
                    && matches!(
                        error.kind(),
                        io::ErrorKind::PermissionDenied | io::ErrorKind::WouldBlock
                    );
                if !retryable || attempt >= attempts {
                    return Err(error);
                }
                std::thread::sleep(std::time::Duration::from_millis(10 * attempt));
            }
        }
    }
}

/// Some filesystems cannot fsync; those failure kinds are not fatal.
fn tolerate_fsync(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::PermissionDenied
                    | io::ErrorKind::Unsupported
                    | io::ErrorKind::InvalidInput
            ) =>
        {
            Ok(())
        }
        other => other,
    }
}

/// Creates `path` exclusively (fails if it exists), writes `bytes` and syncs it.
pub fn write_exclusive(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file: File = OpenOptions::new().write(true).create_new(true).open(path)?;
    let result = file
        .write_all(bytes)
        .and_then(|()| tolerate_fsync(file.sync_all()));
    if result.is_err() {
        drop(file);
        let _ = std::fs::remove_file(path);
    }
    result
}
