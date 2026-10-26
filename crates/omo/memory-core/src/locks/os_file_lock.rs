//! Nonblocking kernel flock attempts with fail-closed errno classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsLockAttempt {
    Acquired,
    Busy,
    Unavailable { errno: Option<i32> },
    Failed { errno: i32 },
}

pub fn os_file_lock_available() -> bool {
    cfg!(any(target_os = "linux", target_os = "macos"))
}

#[cfg(unix)]
pub fn classify_flock_failure(errno: i32) -> OsLockAttempt {
    if errno == libc::EWOULDBLOCK || errno == libc::EAGAIN || errno == libc::EINTR {
        OsLockAttempt::Busy
    } else if errno == libc::ENOLCK
        || errno == libc::EOPNOTSUPP
        || errno == libc::ENOTSUP
        || errno == libc::ENOSYS
    {
        OsLockAttempt::Unavailable { errno: Some(errno) }
    } else {
        OsLockAttempt::Failed { errno }
    }
}

#[cfg(not(unix))]
pub fn classify_flock_failure(errno: i32) -> OsLockAttempt {
    OsLockAttempt::Failed { errno }
}

fn attempt(fd: i32, exclusive: bool) -> OsLockAttempt {
    if !os_file_lock_available() {
        return OsLockAttempt::Unavailable { errno: None };
    }
    #[cfg(unix)]
    {
        let mode = if exclusive {
            libc::LOCK_EX
        } else {
            libc::LOCK_SH
        };
        #[allow(unsafe_code)]
        // SAFETY: flock accepts any integer descriptor; invalid descriptors return EBADF.
        let rc = unsafe { libc::flock(fd, mode | libc::LOCK_NB) };
        if rc == 0 {
            OsLockAttempt::Acquired
        } else {
            classify_flock_failure(std::io::Error::last_os_error().raw_os_error().unwrap_or(-1))
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (fd, exclusive);
        OsLockAttempt::Unavailable { errno: None }
    }
}

pub fn try_lock_shared(fd: i32) -> OsLockAttempt {
    attempt(fd, false)
}
pub fn try_lock_exclusive(fd: i32) -> OsLockAttempt {
    attempt(fd, true)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn flock_failures_are_classified_by_errno() {
        for errno in [libc::EWOULDBLOCK, libc::EAGAIN, libc::EINTR] {
            assert_eq!(classify_flock_failure(errno), OsLockAttempt::Busy);
        }
        for errno in [libc::ENOLCK, libc::EOPNOTSUPP, libc::ENOTSUP, libc::ENOSYS] {
            assert_eq!(
                classify_flock_failure(errno),
                OsLockAttempt::Unavailable { errno: Some(errno) }
            );
        }
        for errno in [libc::EBADF, libc::EINVAL, -1] {
            assert_eq!(
                classify_flock_failure(errno),
                OsLockAttempt::Failed { errno }
            );
        }
    }

    #[test]
    fn invalid_descriptor_is_failed_not_busy() {
        if !os_file_lock_available() {
            return;
        }
        let expected = OsLockAttempt::Failed { errno: libc::EBADF };
        assert_eq!(try_lock_shared(1_000_000), expected);
        assert_eq!(try_lock_exclusive(1_000_000), expected);
    }

    #[test]
    fn conflicting_open_descriptions_are_busy() {
        use std::os::fd::AsRawFd;
        if !os_file_lock_available() {
            return;
        }
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("activity.lock");
        let first = std::fs::File::create(&path).expect("first");
        let second = std::fs::File::open(&path).expect("second");
        assert_eq!(try_lock_shared(first.as_raw_fd()), OsLockAttempt::Acquired);
        assert_eq!(try_lock_exclusive(second.as_raw_fd()), OsLockAttempt::Busy);
        drop(first);
        assert_eq!(
            try_lock_exclusive(second.as_raw_fd()),
            OsLockAttempt::Acquired
        );
    }
}
