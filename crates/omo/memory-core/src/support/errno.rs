//! Node-style `error.code` for I/O errors: the POSIX errno name when known.

/// The errno name Node reports as `error.code` (e.g. `EIO`), else the `ErrorKind` text.
pub fn io_error_code(error: &std::io::Error) -> String {
    #[cfg(unix)]
    if let Some(name) = error.raw_os_error().and_then(errno_name) {
        return name.to_owned();
    }
    error.kind().to_string()
}

#[cfg(unix)]
fn errno_name(errno: i32) -> Option<&'static str> {
    Some(match errno {
        libc::EIO => "EIO",
        libc::EACCES => "EACCES",
        libc::EPERM => "EPERM",
        libc::ENOENT => "ENOENT",
        libc::ENOTDIR => "ENOTDIR",
        libc::EISDIR => "EISDIR",
        libc::EEXIST => "EEXIST",
        libc::EBUSY => "EBUSY",
        libc::EBADF => "EBADF",
        libc::EINVAL => "EINVAL",
        libc::EROFS => "EROFS",
        libc::ENOSPC => "ENOSPC",
        libc::EDQUOT => "EDQUOT",
        libc::EMFILE => "EMFILE",
        libc::ENFILE => "ENFILE",
        libc::EINTR => "EINTR",
        libc::EAGAIN => "EAGAIN",
        libc::ELOOP => "ELOOP",
        libc::ENAMETOOLONG => "ENAMETOOLONG",
        _ => return None,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn maps_raw_os_errors_to_posix_names_and_falls_back_to_kind_text() {
        assert_eq!(
            io_error_code(&std::io::Error::from_raw_os_error(libc::EIO)),
            "EIO"
        );
        let custom = std::io::Error::other("boom");
        assert_eq!(
            io_error_code(&custom),
            std::io::ErrorKind::Other.to_string()
        );
    }
}
