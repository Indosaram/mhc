//! Recursive removal tolerant of read-only files/directories and transient Windows locks.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveTreePlatform {
    Linux,
    Darwin,
    Win32,
}

impl RemoveTreePlatform {
    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Win32
        } else if cfg!(target_os = "macos") {
            Self::Darwin
        } else {
            Self::Linux
        }
    }
}

const WIN32_BUSY_RETRIES: usize = 5;

fn is_permission_or_readonly_error(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        return true;
    }
    match error.raw_os_error() {
        Some(code) => {
            // POSIX EACCES/EPERM or macOS ENOTEMPTY on read-only directory children
            #[cfg(unix)]
            {
                code == libc::EACCES || code == libc::EPERM || code == libc::ENOTEMPTY
            }
            #[cfg(not(unix))]
            {
                let _ = code;
                false
            }
        }
        None => false,
    }
}

fn is_win32_busy_error(error: &std::io::Error) -> bool {
    #[cfg(windows)]
    {
        // ERROR_SHARING_VIOLATION (32), ERROR_LOCK_VIOLATION (33), ERROR_ACCESS_DENIED (5)
        matches!(error.raw_os_error(), Some(5 | 32 | 33))
    }
    #[cfg(not(windows))]
    {
        let _ = error;
        false
    }
}

fn is_retryable(error: &std::io::Error, platform: RemoveTreePlatform) -> bool {
    match platform {
        RemoveTreePlatform::Win32 => {
            is_permission_or_readonly_error(error) || is_win32_busy_error(error)
        }
        RemoveTreePlatform::Linux | RemoveTreePlatform::Darwin => {
            is_permission_or_readonly_error(error)
        }
    }
}

fn remove_path(path: &Path) -> std::io::Result<()> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    if meta.is_dir() {
        match super::resilient::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    } else {
        match super::resilient::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(path: &Path, _mode: u32) -> std::io::Result<()> {
    let mut perms = std::fs::metadata(path)?.permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(path, perms)
}

fn make_writable(path: &Path) -> std::io::Result<()> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if !meta.is_dir() {
        set_mode(path, 0o600)?;
        return Ok(());
    }
    set_mode(path, 0o700)?;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        make_writable(&entry.path())?;
    }
    Ok(())
}

/// Removes a file or directory tree, tolerating read-only content (files 0o400, directories 0o500).
/// Missing paths succeed (`ENOENT`).
pub fn remove_tree(path: &Path) -> std::io::Result<()> {
    remove_tree_with_platform(path, RemoveTreePlatform::current())
}

/// Removes a file or directory tree with explicit platform retry behavior.
pub fn remove_tree_with_platform(path: &Path, platform: RemoveTreePlatform) -> std::io::Result<()> {
    if let Err(err) = remove_path(path) {
        if !is_retryable(&err, platform) {
            return Err(err);
        }
    } else {
        return Ok(());
    }

    make_writable(path)?;
    let attempts = match platform {
        RemoveTreePlatform::Win32 => WIN32_BUSY_RETRIES,
        RemoveTreePlatform::Linux | RemoveTreePlatform::Darwin => 1,
    };
    for attempt in 1..=attempts {
        match remove_path(path) {
            Ok(()) => return Ok(()),
            Err(err) => {
                if attempt >= attempts || !is_win32_busy_error(&err) {
                    return Err(err);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_path_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        remove_tree(&dir.path().join("does-not-exist")).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn removes_readonly_files_and_directories() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("run");
        let nested = tree.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("payload.json");
        std::fs::write(&file, "{}").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o400)).unwrap();
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o500)).unwrap();
        std::fs::set_permissions(&tree, std::fs::Permissions::from_mode(0o500)).unwrap();

        let result = remove_tree(&tree);
        if result.is_err() {
            let _ = make_writable(&tree);
        }
        result.unwrap();
        assert!(!tree.exists());
    }
}
