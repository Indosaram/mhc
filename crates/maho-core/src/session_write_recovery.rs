//! Port of senpi packages/coding-agent/src/core/session-write-recovery.ts.
//!
//! A write that failed part-way (a full disk) can leave a torn final line. Before anything is cut,
//! the torn bytes are copied to `<file>.torn-<time>-<id>` through the atomic writer and the person
//! gets a one-time notice naming that backup (senpi#4295).

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::path::Path;

const NEWLINE: u8 = b'\n';
const SCAN_CHUNK_BYTES: u64 = 64 * 1024;

/// Why a session file's tail was set aside: a write that hit a full disk, any other failed write, or found on open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionRecoveryCause {
    DiskFull,
    WriteFailed,
    #[default]
    Unknown,
}

impl SessionRecoveryCause {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DiskFull => "disk_full",
            Self::WriteFailed => "write_failed",
            Self::Unknown => "unknown",
        }
    }
}

/// One repair of a session file: the incomplete bytes were copied to `backup_path` (never deleted) and cut.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRecovery {
    pub cause: SessionRecoveryCause,
    pub backup_path: String,
    pub dropped_bytes: usize,
    /// The one-time notice for the person.
    pub notice: String,
}

fn is_disk_full(error: &(dyn std::error::Error + 'static)) -> bool {
    let Some(io) = error.downcast_ref::<std::io::Error>() else {
        return false;
    };
    // ENOSPC / EDQUOT (ErrorKind maps both on every platform std knows them on).
    matches!(io.kind(), ErrorKind::StorageFull | ErrorKind::QuotaExceeded)
}

/// ENOSPC/EDQUOT anywhere in the first eight links of the cause chain is a full disk; anything else a failed write.
pub fn session_recovery_cause(error: &(dyn std::error::Error + 'static)) -> SessionRecoveryCause {
    let mut current = Some(error);
    for _ in 0..8 {
        let Some(error) = current else { break };
        if is_disk_full(error) {
            return SessionRecoveryCause::DiskFull;
        }
        // io::Error's source() skips the error it wraps; its get_ref() is the next link.
        current = match error.downcast_ref::<std::io::Error>() {
            Some(io) => io
                .get_ref()
                .map(|inner| inner as &(dyn std::error::Error + 'static)),
            None => error.source(),
        };
    }
    SessionRecoveryCause::WriteFailed
}

fn recovery_notice(cause: SessionRecoveryCause, backup_path: &str) -> String {
    let shortened = crate::paths::shorten_path(backup_path);
    let backup = if shortened.starts_with("~/") || shortened.starts_with("~\\") {
        shortened
    } else {
        "a session recovery backup".to_owned()
    };
    match cause {
        SessionRecoveryCause::DiskFull => format!(
            "The last message wasn't saved because the disk was full. Free disk space to keep saving this session; the incomplete record was kept in {backup}."
        ),
        SessionRecoveryCause::WriteFailed => format!(
            "The last message wasn't saved because writing the session file failed. The incomplete record was kept in {backup}."
        ),
        SessionRecoveryCause::Unknown => format!(
            "The last message of this session wasn't saved completely (an earlier save was interrupted, for example by a full disk). Everything before it was loaded; the incomplete record was kept in {backup}."
        ),
    }
}

/// Copies torn bytes to `<path>.torn-<time>-<id>` through the atomic session-file writer (temp file, fsync,
/// rename), so a full disk leaves no partial copy, and returns the recovery to report. Fails when the copy is refused.
pub fn backup_torn_bytes(
    path: &Path,
    bytes: &[u8],
    cause: SessionRecoveryCause,
) -> std::io::Result<SessionRecovery> {
    let id = uuid::Uuid::new_v4().to_string();
    let backup_path = format!(
        "{}.torn-{}-{}",
        path.display(),
        chrono::Utc::now().timestamp_millis(),
        &id[..8]
    );
    crate::session_file_replace::replace_file_atomically(Path::new(&backup_path), &[bytes], None)?;
    Ok(SessionRecovery {
        cause,
        notice: recovery_notice(cause, &backup_path),
        backup_path,
        dropped_bytes: bytes.len(),
    })
}

/// Cuts a JSONL session file back to its last complete line, first copying what it cuts aside (senpi#4295).
/// An append that failed part-way (ENOSPC) can leave a partial line, and the next append would be glued
/// onto it, losing both entries on reload. A file that already ends in a newline is left untouched and
/// returns `None`; a refused copy fails before anything is cut.
pub fn truncate_to_last_complete_line(
    path: &Path,
    cause: SessionRecoveryCause,
) -> std::io::Result<Option<SessionRecovery>> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let size = file.metadata()?.len();
    let mut chunk = vec![0u8; SCAN_CHUNK_BYTES.min(size) as usize];
    let mut tail: Vec<u8> = Vec::new();
    let mut end = size;
    while end > 0 {
        let start = end.saturating_sub(chunk.len() as u64);
        let read = (end - start) as usize;
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut chunk[..read])?;
        if let Some(newline) = chunk[..read].iter().rposition(|byte| *byte == NEWLINE) {
            let complete_length = start + newline as u64 + 1;
            if complete_length >= size {
                return Ok(None);
            }
            let mut torn = chunk[newline + 1..read].to_vec();
            torn.extend_from_slice(&tail);
            let recovery = backup_torn_bytes(path, &torn, cause)?;
            file.set_len(complete_length)?;
            return Ok(Some(recovery));
        }
        let mut next = chunk[..read].to_vec();
        next.extend_from_slice(&tail);
        tail = next;
        end = start;
    }
    Ok(None)
}

/// The write failed, and its partial file could not be cleaned up either.
#[derive(Debug)]
pub struct SessionWriteCleanupError {
    pub path: String,
    pub write_error: std::io::Error,
    pub cleanup_errors: Vec<std::io::Error>,
}

impl std::fmt::Display for SessionWriteCleanupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Session file write failed and the partial file {} could not be cleaned up",
            self.path
        )
    }
}

impl std::error::Error for SessionWriteCleanupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.write_error)
    }
}

#[derive(Debug)]
pub enum FailedFirstFlush {
    /// Cleanup succeeded: the original write error.
    Write(std::io::Error),
    Cleanup(SessionWriteCleanupError),
}

/// Closes and removes a session file whose first flush failed part-way, then returns the write error. The
/// first flush creates the file exclusively, so a partial file left behind would fail every later flush
/// with EEXIST. Rust closes `file` on drop, which cannot report a failure, so only the removal can fail.
pub fn discard_failed_first_flush(
    path: &str,
    file: Option<File>,
    write_error: std::io::Error,
) -> FailedFirstFlush {
    drop(file);
    match std::fs::remove_file(path) {
        Ok(()) => FailedFirstFlush::Write(write_error),
        Err(error) if error.kind() == ErrorKind::NotFound => FailedFirstFlush::Write(write_error),
        Err(error) => FailedFirstFlush::Cleanup(SessionWriteCleanupError {
            path: path.to_owned(),
            write_error,
            cleanup_errors: vec![error],
        }),
    }
}

/// `discard_failed_first_flush` for the asynchronous header write.
pub async fn discard_failed_first_flush_async(
    path: &str,
    file: Option<tokio::fs::File>,
    write_error: std::io::Error,
) -> FailedFirstFlush {
    drop(file);
    match tokio::fs::remove_file(path).await {
        Ok(()) => FailedFirstFlush::Write(write_error),
        Err(error) if error.kind() == ErrorKind::NotFound => FailedFirstFlush::Write(write_error),
        Err(error) => FailedFirstFlush::Cleanup(SessionWriteCleanupError {
            path: path.to_owned(),
            write_error,
            cleanup_errors: vec![error],
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backups(dir: &Path) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(dir)
            .expect("dir")
            .map(|entry| entry.expect("entry").path())
            .filter(|path| path.to_string_lossy().contains(".torn-"))
            .collect()
    }

    #[test]
    fn a_torn_tail_is_backed_up_before_it_is_cut() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, "{\"a\":1}\n{\"b\":").expect("write");
        let recovery = truncate_to_last_complete_line(&path, SessionRecoveryCause::DiskFull)
            .expect("truncate")
            .expect("recovery");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "{\"a\":1}\n");
        assert_eq!(recovery.dropped_bytes, 5);
        assert_eq!(recovery.cause, SessionRecoveryCause::DiskFull);
        let saved = backups(dir.path());
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].to_string_lossy(), recovery.backup_path);
        assert_eq!(
            std::fs::read_to_string(&saved[0]).expect("backup"),
            "{\"b\":"
        );
    }

    #[test]
    fn a_terminated_file_is_left_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, "{\"a\":1}\n").expect("write");
        assert!(
            truncate_to_last_complete_line(&path, SessionRecoveryCause::WriteFailed)
                .expect("truncate")
                .is_none()
        );
        assert!(backups(dir.path()).is_empty());
    }

    #[test]
    fn a_file_with_no_newline_is_cut_to_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, "{\"torn").expect("write");
        let recovery =
            truncate_to_last_complete_line(&path, SessionRecoveryCause::Unknown).expect("truncate");
        // No complete line at all: nothing to keep and nothing cut (upstream returns undefined too).
        assert!(recovery.is_none());
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "{\"torn");
    }

    #[test]
    fn full_disk_anywhere_in_the_cause_chain_is_disk_full() {
        let full = std::io::Error::from(ErrorKind::StorageFull);
        assert_eq!(
            session_recovery_cause(&full),
            SessionRecoveryCause::DiskFull
        );
        let wrapped = std::io::Error::other(std::io::Error::from(ErrorKind::QuotaExceeded));
        assert_eq!(
            session_recovery_cause(&wrapped),
            SessionRecoveryCause::DiskFull
        );
        let other = std::io::Error::from(ErrorKind::PermissionDenied);
        assert_eq!(
            session_recovery_cause(&other),
            SessionRecoveryCause::WriteFailed
        );
    }

    #[test]
    fn notices_name_the_backup_only_as_a_home_relative_path() {
        let home = crate::config::home_dir();
        let inside = format!("{home}/.senpi/s.jsonl.torn-1-abcdefgh");
        assert!(
            recovery_notice(SessionRecoveryCause::DiskFull, &inside)
                .contains("~/.senpi/s.jsonl.torn-1-abcdefgh")
        );
        assert!(
            recovery_notice(SessionRecoveryCause::WriteFailed, "/elsewhere/s.torn")
                .ends_with("kept in a session recovery backup.")
        );
        assert!(
            recovery_notice(SessionRecoveryCause::Unknown, "/x")
                .starts_with("The last message of this session wasn't saved completely")
        );
    }

    #[test]
    fn removes_the_partial_first_flush_and_returns_the_write_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.jsonl").to_string_lossy().into_owned();
        let file = File::create(&path).ok();
        let result = discard_failed_first_flush(&path, file, std::io::Error::other("ENOSPC"));
        assert!(matches!(result, FailedFirstFlush::Write(error) if error.to_string() == "ENOSPC"));
        assert!(!std::path::Path::new(&path).exists());
    }
}
