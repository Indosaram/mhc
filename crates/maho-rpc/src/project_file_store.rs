//! Port of senpi `packages/coding-agent/src/modes/rpc/project-file-store.ts`.
//!
//! The stored-bytes side of the project file verbs (`read_project_file`/`write_project_file`, the
//! project MCP file and skill file verbs): reading a resolved file without following a link, its
//! etag, the atomic replace, and one write at a time per file inside the engine.

use crate::project_file_path::PROJECT_FILE_MAX_BYTES;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::future::Future;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stored {
    Missing,
    Refused,
    TooLarge,
    File { bytes: Vec<u8>, mode: u32 },
}

pub fn etag_of(bytes: &[u8]) -> String {
    format!("sha256-{}", hex::encode(Sha256::digest(bytes)))
}

/// Reads the stored bytes without following a link and without blocking on a FIFO swapped in after
/// resolution; the size is checked on the open handle before anything is read.
pub async fn read_stored(absolute: &Path) -> std::io::Result<Stored> {
    let path = absolute.to_path_buf();
    tokio::task::spawn_blocking(move || read_stored_sync(&path))
        .await
        .map_err(|err| std::io::Error::other(err.to_string()))?
}

fn read_stored_sync(absolute: &Path) -> std::io::Result<Stored> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = match opts.open(absolute) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Stored::Missing),
        #[cfg(unix)]
        Err(err) if err.raw_os_error() == Some(libc::ELOOP) => return Ok(Stored::Refused),
        Err(err) => return Err(err),
    };
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Ok(Stored::Refused);
    }
    if meta.len() > PROJECT_FILE_MAX_BYTES as u64 {
        return Ok(Stored::TooLarge);
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.read_to_end(&mut bytes)?;
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o777
    };
    #[cfg(not(unix))]
    let mode = 0o644;
    Ok(Stored::File { bytes, mode })
}

/// Returned when the parent folder no longer resolves to itself, i.e. a folder on the way was
/// swapped for a link.
#[derive(Debug, thiserror::Error)]
#[error("parent folder moved: {0}")]
pub struct ParentMoved(pub PathBuf);

#[derive(Debug, thiserror::Error)]
pub enum ReplaceAtomicallyError {
    #[error(transparent)]
    ParentMoved(#[from] ParentMoved),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

async fn assert_parent_in_place(folder: &Path) -> Result<(), ReplaceAtomicallyError> {
    let real = maho_core::paths::strip_verbatim_prefix(tokio::fs::canonicalize(folder).await?);
    if real != folder {
        return Err(ReplaceAtomicallyError::ParentMoved(ParentMoved(
            folder.to_path_buf(),
        )));
    }
    Ok(())
}

/// Temp file + rename in the resolved folder, keeping an existing file's permissions.
///
/// The folder is re-resolved before the temp file is written and again before the rename, so a
/// folder swapped for a link after resolution is refused instead of followed.
pub async fn replace_atomically(
    absolute: &Path,
    content: &str,
    mode: Option<u32>,
) -> Result<(), ReplaceAtomicallyError> {
    let folder = absolute
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing parent"))?;
    let name = absolute
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    let temp = folder.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    assert_parent_in_place(folder).await?;
    let write_res: Result<(), ReplaceAtomicallyError> = async {
        let temp_clone = temp.clone();
        let bytes = content.as_bytes().to_vec();
        let effective_mode = mode.unwrap_or(0o644);
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            use std::io::Write;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(effective_mode);
            }
            let mut f = opts.open(&temp_clone)?;
            f.write_all(&bytes)?;
            #[cfg(unix)]
            if let Some(m) = mode {
                use std::os::unix::fs::PermissionsExt;
                f.set_permissions(std::fs::Permissions::from_mode(m))?;
            }
            Ok(())
        })
        .await
        .map_err(|err| std::io::Error::other(err.to_string()))??;
        assert_parent_in_place(folder).await?;
        tokio::fs::rename(&temp, absolute).await?;
        Ok(())
    }
    .await;
    if write_res.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    write_res
}

fn write_chains() -> &'static Mutex<HashMap<PathBuf, Weak<tokio::sync::Mutex<()>>>> {
    static CHAINS: OnceLock<Mutex<HashMap<PathBuf, Weak<tokio::sync::Mutex<()>>>>> =
        OnceLock::new();
    CHAINS.get_or_init(Mutex::default)
}

/// Runs `write` after every earlier write queued for `key`; a write queued behind a failed one
/// still runs.
pub async fn serialize_write<T, F, Fut>(key: impl Into<PathBuf>, write: F) -> T
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    let key = key.into();
    let lock = {
        let mut map = write_chains()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = map.get(&key).and_then(Weak::upgrade) {
            existing
        } else {
            let fresh = Arc::new(tokio::sync::Mutex::new(()));
            map.insert(key.clone(), Arc::downgrade(&fresh));
            fresh
        }
    };
    let _guard = lock.lock().await;
    let out = write().await;
    drop(_guard);
    drop(lock);
    let mut map = write_chains()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if map.get(&key).is_some_and(|weak| weak.strong_count() == 0) {
        map.remove(&key);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etag_matches_sha256_prefix() {
        assert_eq!(
            etag_of(b"hello"),
            "sha256-2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[tokio::test]
    async fn read_stored_and_replace_atomically_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real_dir = maho_core::paths::strip_verbatim_prefix(
            std::fs::canonicalize(dir.path()).expect("canonical"),
        );
        let file = real_dir.join("AGENTS.md");
        assert_eq!(read_stored(&file).await.expect("read"), Stored::Missing);

        replace_atomically(&file, "first", Some(0o640))
            .await
            .expect("write");
        let Stored::File { bytes, mode } = read_stored(&file).await.expect("read") else {
            panic!("expected file");
        };
        assert_eq!(bytes, b"first");
        #[cfg(unix)]
        assert_eq!(mode, 0o640);
        #[cfg(not(unix))]
        let _ = mode;

        let oversized = real_dir.join("BIG.md");
        std::fs::write(&oversized, vec![b'a'; PROJECT_FILE_MAX_BYTES + 1]).expect("write big");
        assert_eq!(
            read_stored(&oversized).await.expect("read big"),
            Stored::TooLarge
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn read_stored_refuses_symlinks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("real.md");
        std::fs::write(&target, b"hi").expect("write");
        let link = dir.path().join("AGENTS.md");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        assert_eq!(read_stored(&link).await.expect("read"), Stored::Refused);
    }

    #[tokio::test]
    async fn serialize_write_orders_concurrent_writers() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let l1 = log.clone();
        let f1 = serialize_write("/tmp/test-key", || async move {
            let _ = rx.await;
            l1.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(1);
        });
        let l2 = log.clone();
        let f2 = serialize_write("/tmp/test-key", || async move {
            l2.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(2);
        });
        let _ = tx.send(());
        tokio::join!(f1, f2);
        let recorded = log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert_eq!(recorded, vec![1, 2]);
    }
}
