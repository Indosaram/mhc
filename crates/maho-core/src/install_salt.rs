//! Port of senpi packages/coding-agent/src/core/install-salt.ts.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

const SALT_BYTES: usize = 32;

static SALTS_BY_PATH: LazyLock<Mutex<HashMap<PathBuf, [u8; SALT_BYTES]>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn random_salt() -> [u8; SALT_BYTES] {
    let mut out = [0u8; SALT_BYTES];
    out[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    out[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    out
}

#[cfg(unix)]
fn inode_of(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.ino()
}

#[cfg(not(unix))]
fn inode_of(_meta: &std::fs::Metadata) -> u64 {
    0
}

#[cfg(unix)]
fn set_private_mode(file: &std::fs::File, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = file.set_permissions(std::fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn set_private_mode(_file: &std::fs::File, _mode: u32) {}

#[cfg(unix)]
fn create_dir_private(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
fn create_dir_private(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Removes a salt file of the wrong length (truncated or written by something else) so the next create
/// replaces it. A file another process has already replaced (a different inode) is kept.
fn remove_damaged_salt(path: &Path, damaged_inode: u64) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) => {
            if inode_of(&meta) != damaged_inode {
                return true;
            }
            match std::fs::remove_file(path) {
                Ok(()) => true,
                Err(err) => err.kind() == ErrorKind::NotFound,
            }
        }
        Err(err) => err.kind() == ErrorKind::NotFound,
    }
}

fn is_no_hard_links_error(err: &std::io::Error) -> bool {
    if err.kind() == ErrorKind::PermissionDenied || err.kind() == ErrorKind::Unsupported {
        return true;
    }
    #[cfg(unix)]
    if let Some(code) = err.raw_os_error() {
        return code == libc_enosys();
    }
    false
}

#[cfg(unix)]
const fn libc_enosys() -> i32 {
    38
}

fn read_or_create_salt(path: &Path) -> Option<[u8; SALT_BYTES]> {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let inode = inode_of(&meta);
            if let Ok(stored) = std::fs::read(path)
                && stored.len() == SALT_BYTES
            {
                let mut salt = [0u8; SALT_BYTES];
                salt.copy_from_slice(&stored);
                return Some(salt);
            }
            if !remove_damaged_salt(path, inode) {
                return None;
            }
        }
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(_) => return None,
    }

    let suffix = hex::encode(&uuid::Uuid::new_v4().as_bytes()[..6]);
    let staged = PathBuf::from(format!(
        "{}.{}.{}",
        path.display(),
        std::process::id(),
        suffix
    ));
    let mut created_staged = false;
    let create_res: Result<(), std::io::Error> = (|| {
        if let Some(parent) = path.parent() {
            create_dir_private(parent)?;
        }
        let salt = random_salt();
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)?;
        created_staged = true;
        set_private_mode(&file, 0o600);
        file.write_all(&salt)?;
        file.sync_all()?;
        drop(file);
        match std::fs::hard_link(&staged, path) {
            Ok(()) => Ok(()),
            Err(err) if is_no_hard_links_error(&err) => {
                let mut direct = OpenOptions::new().write(true).create_new(true).open(path)?;
                set_private_mode(&direct, 0o600);
                direct.write_all(&salt)?;
                Ok(())
            }
            Err(err) => Err(err),
        }
    })();

    if created_staged {
        let _ = std::fs::remove_file(&staged);
    }
    if let Err(err) = create_res
        && err.kind() != ErrorKind::AlreadyExists
    {
        return None;
    }

    let stored = std::fs::read(path).ok()?;
    if stored.len() != SALT_BYTES {
        return None;
    }
    let mut salt = [0u8; SALT_BYTES];
    salt.copy_from_slice(&stored);
    Some(salt)
}

/// A random 32-byte salt created once per install at `<agentDir>/<relativePath>` (mode 0600).
pub fn install_salt(agent_dir: &Path, relative_path: &str) -> [u8; SALT_BYTES] {
    let path = agent_dir.join(relative_path);
    let mut cache = SALTS_BY_PATH
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(&cached) = cache.get(&path) {
        return cached;
    }
    let salt = read_or_create_salt(&path).unwrap_or_else(random_salt);
    cache.insert(path, salt);
    salt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_persists_and_replaces_damaged_salt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s1 = install_salt(dir.path(), "test-salt");
        // Clear cache for re-read verification from disk
        SALTS_BY_PATH
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        let s2 = install_salt(dir.path(), "test-salt");
        assert_eq!(s1, s2);
        assert_eq!(
            std::fs::read(dir.path().join("test-salt"))
                .expect("read")
                .len(),
            32
        );

        // Damaged salt gets replaced
        SALTS_BY_PATH
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        std::fs::write(dir.path().join("test-salt"), b"short").expect("corrupt");
        let s3 = install_salt(dir.path(), "test-salt");
        assert_ne!(s3, [0u8; 32]);
        assert_eq!(
            std::fs::read(dir.path().join("test-salt"))
                .expect("read")
                .len(),
            32
        );
    }
}
