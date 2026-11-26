//! senpi `modes/rpc/compact-recovery-bytes.ts`: frozen byte streams (exact stored UTF-16LE bytes
//! in leaf order), verification against catalog digests, and bounded page production (S11a).
//!
//! Also hosts the shared `compact-catalog-codec` digest & stored-bytes primitives (`CatalogDomain`,
//! `CatalogEntry`, `stored_bytes`, `CatalogDigester`, `catalog_digest`) reused by
//! `compact_recovery_catalog`, `compact_recovery_base`, `compact_manifest_page`, and
//! `compact_transfer_staging`.

use crate::compact_content_store::LeafId;
use crate::compact_read_lease::{CompactError, CompactReadReason};
use crate::compact_recovery_ids::{BoundedId, ContentDigest, parse_content_digest};
use crate::compact_recovery_types::StorageUnavailableReason;
use futures::future::BoxFuture;
use maho_core::session_transcript_journal::PersistedBaseError;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// D (R4.B3): the most raw bytes one fragment carries; its base64 (174,764) plus H (16,384) fits F (262,144).
pub const FRAGMENT_MAX_BYTES: usize = 131_072;
/// The page budget on the wire (RULING-69 (1)): the base64 envelope of a full fragment.
pub const FRAGMENT_ENVELOPE_MAX_BYTES: usize = FRAGMENT_MAX_BYTES.div_ceil(3) * 4;

pub const CATALOG_FORMAT: &str = "senpi-recovery-catalog";
pub const CATALOG_VERSION: u32 = 1;
const CATALOG_ENCODING: &str = "utf16le";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogDomain {
    Leaf,
    Node,
    Root,
    Pointer,
    Fragment,
}

impl CatalogDomain {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Leaf => "leaf",
            Self::Node => "node",
            Self::Root => "root",
            Self::Pointer => "pointer",
            Self::Fragment => "fragment",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub kind: BoundedId,
    pub id: BoundedId,
    pub length: usize,
    pub digest: ContentDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecoveryBytesError {
    #[error("compact read refused (range)")]
    Range,
    #[error("compact read refused (size_limit)")]
    SizeLimit,
    #[error("compact read refused (count_limit)")]
    CountLimit,
    #[error("compact read refused (ownership)")]
    Ownership,
    #[error("compact read refused (retired)")]
    Retired,
    #[error("compact read refused (released)")]
    Released,
    #[error("compact read refused (integrity)")]
    Integrity,
    #[error("compact read refused (aborted)")]
    Aborted,
    #[error("compact read refused (unavailable)")]
    Unavailable,
    #[error("compact base refused ({0:?})")]
    Base(PersistedBaseError),
    #[error("{0}")]
    Io(String),
}

impl From<CompactReadReason> for RecoveryBytesError {
    fn from(reason: CompactReadReason) -> Self {
        match reason {
            CompactReadReason::Released => Self::Released,
            CompactReadReason::Retired => Self::Retired,
            CompactReadReason::Range => Self::Range,
            CompactReadReason::SizeLimit => Self::SizeLimit,
            CompactReadReason::CountLimit => Self::CountLimit,
            CompactReadReason::Ownership => Self::Ownership,
        }
    }
}

impl From<CompactError> for RecoveryBytesError {
    fn from(err: CompactError) -> Self {
        match err {
            CompactError::Read(e) => e.reason.into(),
            CompactError::StorageRead(_) => Self::Unavailable,
            CompactError::Write(_)
            | CompactError::SpoolRefused(_)
            | CompactError::Transition(_) => Self::Unavailable,
            CompactError::Io(msg) => Self::Io(msg),
        }
    }
}

impl From<PersistedBaseError> for RecoveryBytesError {
    fn from(err: PersistedBaseError) -> Self {
        Self::Base(err)
    }
}

impl From<&RecoveryBytesError> for StorageUnavailableReason {
    fn from(err: &RecoveryBytesError) -> Self {
        match err {
            RecoveryBytesError::Range => Self::Range,
            RecoveryBytesError::SizeLimit => Self::SizeLimit,
            RecoveryBytesError::CountLimit => Self::CountLimit,
            RecoveryBytesError::Ownership => Self::Ownership,
            RecoveryBytesError::Retired => Self::Retired,
            RecoveryBytesError::Released => Self::Unavailable,
            RecoveryBytesError::Integrity => Self::Integrity,
            RecoveryBytesError::Aborted => Self::Aborted,
            RecoveryBytesError::Unavailable | RecoveryBytesError::Io(_) => Self::Unavailable,
            RecoveryBytesError::Base(b) => (*b).into(),
        }
    }
}

/// Encodes `text` as exact stored UTF-16LE bytes (matching `compact_content_store::utf16le`).
pub fn stored_bytes(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// Incremental domain-tagged SHA-256 digester (`u8 tagLength || tag || u64be byteLength || bytes`).
pub struct CatalogDigester {
    hash: Sha256,
    length: usize,
    fed: usize,
}

impl CatalogDigester {
    pub fn new(domain: CatalogDomain, length: usize) -> Self {
        let tag = format!(
            "{CATALOG_FORMAT}/{CATALOG_VERSION}/{CATALOG_ENCODING}/{}",
            domain.as_str()
        );
        let tag_bytes = tag.as_bytes();
        let mut hash = Sha256::new();
        let tag_len = u8::try_from(tag_bytes.len()).unwrap_or(u8::MAX);
        hash.update([tag_len]);
        hash.update(tag_bytes);
        hash.update((length as u64).to_be_bytes());
        Self {
            hash,
            length,
            fed: 0,
        }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        self.fed = self.fed.saturating_add(bytes.len());
        self.hash.update(bytes);
    }

    pub fn finish(self) -> Result<ContentDigest, RecoveryBytesError> {
        if self.fed != self.length {
            return Err(RecoveryBytesError::Integrity);
        }
        let hex_digest = hex::encode(self.hash.finalize());
        parse_content_digest(&hex_digest).map_err(|_| RecoveryBytesError::Integrity)
    }
}

pub fn catalog_digest(domain: CatalogDomain, bytes: &[u8]) -> ContentDigest {
    let mut digester = CatalogDigester::new(domain, bytes.len());
    digester.update(bytes);
    digester
        .finish()
        .unwrap_or_else(|e| panic!("catalog_digest failed on exact length: {e}"))
}

/// One frozen component as the byte stream recovery pages address (S11a).
pub trait FrozenBytes: Send + Sync {
    fn length(&self) -> usize;
    fn digest(&self) -> &ContentDigest;
    fn peak_resident_leaves(&self) -> usize;
    fn range<'a>(
        &'a self,
        offset: usize,
        length: usize,
        valid: Option<&'a (dyn Fn() -> Result<(), RecoveryBytesError> + Sync)>,
    ) -> BoxFuture<'a, Result<Vec<u8>, RecoveryBytesError>>;
}

/// One bounded page of a component's bytes: `digest` is the `fragment`-domain digest of `bytes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryPage {
    pub offset: usize,
    pub length: usize,
    pub bytes: Vec<u8>,
    pub digest: ContentDigest,
    pub next: Option<usize>,
    pub complete: bool,
}

pub type ReadLeafFn =
    Arc<dyn Fn(LeafId) -> BoxFuture<'static, Result<String, RecoveryBytesError>> + Send + Sync>;
pub type ValidCheck<'a> = &'a (dyn Fn() -> Result<(), RecoveryBytesError> + Sync);

#[derive(Clone, Default)]
pub struct CancelSignal {
    aborted: Arc<AtomicBool>,
}

impl CancelSignal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn abort(&self) {
        self.aborted.store(true, Ordering::SeqCst);
    }

    pub fn is_aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }
}

/// The checks every `FrozenBytes::range` makes before touching a byte: shape, D bound, stream end.
pub fn assert_range(offset: usize, length: usize, total: usize) -> Result<(), RecoveryBytesError> {
    if length > FRAGMENT_MAX_BYTES {
        return Err(RecoveryBytesError::SizeLimit);
    }
    match offset.checked_add(length) {
        Some(end) if end <= total => Ok(()),
        _ => Err(RecoveryBytesError::Range),
    }
}

/// The index of the last of `count` ascending starts at or before `offset`: the unit whose byte range holds it.
pub fn start_at_or_before<T: PartialOrd + Copy>(starts: &[T], count: usize, offset: T) -> usize {
    if count == 0 {
        return 0;
    }
    let mut low = 0usize;
    let mut high = count - 1;
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if starts.get(middle).is_some_and(|&v| v <= offset) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    low
}

struct ResidentGuard<'a> {
    counter: &'a AtomicUsize,
}

impl Drop for ResidentGuard<'_> {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct FrozenLeafBytes {
    length: usize,
    digest: ContentDigest,
    peak_resident: AtomicUsize,
    resident: AtomicUsize,
    leaves: Vec<LeafId>,
    starts: Vec<usize>,
    read: ReadLeafFn,
}

impl FrozenLeafBytes {
    pub fn new(
        digest: ContentDigest,
        leaves: Vec<LeafId>,
        starts: Vec<usize>,
        read: ReadLeafFn,
    ) -> Self {
        let length = starts.get(leaves.len()).copied().unwrap_or(0);
        Self {
            length,
            digest,
            peak_resident: AtomicUsize::new(0),
            resident: AtomicUsize::new(0),
            leaves,
            starts,
            read,
        }
    }
}

impl FrozenBytes for FrozenLeafBytes {
    fn length(&self) -> usize {
        self.length
    }

    fn digest(&self) -> &ContentDigest {
        &self.digest
    }

    fn peak_resident_leaves(&self) -> usize {
        self.peak_resident.load(Ordering::SeqCst)
    }

    fn range<'a>(
        &'a self,
        offset: usize,
        length: usize,
        valid: Option<ValidCheck<'a>>,
    ) -> BoxFuture<'a, Result<Vec<u8>, RecoveryBytesError>> {
        Box::pin(async move {
            assert_range(offset, length, self.length)?;
            let mut out = vec![0u8; length];
            let mut written = 0usize;
            let mut index = start_at_or_before(&self.starts, self.leaves.len(), offset);
            while written < length {
                let Some(&leaf) = self.leaves.get(index) else {
                    return Err(RecoveryBytesError::Integrity);
                };
                let Some(&start) = self.starts.get(index) else {
                    return Err(RecoveryBytesError::Integrity);
                };
                let now = self.resident.fetch_add(1, Ordering::SeqCst) + 1;
                self.peak_resident.fetch_max(now, Ordering::SeqCst);
                let _guard = ResidentGuard {
                    counter: &self.resident,
                };
                let text = (self.read)(leaf).await?;
                let stored = stored_bytes(&text);
                if let Some(check) = valid {
                    check()?;
                }
                let from = offset.saturating_sub(start);
                if from < stored.len() {
                    let take = (stored.len() - from).min(length - written);
                    out[written..written + take].copy_from_slice(&stored[from..from + take]);
                    written += take;
                } else {
                    return Err(RecoveryBytesError::Integrity);
                }
                index += 1;
            }
            Ok(out)
        })
    }
}

/// Opens a component's stored leaves as `FrozenBytes` after verifying them, one leaf resident,
/// against the catalog entry's length and digest; a mismatch is `Integrity`.
pub async fn open_frozen_bytes(
    entry_length: usize,
    entry_digest: &ContentDigest,
    leaves: Vec<LeafId>,
    read: ReadLeafFn,
    valid: ValidCheck<'_>,
    signal: Option<&CancelSignal>,
) -> Result<Arc<dyn FrozenBytes>, RecoveryBytesError> {
    let mut digester = CatalogDigester::new(CatalogDomain::Leaf, entry_length);
    let mut starts = Vec::with_capacity(leaves.len() + 1);
    starts.push(0);
    let mut fed = 0usize;
    for &leaf in &leaves {
        if signal.is_some_and(CancelSignal::is_aborted) {
            return Err(RecoveryBytesError::Aborted);
        }
        let text = read(leaf).await?;
        let stored = stored_bytes(&text);
        valid()?;
        fed = fed.saturating_add(stored.len());
        if fed > entry_length {
            return Err(RecoveryBytesError::Integrity);
        }
        digester.update(&stored);
        starts.push(fed);
    }
    if fed != entry_length {
        return Err(RecoveryBytesError::Integrity);
    }
    let computed = digester.finish()?;
    if &computed != entry_digest {
        return Err(RecoveryBytesError::Integrity);
    }
    Ok(Arc::new(FrozenLeafBytes::new(
        entry_digest.clone(),
        leaves,
        starts,
        read,
    )))
}

/// The page at a raw byte offset: at most `FRAGMENT_MAX_BYTES` bytes, `next` the offset the following
/// page starts at (`None` once complete). An offset past the last byte is `Range`; an empty component
/// is one empty page.
pub async fn produce_page(
    bytes: &dyn FrozenBytes,
    offset: usize,
    valid: Option<ValidCheck<'_>>,
) -> Result<RecoveryPage, RecoveryBytesError> {
    let total = bytes.length();
    if offset > total || (offset == total && total > 0) {
        return Err(RecoveryBytesError::Range);
    }
    let length = FRAGMENT_MAX_BYTES.min(total - offset);
    let page = bytes.range(offset, length, valid).await?;
    let next = offset + length;
    let digest = catalog_digest(CatalogDomain::Fragment, &page);
    Ok(RecoveryPage {
        offset,
        length,
        bytes: page,
        digest,
        next: (next < total).then_some(next),
        complete: next == total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn ok_valid() -> Result<(), RecoveryBytesError> {
        Ok(())
    }

    #[test]
    fn binary_searches_starts_and_validates_range() {
        assert_eq!(start_at_or_before(&[0, 10, 20, 30], 4, 0), 0);
        assert_eq!(start_at_or_before(&[0, 10, 20, 30], 4, 19), 1);
        assert_eq!(start_at_or_before(&[0, 10, 20, 30], 4, 20), 2);
        assert_eq!(start_at_or_before(&[0, 10, 20, 30], 4, 99), 3);

        assert!(assert_range(0, 10, 10).is_ok());
        assert_eq!(
            assert_range(0, FRAGMENT_MAX_BYTES + 1, FRAGMENT_MAX_BYTES + 1),
            Err(RecoveryBytesError::SizeLimit)
        );
        assert_eq!(assert_range(5, 6, 10), Err(RecoveryBytesError::Range));
    }

    #[tokio::test]
    async fn verifies_leaves_and_streams_pages_with_one_leaf_resident() {
        let map: HashMap<LeafId, String> =
            HashMap::from([(1, "hello ".to_owned()), (2, "world!".to_owned())]);
        let all = [stored_bytes("hello "), stored_bytes("world!")].concat();
        let digest = catalog_digest(CatalogDomain::Leaf, &all);
        let read: ReadLeafFn = Arc::new(move |id| {
            let val = map.get(&id).cloned();
            Box::pin(async move { val.ok_or(RecoveryBytesError::Ownership) })
        });
        let frozen = open_frozen_bytes(all.len(), &digest, vec![1, 2], read, &ok_valid, None)
            .await
            .expect("open");
        let page = produce_page(frozen.as_ref(), 0, Some(&ok_valid))
            .await
            .expect("page");
        assert!(page.complete);
        assert_eq!(page.bytes, all);
        assert_eq!(frozen.peak_resident_leaves(), 1);
    }

    #[tokio::test]
    async fn rejects_tampered_digest_and_aborted_signal() {
        let read: ReadLeafFn = Arc::new(|_| Box::pin(async { Ok("abc".to_owned()) }));
        let wrong_digest = parse_content_digest(&"0".repeat(64)).expect("digest");
        let err = open_frozen_bytes(6, &wrong_digest, vec![1], read.clone(), &ok_valid, None)
            .await
            .err()
            .expect("integrity error");
        assert_eq!(err, RecoveryBytesError::Integrity);

        let cancel = CancelSignal::new();
        cancel.abort();
        let err = open_frozen_bytes(6, &wrong_digest, vec![1], read, &ok_valid, Some(&cancel))
            .await
            .err()
            .expect("aborted");
        assert_eq!(err, RecoveryBytesError::Aborted);
    }

    #[tokio::test]
    async fn empty_component_produces_single_empty_complete_page() {
        let digest = catalog_digest(CatalogDomain::Leaf, &[]);
        let read: ReadLeafFn = Arc::new(|_| Box::pin(async { Err(RecoveryBytesError::Ownership) }));
        let frozen = open_frozen_bytes(0, &digest, Vec::new(), read, &ok_valid, None)
            .await
            .expect("empty");
        let page = produce_page(frozen.as_ref(), 0, None).await.expect("page");
        assert_eq!(page.length, 0);
        assert!(page.complete);
        assert_eq!(page.next, None);
        assert!(matches!(
            produce_page(frozen.as_ref(), 1, None).await,
            Err(RecoveryBytesError::Range)
        ));
    }
}
