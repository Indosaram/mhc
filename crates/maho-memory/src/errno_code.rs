//! A stable code for an I/O failure.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-support-errno.md`. Callers record "why the write
//! failed" in a lock record, a retry decision, or a user-facing message. `std::io::ErrorKind` collapses
//! distinct failures: `ENOSPC` (disk full) and `EDQUOT` (quota exceeded) are both `Other`, and the
//! remedy for each is different, so the errno name is what has to survive.

pub fn io_error_code(error: &std::io::Error) -> String {
    if let Some(name) = error.raw_os_error().and_then(errno_name) {
        return name.to_owned();
    }
    error.kind().to_string()
}

/// Errno numbers are stable per platform, so the table is written out rather than pulled from `libc`.
///
/// The workspace forbids `unsafe`, and `libc`'s constants are the only thing this crate would use it
/// for. Numbers differ between Linux and the BSDs, so each group is gated by target family; an
/// unrecognised number returns `None` and degrades to the caller's `ErrorKind` text instead of being
/// reported under another platform's name.
fn errno_name(errno: i32) -> Option<&'static str> {
    Some(match errno {
        x if x == EIO => "EIO",
        x if x == EACCES => "EACCES",
        x if x == EPERM => "EPERM",
        x if x == ENOENT => "ENOENT",
        x if x == ENOTDIR => "ENOTDIR",
        x if x == EISDIR => "EISDIR",
        x if x == EEXIST => "EEXIST",
        x if x == EBUSY => "EBUSY",
        x if x == EBADF => "EBADF",
        x if x == EINVAL => "EINVAL",
        x if x == EROFS => "EROFS",
        x if x == ENOSPC => "ENOSPC",
        x if x == EDQUOT => "EDQUOT",
        x if x == EMFILE => "EMFILE",
        x if x == ENFILE => "ENFILE",
        x if x == EINTR => "EINTR",
        x if x == EAGAIN => "EAGAIN",
        x if x == ELOOP => "ELOOP",
        x if x == ENAMETOOLONG => "ENAMETOOLONG",
        _ => return None,
    })
}

#[cfg(target_os = "linux")]
mod numbers {
    pub const EIO: i32 = 5;
    pub const ENOENT: i32 = 2;
    pub const EINTR: i32 = 4;
    pub const EAGAIN: i32 = 11;
    pub const EACCES: i32 = 13;
    pub const EBUSY: i32 = 16;
    pub const EEXIST: i32 = 17;
    pub const ENOTDIR: i32 = 20;
    pub const EISDIR: i32 = 21;
    pub const EINVAL: i32 = 22;
    pub const EMFILE: i32 = 24;
    pub const ENOSPC: i32 = 28;
    pub const EROFS: i32 = 30;
    pub const ELOOP: i32 = 40;
    pub const ENAMETOOLONG: i32 = 36;
    pub const ENFILE: i32 = 23;
    pub const EDQUOT: i32 = 122;
    pub const EPERM: i32 = 1;
    pub const EBADF: i32 = 9;
}

#[cfg(target_os = "macos")]
mod numbers {
    pub const EIO: i32 = 5;
    pub const ENOENT: i32 = 2;
    pub const EINTR: i32 = 4;
    pub const EAGAIN: i32 = 35;
    pub const EACCES: i32 = 13;
    pub const EBUSY: i32 = 16;
    pub const EEXIST: i32 = 17;
    pub const ENOTDIR: i32 = 20;
    pub const EISDIR: i32 = 21;
    pub const EINVAL: i32 = 22;
    pub const EMFILE: i32 = 24;
    pub const ENOSPC: i32 = 28;
    pub const EROFS: i32 = 30;
    pub const ELOOP: i32 = 62;
    pub const ENAMETOOLONG: i32 = 63;
    pub const ENFILE: i32 = 23;
    pub const EDQUOT: i32 = 69;
    pub const EPERM: i32 = 1;
    pub const EBADF: i32 = 9;
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod numbers {
    pub const EIO: i32 = 5;
    pub const ENOENT: i32 = 2;
    pub const EINTR: i32 = 4;
    pub const EAGAIN: i32 = 11;
    pub const EACCES: i32 = 13;
    pub const EBUSY: i32 = 16;
    pub const EEXIST: i32 = 17;
    pub const ENOTDIR: i32 = 20;
    pub const EISDIR: i32 = 21;
    pub const EINVAL: i32 = 22;
    pub const EMFILE: i32 = 24;
    pub const ENOSPC: i32 = 28;
    pub const EROFS: i32 = 30;
    pub const ELOOP: i32 = 40;
    pub const ENAMETOOLONG: i32 = 36;
    pub const ENFILE: i32 = 23;
    pub const EDQUOT: i32 = 122;
    pub const EPERM: i32 = 1;
    pub const EBADF: i32 = 9;
}

use numbers::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_os_error_reports_its_errno_name() {
        assert_eq!(
            io_error_code(&std::io::Error::from_raw_os_error(EIO)),
            "EIO"
        );
    }

    #[test]
    fn disk_full_and_quota_are_distinguishable() {
        let full = io_error_code(&std::io::Error::from_raw_os_error(ENOSPC));
        let quota = io_error_code(&std::io::Error::from_raw_os_error(EDQUOT));
        assert_eq!(full, "ENOSPC");
        assert_eq!(quota, "EDQUOT");
        assert_ne!(full, quota);
    }

    #[test]
    fn a_permission_failure_keeps_its_own_name() {
        assert_eq!(
            io_error_code(&std::io::Error::from_raw_os_error(EACCES)),
            "EACCES"
        );
        assert_eq!(
            io_error_code(&std::io::Error::from_raw_os_error(EPERM)),
            "EPERM"
        );
    }

    #[test]
    fn a_kind_only_error_falls_back_to_the_kind_text() {
        let custom = std::io::Error::other("boom");
        assert_eq!(
            io_error_code(&custom),
            std::io::ErrorKind::Other.to_string()
        );
    }

    #[test]
    fn an_unmapped_number_falls_back_rather_than_guessing() {
        let odd = std::io::Error::from_raw_os_error(9999);
        assert_eq!(io_error_code(&odd), odd.kind().to_string());
    }
}
