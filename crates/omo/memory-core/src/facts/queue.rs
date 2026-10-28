//! Durable facts queue (IC-6). Publication is `.tmp` + rename under the identity-scoped
//! `facts-queue` lock; the enqueue watermark is monotonic so a late-finishing older batch
//! can never republish a range overlapping a retained newer entry.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

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
    FactsQueueLayout, FactsQueueRange, facts_queue_paths, initial_cursor, parse_consumed,
    parse_cursor, parse_queue_entry, parse_queue_header,
};

const LOCK_WAIT_MS: u64 = 2000;

#[derive(Debug, Default)]
struct FactsAbortSignalInner {
    aborted: Arc<AtomicBool>,
    wakers: Mutex<Vec<Waker>>,
}

/// Abort signal for cancelling facts queue operations.
#[derive(Debug, Clone)]
pub struct FactsAbortSignal {
    inner: Arc<FactsAbortSignalInner>,
}

impl Default for FactsAbortSignal {
    fn default() -> Self {
        Self::new()
    }
}

/// Future that resolves when a [`FactsAbortSignal`] is aborted.
#[derive(Debug, Clone)]
pub struct FactsAbortedFuture {
    inner: Arc<FactsAbortSignalInner>,
}

impl Future for FactsAbortedFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.inner.aborted.load(Ordering::SeqCst) {
            return Poll::Ready(());
        }

        let mut wakers = self
            .inner
            .wakers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.inner.aborted.load(Ordering::SeqCst) {
            return Poll::Ready(());
        }

        let waker = cx.waker();
        if !wakers.iter().any(|w| w.will_wake(waker)) {
            wakers.push(waker.clone());
        }

        Poll::Pending
    }
}

impl FactsAbortSignal {
    /// Create a new un-aborted signal.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(FactsAbortSignalInner {
                aborted: Arc::new(AtomicBool::new(false)),
                wakers: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Mark the signal as aborted and wake all registered wakers.
    pub fn abort(&self) {
        self.inner.aborted.store(true, Ordering::SeqCst);
        let wakers = {
            let mut guard = self
                .inner
                .wakers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut *guard)
        };
        for waker in wakers {
            waker.wake();
        }
    }

    /// Check whether the signal has been aborted.
    pub fn is_aborted(&self) -> bool {
        self.inner.aborted.load(Ordering::SeqCst)
    }

    /// Access the underlying atomic flag for shared cancellation state.
    pub fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.inner.aborted)
    }

    /// Returns a future that resolves when the signal is marked as aborted.
    pub fn aborted(&self) -> FactsAbortedFuture {
        FactsAbortedFuture {
            inner: Arc::clone(&self.inner),
        }
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
    Enqueued { entry: Box<FactsQueueEntry> },
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

/// One retained queue file listed without its transcript payload.
#[derive(Debug, Clone)]
pub struct FactsPendingFile {
    /// Queue entry metadata; `entries` is always empty.
    pub header: FactsQueueEntry,
    file_path: PathBuf,
}

impl AsRef<FactsQueueEntry> for FactsPendingFile {
    fn as_ref(&self) -> &FactsQueueEntry {
        &self.header
    }
}

impl FactsPendingFile {
    /// Read the full entry back from disk; `None` once the file was consumed or became invalid.
    pub fn load(&self) -> Option<FactsQueueEntry> {
        let raw = fs::read_to_string(&self.file_path).ok()?;
        parse_queue_entry(&raw).filter(|entry| entry_key(entry) == entry_key(&self.header))
    }
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
                lead: super::queue_lead::lead_of(&request.entries, start_index),
            };

            if aborted_now() {
                return Ok(FactsEnqueueResult::NotEnqueued {
                    reason: "no-new-entries".to_string(),
                });
            }

            self.publish(&entry, &at_iso, request.signal.as_ref())?;

            if aborted_now() {
                return Ok(FactsEnqueueResult::Enqueued { entry: Box::new(entry) });
            }

            self.advance_enqueued(
                &request.conversation_id,
                &entry.range,
                request.signal.as_ref(),
            )?;
            Ok(FactsEnqueueResult::Enqueued { entry: Box::new(entry) })
        })
    }

    /// Every retained queue file, oldest first.
    pub fn list_pending(&self) -> Result<Vec<FactsQueueEntry>, FactsQueueError> {
        self.locked(|| {
            let pending = self.list_pending_unlocked(true)?;
            Ok(pending.into_iter().map(|item| item.entry).collect())
        })
    }

    /// Every retained queue file, oldest first, without loading transcript payloads into memory.
    pub fn list_pending_files(&self) -> Result<Vec<FactsPendingFile>, FactsQueueError> {
        self.locked(|| {
            let pending = self.list_pending_unlocked(false)?;
            Ok(pending
                .into_iter()
                .map(|item| FactsPendingFile {
                    header: item.entry,
                    file_path: item.file_path,
                })
                .collect())
        })
    }

    /// Claim pending entries for one worker, reclaiming claims older than fifteen minutes.
    pub fn claim(
        &self,
        entries: &[FactsQueueEntry],
        claim_id: &str,
    ) -> Result<Vec<FactsQueueEntry>, FactsQueueError> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        self.locked(|| {
            let mut claims = self.read_claims_unlocked()?;
            let reclaim_before = (self.now)() - 15 * 60_000;
            let mut selected = HashSet::new();
            let mut claimed = Vec::new();
            for entry in entries {
                let key = entry_key(entry);
                if selected.contains(&key)
                    || claims
                        .get(&key)
                        .is_some_and(|claim| claim.claimed_at >= reclaim_before)
                {
                    continue;
                }
                let file_path = self.layout.entry_path(
                    &entry.conversation_id,
                    &entry.range.end_message_id,
                    &entry.enqueued_at,
                );
                let Some(current) = fs::read_to_string(&file_path)
                    .ok()
                    .and_then(|raw| parse_queue_entry(&raw))
                else {
                    continue;
                };
                if entry_key(&current) != key {
                    continue;
                }
                selected.insert(key);
                claimed.push(PendingEntryWithFile {
                    entry: current,
                    file_path,
                });
            }
            for pending in &claimed {
                let claimed_at_millis = (self.now)();
                claims.insert(
                    entry_key(&pending.entry),
                    PersistedClaim {
                        claim_id: claim_id.to_string(),
                        claimed_at: claimed_at_millis,
                        claimed_at_iso: format_rfc3339_millis(claimed_at_millis),
                    },
                );
            }
            self.write_claims(&claims)?;
            Ok(claimed.into_iter().map(|pending| pending.entry).collect())
        })
    }

    /// Release only claims still owned by `claim_id`.
    pub fn release_claim(
        &self,
        entries: &[FactsQueueEntry],
        claim_id: &str,
    ) -> Result<(), FactsQueueError> {
        if entries.is_empty() {
            return Ok(());
        }
        self.locked(|| {
            let mut claims = self.read_claims_unlocked()?;
            for entry in entries {
                let key = entry_key(entry);
                if claims
                    .get(&key)
                    .is_some_and(|claim| claim.claim_id == claim_id)
                {
                    claims.remove(&key);
                }
            }
            self.write_claims(&claims)
        })
    }

    /// Terminal success only: deletes the batch's files and advances the consumed watermark.
    pub fn mark_consumed(&self, entries: &[FactsQueueEntry]) -> Result<(), FactsQueueError> {
        if entries.is_empty() {
            return Ok(());
        }
        self.locked(|| {
            let pending = self.list_pending_unlocked(false)?;
            let mut consumed_keys = HashSet::new();
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
            let consumed_keys: HashSet<String> = entries.iter().map(entry_key).collect();
            let mut claims = self.read_claims_unlocked()?;
            let claims_before = claims.len();
            claims.retain(|key, _| !consumed_keys.contains(key));
            // PERF-DIVERGENCE(senpi facts queue): upstream rewrites claims.json even when no claim was removed; this port skips the write/rename when nothing changed and the file already exists. File content is identical.
            if claims.len() != claims_before || !self.layout.queue_dir.join("claims.json").exists()
            {
                self.write_claims(&claims)?;
            }
            self.advance_consumed(entries)?;
            Ok(())
        })
    }

    /// Read cursor for a given conversation.
    pub fn read_cursor(&self, conversation_id: &str) -> Result<FactsCursor, FactsQueueError> {
        self.locked(|| self.read_cursor_unlocked(conversation_id))
    }

    /// Seed the enqueued_through watermark for an uninitialized conversation (e.g. forked session prefix).
    /// Under the queue lock; only when no cursor exists; never lowers an existing cursor;
    /// pending ranges are left untouched.
    pub fn seed_enqueued_through(
        &self,
        conversation_id: &str,
        end_message_id: &str,
        end_snapshot_line: u64,
    ) -> Result<(), FactsQueueError> {
        self.locked(|| {
            let cursor = self.read_cursor_unlocked(conversation_id)?;
            let has_cursor = cursor.enqueued_through_snapshot_line >= 0
                || cursor.enqueued_through_message_id.is_some();
            if has_cursor {
                return Ok(());
            }
            if (end_snapshot_line as i64) <= cursor.enqueued_through_snapshot_line {
                return Ok(());
            }
            let updated = FactsCursor {
                enqueued_through_message_id: Some(end_message_id.to_string()),
                enqueued_through_snapshot_line: end_snapshot_line as i64,
                ..cursor
            };
            self.write_cursor(conversation_id, &updated)?;
            Ok(())
        })
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
        with_lock(&self.lock_path, &record, &options, || {
            super::parked::migrate_parked_entries(&self.layout)?;
            task()
        })
        .map_err(FactsQueueError::from)
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
        let mut retained =
            super::queue_scan::scan_queue_entries(&self.layout.queue_dir, Some(conversation_id))?;
        retained.extend(super::queue_scan::scan_queue_entries(
            &self.layout.parked_dir,
            Some(conversation_id),
        )?);
        for pending in retained {
            if pending.entry.conversation_id == conversation_id {
                candidates.push(Some(pending.entry.range.end_message_id));
            }
        }

        // PERF-DIVERGENCE(senpi facts queue): upstream rescans the transcript from the end once per anchor candidate; this port indexes the last canonical position per message id once. Positions are identical.
        let mut last_canonical: HashMap<&str, i64> = HashMap::new();
        for (index, entry) in entries.iter().enumerate() {
            if is_canonical_entry(entry) {
                last_canonical.insert(entry.source_message_id(), index as i64);
            }
        }
        let mut anchor: i64 = -1;
        for candidate in candidates {
            let position = candidate
                .as_deref()
                .and_then(|id| last_canonical.get(id).copied())
                .unwrap_or(-1);
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
        if let Some(hook) = &self.on_publish {
            hook();
        }
        Ok(())
    }

    /// Lists valid queue files; without `with_entries` the header and transcript entries are
    /// streamed via a visitor so transcripts are not materialized into memory.
    fn list_pending_unlocked(
        &self,
        with_entries: bool,
    ) -> Result<Vec<PendingEntryWithFile>, FactsQueueError> {
        let read_dir = match fs::read_dir(&self.layout.queue_dir) {
            Ok(dir) => dir,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut names = Vec::new();
        for dir_entry in read_dir {
            let dir_entry = dir_entry?;
            let file_name = dir_entry.file_name().to_string_lossy().into_owned();
            names.push(file_name);
        }
        names.sort();

        let mut entries = Vec::new();
        for name in names {
            if !name.ends_with(".json")
                || name == "consumed.json"
                || name == "failures.json"
                || name == "claims.json"
            {
                continue;
            }
            let file_path = self.layout.queue_dir.join(&name);
            let parsed = if with_entries {
                let raw = match fs::read_to_string(&file_path) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                match parse_queue_entry(&raw) {
                    Some(parsed) => parsed,
                    None => continue,
                }
            } else {
                let file = match fs::File::open(&file_path) {
                    Ok(f) => f,
                    Err(_) => continue,
                };
                let reader = io::BufReader::new(file);
                match parse_queue_header(reader) {
                    Some(parsed) => parsed,
                    None => continue,
                }
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

    fn read_claims_unlocked(&self) -> Result<BTreeMap<String, PersistedClaim>, FactsQueueError> {
        let path = self.layout.queue_dir.join("claims.json");
        let raw = match fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(err) => return Err(FactsQueueError::Io(err)),
        };
        let Ok(values) = serde_json::from_str::<BTreeMap<String, serde_json::Value>>(&raw) else {
            return Ok(BTreeMap::new());
        };
        Ok(values
            .into_iter()
            .filter_map(|(key, value)| {
                let claim_id = value.get("claimId")?.as_str()?;
                let claimed_at_iso = value.get("claimedAt")?.as_str()?;
                let claimed_at = crate::support::time::parse_rfc3339(claimed_at_iso)?;
                (!claim_id.is_empty()).then(|| {
                    (
                        key,
                        PersistedClaim {
                            claim_id: claim_id.to_string(),
                            claimed_at,
                            claimed_at_iso: claimed_at_iso.to_string(),
                        },
                    )
                })
            })
            .collect())
    }

    fn write_claims(
        &self,
        claims: &BTreeMap<String, PersistedClaim>,
    ) -> Result<(), FactsQueueError> {
        let payload: BTreeMap<_, _> = claims
            .iter()
            .map(|(key, claim)| {
                (
                    key,
                    serde_json::json!({"claimId": claim.claim_id, "claimedAt": claim.claimed_at_iso}),
                )
            })
            .collect();
        let target = self.layout.queue_dir.join("claims.json");
        let temporary = PathBuf::from(format!("{}.tmp", target.display()));
        let mut json = serde_json::to_string_pretty(&payload)?;
        json.push('\n');
        write_file_0600(&temporary, json.as_bytes())?;
        fs::rename(temporary, target)?;
        Ok(())
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
        // PERF-DIVERGENCE(senpi facts queue): upstream re-reads and rewrites the cursor file once per entry; this port keeps each conversation's cursor in memory and writes it once after the loop. The final file content is identical because the last write wins.
        let mut cursors: HashMap<String, FactsCursor> = HashMap::new();
        let mut dirty: Vec<String> = Vec::new();

        for entry in entries {
            let cursor = match cursors.remove(&entry.conversation_id) {
                Some(cursor) => cursor,
                None => self.read_cursor_unlocked(&entry.conversation_id)?,
            };
            if (entry.range.end_snapshot_line as i64) <= cursor.consumed_through_snapshot_line {
                cursors.insert(entry.conversation_id.clone(), cursor);
                continue;
            }
            let updated = FactsCursor {
                consumed_through_message_id: Some(entry.range.end_message_id.clone()),
                consumed_through_snapshot_line: entry.range.end_snapshot_line as i64,
                ..cursor
            };
            if !dirty.contains(&entry.conversation_id) {
                dirty.push(entry.conversation_id.clone());
            }
            cursors.insert(entry.conversation_id.clone(), updated);
            next.insert(
                entry.conversation_id.clone(),
                FactsConsumedRecord {
                    end_message_id: entry.range.end_message_id.clone(),
                    end_snapshot_line: entry.range.end_snapshot_line as i64,
                    consumed_at: consumed_at.clone(),
                },
            );
        }
        for conversation_id in &dirty {
            if let Some(cursor) = cursors.get(conversation_id) {
                self.write_cursor(conversation_id, cursor)?;
            }
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
        Ok(())
    }
}

#[derive(Debug)]
struct PersistedClaim {
    claim_id: String,
    claimed_at: i64,
    claimed_at_iso: String,
}

fn entry_key(entry: &FactsQueueEntry) -> String {
    format!("{}\0{}", entry.conversation_id, entry.range.end_message_id)
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
    Ok(())
}

#[cfg(test)]
#[path = "queue_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "queue_extra_tests.rs"]
mod extra_tests;
