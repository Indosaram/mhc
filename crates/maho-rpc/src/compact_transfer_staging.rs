//! senpi `modes/rpc/compact-transfer-staging.ts`: the shared bounded staging substrate (S12a;
//! S12b's worker transfers and S13's leases reuse it).

use crate::compact_content_store::{
    COMPACT_LEAF_CODE_UNITS, CompactContentStore, CompactStorageStatus, LeafId, utf16_len,
    utf16_slice,
};
use crate::compact_recovery_bytes::{
    CatalogDigester, CatalogDomain, CatalogEntry, FRAGMENT_MAX_BYTES, FrozenBytes,
    RecoveryBytesError, ValidCheck, catalog_digest, open_frozen_bytes, stored_bytes,
};
use crate::compact_recovery_ids::{BoundedId, ContentDigest};
use futures::stream::{self, BoxStream, StreamExt};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

/// B_worker (S12 design section 3): max stored bytes staged and not yet adopted.
pub const TRANSFER_STAGING_MAX_BYTES: usize = 64 * 1024 * 1024;
/// Code units one fragment carries: D (`FRAGMENT_MAX_BYTES`) stored bytes, two per UTF-16 code unit.
const FRAGMENT_MAX_UNITS: usize = FRAGMENT_MAX_BYTES / 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StagingRefusal {
    StagingOverByteLimit,
    FragmentOverLimit,
    LengthMismatch,
    StagingClosed,
}

impl StagingRefusal {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StagingOverByteLimit => "staging_over_byte_limit",
            Self::FragmentOverLimit => "fragment_over_limit",
            Self::LengthMismatch => "length_mismatch",
            Self::StagingClosed => "staging_closed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("transfer staging refused ({})", .reason.as_str())]
pub struct TransferStagingError {
    pub reason: StagingRefusal,
}

impl TransferStagingError {
    pub const fn new(reason: StagingRefusal) -> Self {
        Self { reason }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StagedReadReason {
    SegmentMissing,
    SegmentMismatch,
}

impl StagedReadReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SegmentMissing => "segment_missing",
            Self::SegmentMismatch => "segment_mismatch",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("staged read refused ({})", .reason.as_str())]
pub struct StagedReadError {
    pub reason: StagedReadReason,
}

impl StagedReadError {
    pub const fn new(reason: StagedReadReason) -> Self {
        Self { reason }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StagingOpError {
    #[error(transparent)]
    Transfer(#[from] TransferStagingError),
    #[error(transparent)]
    Bytes(#[from] RecoveryBytesError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrozenRef {
    Staged { entry: CatalogEntry },
}

impl FrozenRef {
    pub fn entry(&self) -> &CatalogEntry {
        match self {
            Self::Staged { entry } => entry,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedContent {
    pub id: u64,
    pub ref_: FrozenRef,
    pub leaves: Vec<LeafId>,
    pub leaf_digests: Vec<ContentDigest>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagingStatus {
    pub store: CompactStorageStatus,
    pub staged_bytes: usize,
    pub open_transfers: usize,
}

struct StagingState {
    open_transfers: HashSet<u64>,
    staged: HashMap<u64, StagedContent>,
    reserved: usize,
    closed: bool,
    next_id: u64,
}

/// One logical value arriving in fragments (S12 design section 3).
pub struct StagedTransfer {
    id: u64,
    store: CompactContentStore,
    state: Arc<Mutex<StagingState>>,
    kind: BoundedId,
    entry_id: BoundedId,
    expected_length: usize,
    digester: Option<CatalogDigester>,
    leaves: Vec<LeafId>,
    leaf_digests: Vec<ContentDigest>,
    fed: usize,
    open: bool,
}

impl StagedTransfer {
    pub fn append(&mut self, text: &str) -> Result<(), StagingOpError> {
        if !self.open {
            return Err(TransferStagingError::new(StagingRefusal::StagingClosed).into());
        }
        let units = usize::try_from(utf16_len(text)).unwrap_or(0);
        let bytes = units * 2;
        if bytes > FRAGMENT_MAX_BYTES {
            self.abort();
            return Err(TransferStagingError::new(StagingRefusal::FragmentOverLimit).into());
        }
        if self.fed + bytes > self.expected_length {
            self.abort();
            return Err(TransferStagingError::new(StagingRefusal::LengthMismatch).into());
        }
        let mut at = 0usize;
        while at < units {
            let piece = utf16_slice(text, at, at + COMPACT_LEAF_CODE_UNITS);
            let piece_units = usize::try_from(utf16_len(piece)).unwrap_or(0).max(1);
            match self.store.create_leaf(piece, false) {
                Ok(leaf) => {
                    self.leaves.push(leaf);
                    self.leaf_digests
                        .push(catalog_digest(CatalogDomain::Leaf, &stored_bytes(piece)));
                }
                Err(err) => {
                    self.abort();
                    return Err(RecoveryBytesError::from(err).into());
                }
            }
            at += piece_units;
        }
        if let Some(digester) = &mut self.digester {
            digester.update(&stored_bytes(text));
        }
        self.fed += bytes;
        Ok(())
    }

    pub fn commit(mut self) -> Result<StagedContent, StagingOpError> {
        if !self.open {
            return Err(TransferStagingError::new(StagingRefusal::StagingClosed).into());
        }
        if self.fed != self.expected_length {
            self.abort();
            return Err(TransferStagingError::new(StagingRefusal::LengthMismatch).into());
        }
        self.open = false;
        let digester = self
            .digester
            .take()
            .ok_or_else(|| TransferStagingError::new(StagingRefusal::StagingClosed))?;
        let digest = digester.finish()?;
        let content = StagedContent {
            id: self.id,
            ref_: FrozenRef::Staged {
                entry: CatalogEntry {
                    kind: self.kind.clone(),
                    id: self.entry_id.clone(),
                    length: self.expected_length,
                    digest,
                },
            },
            leaves: std::mem::take(&mut self.leaves),
            leaf_digests: std::mem::take(&mut self.leaf_digests),
        };
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.open_transfers.remove(&self.id);
        state.staged.insert(self.id, content.clone());
        Ok(content)
    }

    pub fn abort(&mut self) {
        if !self.open {
            return;
        }
        self.open = false;
        self.digester = None;
        for leaf in self.leaves.drain(..) {
            let _ = self.store.adjust(leaf, -1, 0);
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.open_transfers.remove(&self.id) {
            state.reserved = state.reserved.saturating_sub(self.expected_length);
        }
    }
}

impl Drop for StagedTransfer {
    fn drop(&mut self) {
        if self.open {
            self.abort();
        }
    }
}

/// Shared bounded staging substrate (S12a).
#[derive(Clone)]
pub struct TransferStaging {
    pub store: CompactContentStore,
    max_bytes: usize,
    state: Arc<Mutex<StagingState>>,
}

impl TransferStaging {
    pub fn new(store: CompactContentStore, max_bytes: Option<usize>) -> Self {
        Self {
            store,
            max_bytes: max_bytes.unwrap_or(TRANSFER_STAGING_MAX_BYTES),
            state: Arc::new(Mutex::new(StagingState {
                open_transfers: HashSet::new(),
                staged: HashMap::new(),
                reserved: 0,
                closed: false,
                next_id: 0,
            })),
        }
    }

    pub fn staged_bytes(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .reserved
    }

    pub fn resident_bytes(&self) -> i64 {
        self.store.status().resident_payload_bytes
    }

    pub fn begin(
        &self,
        kind: BoundedId,
        id: BoundedId,
        length: usize,
    ) -> Result<StagedTransfer, TransferStagingError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.closed {
            return Err(TransferStagingError::new(StagingRefusal::StagingClosed));
        }
        if !length.is_multiple_of(2) {
            return Err(TransferStagingError::new(StagingRefusal::LengthMismatch));
        }
        if state.reserved.saturating_add(length) > self.max_bytes {
            return Err(TransferStagingError::new(
                StagingRefusal::StagingOverByteLimit,
            ));
        }
        state.reserved += length;
        state.next_id += 1;
        let transfer_id = state.next_id;
        state.open_transfers.insert(transfer_id);
        Ok(StagedTransfer {
            id: transfer_id,
            store: self.store.clone(),
            state: self.state.clone(),
            kind,
            entry_id: id,
            expected_length: length,
            digester: Some(CatalogDigester::new(CatalogDomain::Leaf, length)),
            leaves: Vec::new(),
            leaf_digests: Vec::new(),
            fed: 0,
            open: true,
        })
    }

    pub fn stage(
        &self,
        kind: BoundedId,
        id: BoundedId,
        text: &str,
    ) -> Result<StagedContent, StagingOpError> {
        let units = usize::try_from(utf16_len(text)).unwrap_or(0);
        let mut transfer = self.begin(kind, id, units * 2)?;
        let mut at = 0usize;
        while at < units {
            let chunk = utf16_slice(text, at, at + FRAGMENT_MAX_UNITS);
            let step = usize::try_from(utf16_len(chunk)).unwrap_or(0).max(1);
            transfer.append(chunk)?;
            at += step;
        }
        transfer.commit()
    }

    pub fn adopt(&self, content: &StagedContent) -> Result<(), RecoveryBytesError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(_) = state.staged.remove(&content.id) else {
            return Err(RecoveryBytesError::Ownership);
        };
        state.reserved = state.reserved.saturating_sub(content.ref_.entry().length);
        Ok(())
    }

    pub fn release(&self, content: &StagedContent) {
        {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.staged.remove(&content.id).is_some() {
                state.reserved = state.reserved.saturating_sub(content.ref_.entry().length);
            }
        }
        for &leaf in &content.leaves {
            let _ = self.store.adjust(leaf, -1, 0);
        }
    }

    pub fn abandon(&self, content: &StagedContent) {
        let present = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .staged
            .contains_key(&content.id);
        if present {
            self.release(content);
        }
    }

    pub fn pin(&self, content: &StagedContent, pins: i64) -> Result<(), RecoveryBytesError> {
        for &leaf in &content.leaves {
            self.store.adjust(leaf, 0, pins)?;
        }
        Ok(())
    }

    pub async fn open(
        &self,
        content: &StagedContent,
        valid: ValidCheck<'_>,
    ) -> Result<Arc<dyn FrozenBytes>, RecoveryBytesError> {
        let store = self.store.clone();
        let read: crate::compact_recovery_bytes::ReadLeafFn = Arc::new(move |id| {
            let store = store.clone();
            Box::pin(async move { store.read_leaf(id).await.map_err(Into::into) })
        });
        let entry = content.ref_.entry();
        open_frozen_bytes(
            entry.length,
            &entry.digest,
            content.leaves.clone(),
            read,
            valid,
            None,
        )
        .await
    }

    /// The content's text leaf by leaf, each leaf verified against its digest before it is yielded.
    pub fn chunks<'a>(
        &'a self,
        content: &'a StagedContent,
        valid: Option<Arc<dyn Fn() -> Result<(), RecoveryBytesError> + Send + Sync>>,
    ) -> BoxStream<'a, Result<String, StagedReadError>> {
        stream::unfold((0usize, 0usize, false), move |(idx, total_bytes, done)| {
            let valid = valid.clone();
            async move {
                if done {
                    return None;
                }
                if idx >= content.leaves.len() {
                    if total_bytes != content.ref_.entry().length {
                        return Some((
                            Err(StagedReadError::new(StagedReadReason::SegmentMismatch)),
                            (idx, total_bytes, true),
                        ));
                    }
                    return None;
                }
                let leaf = content.leaves[idx];
                let text = match self.store.read_leaf(leaf).await {
                    Ok(t) => t,
                    Err(_) => {
                        return Some((
                            Err(StagedReadError::new(StagedReadReason::SegmentMissing)),
                            (idx + 1, total_bytes, true),
                        ));
                    }
                };
                if let Some(check) = &valid
                    && check().is_err()
                {
                    return Some((
                        Err(StagedReadError::new(StagedReadReason::SegmentMissing)),
                        (idx + 1, total_bytes, true),
                    ));
                }
                let stored = stored_bytes(&text);
                if catalog_digest(CatalogDomain::Leaf, &stored) != content.leaf_digests[idx] {
                    return Some((
                        Err(StagedReadError::new(StagedReadReason::SegmentMismatch)),
                        (idx + 1, total_bytes, true),
                    ));
                }
                let next_total = total_bytes + stored.len();
                Some((Ok(text), (idx + 1, next_total, false)))
            }
        })
        .boxed()
    }

    pub fn spill(&self, content: &StagedContent) {
        for &leaf in &content.leaves {
            self.store.enqueue(
                crate::compact_storage_io::CompactStorageOperation::SpillWrite,
                crate::compact_spool_storage::job(move |store| async move {
                    store.flush_leaf(leaf).await
                }),
            );
        }
    }

    pub fn status(&self) -> StagingStatus {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        StagingStatus {
            store: self.store.status(),
            staged_bytes: state.reserved,
            open_transfers: state.open_transfers.len(),
        }
    }

    pub async fn idle(&self) {
        self.store.idle().await;
    }

    pub async fn close(&self) -> Result<(), RecoveryBytesError> {
        let to_release: Vec<StagedContent> = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.closed = true;
            state.staged.values().cloned().collect()
        };
        for content in to_release {
            self.release(&content);
        }
        self.store.retire().await.map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_content_store::CompactContentStoreOptions;
    use crate::compact_recovery_ids::protocol_id;

    fn ok_valid() -> Result<(), RecoveryBytesError> {
        Ok(())
    }

    #[tokio::test]
    async fn stages_verifies_chunks_and_opens_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let staging = TransferStaging::new(store, None);
        let staged = staging
            .stage(protocol_id("event"), protocol_id("rec"), "hello world")
            .expect("stage");
        assert_eq!(staging.staged_bytes(), 22);

        let bytes = staging.open(&staged, &ok_valid).await.expect("open");
        assert_eq!(bytes.length(), 22);

        let chunks: Vec<_> = staging
            .chunks(&staged, None)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("chunks");
        assert_eq!(chunks.concat(), "hello world");

        staging.adopt(&staged).expect("adopt");
        assert_eq!(staging.staged_bytes(), 0);
        staging.release(&staged);
        staging.close().await.expect("close");
    }

    #[tokio::test]
    async fn enforces_byte_limit_and_length_mismatch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let staging = TransferStaging::new(store, Some(10));
        let err = staging
            .stage(protocol_id("event"), protocol_id("rec"), "123456")
            .expect_err("over limit");
        assert!(matches!(
            err,
            StagingOpError::Transfer(TransferStagingError {
                reason: StagingRefusal::StagingOverByteLimit
            })
        ));
        assert_eq!(staging.staged_bytes(), 0);

        let mut transfer = staging
            .begin(protocol_id("event"), protocol_id("rec"), 8)
            .expect("begin");
        transfer.append("ab").expect("append");
        let mismatch = transfer.commit().expect_err("short commit");
        assert!(matches!(
            mismatch,
            StagingOpError::Transfer(TransferStagingError {
                reason: StagingRefusal::LengthMismatch
            })
        ));
        assert_eq!(staging.staged_bytes(), 0);
        staging.close().await.expect("close");
    }
}
