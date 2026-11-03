//! senpi `modes/rpc/compact-segment-storage.ts`: segment packing of compact current-state storage
//! (senpi#4272, RULING-47). A flush batch is one spool file and each leaf is read back by offset. A
//! segment is deleted once its last live leaf is released and no read of it is in flight. When its
//! live bytes fall below half its size it is rewritten once, so dead bytes on disk stay at most the
//! live bytes of the segments holding them; a failed rewrite halts spilling and keeps the old segment
//! readable.
//!
//! Rust shape: segments live in the store state by id; a leaf's slot is its segment id plus the
//! offset recorded in that segment's live map, so a rewrite repoints a leaf by moving its entry.
use crate::compact_content_store::{CompactContentStore, LeafId, StoreState};
use crate::compact_read_lease::{CompactError, CompactReadReason};
use crate::compact_storage_io::{CompactFileRange, CompactStorageOperation};
use std::collections::{BTreeMap, HashMap};

/// Where a committed leaf's bytes are inside one segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactSlot {
    pub offset: i64,
    pub bytes: i64,
}

/// One flush batch in one spool file, with the leaves still live in it.
#[derive(Debug, Clone)]
pub struct CompactSegment {
    pub path: String,
    pub bytes: i64,
    pub operation: CompactStorageOperation,
    pub live: BTreeMap<LeafId, CompactSlot>,
    pub live_bytes: i64,
    pub readers: usize,
    pub rewritten: bool,
}

#[derive(Default)]
pub(crate) struct SegmentState {
    segments: HashMap<u64, CompactSegment>,
    next: u64,
}

impl StoreState {
    pub(crate) fn place(&mut self, segment: u64, id: LeafId, offset: i64, bytes: i64) {
        if let Some(segment) = self.segments.segments.get_mut(&segment) {
            segment.live.insert(id, CompactSlot { offset, bytes });
            segment.live_bytes += bytes;
        }
    }

    pub(crate) fn leave(&mut self, id: LeafId, segment: u64) {
        if let Some(entry) = self.segments.segments.get_mut(&segment)
            && let Some(slot) = entry.live.remove(&id)
        {
            entry.live_bytes -= slot.bytes;
        }
        self.settle(segment);
    }

    /// Deletes an empty segment no read holds, or queues the one rewrite of a mostly dead segment.
    pub(crate) fn settle(&mut self, id: u64) {
        let Some(segment) = self.segments.segments.get_mut(&id) else {
            return;
        };
        if segment.live.is_empty() {
            if segment.readers == 0
                && let Some(segment) = self.segments.segments.remove(&id)
            {
                self.discard(&segment.path, segment.bytes);
            }
        } else if !segment.rewritten && segment.live_bytes * 2 < segment.bytes {
            segment.rewritten = true;
            self.spool.enqueue(
                CompactStorageOperation::Reclaim,
                crate::compact_spool_storage::job(move |store: CompactContentStore| async move {
                    store.rewrite(id).await
                }),
            );
        }
    }

    fn add_reader(&mut self, id: u64, delta: isize) {
        if let Some(segment) = self.segments.segments.get_mut(&id) {
            segment.readers = segment.readers.saturating_add_signed(delta);
        }
    }

    pub fn segment_count(&self) -> usize {
        self.segments.segments.len()
    }
}

impl CompactContentStore {
    /// Writes the bytes as one new segment file; `None` when the write failed and was reported.
    pub(crate) async fn write_segment(
        &self,
        data: Vec<u8>,
        operation: CompactStorageOperation,
    ) -> Option<u64> {
        let bytes = i64::try_from(data.len()).unwrap_or(i64::MAX);
        let path = self.write_file(data, operation, bytes).await?;
        let mut state = self.lock();
        let id = state.segments.next;
        state.segments.next += 1;
        state.segments.segments.insert(
            id,
            CompactSegment {
                path,
                bytes,
                operation,
                live: BTreeMap::new(),
                live_bytes: 0,
                readers: 0,
                rewritten: false,
            },
        );
        Some(id)
    }

    /// Reads one committed leaf by its slot; the segment stays on disk while the read is in flight.
    pub(crate) async fn read_slot(&self, id: LeafId) -> Result<Vec<u8>, CompactError> {
        let (segment, path, slot) = self.with(|state| {
            let segment = state
                .leaves
                .get(&id)
                .and_then(|leaf| leaf.slot)
                .ok_or_else(|| CompactError::read(CompactReadReason::Ownership))?;
            let entry = state
                .segments
                .segments
                .get(&segment)
                .ok_or_else(|| CompactError::read(CompactReadReason::Ownership))?;
            let slot = *entry
                .live
                .get(&id)
                .ok_or_else(|| CompactError::read(CompactReadReason::Ownership))?;
            let path = entry.path.clone();
            state.add_reader(segment, 1);
            Ok::<_, CompactError>((segment, path, slot))
        })?;
        let result = self
            .read_file(
                &path,
                usize::try_from(slot.bytes).unwrap_or(0),
                CompactStorageOperation::LeafRead,
                Some(CompactFileRange {
                    offset: u64::try_from(slot.offset).unwrap_or(0),
                    length: usize::try_from(slot.bytes).unwrap_or(0),
                }),
            )
            .await;
        self.with(|state| {
            state.add_reader(segment, -1);
            state.settle(segment);
        });
        result
    }

    /// Copies the live leaves into a new segment, then repoints their slots; the old file goes when
    /// unread. Only live bytes are read (one range per adjacent run) and they count as I/O copy bytes
    /// until the end.
    pub(crate) async fn rewrite(&self, id: u64) -> Result<(), CompactError> {
        let snapshot = self.with(|state| {
            if !state.spool.spill_allowed() {
                return None;
            }
            let segment = state.segments.segments.get(&id)?;
            if segment.live.is_empty() {
                return None;
            }
            let mut moving: Vec<(LeafId, CompactSlot)> =
                segment.live.iter().map(|(id, slot)| (*id, *slot)).collect();
            moving.sort_by_key(|(_, slot)| slot.offset);
            let snapshot = (segment.path.clone(), segment.operation, moving);
            state.add_reader(id, 1);
            Some(snapshot)
        });
        let Some((path, operation, moving)) = snapshot else {
            return Ok(());
        };
        let mut copied = 0i64;
        let result = async {
            let mut data = Vec::new();
            let mut n = 0;
            while n < moving.len() {
                let start = moving[n].1.offset;
                let mut end = start;
                while n < moving.len() && moving[n].1.offset == end {
                    end += moving[n].1.bytes;
                    n += 1;
                }
                let length = usize::try_from(end - start).unwrap_or(0);
                let run = self
                    .read_file(
                        &path,
                        length,
                        CompactStorageOperation::LeafRead,
                        Some(CompactFileRange {
                            offset: u64::try_from(start).unwrap_or(0),
                            length,
                        }),
                    )
                    .await?;
                let bytes = i64::try_from(run.len()).unwrap_or(i64::MAX);
                self.lock().spool.pending_io_copy_bytes += bytes;
                copied += bytes;
                data.extend(run);
            }
            Ok::<_, CompactError>(self.write_segment(data, operation).await)
        }
        .await;
        self.with(|state| {
            state.spool.pending_io_copy_bytes -= copied;
            let next = match &result {
                Ok(next) => *next,
                Err(_) => None,
            };
            if let Some(next) = next {
                let mut offset = 0;
                for (leaf, slot) in &moving {
                    // A leaf released while the copy was written stays out of the new segment's live set.
                    let still = state
                        .segments
                        .segments
                        .get_mut(&id)
                        .and_then(|segment| {
                            let removed = segment.live.remove(leaf)?;
                            segment.live_bytes -= removed.bytes;
                            Some(())
                        })
                        .is_some();
                    if still {
                        state.place(next, *leaf, offset, slot.bytes);
                        if let Some(entry) = state.leaves.get_mut(leaf) {
                            entry.slot = Some(next);
                        }
                    }
                    offset += slot.bytes;
                }
            }
            state.add_reader(id, -1);
            state.settle(id);
            if let Some(next) = next {
                state.settle(next);
            }
        });
        match result {
            // Retirement during the copy is an orderly abort: the epoch directory goes with the store.
            Err(error) if error.is_retired() => Ok(()),
            Err(error) => Err(error),
            Ok(_) => Ok(()),
        }
    }

    /// Spool files currently holding live leaves (diagnostics and tests).
    pub fn segment_count(&self) -> usize {
        self.lock().segment_count()
    }
}

#[cfg(test)]
mod tests {
    use crate::compact_content_store::{CompactContentStore, CompactContentStoreOptions};

    async fn value_files(dir: &str) -> Vec<u64> {
        let mut sizes = Vec::new();
        let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
            return sizes;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry.file_name().to_string_lossy().starts_with("value-") {
                sizes.push(entry.metadata().await.expect("meta").len());
            }
        }
        sizes.sort_unstable();
        sizes
    }

    // senpi 4272-segment-reclamation: dead bytes never exceed live bytes; the last release deletes the file.
    #[tokio::test]
    async fn rewrites_each_segment_once_below_half_live_and_deletes_it_with_its_last_leaf() {
        let root = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: root.path().join("recovery/epoch-1").display().to_string(),
            ..Default::default()
        });
        let ids: Vec<_> = (0..8)
            .map(|n| {
                store
                    .create_leaf(&n.to_string().repeat(64), true)
                    .expect("leaf")
            })
            .collect();
        store.flush_leaves(ids.clone()).await.expect("flush");
        store.idle().await;
        assert_eq!(value_files(store.dir()).await, [1024]);
        for (n, id) in ids.iter().enumerate() {
            store.adjust(*id, -1, 0).expect("release");
            store.idle().await;
            let live = store.status().structured_bytes.spilled;
            assert_eq!(live, (7 - n as i64) * 128, "after releasing {}", n + 1);
            let on_disk: u64 = value_files(store.dir()).await.iter().sum();
            assert!(
                on_disk as i64 - live <= live,
                "dead bytes after releasing {}",
                n + 1
            );
            if n < 7 {
                assert_eq!(store.read_leaf(ids[7]).await.expect("read"), "7".repeat(64));
            }
        }
        assert!(value_files(store.dir()).await.is_empty());
        assert_eq!(store.status().reclaim_debt_bytes, 0);
        store.retire().await.expect("retire");
    }
}
