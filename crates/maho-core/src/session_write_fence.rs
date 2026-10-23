//! Port of senpi packages/coding-agent/src/core/session-write-fence.ts (senpi#3652).
//!
//! A host may take a session file over from another generation whose claim it could not prove
//! alive. That verdict can be wrong (a frozen or slept generation also stops heartbeating), so the
//! takeover replaces the token in `<session file>.fence` before the new owner opens the file; every
//! writer remembers the token it saw when it opened the file and re-reads it right before each
//! write. A writer that finds another token refuses the write and every later one.

use std::io::Write;
use std::path::PathBuf;

pub fn session_fence_file(session_file: &str) -> String {
    format!("{session_file}.fence")
}

/// The fence token now on disk; empty when the file was never taken over. Any other read failure errors.
pub fn read_session_fence(session_file: &str) -> std::io::Result<String> {
    match std::fs::read_to_string(session_fence_file(session_file)) {
        Ok(text) => Ok(text.trim().to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error),
    }
}

fn open_private(path: &PathBuf) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Gives the file a new token, so every writer that opened it before this call refuses its next write.
pub fn advance_session_fence(session_file: &str) -> std::io::Result<()> {
    let file = session_fence_file(session_file);
    let temporary = PathBuf::from(format!(
        "{file}.{}.{}.tmp",
        std::process::id(),
        &uuid::Uuid::new_v4().to_string()[..8]
    ));
    let result = (|| -> std::io::Result<()> {
        open_private(&temporary)?.write_all(format!("{}\n", uuid::Uuid::new_v4()).as_bytes())?;
        std::fs::rename(&temporary, &file)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "{session_file} was taken over by another host generation; this writer no longer owns it and will not write to it"
)]
pub struct SessionFencedError {
    pub session_file: String,
}

impl SessionFencedError {
    pub const CODE: &'static str = "session_fenced";

    pub fn new(session_file: &str) -> Self {
        Self {
            session_file: session_file.to_owned(),
        }
    }

    pub fn code(&self) -> &'static str {
        Self::CODE
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionWriteFenceError {
    #[error(transparent)]
    Fenced(#[from] SessionFencedError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// One writer's view of its file's fence: the token seen when the file was opened, re-checked before every write.
#[derive(Debug, Clone, Default)]
pub struct SessionWriteFence {
    seen: Option<(String, String)>,
    fenced_file: Option<String>,
}

impl SessionWriteFence {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn opened(&mut self, session_file: &str) -> std::io::Result<()> {
        if self.fenced_file.as_deref() != Some(session_file) {
            self.fenced_file = None;
        }
        let token = read_session_fence(session_file)?;
        self.seen = Some((session_file.to_owned(), token));
        Ok(())
    }

    pub fn assert_owned(&mut self, session_file: &str) -> Result<(), SessionWriteFenceError> {
        if self.fenced_file.as_deref() == Some(session_file) {
            return Err(SessionFencedError::new(session_file).into());
        }
        // A file this writer starts itself (a new session, a fork) has no earlier owner to fence out.
        let token = self
            .seen
            .as_ref()
            .filter(|(file, _)| file == session_file)
            .map(|(_, token)| token.clone());
        let Some(token) = token else {
            self.opened(session_file)?;
            return Ok(());
        };
        if read_session_fence(session_file)? == token {
            return Ok(());
        }
        self.fenced_file = Some(session_file.to_owned());
        Err(SessionFencedError::new(session_file).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_file(dir: &tempfile::TempDir) -> String {
        dir.path().join("s.jsonl").to_string_lossy().into_owned()
    }

    #[test]
    fn a_never_fenced_file_reads_as_an_empty_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(read_session_fence(&session_file(&dir)).expect("read"), "");
    }

    #[test]
    fn advancing_the_fence_refuses_the_earlier_writer_for_good() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = session_file(&dir);
        let mut fence = SessionWriteFence::new();
        fence.opened(&file).expect("opened");
        fence.assert_owned(&file).expect("owned");
        advance_session_fence(&file).expect("advance");
        let error = fence.assert_owned(&file).expect_err("fenced");
        assert!(
            matches!(&error, SessionWriteFenceError::Fenced(fenced) if fenced.code() == "session_fenced")
        );
        assert!(
            error
                .to_string()
                .contains("was taken over by another host generation")
        );
        assert!(
            fence.assert_owned(&file).is_err(),
            "every later write is refused too"
        );
    }

    #[test]
    fn a_writer_opening_after_the_advance_owns_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = session_file(&dir);
        advance_session_fence(&file).expect("advance");
        let mut fence = SessionWriteFence::new();
        fence.opened(&file).expect("opened");
        fence.assert_owned(&file).expect("owned");
        assert!(!read_session_fence(&file).expect("read").is_empty());
    }

    #[test]
    fn an_unopened_file_is_adopted_on_first_assert() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = session_file(&dir);
        let mut fence = SessionWriteFence::new();
        fence.assert_owned(&file).expect("adopted");
        advance_session_fence(&file).expect("advance");
        assert!(fence.assert_owned(&file).is_err());
    }
}
