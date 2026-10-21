//! fsync that tolerates filesystems refusing it (EPERM/EACCES/ENOTSUP/EINVAL).

use std::fs::File;
use std::io;

#[cfg(not(windows))]
const TOLERATED_FSYNC_CODES: [i32; 4] = [libc::EPERM, libc::EACCES, libc::ENOTSUP, libc::EINVAL];

fn is_tolerated(error: &io::Error) -> bool {
    #[cfg(windows)]
    {
        // libuv maps these Win32 failures to EPERM/EACCES/ENOTSUP/EINVAL.
        matches!(error.raw_os_error(), Some(5 | 50 | 87 | 1314))
            || (error.raw_os_error().is_none()
                && matches!(error.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported | io::ErrorKind::InvalidInput))
    }
    #[cfg(not(windows))]
    {
        error.raw_os_error().is_some_and(|code| TOLERATED_FSYNC_CODES.contains(&code))
    }
}

pub(crate) fn tolerant_fsync(file: &File, _context_label: &str) -> io::Result<()> {
    match file.sync_all() {
        Ok(()) => Ok(()),
        Err(error) if is_tolerated(&error) => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn tolerates_win32_mapped_errors_but_not_io_failure() {
        for code in [5, 50, 87, 1314] {
            assert!(is_tolerated(&io::Error::from_raw_os_error(code)));
        }
        assert!(!is_tolerated(&io::Error::from_raw_os_error(1117)));
    }

    #[cfg(not(windows))]
    #[test]
    fn tolerates_upstream_errno_set_but_not_io_failure() {
        for code in TOLERATED_FSYNC_CODES {
            assert!(is_tolerated(&io::Error::from_raw_os_error(code)));
        }
        assert!(!is_tolerated(&io::Error::from_raw_os_error(libc::EIO)));
    }
}
