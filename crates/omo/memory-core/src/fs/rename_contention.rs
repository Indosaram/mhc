//! Windows rename contention retry for atomic memory writes.

use std::path::Path;

/// Delay ladder applied between rename attempts on Windows (pin `rename-contention.ts`).
pub const CONTENTION_DELAYS_MS: [u64; 6] = [10, 25, 50, 100, 200, 400];

/// Platform gate for the rename contention ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenamePlatform {
    Windows,
    Other,
}

impl RenamePlatform {
    /// Platform of the running process.
    pub fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

/// True when the error is a Windows file-sharing contention code.
///
/// Covers the POSIX errno values the pinned source names (EPERM/EBUSY/EACCES) and the Win32 codes
/// Windows reports for a held handle (ERROR_ACCESS_DENIED 5, ERROR_SHARING_VIOLATION 32,
/// ERROR_LOCK_VIOLATION 33).
#[cfg(windows)]
pub fn is_contention_error(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::PermissionDenied
        || matches!(error.raw_os_error(), Some(1 | 5 | 13 | 16 | 32 | 33))
}

/// True when the error is a POSIX permission/busy contention code; Windows sharing codes differ.
#[cfg(not(windows))]
pub fn is_contention_error(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(1 | 13 | 16))
}

/// Renames `from` onto `to`, retrying Windows sharing contention along the delay ladder.
pub fn rename_with_contention_retry(
    rename: &mut dyn FnMut(&Path, &Path) -> std::io::Result<()>,
    from: &Path,
    to: &Path,
    platform: RenamePlatform,
    delays_ms: &[u64],
    sleep_ms: &mut dyn FnMut(u64),
) -> std::io::Result<()> {
    let delays: &[u64] = if platform == RenamePlatform::Windows {
        delays_ms
    } else {
        &[]
    };
    let mut attempt = 0usize;
    loop {
        match rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) => {
                let Some(delay) = delays.get(attempt) else {
                    return Err(error);
                };
                if !is_contention_error(&error) {
                    return Err(error);
                }
                sleep_ms(*delay);
                attempt += 1;
            }
        }
    }
}

#[cfg(test)]
#[path = "rename_contention_tests.rs"]
mod tests;
