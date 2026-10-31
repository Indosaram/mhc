//! senpi `modes/rpc/compact-content-store.ts`: owned bounded leaves for compact current state
//! (senpi#4272, R3.C6.4 / R3.C2.4).
//!
//! Acceptance never waits here: a leaf is created synchronously in RAM, and a spill only relocates
//! that immutable value. Its RAM copy is released only after the whole file was written and closed;
//! a failed write keeps the bytes resident, halts further spilling and is reported through `status()`,
//! never dropped. All storage work runs as one serial asynchronous job chain.
//!
//! Rust shape: upstream's class hierarchy (`CompactSpoolStorage` <- `CompactSegmentStorage` <-
//! `CompactContentStore`, plus the `text` roots member) is one cloneable handle over one locked
//! `StoreState`; the spool, segment and text-root modules add `impl` blocks to it. Every synchronous
//! upstream method runs inside one `with` critical section, which then delivers commit notifications and
//! starts the job pump outside the lock.
//!
//! PERF-DIVERGENCE(senpi UTF-16 leaves): upstream leaves are JS strings measured in UTF-16 code units and
//! spilled as UTF-16LE. Rust leaves are `str`; sizes are still counted in UTF-16 code units and files are
//! still UTF-16LE, but a split never cuts a surrogate pair (a leaf may end one unit short instead).
use crate::compact_read_lease::{CompactError, CompactReadReason, lock};
use crate::compact_spool_storage::{RECLAIM_WORK_ALLOWANCE_BYTES, SpoolState, job};
use crate::compact_storage_io::{
    CompactStorageFailure, CompactStorageIo, CompactStorageOperation, node_compact_storage_io,
};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

/// 32,768 UTF-16 code units (64 KiB as UTF-16LE), so a leaf never exceeds one bounded read.
pub const COMPACT_LEAF_CODE_UNITS: usize = 32_768;

/// Positive ids are leaves, negative ids are text-root nodes, zero is empty.
pub type LeafId = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactWriteReason {
    SizeLimit,
    StorageUnavailable,
    DepthLimit,
    CountLimit,
    Cycle,
    UnsupportedValue,
}

impl CompactWriteReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SizeLimit => "size_limit",
            Self::StorageUnavailable => "storage_unavailable",
            Self::DepthLimit => "depth_limit",
            Self::CountLimit => "count_limit",
            Self::Cycle => "cycle",
            Self::UnsupportedValue => "unsupported_value",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("compact write refused ({})", .reason.as_str())]
pub struct CompactWriteError {
    pub reason: CompactWriteReason,
}

impl CompactWriteError {
    pub fn new(reason: CompactWriteReason) -> Self {
        Self { reason }
    }
}

impl From<CompactWriteReason> for CompactError {
    fn from(reason: CompactWriteReason) -> Self {
        Self::Write(CompactWriteError::new(reason))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StructuredBytes {
    pub resident: i64,
    pub spilled: i64,
    pub pinned: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactStorageStatus {
    pub structured_bytes: StructuredBytes,
    pub resident_payload_bytes: i64,
    pub pending_io_copy_bytes: i64,
    pub pinned_only_bytes: i64,
    pub spilled_live_bytes: i64,
    pub spill_bytes_written: i64,
    pub reclaim_debt_bytes: i64,
    /// Resident bytes above the spill target that a halted spool keeps in RAM (ruling 08 condition 1).
    pub spill_failure_retained_bytes: i64,
    pub spill_halted: bool,
    pub failures: Vec<CompactStorageFailure>,
}

#[derive(Clone, Default)]
pub struct CompactContentStoreOptions {
    /// Private epoch spool directory the store creates itself (0700, never an existing one; files 0600).
    /// Not a transcript.
    pub dir: String,
    pub io: Option<Arc<dyn CompactStorageIo>>,
    /// Spilling starts above this many resident leaf bytes and stops at the target.
    pub spill_above_bytes: Option<i64>,
    pub spill_target_bytes: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LeafState {
    Ram,
    Pending,
    Committed,
}

pub(crate) struct Leaf {
    pub(crate) value: bool,
    pub(crate) text: Option<Arc<str>>,
    pub(crate) units: i64,
    pub(crate) live: i64,
    pub(crate) pins: i64,
    pub(crate) state: LeafState,
    pub(crate) slot: Option<u64>,
    pub(crate) file: Option<(String, i64)>,
}

pub(crate) struct StoreState {
    pub(crate) spool: SpoolState,
    pub(crate) segments: crate::compact_segment_storage::SegmentState,
    pub(crate) text: crate::compact_text_roots::TextRootsState,
    pub(crate) leaves: HashMap<LeafId, Leaf>,
    /// Resident leaves in creation (id) order, so each spill takes the oldest without rescanning.
    ram_leaves: BTreeSet<LeafId>,
    ram_values: BTreeSet<LeafId>,
    value_queued: bool,
    structured: StructuredBytes,
    next_leaf_id: LeafId,
    resident_payload_bytes: i64,
    pinned_only_bytes: i64,
    spilled_live_bytes: i64,
    spill_above_bytes: i64,
    spill_target_bytes: i64,
    /// Ids committed inside the current critical section; subscribers hear them after it.
    committed_queue: Vec<LeafId>,
}

type CommitListener = Arc<dyn Fn(LeafId) + Send + Sync>;

pub(crate) struct StoreInner {
    pub(crate) dir: String,
    pub(crate) io: Arc<dyn CompactStorageIo>,
    pub(crate) dir_ready: tokio::sync::OnceCell<bool>,
    pub(crate) changed: tokio::sync::Notify,
    state: Mutex<StoreState>,
    subscribers: Mutex<Vec<CommitListener>>,
}

/// One store handle; clones share the same leaves, spool and job chain.
#[derive(Clone)]
pub struct CompactContentStore(pub(crate) Arc<StoreInner>);

pub(crate) fn utf16_len(text: &str) -> i64 {
    i64::try_from(text.encode_utf16().count()).unwrap_or(i64::MAX)
}

/// Byte index of the largest char boundary at or below `units` UTF-16 code units.
pub(crate) fn utf16_boundary(text: &str, units: usize) -> usize {
    let mut counted = 0;
    for (index, ch) in text.char_indices() {
        if counted + ch.len_utf16() > units {
            return index;
        }
        counted += ch.len_utf16();
    }
    text.len()
}

/// `text.slice(start, end)` in UTF-16 code units, clamped to char boundaries.
pub(crate) fn utf16_slice(text: &str, start: usize, end: usize) -> &str {
    let from = utf16_boundary(text, start);
    let to = utf16_boundary(text, end.max(start));
    &text[from..to.max(from)]
}

pub(crate) fn utf16le(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

pub(crate) fn from_utf16le(data: &[u8]) -> String {
    let units: Vec<u16> = data
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

impl CompactContentStore {
    pub fn new(options: CompactContentStoreOptions) -> Self {
        let state = StoreState {
            spool: SpoolState::default(),
            segments: Default::default(),
            text: Default::default(),
            leaves: HashMap::new(),
            ram_leaves: BTreeSet::new(),
            ram_values: BTreeSet::new(),
            value_queued: false,
            structured: StructuredBytes::default(),
            next_leaf_id: 1,
            resident_payload_bytes: 0,
            pinned_only_bytes: 0,
            spilled_live_bytes: 0,
            spill_above_bytes: options.spill_above_bytes.unwrap_or(4 * 1024 * 1024),
            spill_target_bytes: options.spill_target_bytes.unwrap_or(2 * 1024 * 1024),
            committed_queue: Vec::new(),
        };
        Self(Arc::new(StoreInner {
            dir: options.dir,
            io: options.io.unwrap_or_else(node_compact_storage_io),
            dir_ready: tokio::sync::OnceCell::new(),
            changed: tokio::sync::Notify::new(),
            state: Mutex::new(state),
            subscribers: Mutex::new(Vec::new()),
        }))
    }

    pub fn dir(&self) -> &str {
        &self.0.dir
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, StoreState> {
        lock(&self.0.state)
    }

    /// One synchronous critical section; commit notifications and queued jobs run after it.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut StoreState) -> R) -> R {
        let (result, committed) = {
            let mut state = self.lock();
            let result = f(&mut state);
            (result, std::mem::take(&mut state.committed_queue))
        };
        if !committed.is_empty() {
            let listeners = lock(&self.0.subscribers).clone();
            for id in committed {
                for listener in &listeners {
                    listener(id);
                }
            }
        }
        self.start_pump();
        result
    }

    pub fn subscribe_committed(&self, listener: impl Fn(LeafId) + Send + Sync + 'static) {
        lock(&self.0.subscribers).push(Arc::new(listener));
    }

    pub fn leaf_units(&self, id: LeafId) -> Result<i64, CompactError> {
        self.lock().leaf_units(id)
    }

    /// Takes ownership of an immutable value with one live reference.
    pub fn create_leaf(&self, text: &str, value: bool) -> Result<LeafId, CompactError> {
        self.with(|state| state.create_leaf(text, value))
    }

    /// A committed index file participates in the same ownership graph as its descriptors.
    pub fn own_file(&self, path: &str, bytes: i64) -> Result<LeafId, CompactError> {
        self.with(|state| state.own_file(path, bytes))
    }

    /// Moves live and cut-pin references; the last reference drops RAM or reclaims the file.
    pub fn adjust(&self, id: LeafId, live: i64, pins: i64) -> Result<(), CompactError> {
        self.with(|state| state.adjust(id, live, pins))
    }

    pub fn is_committed(&self, id: LeafId) -> bool {
        self.lock().is_committed(id)
    }

    /// Reads a leaf the caller still references, from RAM or its committed file.
    pub async fn read_leaf(&self, id: LeafId) -> Result<String, CompactError> {
        let resident = self.with(|state| -> Result<Option<Arc<str>>, CompactError> {
            state.spool.assert_readable()?;
            let text = match state.leaves.get(&id) {
                Some(leaf) => leaf.text.clone(),
                None => return Err(CompactError::read(CompactReadReason::Ownership)),
            };
            state.adjust(id, 0, 1)?;
            Ok(text)
        })?;
        let result = match resident {
            Some(text) => Ok(text.to_string()),
            None => self.read_slot(id).await.map(|data| from_utf16le(&data)),
        };
        let unpinned = self.adjust(id, 0, -1);
        let text = result?;
        unpinned?;
        Ok(text)
    }

    pub fn status(&self) -> CompactStorageStatus {
        self.lock().status()
    }

    /// Commit at most one batch of structural leaves ahead of a cold-index rewrite.
    pub async fn flush_values(&self) -> Result<(), CompactError> {
        // Amortize a cold-index rewrite across a bounded batch, without waiting for every pending owner.
        for _ in 0..128 {
            let next = {
                let state = self.lock();
                if state.spool.spill_stopped() {
                    None
                } else {
                    state.ram_values.first().copied()
                }
            };
            let Some(id) = next else { break };
            self.flush_leaf(id).await?;
        }
        Ok(())
    }

    /// Serial storage jobs can flush one newly built structural page without waiting on their own queue.
    pub async fn flush_leaf(&self, id: LeafId) -> Result<(), CompactError> {
        self.flush_leaves(vec![id]).await
    }

    /// Writes the resident leaves among `ids`, all of one kind, as one segment; each is read back by offset.
    pub async fn flush_leaves(&self, ids: Vec<LeafId>) -> Result<(), CompactError> {
        let batch = self.with(|state| {
            if !state.spool.spill_allowed() {
                return Vec::new();
            }
            let mut batch = Vec::new();
            for id in ids {
                let Some(leaf) = state.leaves.get_mut(&id) else {
                    continue;
                };
                let Some(text) = leaf.text.clone().filter(|_| leaf.state == LeafState::Ram) else {
                    continue;
                };
                leaf.state = LeafState::Pending;
                let value = leaf.value;
                if value {
                    state.ram_values.remove(&id);
                } else {
                    state.ram_leaves.remove(&id);
                }
                batch.push((id, value, text));
            }
            batch
        });
        let Some((_, value, _)) = batch.first() else {
            return Ok(());
        };
        let operation = if *value {
            CompactStorageOperation::ValueWrite
        } else {
            CompactStorageOperation::SpillWrite
        };
        let data: Vec<u8> = batch
            .iter()
            .flat_map(|(_, _, text)| utf16le(text))
            .collect();
        let segment = self.write_segment(data, operation).await;
        self.with(|state| {
            let Some(segment) = segment else {
                // A failed write keeps the leaves resident and spillable.
                for (id, value, _) in &batch {
                    if let Some(leaf) = state.leaves.get_mut(id) {
                        leaf.state = LeafState::Ram;
                        if *value {
                            state.ram_values.insert(*id);
                        } else {
                            state.ram_leaves.insert(*id);
                        }
                    }
                }
                return;
            };
            let mut offset = 0;
            let retired = state.spool.retired;
            for (id, value, text) in &batch {
                let bytes = utf16_len(text) * 2;
                // A leaf released during the write, or any leaf once retirement began, is not published.
                if !retired && let Some(leaf) = state.leaves.get_mut(id) {
                    leaf.state = LeafState::Committed;
                    leaf.text = None;
                    leaf.slot = Some(segment);
                    if *value {
                        state.structured.resident -= bytes;
                        state.structured.spilled += bytes;
                    } else {
                        state.resident_payload_bytes -= bytes;
                        state.spilled_live_bytes += bytes;
                    }
                    state.place(segment, *id, offset, bytes);
                    state.committed(*id);
                }
                offset += bytes;
            }
            state.settle(segment);
        });
        Ok(())
    }

    /// Spills one oldest resident leaf per job, so other storage work interleaves fairly.
    async fn spill_next(&self) -> Result<(), CompactError> {
        let next = self.lock().ram_leaves.first().copied();
        if let Some(id) = next {
            self.flush_leaf(id).await?;
        }
        self.with(|state| {
            state.spool.spill_queued = false;
            if state.resident_payload_bytes > state.spill_target_bytes {
                state.spool.spill_queued =
                    state.spool.spill_allowed() && !state.ram_leaves.is_empty();
                if state.spool.spill_queued {
                    state.enqueue_spill();
                }
            }
        });
        Ok(())
    }
}

impl StoreState {
    pub(crate) fn leaf_units(&self, id: LeafId) -> Result<i64, CompactError> {
        self.leaves
            .get(&id)
            .map(|leaf| leaf.units)
            .ok_or_else(|| CompactError::read(CompactReadReason::Ownership))
    }

    pub(crate) fn create_leaf(&mut self, text: &str, value: bool) -> Result<LeafId, CompactError> {
        self.spool.assert_readable()?;
        let units = utf16_len(text);
        if units > COMPACT_LEAF_CODE_UNITS as i64 {
            return Err(crate::compact_content_store::CompactWriteReason::SizeLimit.into());
        }
        let id = self.next_leaf_id;
        self.next_leaf_id += 1;
        self.leaves.insert(
            id,
            Leaf {
                value,
                text: Some(Arc::from(text)),
                units,
                live: 1,
                pins: 0,
                state: LeafState::Ram,
                slot: None,
                file: None,
            },
        );
        if value {
            self.ram_values.insert(id);
            self.structured.resident += units * 2;
        } else {
            self.ram_leaves.insert(id);
            self.resident_payload_bytes += units * 2;
        }
        self.schedule_spill();
        Ok(id)
    }

    pub(crate) fn own_file(&mut self, path: &str, bytes: i64) -> Result<LeafId, CompactError> {
        let id = self.create_leaf("", true)?;
        if let Some(leaf) = self.leaves.get_mut(&id) {
            leaf.file = Some((path.to_owned(), bytes));
        }
        Ok(id)
    }

    pub(crate) fn adjust(&mut self, id: LeafId, live: i64, pins: i64) -> Result<(), CompactError> {
        if id == 0 {
            return Ok(());
        }
        if id < 0 {
            return self.text_adjust(id, live, pins);
        }
        let Some(leaf) = self.leaves.get_mut(&id) else {
            return Err(CompactError::read(CompactReadReason::Ownership));
        };
        if leaf.live + live < 0 || leaf.pins + pins < 0 {
            return Err(CompactError::read(CompactReadReason::Ownership));
        }
        let pinned_only = leaf.live == 0 && leaf.pins > 0;
        leaf.live += live;
        leaf.pins += pins;
        if pinned_only != (leaf.live == 0 && leaf.pins > 0) {
            let bytes = if pinned_only { -2 } else { 2 } * leaf.units;
            if leaf.value {
                self.structured.pinned += bytes;
            } else {
                self.pinned_only_bytes += bytes;
            }
        }
        if leaf.live > 0 || leaf.pins > 0 {
            return Ok(());
        }
        let Some(leaf) = self.leaves.remove(&id) else {
            return Ok(());
        };
        if let Some((path, bytes)) = &leaf.file {
            self.discard(path, *bytes);
        }
        if leaf.value {
            self.ram_values.remove(&id);
        } else {
            self.ram_leaves.remove(&id);
        }
        if leaf.state == LeafState::Committed
            && let Some(segment) = leaf.slot
        {
            if leaf.value {
                self.structured.spilled -= leaf.units * 2;
            } else {
                self.spilled_live_bytes -= leaf.units * 2;
            }
            self.leave(id, segment);
            return Ok(());
        }
        if leaf.value {
            self.structured.resident -= leaf.units * 2;
        } else {
            self.resident_payload_bytes -= leaf.units * 2;
        }
        Ok(())
    }

    pub(crate) fn is_committed(&self, id: LeafId) -> bool {
        if id <= 0 {
            return id == 0 || self.text_is_committed(id);
        }
        self.leaves
            .get(&id)
            .is_some_and(|leaf| leaf.state == LeafState::Committed)
    }

    /// Notifies every commit subscriber (after the critical section) and the text roots' own readiness.
    pub(crate) fn committed(&mut self, id: LeafId) {
        let mut work = vec![id];
        while let Some(id) = work.pop() {
            self.committed_queue.push(id);
            work.extend(self.text.pending.committed(id));
        }
    }

    pub(crate) fn status(&self) -> CompactStorageStatus {
        let stopped = self.spool.spill_stopped();
        CompactStorageStatus {
            structured_bytes: self.structured,
            resident_payload_bytes: self.resident_payload_bytes,
            pending_io_copy_bytes: self.spool.pending_io_copy_bytes,
            pinned_only_bytes: self.pinned_only_bytes,
            spilled_live_bytes: self.spilled_live_bytes,
            spill_bytes_written: self.spool.spill_bytes_written,
            reclaim_debt_bytes: self.spool.reclaim_debt_bytes,
            spill_failure_retained_bytes: if stopped {
                (self.resident_payload_bytes - self.spill_target_bytes).max(0)
            } else {
                0
            },
            spill_halted: stopped,
            failures: self.spool.failures.iter().cloned().collect(),
        }
    }

    pub(crate) fn schedule_spill(&mut self) {
        if !self.value_queued
            && self.spool.spill_allowed()
            && !self.ram_values.is_empty()
            && self.spool.reclaim_debt_bytes <= RECLAIM_WORK_ALLOWANCE_BYTES
        {
            self.value_queued = true;
            self.spool.enqueue(
                CompactStorageOperation::ValueWrite,
                job(|store: CompactContentStore| async move {
                    let next = store.lock().ram_values.first().copied();
                    let result = match next {
                        Some(id) => store.flush_leaf(id).await,
                        None => Ok(()),
                    };
                    store.with(|state| {
                        state.value_queued = false;
                        state.schedule_spill();
                    });
                    result
                }),
            );
        }
        if self.spool.spill_queued
            || !self.spool.spill_allowed()
            || self.resident_payload_bytes <= self.spill_above_bytes
            || self.spool.reclaim_debt_bytes > RECLAIM_WORK_ALLOWANCE_BYTES
        {
            return;
        }
        self.spool.spill_queued = true;
        self.enqueue_spill();
    }

    fn enqueue_spill(&mut self) {
        self.spool.enqueue(
            CompactStorageOperation::SpillWrite,
            job(|store: CompactContentStore| async move { store.spill_next().await }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &tempfile::TempDir, above: i64, target: i64) -> CompactContentStore {
        CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("recovery/epoch-1").display().to_string(),
            io: None,
            spill_above_bytes: Some(above),
            spill_target_bytes: Some(target),
        })
    }

    #[tokio::test]
    async fn spills_the_oldest_leaves_above_the_threshold_and_reads_them_back() {
        let root = tempfile::tempdir().expect("tempdir");
        let store = store(&root, 16, 8);
        let a = store.create_leaf("aaaaaaaa", false).expect("a");
        let b = store.create_leaf("bbbb", false).expect("b");
        store.idle().await;
        // 24 resident bytes > 16: spilling takes `a` (16 bytes) and stops at the 8-byte target.
        let status = store.status();
        assert_eq!(status.spilled_live_bytes, 16);
        assert_eq!(status.resident_payload_bytes, 8);
        assert!(store.is_committed(a));
        assert!(!store.is_committed(b));
        assert_eq!(store.read_leaf(a).await.expect("read a"), "aaaaaaaa");
        store.adjust(a, -1, 0).expect("release a");
        store.adjust(b, -1, 0).expect("release b");
        store.idle().await;
        let status = store.status();
        assert_eq!(status.spilled_live_bytes, 0);
        assert_eq!(status.resident_payload_bytes, 0);
        assert_eq!(status.reclaim_debt_bytes, 0);
        store.retire().await.expect("retire");
        assert!(!std::path::Path::new(store.dir()).exists());
    }

    #[tokio::test]
    async fn refuses_oversized_leaves_and_foreign_references() {
        let root = tempfile::tempdir().expect("tempdir");
        let store = store(&root, i64::MAX, 0);
        let error = store
            .create_leaf(&"x".repeat(COMPACT_LEAF_CODE_UNITS + 1), false)
            .expect_err("oversized");
        assert!(matches!(
            error,
            CompactError::Write(CompactWriteError {
                reason: CompactWriteReason::SizeLimit
            })
        ));
        assert_eq!(
            store.adjust(99, -1, 0).expect_err("unknown").read_reason(),
            Some(CompactReadReason::Ownership)
        );
        let id = store.create_leaf("x", true).expect("leaf");
        store.adjust(id, 0, 1).expect("pin");
        store.adjust(id, -1, 0).expect("unlive");
        assert_eq!(store.status().structured_bytes.pinned, 2);
        store.adjust(id, 0, -1).expect("unpin");
        assert_eq!(store.status().structured_bytes, StructuredBytes::default());
        store.retire().await.expect("retire");
        assert_eq!(
            store
                .create_leaf("y", false)
                .expect_err("retired")
                .read_reason(),
            Some(CompactReadReason::Retired)
        );
    }

    #[test]
    fn utf16_helpers_never_split_a_pair() {
        let text = "a\u{1F600}b";
        assert_eq!(utf16_len(text), 4);
        assert_eq!(utf16_boundary(text, 2), 1);
        assert_eq!(utf16_slice(text, 1, 3), "\u{1F600}");
        assert_eq!(from_utf16le(&utf16le(text)), text);
    }
}
