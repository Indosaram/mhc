//! fsync that tolerates filesystems refusing it (EPERM/EACCES/ENOTSUP/EINVAL).

use std::fs::File;
use std::io;

const TOLERATED_FSYNC_CODES: [i32; 4] = [libc::EPERM, libc::EACCES, libc::ENOTSUP, libc::EINVAL];

pub(crate) fn tolerant_fsync(file: &File, _context_label: &str) -> io::Result<()> {
    match file.sync_all() {
        Ok(()) => Ok(()),
        Err(error)
            if error
                .raw_os_error()
                .is_some_and(|code| TOLERATED_FSYNC_CODES.contains(&code)) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}
