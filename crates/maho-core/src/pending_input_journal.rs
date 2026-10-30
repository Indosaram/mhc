//! Port of senpi packages/coding-agent/src/core/pending-input-journal.ts.
//!
//! The session JSONL is the pending-input journal (senpi#3549): each transition is one `custom` entry, on
//! disk and synced before the store publishes or dispatches it, even before the session's first message.
//! The session is reached through [`JournalSession`], upstream's `Pick<SessionManager, ...>`.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

use crate::pending_input_store::{
    PENDING_INPUT_TRANSITION, PendingInputError, PendingInputJournal, PendingInputJournalRecord,
};

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

/// The slice of the session the journal needs (upstream `JournalSession`).
#[async_trait::async_trait]
pub trait JournalSession: Send + Sync {
    fn append_custom_entry(&self, custom_type: &str, data: Value);
    /// Writes the header so later entries reach the file; resolves once it is on disk.
    async fn persist_header_now(&self) -> Result<(), String>;
    fn entries(&self) -> Vec<Value>;
    fn session_file(&self) -> Option<String>;
    fn is_persisted(&self) -> bool;
    fn is_transcript_flushed(&self) -> bool;
}

/// The session JSONL as the pending-input journal.
pub struct SessionPendingInputJournal {
    session: Arc<dyn JournalSession>,
    durability: Arc<dyn JournalDurability>,
    directory_synced: AtomicBool,
}

impl SessionPendingInputJournal {
    pub fn new(session: Arc<dyn JournalSession>) -> Self {
        Self::with_durability(session, Arc::new(FsJournalDurability::default()))
    }

    pub fn with_durability(
        session: Arc<dyn JournalSession>,
        durability: Arc<dyn JournalDurability>,
    ) -> Self {
        Self {
            session,
            durability,
            directory_synced: AtomicBool::new(false),
        }
    }

    /// A persisted session's transitions come from one streaming pass over its file, never from the
    /// whole-history entries (senpi#3519, #3535). An in-memory session, or one whose file is not written yet,
    /// holds its transitions only in memory (#3586 review H1).
    fn transition_entries(&self) -> Result<Vec<TransitionEntry>, PendingInputError> {
        let file = if self.session.is_persisted() && self.session.is_transcript_flushed() {
            self.session.session_file()
        } else {
            None
        };
        let Some(file) = file else {
            return Ok(self
                .session
                .entries()
                .into_iter()
                .filter(|entry| {
                    entry.get("type").and_then(Value::as_str) == Some("custom")
                        && entry.get("customType").and_then(Value::as_str)
                            == Some(PENDING_INPUT_TRANSITION)
                })
                .map(|entry| TransitionEntry {
                    data: entry.get("data").cloned().unwrap_or(Value::Null),
                    id: entry.get("id").map(entry_id_string).unwrap_or_default(),
                })
                .collect());
        };
        match scan_transitions(Path::new(&file)) {
            Err(ScanError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(ScanError::Io(error)) => Err(PendingInputError::Journal(error.to_string())),
            Err(ScanError::Corrupt(line_number)) => {
                Err(PendingInputError::JournalCorrupt { line_number })
            }
            Ok(entries) => Ok(entries),
        }
    }
}

#[async_trait::async_trait]
impl PendingInputJournal for SessionPendingInputJournal {
    /// A session opened without persistence keeps the journal in memory only. A persisted session must have
    /// its file on disk before the entry counts: the header is forced, the entry must reach the file itself,
    /// and the file and, once, its directory are synced before this returns.
    async fn append(&self, record: &PendingInputJournalRecord) -> Result<(), PendingInputError> {
        let data = serde_json::to_value(record)
            .map_err(|error| PendingInputError::Journal(error.to_string()))?;
        if !self.session.is_persisted() {
            self.session
                .append_custom_entry(PENDING_INPUT_TRANSITION, data);
            return Ok(());
        }
        self.session
            .persist_header_now()
            .await
            .map_err(PendingInputError::Journal)?;
        let file = self.session.session_file();
        let Some(file) = file.filter(|_| self.session.is_transcript_flushed()) else {
            return Err(PendingInputError::NotDurable);
        };
        self.session
            .append_custom_entry(PENDING_INPUT_TRANSITION, data);
        // From here the entry is in the file: a failure can no longer be undone, only reported as uncertain.
        let path = Path::new(&file);
        let synced = self.durability.sync_file(path).and_then(|()| {
            if self.directory_synced.load(Ordering::SeqCst) {
                return Ok(());
            }
            let directory = path.parent().unwrap_or_else(|| Path::new("."));
            self.durability.sync_directory(directory)?;
            self.directory_synced.store(true, Ordering::SeqCst);
            Ok(())
        });
        synced.map_err(|error| {
            let uncertain = PendingInputAppendUncertainError::new(record.cursor, &file, &error);
            PendingInputError::AppendUncertain {
                cursor: record.cursor,
                message: uncertain.to_string(),
            }
        })
    }

    async fn read(&self) -> Result<Vec<PendingInputJournalRecord>, PendingInputError> {
        let mut records = self
            .transition_entries()?
            .into_iter()
            .map(|entry| parse_record(entry.data, &entry.id))
            .collect::<Result<Vec<_>, _>>()?;
        records.sort_by_key(|record| record.cursor);
        // The session loader skips a line that does not parse, so a lost transition shows only as a cursor gap.
        for (index, record) in records.iter().enumerate() {
            let expected = index as u64 + 1;
            if record.cursor != expected {
                return Err(PendingInputError::JournalGap {
                    missing_cursor: expected,
                });
            }
        }
        Ok(records)
    }
}

struct TransitionEntry {
    data: Value,
    id: String,
}

enum ScanError {
    Io(io::Error),
    Corrupt(usize),
}

fn entry_id_string(id: &Value) -> String {
    match id {
        Value::String(id) => id.clone(),
        other => other.to_string(),
    }
}

/// A line that ends in a newline was written completely: if it names a transition and does not parse, it is
/// corrupt and refused, the last line included. A final segment with no newline is a tail torn by a crash: it
/// was never committed, so it is tolerated. Only lines naming a transition are parsed, one line at a time.
fn scan_transitions(file: &Path) -> Result<Vec<TransitionEntry>, ScanError> {
    let mut reader =
        BufReader::with_capacity(1024 * 1024, File::open(file).map_err(ScanError::Io)?);
    let mut entries = Vec::new();
    let mut line = Vec::new();
    let mut line_number = 0;
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line).map_err(ScanError::Io)?;
        if read == 0 {
            break;
        }
        line_number += 1;
        let text = String::from_utf8_lossy(&line);
        if let Some(stripped) = text.strip_suffix('\n') {
            if let Some(entry) = transition_of(stripped, line_number)? {
                entries.push(entry);
            }
        } else if let Some(entry) = final_segment_transition(&text, line_number)? {
            // A final line that parses was written whole (a writer that omits the newline); the loader keeps it.
            entries.push(entry);
        }
    }
    Ok(entries)
}

fn final_segment_transition(
    segment: &str,
    line_number: usize,
) -> Result<Option<TransitionEntry>, ScanError> {
    if !segment.contains(PENDING_INPUT_TRANSITION)
        || serde_json::from_str::<Value>(segment).is_err()
    {
        return Ok(None);
    }
    transition_of(segment, line_number)
}

fn transition_of(line: &str, line_number: usize) -> Result<Option<TransitionEntry>, ScanError> {
    if !line.contains(PENDING_INPUT_TRANSITION) {
        return Ok(None);
    }
    let parsed: Value = serde_json::from_str(line).map_err(|_| ScanError::Corrupt(line_number))?;
    if parsed.get("type").and_then(Value::as_str) != Some("custom")
        || parsed.get("customType").and_then(Value::as_str) != Some(PENDING_INPUT_TRANSITION)
    {
        return Ok(None);
    }
    Ok(Some(TransitionEntry {
        data: parsed.get("data").cloned().unwrap_or(Value::Null),
        id: parsed.get("id").map(entry_id_string).unwrap_or_default(),
    }))
}

fn parse_record(
    data: Value,
    entry_id: &str,
) -> Result<PendingInputJournalRecord, PendingInputError> {
    let invalid = || PendingInputError::JournalInvalid {
        entry_id: entry_id.to_owned(),
    };
    let shaped = data.get("type").and_then(Value::as_str) == Some(PENDING_INPUT_TRANSITION)
        && data.get("cursor").is_some_and(Value::is_number)
        && data.get("input").is_some_and(is_pending_input)
        && data.get("result").is_none_or(is_operation_result)
        && data.get("batch").is_none_or(|batch| {
            batch
                .as_array()
                .is_some_and(|items| items.iter().all(is_pending_input))
        });
    if !shaped {
        return Err(invalid());
    }
    serde_json::from_value(data).map_err(|_| invalid())
}

fn is_operation_result(value: &Value) -> bool {
    value.get("outcome").is_some_and(Value::is_string)
        && value.get("clientMessageId").is_some_and(Value::is_string)
}

fn is_pending_input(value: &Value) -> bool {
    value.get("clientMessageId").is_some_and(Value::is_string)
        && value.get("phase").is_some_and(Value::is_string)
        && value.get("handle").is_some_and(Value::is_object)
        && value.get("revision").is_some_and(Value::is_number)
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
        // The unix branch really opens the directory to fsync it, which Windows cannot do at all
        // (`File::open` on a directory is Access Denied), so that half only runs off Windows.
        #[cfg(not(windows))]
        {
            let unix = fs_journal_durability(false, recording_open);
            assert!(unix.sync_directory(&dir).is_ok());
            assert_eq!(
                OPENED
                    .lock()
                    .map(|opened| opened.clone())
                    .unwrap_or_default(),
                vec![true, false]
            );
        }
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

    // --- senpi test/suite/pending-input-journal.test.ts over a file-backed session double ---

    use crate::pending_input_store::test_support::{options, submit_request};
    use crate::pending_input_store::{PendingInputRead, PendingInputStore, SubmitRequest};
    use crate::pending_input_types::{MutationTarget, PendingInputEvent, PendingInputHandle};
    use std::sync::atomic::AtomicUsize;

    /// A session that writes real JSONL: entries are buffered until the header is persisted, then appended.
    struct FileSession {
        file: Option<String>,
        persisted: bool,
        flushed: AtomicBool,
        entries: Mutex<Vec<Value>>,
        next_id: AtomicUsize,
    }

    impl FileSession {
        fn create(dir: &Path) -> Arc<Self> {
            let file = dir
                .join(format!("{}.jsonl", uuid::Uuid::new_v4()))
                .to_string_lossy()
                .into_owned();
            Arc::new(Self::new(Some(file), true, false, Vec::new()))
        }

        fn in_memory() -> Arc<Self> {
            Arc::new(Self::new(None, false, false, Vec::new()))
        }

        /// Reopens a written file, keeping only lines that parse, like the session loader.
        fn open(file: &str) -> Arc<Self> {
            let entries = std::fs::read_to_string(file)
                .unwrap_or_default()
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .filter(|entry| entry.get("type").and_then(Value::as_str) != Some("session"))
                .collect();
            Arc::new(Self::new(Some(file.to_owned()), true, true, entries))
        }

        fn new(file: Option<String>, persisted: bool, flushed: bool, entries: Vec<Value>) -> Self {
            let next_id = AtomicUsize::new(entries.len());
            Self {
                file,
                persisted,
                flushed: AtomicBool::new(flushed),
                entries: Mutex::new(entries),
                next_id,
            }
        }

        fn file(&self) -> String {
            self.file.clone().unwrap_or_default()
        }

        fn append_line(&self, entry: &Value) {
            use std::io::Write;
            let opened = OpenOptions::new().append(true).open(self.file());
            if let Ok(mut file) = opened {
                let _ = writeln!(file, "{entry}");
            }
        }
    }

    #[async_trait::async_trait]
    impl JournalSession for FileSession {
        fn append_custom_entry(&self, custom_type: &str, data: Value) {
            let id = self.next_id.fetch_add(1, Ordering::SeqCst);
            let entry = serde_json::json!({"type": "custom", "customType": custom_type, "data": data, "id": format!("e{id}")});
            if self.persisted && self.flushed.load(Ordering::SeqCst) {
                self.append_line(&entry);
            }
            if let Ok(mut entries) = self.entries.lock() {
                entries.push(entry);
            }
        }

        async fn persist_header_now(&self) -> Result<(), String> {
            if !self.persisted || self.flushed.load(Ordering::SeqCst) {
                return Ok(());
            }
            let mut text = format!("{}\n", serde_json::json!({"type": "session", "id": "s"}));
            for entry in self.entries() {
                text.push_str(&format!("{entry}\n"));
            }
            std::fs::write(self.file(), text).map_err(|error| error.to_string())?;
            self.flushed.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn entries(&self) -> Vec<Value> {
            self.entries
                .lock()
                .map(|entries| entries.clone())
                .unwrap_or_default()
        }

        fn session_file(&self) -> Option<String> {
            self.file.clone()
        }

        fn is_persisted(&self) -> bool {
            self.persisted
        }

        fn is_transcript_flushed(&self) -> bool {
            self.flushed.load(Ordering::SeqCst)
        }
    }

    struct NoSync;

    impl JournalDurability for NoSync {
        fn sync_file(&self, _: &Path) -> io::Result<()> {
            Ok(())
        }
        fn sync_directory(&self, _: &Path) -> io::Result<()> {
            Ok(())
        }
    }

    fn temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pending-journal-{}", uuid::Uuid::new_v4()));
        assert!(std::fs::create_dir_all(&dir).is_ok());
        dir
    }

    fn journal_over(session: Arc<FileSession>) -> Arc<SessionPendingInputJournal> {
        Arc::new(SessionPendingInputJournal::with_durability(
            session,
            Arc::new(NoSync),
        ))
    }

    async fn store_over(session: Arc<FileSession>, lifetime_id: &str) -> Arc<PendingInputStore> {
        PendingInputStore::open(options(journal_over(session), lifetime_id))
            .await
            .unwrap_or_else(|error| panic!("open: {error}"))
    }

    fn second(id: &str, fingerprint: &str) -> SubmitRequest {
        SubmitRequest {
            client_message_id: id.into(),
            fingerprint: fingerprint.into(),
            ..submit_request()
        }
    }

    fn sample_record(cursor: u64) -> Value {
        serde_json::json!({
            "type": PENDING_INPUT_TRANSITION,
            "cursor": cursor,
            "input": {
                "clientMessageId": "c-1", "fingerprint": "fp",
                "handle": {"lifetimeId": "life-1", "queuedId": "q_1"},
                "revision": 1, "hasEdits": false, "text": "check the logs", "attachments": [],
                "requestedMode": "followUp", "phase": "queued", "attemptCount": 0,
                "superseded": [], "operations": {}
            }
        })
    }

    fn admitted(store: &PendingInputStore, id: &str) -> bool {
        matches!(store.read(id), PendingInputRead::Admitted { .. })
    }

    fn append_raw(file: &str, text: &str) {
        use std::io::Write;
        if let Ok(mut handle) = OpenOptions::new().append(true).open(file) {
            let _ = handle.write_all(text.as_bytes());
        }
    }

    #[tokio::test]
    async fn puts_an_admission_on_disk_before_the_session_has_any_message() {
        let dir = temp_dir();
        let session = FileSession::create(&dir);
        let store = store_over(session.clone(), "life-1").await;
        let _ = store.submit(submit_request()).await;
        let on_disk = std::fs::read_to_string(session.file()).unwrap_or_default();
        let found = on_disk
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .any(|entry| {
                entry.get("customType").and_then(Value::as_str) == Some(PENDING_INPUT_TRANSITION)
                    && entry
                        .pointer("/data/input/clientMessageId")
                        .and_then(Value::as_str)
                        == Some("c-1")
            });
        assert!(found);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn rebuilds_custody_and_edits_from_the_file_after_a_reopen() {
        let dir = temp_dir();
        let session = FileSession::create(&dir);
        let store = store_over(session.clone(), "life-1").await;
        let _ = store.submit(submit_request()).await;
        let edit = PendingInputEvent::Edit {
            target: MutationTarget {
                effect_id: "e1".into(),
                handle: PendingInputHandle {
                    lifetime_id: "life-1".into(),
                    queued_id: "q_1".into(),
                },
                expected_revision: 1,
            },
            text: "edited".into(),
            attachments: Vec::new(),
        };
        let _ = store.apply("c-1", edit).await;
        let reopened = store_over(FileSession::open(&session.file()), "life-2").await;
        match reopened.read("c-1") {
            PendingInputRead::Admitted { projection } => {
                assert_eq!(
                    (projection.text.as_str(), projection.revision),
                    ("edited", 1)
                );
            }
            PendingInputRead::NotAdmitted { .. } => panic!("not admitted"),
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn syncs_the_file_on_every_append_and_its_directory_once() {
        struct Recording(Mutex<Vec<String>>, String, std::path::PathBuf);
        impl JournalDurability for Recording {
            fn sync_file(&self, path: &Path) -> io::Result<()> {
                if let Ok(mut seen) = self.0.lock() {
                    seen.push(format!("file:{}", path == Path::new(&self.1)));
                }
                Ok(())
            }
            fn sync_directory(&self, path: &Path) -> io::Result<()> {
                if let Ok(mut seen) = self.0.lock() {
                    seen.push(format!("dir:{}", path == self.2));
                }
                Ok(())
            }
        }
        let dir = temp_dir();
        let session = FileSession::create(&dir);
        let durability = Arc::new(Recording(
            Mutex::new(Vec::new()),
            session.file(),
            dir.clone(),
        ));
        let journal = Arc::new(SessionPendingInputJournal::with_durability(
            session,
            durability.clone(),
        ));
        let store = PendingInputStore::open(options(journal, "life-1"))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let seen = || {
            durability
                .0
                .lock()
                .map(|seen| seen.clone())
                .unwrap_or_default()
        };
        let _ = store.submit(submit_request()).await;
        assert_eq!(seen(), vec!["file:true", "dir:true"]);
        let _ = store.submit(second("c-2", "fp2")).await;
        assert_eq!(seen(), vec!["file:true", "dir:true", "file:true"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn refuses_to_count_an_entry_that_did_not_reach_the_session_file() {
        struct Unflushed;
        #[async_trait::async_trait]
        impl JournalSession for Unflushed {
            fn append_custom_entry(&self, _: &str, _: Value) {}
            async fn persist_header_now(&self) -> Result<(), String> {
                Ok(())
            }
            fn entries(&self) -> Vec<Value> {
                Vec::new()
            }
            fn session_file(&self) -> Option<String> {
                Some("/tmp/never-written.jsonl".into())
            }
            fn is_persisted(&self) -> bool {
                true
            }
            fn is_transcript_flushed(&self) -> bool {
                false
            }
        }
        let journal =
            SessionPendingInputJournal::with_durability(Arc::new(Unflushed), Arc::new(NoSync));
        let record = serde_json::from_value::<PendingInputJournalRecord>(sample_record(1));
        let record = record.unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            journal.append(&record).await,
            Err(PendingInputError::NotDurable)
        );
    }

    #[tokio::test]
    async fn keeps_a_session_opened_without_persistence_in_memory() {
        let session = FileSession::in_memory();
        let store = store_over(session.clone(), "life-1").await;
        let _ = store.submit(submit_request()).await;
        assert!(session.session_file().is_none());
        assert!(admitted(&store, "c-1"));
    }

    #[tokio::test]
    async fn refuses_a_journal_with_a_missing_transition() {
        let dir = temp_dir();
        let session = FileSession::create(&dir);
        let _ = session.persist_header_now().await;
        session.append_custom_entry(PENDING_INPUT_TRANSITION, sample_record(1));
        session.append_custom_entry(PENDING_INPUT_TRANSITION, sample_record(3));
        let read = journal_over(session).read().await;
        assert_eq!(
            read.err(),
            Some(PendingInputError::JournalGap { missing_cursor: 2 })
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn tolerates_a_tail_a_crash_tore_and_keeps_every_committed_transition() {
        let dir = temp_dir();
        let session = FileSession::create(&dir);
        let _ = store_over(session.clone(), "life-1")
            .await
            .submit(submit_request())
            .await;
        append_raw(
            &session.file(),
            &format!(
                r#"{{"type":"custom","customType":"{PENDING_INPUT_TRANSITION}","data":{{"cursor":2,"inp"#
            ),
        );
        let records = journal_over(FileSession::open(&session.file()))
            .read()
            .await;
        assert_eq!(records.map(|records| records.len()), Ok(1));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn reads_back_a_batch_record_with_every_input_it_names() {
        let dir = temp_dir();
        let session = FileSession::create(&dir);
        let _ = store_over(session.clone(), "life-1")
            .await
            .submit(submit_request())
            .await;
        let journal = journal_over(session.clone());
        let mut first = journal
            .read()
            .await
            .ok()
            .and_then(|r| r.into_iter().next())
            .unwrap_or_else(|| panic!("record"));
        let mut other = first.input.clone();
        other.client_message_id = "c-2".into();
        first.cursor = 2;
        first.batch = Some(vec![other]);
        assert!(journal.append(&first).await.is_ok());
        let records = journal_over(FileSession::open(&session.file()))
            .read()
            .await
            .unwrap_or_default();
        let batch = records
            .last()
            .and_then(|r| r.batch.clone())
            .unwrap_or_default();
        assert_eq!(
            batch
                .iter()
                .map(|i| i.client_message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["c-2"]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn reads_a_persisted_sessions_journal_from_its_file_never_from_entries() {
        let dir = temp_dir();
        let session = FileSession::create(&dir);
        let _ = store_over(session.clone(), "life-1")
            .await
            .submit(submit_request())
            .await;
        // A reopened session with no resident entries: only the file can answer.
        let no_history = Arc::new(FileSession::new(
            Some(session.file()),
            true,
            true,
            Vec::new(),
        ));
        let records = journal_over(no_history).read().await.unwrap_or_default();
        assert_eq!(
            records
                .iter()
                .map(|r| r.input.client_message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["c-1"]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn reads_a_final_transition_written_whole_but_without_its_newline() {
        let dir = temp_dir();
        let session = FileSession::create(&dir);
        let _ = store_over(session.clone(), "life-1")
            .await
            .submit(submit_request())
            .await;
        let text = std::fs::read_to_string(session.file()).unwrap_or_default();
        assert!(std::fs::write(session.file(), text.trim_end_matches('\n')).is_ok());
        let records = journal_over(session).read().await.unwrap_or_default();
        assert_eq!(records.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn reads_the_same_records_from_the_file_as_from_the_sessions_entries() {
        let dir = temp_dir();
        let session = FileSession::create(&dir);
        let store = store_over(session.clone(), "life-1").await;
        let _ = store.submit(submit_request()).await;
        let _ = store.submit(second("c-2", "fp2")).await;
        let reopened = FileSession::open(&session.file());
        let from_entries = Arc::new(FileSession::new(None, false, false, reopened.entries()));
        let from_file = journal_over(reopened).read().await;
        assert_eq!(from_file, journal_over(from_entries).read().await);
        assert_eq!(from_file.map(|r| r.len()), Ok(2));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn refuses_a_corrupt_transition_in_the_middle_of_the_file() {
        let dir = temp_dir();
        let session = FileSession::create(&dir);
        let _ = store_over(session.clone(), "life-1")
            .await
            .submit(submit_request())
            .await;
        let later = serde_json::json!({"type": "custom", "customType": "other", "id": "x1"});
        append_raw(
            &session.file(),
            &format!(
                "{{\"type\":\"custom\",\"customType\":\"{PENDING_INPUT_TRANSITION}\",\"data\":{{\"cursor\":2,\"inp\n{later}\n"
            ),
        );
        let read = journal_over(FileSession::open(&session.file()))
            .read()
            .await;
        assert_eq!(
            read.err().map(|e| e.code()),
            Some("pending_input_journal_corrupt")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn refuses_a_corrupt_last_transition_that_was_written_completely() {
        let dir = temp_dir();
        let session = FileSession::create(&dir);
        let store = store_over(session.clone(), "life-1").await;
        let _ = store.submit(submit_request()).await;
        let _ = store.submit(second("c-2", "fp2")).await;
        let text = std::fs::read_to_string(session.file()).unwrap_or_default();
        let mut lines: Vec<String> = text
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect();
        if let Some(last) = lines.last_mut() {
            *last = format!(
                r#"{{"type":"custom","customType":"{PENDING_INPUT_TRANSITION}","data":{{"cursor":2,"inp#corrupt"#
            );
        }
        assert!(std::fs::write(session.file(), format!("{}\n", lines.join("\n"))).is_ok());
        let read = journal_over(session).read().await;
        assert_eq!(
            read.err().map(|e| e.code()),
            Some("pending_input_journal_corrupt")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn fails_a_write_whose_sync_failed_refuses_further_writes_and_stays_reopenable() {
        struct FailSecond(AtomicUsize);
        impl JournalDurability for FailSecond {
            fn sync_file(&self, _: &Path) -> io::Result<()> {
                if self.0.fetch_add(1, Ordering::SeqCst) + 1 == 2 {
                    return Err(io::Error::other("EIO"));
                }
                Ok(())
            }
            fn sync_directory(&self, _: &Path) -> io::Result<()> {
                Ok(())
            }
        }
        let dir = temp_dir();
        let session = FileSession::create(&dir);
        let journal = Arc::new(SessionPendingInputJournal::with_durability(
            session.clone(),
            Arc::new(FailSecond(AtomicUsize::new(0))),
        ));
        let store = PendingInputStore::open(options(journal, "life-1"))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(store.submit(submit_request()).await.is_ok());
        let failed = store.submit(second("c-2", "fp2")).await;
        assert_eq!(
            failed.err().map(|e| e.code()),
            Some("pending_input_append_uncertain")
        );
        let refused = store.submit(second("c-3", "fp3")).await;
        assert_eq!(
            refused.err().map(|e| e.code()),
            Some("pending_input_store_unavailable")
        );
        let reopened = store_over(FileSession::open(&session.file()), "life-2").await;
        assert!(admitted(&reopened, "c-1"));
        assert!(admitted(&reopened, "c-2"));
        assert!(!admitted(&reopened, "c-3"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn refuses_a_malformed_journal_entry_instead_of_treating_it_as_absence() {
        let dir = temp_dir();
        let session = FileSession::create(&dir);
        let _ = session.persist_header_now().await;
        session.append_custom_entry(
            PENDING_INPUT_TRANSITION,
            serde_json::json!({"type": PENDING_INPUT_TRANSITION, "cursor": 1}),
        );
        let read = journal_over(session).read().await;
        assert_eq!(
            read.err().map(|e| e.code()),
            Some("pending_input_journal_invalid")
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
