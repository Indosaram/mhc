//! Port of senpi packages/coding-agent/src/core/session-file-replace.ts.
//!
//! Replaces a session file through a temp file in the same directory: write, fsync, rename, then fsync
//! the directory, so a crash or a full disk never leaves a half-written file at the path. When the
//! directory refuses the temp file (EACCES/EPERM/EROFS) the file is rewritten in place as before, so a
//! writable file in a read-only directory still migrates.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use crate::session_file_identity::{FileIdentity, capture_and_close_file_identity};

/// Receives the identity of the descriptor that wrote the replacement, before it closed.
pub type CaptureIdentity<'a> = &'a mut dyn FnMut(Option<FileIdentity>);

struct ReplaceTarget {
    path: PathBuf,
    #[cfg_attr(not(unix), allow(dead_code))]
    mode: Option<u32>,
}

/// A symlinked session file keeps its link: the replacement lands on the file it points at.
fn resolve_target(file_path: &Path) -> std::io::Result<ReplaceTarget> {
    match std::fs::canonicalize(file_path) {
        Ok(path) => {
            let metadata = std::fs::metadata(&path)?;
            Ok(ReplaceTarget {
                mode: mode_of(&metadata),
                path,
            })
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(ReplaceTarget {
            path: file_path.to_owned(),
            mode: None,
        }),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn mode_of(metadata: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(metadata.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn mode_of(_metadata: &std::fs::Metadata) -> Option<u32> {
    None
}

/// The temp file cannot be created because the directory refuses new entries.
fn directory_not_writable(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem
    )
}

fn open_temp(temp_path: &Path) -> std::io::Result<Option<File>> {
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

fn finish(file: File, path: &Path, capture: &mut Option<CaptureIdentity<'_>>) {
    match capture {
        Some(capture) => (*capture)(capture_and_close_file_identity(
            file,
            &path.to_string_lossy(),
        )),
        None => drop(file),
    }
}

/// The pre-atomic rewrite, kept for a writable file in a directory that takes no new entries.
fn rewrite_in_place(
    path: &Path,
    chunks: &[&[u8]],
    capture: &mut Option<CaptureIdentity<'_>>,
) -> std::io::Result<()> {
    let mut file = File::create(path)?;
    let result = chunks.iter().try_for_each(|chunk| file.write_all(chunk));
    finish(file, path, capture);
    result
}

fn write_durably(
    mut file: File,
    chunks: &[&[u8]],
    target: &ReplaceTarget,
    path: &Path,
    capture: &mut Option<CaptureIdentity<'_>>,
) -> std::io::Result<()> {
    let result = (|| {
        #[cfg(unix)]
        if let Some(mode) = target.mode {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        }
        #[cfg(not(unix))]
        let _ = target;
        for chunk in chunks {
            file.write_all(chunk)?;
        }
        file.sync_all()
    })();
    finish(file, path, capture);
    result
}

/// Directory fsync is not a supported operation everywhere; the rename itself already happened.
fn sync_directory(directory: &Path) -> std::io::Result<()> {
    let Ok(handle) = File::open(directory) else {
        return Ok(());
    };
    match handle.sync_all() {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::InvalidInput | ErrorKind::Unsupported | ErrorKind::IsADirectory
            ) || error.raw_os_error() == Some(95) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// Replaces `file_path` with `chunks` atomically. `capture` receives the written descriptor's identity
/// before it closes (senpi#4272); it is delivered only once the replacement is in place.
pub fn replace_file_atomically(
    file_path: &Path,
    chunks: &[&[u8]],
    capture: Option<CaptureIdentity<'_>>,
) -> std::io::Result<()> {
    let mut identity: Option<Option<FileIdentity>> = None;
    let mut record = |value: Option<FileIdentity>| identity = Some(value);
    let mut captured: Option<CaptureIdentity<'_>> = if capture.is_some() {
        Some(&mut record as CaptureIdentity<'_>)
    } else {
        None
    };
    let target = resolve_target(file_path)?;
    let directory = target
        .path
        .parent()
        .map(Path::to_owned)
        .unwrap_or_else(|| PathBuf::from("."));
    let base = target
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp_path = directory.join(format!(".{base}.{}.tmp", uuid::Uuid::new_v4()));
    let Some(temp) = open_temp(&temp_path)? else {
        rewrite_in_place(&target.path, chunks, &mut captured)?;
        drop(captured);
        if let (Some(capture), Some(identity)) = (capture, identity) {
            capture(identity);
        }
        return Ok(());
    };
    let written = write_durably(temp, chunks, &target, file_path, &mut captured)
        .and_then(|()| std::fs::rename(&temp_path, &target.path));
    if let Err(error) = written {
        // A temp file that cannot be removed is reported with the write error (upstream AggregateError).
        return match std::fs::remove_file(&temp_path) {
            Ok(()) => Err(error),
            Err(cleanup) if cleanup.kind() == ErrorKind::NotFound => Err(error),
            Err(cleanup) => Err(std::io::Error::new(
                error.kind(),
                format!(
                    "session file rewrite failed and its temporary file could not be removed: {error}; {cleanup}"
                ),
            )),
        };
    }
    sync_directory(&directory)?;
    drop(captured);
    if let (Some(capture), Some(identity)) = (capture, identity) {
        capture(identity);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_the_file_and_leaves_no_temp_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, "old\n").expect("write");
        replace_file_atomically(&path, &[b"a\n".as_slice(), [0xff, b'\n'].as_slice()], None)
            .expect("replace");
        assert_eq!(std::fs::read(&path).expect("read"), b"a\n\xff\n");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .expect("dir")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(names.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn hands_the_written_descriptor_identity_to_the_capture() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.jsonl");
        let mut seen = None;
        let mut capture = |identity: Option<FileIdentity>| seen = identity;
        replace_file_atomically(
            &path,
            &[b"x\n".as_slice()],
            Some(&mut capture as CaptureIdentity<'_>),
        )
        .expect("replace");
        let identity = seen.expect("identity");
        let metadata = std::fs::metadata(&path).expect("stat");
        assert!(crate::session_file_identity::same_file(
            &metadata, &identity
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_file_keeps_its_link_and_its_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("real.jsonl");
        std::fs::write(&real, "old\n").expect("write");
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        let link = dir.path().join("link.jsonl");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        replace_file_atomically(&link, &[b"new\n".as_slice()], None).expect("replace");
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("lstat")
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&real).expect("read"), "new\n");
        assert_eq!(
            std::fs::metadata(&real).expect("stat").permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn creates_a_missing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("new.jsonl");
        replace_file_atomically(&target, &[b"x\n".as_slice()], None).expect("replace");
        assert_eq!(std::fs::read_to_string(&target).expect("read"), "x\n");
    }
}
