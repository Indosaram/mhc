//! Port of senpi packages/coding-agent/src/core/session-write-recovery.ts.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};

const NEWLINE: u8 = b'\n';
const SCAN_CHUNK_BYTES: u64 = 64 * 1024;

/// Cuts a JSONL session file back to its last complete line. An append that failed part-way (ENOSPC) can
/// leave a partial line, and the next append would be glued onto it. A file that already ends in a
/// newline is left untouched.
pub fn truncate_to_last_complete_line(path: &str) -> io::Result<()> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let size = file.metadata()?.len();
    let mut chunk = vec![0u8; SCAN_CHUNK_BYTES.min(size) as usize];
    let mut end = size;
    while end > 0 {
        let start = end.saturating_sub(chunk.len() as u64);
        let length = (end - start) as usize;
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut chunk[..length])?;
        if let Some(newline) = chunk[..length].iter().rposition(|byte| *byte == NEWLINE) {
            let complete_length = start + newline as u64 + 1;
            if complete_length < size {
                file.set_len(complete_length)?;
            }
            return Ok(());
        }
        end = start;
    }
    Ok(())
}

/// The write failed, and its partial file could not be cleaned up either.
#[derive(Debug)]
pub struct SessionWriteCleanupError {
    pub path: String,
    pub write_error: io::Error,
    pub cleanup_errors: Vec<io::Error>,
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
    Write(io::Error),
    Cleanup(SessionWriteCleanupError),
}

/// Closes and removes a session file whose first flush failed part-way, then returns the write error. The
/// first flush creates the file exclusively, so a partial file left behind would fail every later flush
/// with EEXIST. Rust closes `file` on drop, which cannot report a failure, so only the removal can fail.
pub fn discard_failed_first_flush(
    path: &str,
    file: Option<File>,
    write_error: io::Error,
) -> FailedFirstFlush {
    drop(file);
    match std::fs::remove_file(path) {
        Ok(()) => FailedFirstFlush::Write(write_error),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            FailedFirstFlush::Write(write_error)
        }
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
    write_error: io::Error,
) -> FailedFirstFlush {
    drop(file);
    match tokio::fs::remove_file(path).await {
        Ok(()) => FailedFirstFlush::Write(write_error),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            FailedFirstFlush::Write(write_error)
        }
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

    #[test]
    fn cuts_a_torn_tail_and_leaves_a_complete_file_untouched() {
        let Some(dir) = tempfile::tempdir().ok() else {
            panic!("tempdir")
        };
        let path = dir.path().join("s.jsonl").to_string_lossy().into_owned();
        assert!(std::fs::write(&path, "{\"a\":1}\n{\"b\":").is_ok());
        assert!(truncate_to_last_complete_line(&path).is_ok());
        assert_eq!(
            std::fs::read_to_string(&path).ok().as_deref(),
            Some("{\"a\":1}\n")
        );
        assert!(truncate_to_last_complete_line(&path).is_ok());
        assert_eq!(
            std::fs::read_to_string(&path).ok().as_deref(),
            Some("{\"a\":1}\n")
        );
    }

    #[test]
    fn keeps_a_file_with_no_complete_line_and_scans_across_chunks() {
        let Some(dir) = tempfile::tempdir().ok() else {
            panic!("tempdir")
        };
        let path = dir.path().join("s.jsonl").to_string_lossy().into_owned();
        assert!(std::fs::write(&path, "partial").is_ok());
        assert!(truncate_to_last_complete_line(&path).is_ok());
        assert_eq!(
            std::fs::read_to_string(&path).ok().as_deref(),
            Some("partial")
        );
        let long = format!("x\n{}", "y".repeat(SCAN_CHUNK_BYTES as usize + 10));
        assert!(std::fs::write(&path, &long).is_ok());
        assert!(truncate_to_last_complete_line(&path).is_ok());
        assert_eq!(std::fs::read_to_string(&path).ok().as_deref(), Some("x\n"));
    }

    #[test]
    fn removes_the_partial_first_flush_and_returns_the_write_error() {
        let Some(dir) = tempfile::tempdir().ok() else {
            panic!("tempdir")
        };
        let path = dir.path().join("s.jsonl").to_string_lossy().into_owned();
        let file = File::create(&path).ok();
        let result = discard_failed_first_flush(&path, file, io::Error::other("ENOSPC"));
        assert!(matches!(result, FailedFirstFlush::Write(error) if error.to_string() == "ENOSPC"));
        assert!(!std::path::Path::new(&path).exists());
    }
}
