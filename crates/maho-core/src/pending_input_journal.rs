//! Port of the durability part of senpi packages/coding-agent/src/core/pending-input-journal.ts.
//!
//! The session JSONL is the pending-input journal; each transition must be on disk and synced before it
//! counts. Only the file-sync layer (senpi#3866) and its uncertain-append error live here: the store and
//! journal reader depend on the pending-input store, which mhc has not ported.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// How the journal makes an append durable: the file's data, and once the directory entry that names it.
pub trait JournalDurability: Send + Sync {
    fn sync_file(&self, path: &Path) -> io::Result<()>;
    fn sync_directory(&self, path: &Path) -> io::Result<()>;
}

/// Opens a path with write access (`r+`, `write = true`) or read-only (`r`, `write = false`).
pub type JournalFileOpener = fn(&Path, bool) -> io::Result<File>;

pub fn open_journal_file(path: &Path, write: bool) -> io::Result<File> {
    OpenOptions::new().read(true).write(write).open(path)
}

/// Windows flushes only through a handle with write access (FlushFileBuffers on a read-only handle fails
/// with EPERM, senpi#3866), so the file is synced through an `r+` handle. A directory cannot be opened for
/// writing anywhere, and on Windows a directory handle cannot be flushed at all, so the directory sync is
/// skipped there.
pub struct FsJournalDurability {
    windows: bool,
    open_file: JournalFileOpener,
}

pub fn fs_journal_durability(windows: bool, open_file: JournalFileOpener) -> FsJournalDurability {
    FsJournalDurability { windows, open_file }
}

impl Default for FsJournalDurability {
    fn default() -> Self {
        fs_journal_durability(cfg!(windows), open_journal_file)
    }
}

impl JournalDurability for FsJournalDurability {
    fn sync_file(&self, path: &Path) -> io::Result<()> {
        (self.open_file)(path, true)?.sync_all()
    }

    fn sync_directory(&self, path: &Path) -> io::Result<()> {
        if self.windows {
            return Ok(());
        }
        (self.open_file)(path, false)?.sync_all()
    }
}

/// The entry reached the file but could not be synced: it may or may not survive a crash. A local failure
/// never walks the model fallback chain.
#[derive(Debug, thiserror::Error)]
#[error(
    "Pending input transition {cursor} was written to {file} but could not be synced ({reason}). This is a local file error, not a model error; reopen the session to recover."
)]
pub struct PendingInputAppendUncertainError {
    pub cursor: u64,
    pub file: String,
    pub reason: String,
}

impl PendingInputAppendUncertainError {
    pub const CODE: &'static str = "pending_input_append_uncertain";

    pub fn new(cursor: u64, file: &str, cause: &dyn std::fmt::Display) -> Self {
        Self {
            cursor,
            file: file.to_owned(),
            reason: cause.to_string(),
        }
    }

    pub fn code(&self) -> &'static str {
        Self::CODE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static OPENED: Mutex<Vec<bool>> = Mutex::new(Vec::new());

    fn recording_open(path: &Path, write: bool) -> io::Result<File> {
        if let Ok(mut opened) = OPENED.lock() {
            opened.push(write);
        }
        open_journal_file(path, write)
    }

    #[test]
    fn syncs_file_through_write_handle_and_skips_directory_on_windows() {
        let dir = std::env::temp_dir();
        let file = dir.join(format!("mhc-journal-{}.jsonl", uuid::Uuid::new_v4()));
        assert!(std::fs::write(&file, b"{}\n").is_ok());
        let windows = fs_journal_durability(true, recording_open);
        assert!(windows.sync_file(&file).is_ok());
        assert!(windows.sync_directory(&dir).is_ok());
        assert_eq!(
            OPENED
                .lock()
                .map(|opened| opened.clone())
                .unwrap_or_default(),
            vec![true]
        );
        let unix = fs_journal_durability(false, recording_open);
        assert!(unix.sync_directory(&dir).is_ok());
        assert_eq!(
            OPENED
                .lock()
                .map(|opened| opened.clone())
                .unwrap_or_default(),
            vec![true, false]
        );
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn uncertain_error_message_matches_upstream() {
        let error = PendingInputAppendUncertainError::new(3, "/s.jsonl", &"EIO");
        assert_eq!(
            error.to_string(),
            "Pending input transition 3 was written to /s.jsonl but could not be synced (EIO). This is a local file error, not a model error; reopen the session to recover."
        );
        assert_eq!(error.code(), "pending_input_append_uncertain");
    }
}
