//! Atomic filesystem operations for Git worktree state transitions.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

use crate::support::random::random_uuid;

use super::errors::GitError;

/// Atomically writes content to a worktree file with specified permissions.
pub fn write_worktree_file(
    root: &Path,
    path: &str,
    content: &str,
    mode: u32,
    exclusive: bool,
) -> Result<bool, GitError> {
    let target = root.join(path);
    assert_safe_parents(root, path)?;
    if !exclusive {
        assert_replaceable_target(&target, path)?;
    }
    let parent = target.parent().unwrap_or(root);
    fs::create_dir_all(parent).map_err(GitError::Io)?;
    assert_safe_parents(root, path)?;

    let file_name = target
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let temp_name = format!(".{file_name}.omo-{}-{}", std::process::id(), random_uuid());
    let temp_path = parent.join(temp_name);

    let mut open_opts = OpenOptions::new();
    open_opts.write(true).create_new(true);
    #[cfg(unix)]
    open_opts.mode(mode);

    let mut file = match open_opts.open(&temp_path) {
        Ok(f) => f,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(err) => return Err(GitError::Io(err)),
    };

    let write_res = (|| -> std::io::Result<()> {
        file.write_all(content.as_bytes())?;
        #[cfg(unix)]
        {
            let perms = fs::Permissions::from_mode(mode);
            file.set_permissions(perms)?;
        }
        file.sync_all()?;
        Ok(())
    })();

    if let Err(err) = write_res {
        let _ = fs::remove_file(&temp_path);
        return Err(GitError::Io(err));
    }
    drop(file);

    if exclusive {
        match fs::hard_link(&temp_path, &target) {
            Ok(()) => {
                let _ = fs::remove_file(&temp_path);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&temp_path);
                return Ok(false);
            }
            Err(err) => {
                let _ = fs::remove_file(&temp_path);
                return Err(GitError::Io(err));
            }
        }
    } else {
        fs::rename(&temp_path, &target).map_err(GitError::Io)?;
    }

    sync_directory(parent)?;
    Ok(true)
}

/// Moves a worktree file to a process-unique temporary name, returning its path.
pub fn move_worktree_file(root: &Path, path: &str) -> Result<Option<PathBuf>, GitError> {
    let target = root.join(path);
    assert_safe_parents(root, path)?;
    let meta = match fs::symlink_metadata(&target) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(GitError::Io(err)),
    };
    if meta.file_type().is_symlink() {
        return Err(unsupported_worktree(path, "symlink"));
    }
    if !meta.is_file() {
        return Err(unsupported_worktree(path, "non-file"));
    }

    let parent = target.parent().unwrap_or(root);
    let file_name = target
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let moved_name = format!(
        ".{file_name}.omo-moved-{}-{}",
        std::process::id(),
        random_uuid()
    );
    let moved_path = parent.join(moved_name);

    fs::rename(&target, &moved_path).map_err(GitError::Io)?;
    Ok(Some(moved_path))
}

/// Restores a previously moved worktree file back to its original location.
pub fn restore_moved_worktree_file(
    root: &Path,
    path: &str,
    moved: &Path,
) -> Result<bool, GitError> {
    let target = root.join(path);
    match fs::hard_link(moved, &target) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(err) => return Err(GitError::Io(err)),
    }
    let _ = fs::remove_file(moved);
    if let Some(parent) = target.parent() {
        sync_directory(parent)?;
    }
    Ok(true)
}

/// Removes a worktree file atomically.
pub fn remove_worktree_file(root: &Path, path: &str) -> Result<(), GitError> {
    let target = root.join(path);
    assert_safe_parents(root, path)?;
    let meta = match fs::symlink_metadata(&target) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(GitError::Io(err)),
    };
    if meta.file_type().is_symlink() {
        return Err(unsupported_worktree(path, "symlink"));
    }
    if !meta.is_file() {
        return Err(unsupported_worktree(path, "non-file"));
    }
    fs::remove_file(&target).map_err(GitError::Io)?;
    if let Some(parent) = target.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

/// Discards a moved temporary file.
pub fn discard_moved_worktree_file(moved: &Path) -> Result<(), GitError> {
    if moved.exists() {
        fs::remove_file(moved).map_err(GitError::Io)?;
    }
    Ok(())
}

/// Restores a claimed reservation file to target location.
pub fn restore_claimed_worktree_file(
    root: &Path,
    path: &str,
    claimed: &Path,
) -> Result<Result<(), PathBuf>, GitError> {
    let meta = match fs::symlink_metadata(claimed) {
        Ok(m) => m,
        Err(err) => return Err(GitError::Io(err)),
    };
    if !meta.is_file() {
        return Ok(Err(claimed.to_path_buf()));
    }
    let target = root.join(path);
    match fs::hard_link(claimed, &target) {
        Ok(()) => {
            let _ = fs::remove_file(claimed);
            Ok(Ok(()))
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            Ok(Err(claimed.to_path_buf()))
        }
        Err(err) => Err(GitError::Io(err)),
    }
}

/// Finalizer callback for worktree deletion reservation.
pub struct WorktreeDeletionFinalizer {
    finish_fn: Box<dyn FnOnce() -> Result<Result<bool, PathBuf>, GitError>>,
}

impl WorktreeDeletionFinalizer {
    pub fn finish(self) -> Result<Result<bool, PathBuf>, GitError> {
        (self.finish_fn)()
    }
}

/// Worktree deletion reservation for verifying and finishing atomic deletion.
pub struct WorktreeDeletionReservation {
    verify_fn: Box<dyn FnOnce() -> Result<Option<WorktreeDeletionFinalizer>, GitError>>,
}

impl WorktreeDeletionReservation {
    pub fn verify(self) -> Result<Option<WorktreeDeletionFinalizer>, GitError> {
        (self.verify_fn)()
    }
}

/// Reserves deletion of a moved worktree file by creating a marker directory.
pub fn reserve_moved_worktree_deletion<F>(
    root: &Path,
    path: &str,
    moved: &Path,
    restore_claimed: F,
) -> Result<Option<WorktreeDeletionReservation>, GitError>
where
    F: Fn(&Path) -> Result<Result<(), PathBuf>, GitError> + 'static,
{
    let target = root.join(path);
    let marker = random_uuid();

    #[cfg(unix)]
    {
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&target) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok(None),
            Err(err) => return Err(GitError::Io(err)),
        }
    }
    #[cfg(not(unix))]
    {
        match fs::create_dir(&target) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok(None),
            Err(err) => return Err(GitError::Io(err)),
        }
    }

    let marker_path = target.join(".omo-reservation");
    fs::write(&marker_path, &marker).map_err(GitError::Io)?;

    let reserved = fs::symlink_metadata(&target).map_err(GitError::Io)?;
    #[cfg(unix)]
    let (res_dev, res_ino) = (reserved.dev(), reserved.ino());
    #[cfg(not(unix))]
    let (res_dev, res_ino) = (0u64, 0u64);

    let moved_path = moved.to_path_buf();
    let root_path = root.to_path_buf();

    let reservation = WorktreeDeletionReservation {
        verify_fn: Box::new(move || {
            let current = match fs::symlink_metadata(&target) {
                Ok(m) => m,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(err) => return Err(GitError::Io(err)),
            };

            #[cfg(unix)]
            let dev_ino_match = current.dev() == res_dev && current.ino() == res_ino;
            #[cfg(not(unix))]
            let dev_ino_match = true;

            if !current.is_dir() || !dev_ino_match {
                return Ok(None);
            }

            match fs::read_to_string(&marker_path) {
                Ok(content) if content == marker => {}
                _ => return Ok(None),
            }

            let finalizer = WorktreeDeletionFinalizer {
                finish_fn: Box::new(move || {
                    let parent = target.parent().unwrap_or(&root_path);
                    let file_name = target
                        .file_name()
                        .map(|n| n.to_string_lossy())
                        .unwrap_or_default();
                    let claimed_name = format!(
                        ".{file_name}.omo-reserved-{}-{}",
                        std::process::id(),
                        random_uuid()
                    );
                    let claimed = parent.join(claimed_name);

                    match fs::rename(&target, &claimed) {
                        Ok(()) => {}
                        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                            return Ok(Ok(false));
                        }
                        Err(err) => return Err(GitError::Io(err)),
                    }

                    let owned = fs::symlink_metadata(&claimed).map_err(GitError::Io)?;
                    #[cfg(unix)]
                    let owned_dev_ino_match = owned.dev() == res_dev && owned.ino() == res_ino;
                    #[cfg(not(unix))]
                    let owned_dev_ino_match = true;

                    let claimed_marker = fs::read_to_string(claimed.join(".omo-reservation")).ok();

                    if !owned.is_dir()
                        || !owned_dev_ino_match
                        || claimed_marker.as_deref() != Some(&marker)
                    {
                        let restored = restore_claimed(&claimed)?;
                        return match restored {
                            Ok(()) => Ok(Ok(false)),
                            Err(path) => Ok(Err(path)),
                        };
                    }

                    let _ = fs::remove_file(&moved_path);
                    let _ = fs::remove_dir_all(&claimed);
                    sync_directory(parent)?;

                    match fs::symlink_metadata(&target) {
                        Ok(_) => Ok(Ok(false)),
                        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Ok(true)),
                        Err(err) => Err(GitError::Io(err)),
                    }
                }),
            };

            Ok(Some(finalizer))
        }),
    };

    Ok(Some(reservation))
}

/// Asserts that all parent directories up to root exist, are directories, and are not symlinks.
pub fn assert_safe_parents(root: &Path, path: &str) -> Result<(), GitError> {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 1 {
        return Ok(());
    }
    let mut current = root.to_path_buf();
    for part in &parts[..parts.len() - 1] {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) => {
                if meta.file_type().is_symlink() || !meta.is_dir() {
                    return Err(unsupported_worktree(path, "unsafe parent"));
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(GitError::Io(err)),
        }
    }
    Ok(())
}

/// Generates an unsupported worktree state GitError.
pub fn unsupported_worktree(path: &str, kind: &str) -> GitError {
    GitError::PathState(format!("Unsupported worktree {kind} at path: {path}"))
}

fn assert_replaceable_target(path: &Path, display_path: &str) -> Result<(), GitError> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(unsupported_worktree(display_path, "symlink"));
            }
            if !meta.is_file() {
                return Err(unsupported_worktree(display_path, "non-file"));
            }
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(GitError::Io(err)),
    }
}

fn sync_directory(path: &Path) -> Result<(), GitError> {
    #[cfg(unix)]
    {
        if let Ok(dir) = File::open(path) {
            let _ = dir.sync_all();
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}
