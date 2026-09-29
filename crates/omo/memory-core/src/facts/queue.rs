//! Durable facts queue (IC-6). Publication is `.tmp` + rename under the identity-scoped
//! `facts-queue` lock; the enqueue watermark is monotonic so a late-finishing older batch
//! can never republish a range overlapping a retained newer entry.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::identity::MemoryIdentityPaths;
use crate::journal::cursor::is_canonical_entry;
use crate::journal::entries::TranscriptEntry;
use crate::locks::{
    AcquireLockError, AcquireLockOptions, CreateLockRecordOptions, WithLockError,
    create_lock_record, facts_queue_lock_path, with_lock,
};
use crate::support::time::{format_rfc3339_millis, now_millis};

use super::schema::{
    FACTS_QUEUE_VERSION, FactsConsumedRecord, FactsConsumedWatermark, FactsCursor, FactsQueueEntry,
    FactsQueueLayout, FactsQueueRange, canonical_position, facts_queue_paths, initial_cursor,
    parse_consumed, parse_cursor, parse_queue_entry,
};

const LOCK_WAIT_MS: u64 = 2000;

/// Abort signal for cancelling facts queue operations.
#[derive(Debug, Clone, Default)]
pub struct FactsAbortSignal {
    aborted: Arc<AtomicBool>,
}

impl FactsAbortSignal {
    /// Create a new un-aborted signal.
    pub fn new() -> Self {
        Self {
            aborted: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Mark the signal as aborted.
    pub fn abort(&self) {
        self.aborted.store(true, Ordering::SeqCst);
    }

    /// Check whether the signal has been aborted.
    pub fn is_aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }
}

/// Request payload for enqueuing journal entries into the facts queue.
#[derive(Debug, Clone)]
pub struct FactsEnqueueRequest {
    pub identity: String,
    pub session_id: String,
    pub conversation_id: String,
    pub entries: Vec<TranscriptEntry>,
    pub signal: Option<FactsAbortSignal>,
}

/// Result of an enqueue attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "enqueued")]
pub enum FactsEnqueueResult {
    #[serde(rename = "true")]
    Enqueued { entry: FactsQueueEntry },
    #[serde(rename = "false")]
    NotEnqueued { reason: String },
}

impl FactsEnqueueResult {
    /// Returns true if entries were enqueued.
    pub fn is_enqueued(&self) -> bool {
        matches!(self, Self::Enqueued { .. })
    }

    /// The newly enqueued entry if enqueued.
    pub fn entry(&self) -> Option<&FactsQueueEntry> {
        match self {
            Self::Enqueued { entry } => Some(entry),
            Self::NotEnqueued { .. } => None,
        }
    }

    /// The failure reason if not enqueued.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Enqueued { .. } => None,
            Self::NotEnqueued { reason } => Some(reason.as_str()),
        }
    }
}

/// Options for configuring a facts queue instance.
pub struct FactsQueueOptions {
    pub identity_paths: MemoryIdentityPaths,
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    /// Test seam invoked after a queue entry is durably published.
    pub on_publish: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// Errors returned by facts queue operations.
#[derive(Debug)]
pub enum FactsQueueError {
    Io(io::Error),
    Json(serde_json::Error),
    Lock(AcquireLockError),
    Corrupt(String),
}

impl std::fmt::Display for FactsQueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "facts queue io error: {err}"),
            Self::Json(err) => write!(f, "facts queue json error: {err}"),
            Self::Lock(err) => write!(f, "facts queue lock error: {err}"),
            Self::Corrupt(msg) => write!(f, "facts queue corrupt: {msg}"),
        }
    }
}

impl std::error::Error for FactsQueueError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Json(err) => Some(err),
            Self::Lock(err) => Some(err),
            Self::Corrupt(_) => None,
        }
    }
}

impl From<io::Error> for FactsQueueError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<serde_json::Error> for FactsQueueError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

impl From<AcquireLockError> for FactsQueueError {
    fn from(err: AcquireLockError) -> Self {
        Self::Lock(err)
    }
}

impl From<WithLockError<FactsQueueError>> for FactsQueueError {
    fn from(err: WithLockError<Self>) -> Self {
        match err {
            WithLockError::Acquire(err) => Self::Lock(err),
            WithLockError::User(err) => err,
        }
    }
}

struct PendingEntryWithFile {
    entry: FactsQueueEntry,
    file_path: PathBuf,
}

/// Durable facts queue with monotonic watermarks and atomic publishing.
pub struct FactsQueue {
    layout: FactsQueueLayout,
    lock_path: PathBuf,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    on_publish: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl FactsQueue {
    /// Create a new facts queue instance.
    pub fn new(options: FactsQueueOptions) -> Self {
        let layout = facts_queue_paths(&options.identity_paths);
        let lock_path = facts_queue_lock_path(&options.identity_paths.locks);
        let now = options.now.unwrap_or_else(|| Arc::new(now_millis));
        Self {
            layout,
            lock_path,
            now,
            on_publish: options.on_publish,
        }
    }

    /// Publishes the delta strictly after the effective enqueue anchor.
    pub fn enqueue(
        &self,
        request: FactsEnqueueRequest,
    ) -> Result<FactsEnqueueResult, FactsQueueError> {
        let aborted_now = || request.signal.as_ref().is_some_and(|s| s.is_aborted());
        if aborted_now() {
            return Ok(FactsEnqueueResult::NotEnqueued {
                reason: "no-new-entries".to_string(),
            });
        }
        self.locked(|| {
            let anchor = self.effective_anchor(&request.conversation_id, &request.entries)?;
            let start_index = request
                .entries
                .iter()
                .enumerate()
                .position(|(idx, entry)| (idx as i64) > anchor && is_canonical_entry(entry));

            let Some(start_index) = start_index else {
                return Ok(FactsEnqueueResult::NotEnqueued {
                    reason: "no-new-entries".to_string(),
                });
            };

            let mut end_index = None;
            for idx in (start_index..request.entries.len()).rev() {
                if let Some(entry) = request.entries.get(idx)
                    && is_canonical_entry(entry)
                {
                    end_index = Some(idx);
                    break;
                }
            }

            let Some(end_index) = end_index else {
                return Ok(FactsEnqueueResult::NotEnqueued {
                    reason: "no-new-entries".to_string(),
                });
            };

            let start = &request.entries[start_index];
            let end = &request.entries[end_index];
            let at_millis = (self.now)();
            let at_iso = format_rfc3339_millis(at_millis);

            let entry = FactsQueueEntry {
                version: FACTS_QUEUE_VERSION,
                identity: request.identity,
                session_id: request.session_id,
                conversation_id: request.conversation_id.clone(),
                range: FactsQueueRange {
                    start_message_id: start.source_message_id().to_string(),
                    end_message_id: end.source_message_id().to_string(),
                    start_line: start_index as u64,
                    end_snapshot_line: request.entries.len() as u64,
                },
                enqueued_at: at_iso.clone(),
                entries: request.entries[start_index..].to_vec(),
            };

            if aborted_now() {
                return Ok(FactsEnqueueResult::NotEnqueued {
                    reason: "no-new-entries".to_string(),
                });
            }

            self.publish(&entry, &at_iso, request.signal.as_ref())?;

            if aborted_now() {
                return Ok(FactsEnqueueResult::Enqueued { entry });
            }

            self.advance_enqueued(
                &request.conversation_id,
                &entry.range,
                request.signal.as_ref(),
            )?;
            Ok(FactsEnqueueResult::Enqueued { entry })
        })
    }

    /// Every retained queue file, oldest first.
    pub fn list_pending(&self) -> Result<Vec<FactsQueueEntry>, FactsQueueError> {
        self.locked(|| {
            let pending = self.list_pending_unlocked()?;
            Ok(pending.into_iter().map(|item| item.entry).collect())
        })
    }

    /// Terminal success only: deletes the batch's files and advances the consumed watermark.
    pub fn mark_consumed(&self, entries: &[FactsQueueEntry]) -> Result<(), FactsQueueError> {
        if entries.is_empty() {
            return Ok(());
        }
        self.locked(|| {
            let pending = self.list_pending_unlocked()?;
            let mut consumed_keys = std::collections::HashSet::new();
            for entry in entries {
                consumed_keys.insert(format!(
                    "{}\0{}",
                    entry.conversation_id, entry.range.end_message_id
                ));
            }
            for candidate in pending {
                let key = format!(
                    "{}\0{}",
                    candidate.entry.conversation_id, candidate.entry.range.end_message_id
                );
                if consumed_keys.contains(&key) {
                    let _ = fs::remove_file(&candidate.file_path);
                }
            }
            self.advance_consumed(entries)?;
            Ok(())
        })
    }

    /// Read cursor for a given conversation.
    pub fn read_cursor(&self, conversation_id: &str) -> Result<FactsCursor, FactsQueueError> {
        self.locked(|| self.read_cursor_unlocked(conversation_id))
    }

    fn locked<T, F>(&self, task: F) -> Result<T, FactsQueueError>
    where
        F: FnOnce() -> Result<T, FactsQueueError>,
    {
        fs::create_dir_all(&self.layout.queue_dir)?;
        if let Some(parent) = self.lock_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let record = create_lock_record("facts-queue", CreateLockRecordOptions::default())
            .map_err(|err| FactsQueueError::Io(io::Error::other(format!("{err:?}"))))?;
        let options = AcquireLockOptions {
            wait_timeout_ms: Some(LOCK_WAIT_MS),
            ..Default::default()
        };
        with_lock(&self.lock_path, &record, &options, task).map_err(FactsQueueError::from)
    }

    fn effective_anchor(
        &self,
        conversation_id: &str,
        entries: &[TranscriptEntry],
    ) -> Result<i64, FactsQueueError> {
        let cursor = self.read_cursor_unlocked(conversation_id)?;
        let consumed = self.read_consumed_unlocked()?;
        let mut candidates: Vec<Option<String>> = vec![
            cursor.enqueued_through_message_id,
            cursor.consumed_through_message_id,
            consumed
                .consumed
                .get(conversation_id)
                .map(|rec| rec.end_message_id.clone()),
        ];
        for pending in self.list_pending_unlocked()? {
            if pending.entry.conversation_id == conversation_id {
                candidates.push(Some(pending.entry.range.end_message_id));
            }
        }

        let mut anchor: i64 = -1;
        for candidate in candidates {
            let position = canonical_position(entries, candidate.as_deref());
            if position >= entries.len() as i64 {
                continue;
            }
            if position > anchor {
                anchor = position;
            }
        }
        Ok(anchor)
    }

    fn publish(
        &self,
        entry: &FactsQueueEntry,
        at_iso: &str,
        signal: Option<&FactsAbortSignal>,
    ) -> Result<(), FactsQueueError> {
        let target =
            self.layout
                .entry_path(&entry.conversation_id, &entry.range.end_message_id, at_iso);
        let temporary = PathBuf::from(format!("{}.tmp", target.display()));
        let mut json = serde_json::to_string_pretty(entry)?;
        json.push('\n');
        write_file_0600(&temporary, json.as_bytes())?;
        if signal.is_some_and(FactsAbortSignal::is_aborted) {
            let _ = fs::remove_file(&temporary);
            return Ok(());
        }
        fs::rename(&temporary, &target)?;
        if let Some(parent) = target.parent() {
            let _ = sync_dir(parent);
        }
        if let Some(hook) = &self.on_publish {
            hook();
        }
        Ok(())
    }

    fn list_pending_unlocked(&self) -> Result<Vec<PendingEntryWithFile>, FactsQueueError> {
        let read_dir = match fs::read_dir(&self.layout.queue_dir) {
            Ok(dir) => dir,
            Err(_) => return Ok(Vec::new()),
        };
        let mut names = Vec::new();
        for dir_entry in read_dir {
            let dir_entry = match dir_entry {
                Ok(de) => de,
                Err(_) => continue,
            };
            let file_name = dir_entry.file_name().to_string_lossy().into_owned();
            names.push(file_name);
        }
        names.sort();

        let mut entries = Vec::new();
        for name in names {
            if !name.ends_with(".json") || name == "consumed.json" || name == "failures.json" {
                continue;
            }
            let file_path = self.layout.queue_dir.join(&name);
            let raw = match fs::read_to_string(&file_path) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let Some(parsed) = parse_queue_entry(&raw) else {
                continue;
            };
            entries.push(PendingEntryWithFile {
                entry: parsed,
                file_path,
            });
        }
        Ok(entries)
    }

    fn read_cursor_unlocked(&self, conversation_id: &str) -> Result<FactsCursor, FactsQueueError> {
        let path = self.layout.cursor_path(conversation_id);
        match fs::read_to_string(&path) {
            Ok(raw) => Ok(parse_cursor(&raw)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(initial_cursor()),
            Err(err) => Err(FactsQueueError::Io(err)),
        }
    }

    fn read_consumed_unlocked(&self) -> Result<FactsConsumedWatermark, FactsQueueError> {
        match fs::read_to_string(&self.layout.consumed_path) {
            Ok(raw) => Ok(parse_consumed(&raw)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(FactsConsumedWatermark {
                version: FACTS_QUEUE_VERSION,
                consumed: BTreeMap::new(),
            }),
            Err(err) => Err(FactsQueueError::Io(err)),
        }
    }

    fn advance_enqueued(
        &self,
        conversation_id: &str,
        range: &FactsQueueRange,
        signal: Option<&FactsAbortSignal>,
    ) -> Result<(), FactsQueueError> {
        let cursor = self.read_cursor_unlocked(conversation_id)?;
        if (range.end_snapshot_line as i64) <= cursor.enqueued_through_snapshot_line {
            return Ok(());
        }
        if signal.is_some_and(|s| s.is_aborted()) {
            return Ok(());
        }
        let updated = FactsCursor {
            enqueued_through_message_id: Some(range.end_message_id.clone()),
            enqueued_through_snapshot_line: range.end_snapshot_line as i64,
            ..cursor
        };
        self.write_cursor(conversation_id, &updated)?;
        Ok(())
    }

    fn advance_consumed(&self, entries: &[FactsQueueEntry]) -> Result<(), FactsQueueError> {
        let consumed = self.read_consumed_unlocked()?;
        let mut next = consumed.consumed;
        let consumed_at = format_rfc3339_millis((self.now)());

        for entry in entries {
            let cursor = self.read_cursor_unlocked(&entry.conversation_id)?;
            if (entry.range.end_snapshot_line as i64) <= cursor.consumed_through_snapshot_line {
                continue;
            }
            let updated = FactsCursor {
                consumed_through_message_id: Some(entry.range.end_message_id.clone()),
                consumed_through_snapshot_line: entry.range.end_snapshot_line as i64,
                ..cursor
            };
            self.write_cursor(&entry.conversation_id, &updated)?;
            next.insert(
                entry.conversation_id.clone(),
                FactsConsumedRecord {
                    end_message_id: entry.range.end_message_id.clone(),
                    end_snapshot_line: entry.range.end_snapshot_line as i64,
                    consumed_at: consumed_at.clone(),
                },
            );
        }

        let payload = FactsConsumedWatermark {
            version: FACTS_QUEUE_VERSION,
            consumed: next,
        };
        let temporary = PathBuf::from(format!("{}.tmp", self.layout.consumed_path.display()));
        let mut json = serde_json::to_string_pretty(&payload)?;
        json.push('\n');
        write_file_0600(&temporary, json.as_bytes())?;
        fs::rename(&temporary, &self.layout.consumed_path)?;
        if let Some(parent) = self.layout.consumed_path.parent() {
            let _ = sync_dir(parent);
        }
        Ok(())
    }

    fn write_cursor(
        &self,
        conversation_id: &str,
        cursor: &FactsCursor,
    ) -> Result<(), FactsQueueError> {
        fs::create_dir_all(&self.layout.cursor_dir)?;
        let target = self.layout.cursor_path(conversation_id);
        let temporary = PathBuf::from(format!("{}.tmp", target.display()));
        let mut json = serde_json::to_string_pretty(cursor)?;
        json.push('\n');
        write_file_0600(&temporary, json.as_bytes())?;
        fs::rename(&temporary, &target)?;
        if let Some(parent) = target.parent() {
            let _ = sync_dir(parent);
        }
        Ok(())
    }
}

fn write_file_0600(path: &Path, content: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    io::Write::write_all(&mut file, content)?;
    file.sync_all()?;
    Ok(())
}

fn sync_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        if let Ok(file) = fs::File::open(path) {
            let _ = file.sync_all();
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "queue_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "queue_extra_tests.rs"]
mod extra_tests;
