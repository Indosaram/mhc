//! The persisted child transcript locator the runner hands to the host (the host's
//! `SessionManager.create` / `SessionManager.open` as used by `runners/in-process.ts`).

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use serde_json::{Value, json};

static NEXT_SESSION: AtomicU64 = AtomicU64::new(0);

pub struct ChildSessionManager {
    cwd: String,
    session_dir: String,
    session_file: PathBuf,
    session_id: String,
    header_written: Mutex<bool>,
}

impl ChildSessionManager {
    /// A new persisted session under `session_dir`; the directory must be creatable.
    pub fn create(cwd: &str, session_dir: &str) -> io::Result<Self> {
        fs::create_dir_all(session_dir)?;
        let now = chrono::Utc::now();
        let session_id = format!(
            "{:x}-{:x}",
            now.timestamp_nanos_opt().unwrap_or_default(),
            NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
        );
        let stamp = now.format("%Y-%m-%dT%H-%M-%S-%3fZ");
        Ok(Self {
            cwd: cwd.to_string(),
            session_dir: session_dir.to_string(),
            session_file: Path::new(session_dir).join(format!("{stamp}_{session_id}.jsonl")),
            session_id,
            header_written: Mutex::new(false),
        })
    }

    /// Reopen an existing transcript; appends continue the same file.
    pub fn open(session_path: &Path, session_dir: &str, cwd: &str) -> Self {
        Self {
            cwd: cwd.to_string(),
            session_dir: session_dir.to_string(),
            session_file: session_path.to_path_buf(),
            session_id: String::new(),
            header_written: Mutex::new(true),
        }
    }

    pub fn is_persisted(&self) -> bool {
        true
    }

    pub fn session_dir(&self) -> &str {
        &self.session_dir
    }

    pub fn session_file(&self) -> &Path {
        &self.session_file
    }

    pub fn append_message(&self, message: &Value) -> io::Result<()> {
        let mut header_written = self
            .header_written
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.session_file)?;
        if !*header_written {
            let header = json!({
                "type": "session",
                "id": self.session_id,
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "cwd": self.cwd,
            });
            writeln!(file, "{header}")?;
            *header_written = true;
        }
        writeln!(file, "{}", json!({ "type": "message", "message": message }))
    }
}
