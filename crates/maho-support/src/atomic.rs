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

fn rename_over(from: &Path, to: &Path) -> io::Result<()> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(error) if cfg!(windows) && error.kind() == io::ErrorKind::PermissionDenied => {
            let _ = std::fs::remove_file(to);
            std::fs::rename(from, to)
        }
        Err(error) => Err(error),
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
