//! Port of senpi packages/coding-agent/src/core/session-file-replace.ts.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

struct ReplaceTarget {
    path: PathBuf,
    mode: Option<u32>,
}

/// A symlinked session file keeps its link: the replacement lands on the file it points at.
fn resolve_target(file_path: &Path) -> io::Result<ReplaceTarget> {
    match std::fs::canonicalize(file_path) {
        Ok(path) => {
            let mode = file_mode(&path)?;
            Ok(ReplaceTarget { path, mode })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(ReplaceTarget {
            path: file_path.to_path_buf(),
            mode: None,
        }),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn file_mode(path: &Path) -> io::Result<Option<u32>> {
    use std::os::unix::fs::PermissionsExt;
    Ok(Some(std::fs::metadata(path)?.permissions().mode() & 0o7777))
}

#[cfg(not(unix))]
fn file_mode(_path: &Path) -> io::Result<Option<u32>> {
    Ok(None)
}

/// The temp file cannot be created because the directory refuses new entries (EACCES/EPERM/EROFS).
fn directory_not_writable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
    )
}

fn open_temp(temp_path: &Path) -> io::Result<Option<File>> {
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp_path)
    {
        Ok(file) => Ok(Some(file)),
        Err(error) if directory_not_writable(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// The pre-atomic rewrite, kept for a writable file in a directory that takes no new entries.
fn rewrite_in_place<'a>(path: &Path, chunks: impl IntoIterator<Item = &'a str>) -> io::Result<()> {
    let mut file = File::create(path)?;
    for chunk in chunks {
        file.write_all(chunk.as_bytes())?;
    }
    Ok(())
}

fn write_durably<'a>(
    mut file: File,
    chunks: impl IntoIterator<Item = &'a str>,
    mode: Option<u32>,
) -> io::Result<()> {
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    for chunk in chunks {
        file.write_all(chunk.as_bytes())?;
    }
    file.sync_all()
}

/// Directory fsync is not supported everywhere (EINVAL, ENOTSUP, EISDIR); the rename itself already happened.
#[cfg(unix)]
fn sync_directory(directory: &Path) -> io::Result<()> {
    let handle = File::open(directory)?;
    match handle.sync_all() {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::InvalidInput
                    | io::ErrorKind::Unsupported
                    | io::ErrorKind::IsADirectory
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

#[cfg(not(unix))]
fn sync_directory(_directory: &Path) -> io::Result<()> {
    Ok(())
}

/// Replaces `file_path` with the concatenated `chunks` so that no failure can leave a partial file behind:
/// the content goes to a temp file in the same directory, is fsynced, and is renamed over the target, then
/// the directory entry is fsynced where supported. A failure before the rename leaves the original
/// byte-identical and removes the temp file. When the directory refuses the temp file the file is
/// rewritten in place, so a writable file in a read-only directory still migrates.
pub fn replace_file_atomically<'a>(
    file_path: &Path,
    chunks: impl IntoIterator<Item = &'a str>,
) -> io::Result<()> {
    let target = resolve_target(file_path)?;
    let directory = target
        .path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let name = target
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp_path = directory.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let Some(temp) = open_temp(&temp_path)? else {
        return rewrite_in_place(&target.path, chunks);
    };
    let replaced = write_durably(temp, chunks, target.mode)
        .and_then(|()| std::fs::rename(&temp_path, &target.path));
    if let Err(error) = replaced {
        return match std::fs::remove_file(&temp_path) {
            Ok(()) => Err(error),
            Err(cleanup) if cleanup.kind() == io::ErrorKind::NotFound => Err(error),
            Err(cleanup) => Err(io::Error::new(
                error.kind(),
                format!(
                    "session file rewrite failed and its temporary file could not be removed: {error}; {cleanup}"
                ),
            )),
        };
    }
    sync_directory(&directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_the_file_and_keeps_its_mode_and_symlink() {
        let Some(dir) = tempfile::tempdir().ok() else {
            panic!("tempdir")
        };
        let target = dir.path().join("s.jsonl");
        assert!(std::fs::write(&target, "old\n").is_ok());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert!(
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).is_ok()
            );
            let link = dir.path().join("link.jsonl");
            assert!(std::os::unix::fs::symlink(&target, &link).is_ok());
            assert!(replace_file_atomically(&link, ["a\n", "b\n"]).is_ok());
            assert!(
                std::fs::symlink_metadata(&link)
                    .map(|m| m.file_type().is_symlink())
                    .unwrap_or(false)
            );
            let mode = std::fs::metadata(&target)
                .map(|m| m.permissions().mode() & 0o7777)
                .unwrap_or(0);
            assert_eq!(mode, 0o640);
        }
        #[cfg(not(unix))]
        assert!(replace_file_atomically(&target, ["a\n", "b\n"]).is_ok());
        assert_eq!(
            std::fs::read_to_string(&target).ok().as_deref(),
            Some("a\nb\n")
        );
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
                    .count()
            })
            .into_iter()
            .collect();
        assert_eq!(leftovers, vec![0]);
    }

    #[test]
    fn creates_a_missing_file() {
        let Some(dir) = tempfile::tempdir().ok() else {
            panic!("tempdir")
        };
        let target = dir.path().join("new.jsonl");
        assert!(replace_file_atomically(&target, ["x\n"]).is_ok());
        assert_eq!(
            std::fs::read_to_string(&target).ok().as_deref(),
            Some("x\n")
        );
    }
}
