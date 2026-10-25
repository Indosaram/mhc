//! Filesystem allowlist for inherited open-description locks.
use std::path::Path;

pub fn is_local_filesystem_type(platform: &str, kind: u64, root: Option<u64>) -> bool {
    match platform {
        "linux" => matches!(
            kind,
            0xef53
                | 0x58465342
                | 0x9123683e
                | 0x01021994
                | 0xf2f52010
                | 0x2fc12fc1
                | 0x794c7630
                | 0xca451a4e
        ),
        "macos" | "darwin" => root == Some(kind),
        _ => false,
    }
}

#[cfg(unix)]
fn filesystem_type(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: path is NUL terminated and stat points to writable storage of the correct size.
    #[allow(unsafe_code)]
    let stat = unsafe {
        if libc::statfs(path.as_ptr(), stat.as_mut_ptr()) != 0 {
            return None;
        }
        stat.assume_init()
    };
    #[cfg(target_os = "linux")]
    return Some(stat.f_type as u64);
    #[cfg(target_os = "macos")]
    return Some(u64::from(stat.f_type));
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = stat;
        None
    }
}

pub fn is_on_local_filesystem(path: &Path) -> bool {
    #[cfg(unix)]
    {
        let Some(kind) = filesystem_type(path) else {
            return false;
        };
        let root = if cfg!(target_os = "macos") {
            filesystem_type(Path::new("/"))
        } else {
            None
        };
        is_local_filesystem_type(std::env::consts::OS, kind, root)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_known_inheritable_filesystems_vouch() {
        assert!(is_local_filesystem_type("linux", 0xef53, None));
        for kind in [0x6969, 0xff534d42, 0x65735546, 0] {
            assert!(!is_local_filesystem_type("linux", kind, None));
        }
        assert!(is_local_filesystem_type("darwin", 24, Some(24)));
        assert!(!is_local_filesystem_type("darwin", 25, Some(24)));
        assert!(!is_local_filesystem_type("win32", 24, Some(24)));
    }
}
