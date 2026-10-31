//! senpi `modes/rpc/compact-content-index.ts`: pending overlays stay resident; settled keys and
//! navigation paths live in spillable nodes.
//!
//! Rust shape: the index is a cloneable handle (`Arc`) so the compactor's storage job and the store's
//! commit subscription can reach it. A cut (`FrozenIndex`) carries its own read lease instead of
//! upstream's `WeakMap<cut, lease>`.
use crate::compact_commit_dependencies::CompactCommitDependencies;
use crate::compact_content_store::{CompactContentStore, LeafId};
use crate::compact_index_compactor::CompactIndexCompactor;
use crate::compact_index_traversal::{IndexPageRead, read_index_entries, read_index_page};
use crate::compact_index_tree::{CompactIndexTree, IndexRoot};
use crate::compact_paged_tree::{CompactPagedTree, PagedRoot};
use crate::compact_read_lease::{CompactError, CompactReadLease, CompactReadReason, lock};
use crate::compact_storage_io::CompactStorageOperation;
use crate::compact_values::{CompactValues, ValueRef};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexRow {
    pub key: ValueRef,
    pub value: ValueRef,
    pub payload: LeafId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexPage {
    pub path: String,
    pub bytes: i64,
    pub count: usize,
    pub owner: LeafId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaseRow {
    pub page: IndexPage,
    pub offset: usize,
}

#[derive(Debug, Clone)]
pub struct IndexOverride {
    pub row: Option<IndexRow>,
    pub patch: Vec<(String, ValueRef)>,
    pub patch_value: Option<ValueRef>,
    pub owner: LeafId,
    pub deleted: bool,
}

/// Overlay values are compared by identity (upstream `current !== folded`).
pub type Override = Arc<IndexOverride>;

#[derive(Clone)]
pub struct FrozenIndex {
    pub base: Option<PagedRoot>,
    pub overrides: IndexRoot<Override>,
    id: u64,
    lease: Arc<CompactReadLease>,
}

#[derive(Debug, Clone)]
pub struct IndexCursor {
    pub cut: u64,
    pub after: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IndexStatus {
    pub override_slots: i64,
    pub deletion_masks: i64,
    pub deletion_mask_bytes: i64,
    pub base_pages: usize,
    pub base_rows: i64,
    pub resident_index_nodes: i64,
    pub allocated_index_nodes: i64,
    pub resident_pages: i64,
    pub peak_resident_pages: i64,
}

pub(crate) struct IndexState {
    pub(crate) base: Option<PagedRoot>,
    pub(crate) overrides: IndexRoot<Override>,
    pub(crate) pages: Vec<IndexPage>,
    pub(crate) pending: CompactCommitDependencies<String>,
    tombstones: i64,
    mask_units: i64,
}

type LeavesOf<Row> = Box<dyn Fn(&Row) -> Vec<LeafId> + Send + Sync>;

pub struct IndexInner<Row> {
    pub store: CompactContentStore,
    pub values: CompactValues,
    pub bases: CompactPagedTree<BaseRow>,
    pub overlays: CompactIndexTree<Override>,
    pub(crate) state: Mutex<IndexState>,
    leaves_of: LeavesOf<Row>,
    pub(crate) compactor: CompactIndexCompactor,
    pub(crate) resident_pages: AtomicI64,
    pub(crate) peak_resident_pages: AtomicI64,
    next_cut: AtomicU64,
}

#[derive(Clone)]
pub struct CompactContentIndex<Row>(pub(crate) Arc<IndexInner<Row>>);

/// One resident decoded page of index rows; dropping it ends its residency.
pub struct HeldRows {
    pub rows: Vec<IndexRow>,
    counter: Arc<IndexCounter>,
}

pub(crate) struct IndexCounter(Weak<dyn ResidentPages>);

pub(crate) trait ResidentPages: Send + Sync {
    fn resident(&self) -> &AtomicI64;
}

impl<Row: Send + Sync + 'static> ResidentPages for IndexInner<Row> {
    fn resident(&self) -> &AtomicI64 {
        &self.resident_pages
    }
}

impl Drop for HeldRows {
    fn drop(&mut self) {
        if let Some(index) = self.counter.0.upgrade() {
            index.resident().fetch_sub(1, Ordering::SeqCst);
        }
    }
}

fn ownership() -> CompactError {
    CompactError::read(CompactReadReason::Ownership)
}

impl<Row> CompactContentIndex<Row>
where
    Row: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    pub fn new(
        store: CompactContentStore,
        leaves_of: impl Fn(&Row) -> Vec<LeafId> + Send + Sync + 'static,
        threshold: usize,
    ) -> Self {
        let retain_store = store.clone();
        let index = Self(Arc::new(IndexInner {
            values: CompactValues::new(store.clone()),
            bases: CompactPagedTree::new(store.clone(), |row: &BaseRow| vec![row.page.owner]),
            overlays: CompactIndexTree::new(move |value: &Override, live, pins| {
                retain_store.adjust(value.owner, live, pins)
            }),
            state: Mutex::new(IndexState {
                base: None,
                overrides: None,
                pages: Vec::new(),
                pending: CompactCommitDependencies::default(),
                tombstones: 0,
                mask_units: 0,
            }),
            leaves_of: Box::new(leaves_of),
            compactor: CompactIndexCompactor::new(threshold),
            resident_pages: AtomicI64::new(0),
            peak_resident_pages: AtomicI64::new(0),
            next_cut: AtomicU64::new(1),
            store: store.clone(),
        }));
        let weak = Arc::downgrade(&index.0);
        store.subscribe_committed(move |id| {
            if let Some(inner) = weak.upgrade() {
                let ready = !lock(&inner.state).pending.committed(id).is_empty();
                if ready {
                    CompactContentIndex(inner).maybe_compact();
                }
            }
        });
        index
    }

    pub fn store(&self) -> &CompactContentStore {
        &self.0.store
    }

    pub fn values(&self) -> &CompactValues {
        &self.0.values
    }

    pub fn set(&self, key: &str, row: &Row) -> Result<(), CompactError> {
        let value = self.0.values.own(&serde_json::to_value(row)?)?;
        let mut refs = vec![value.owner];
        let result = (|| {
            let key_ref = self.0.values.own(&Value::String(key.to_owned()))?;
            refs.push(key_ref.owner);
            let payload = self.0.store.text().own(&(self.0.leaves_of)(row))?;
            refs.push(payload);
            self.put(
                key,
                Some(IndexRow {
                    key: key_ref,
                    value: value.clone(),
                    payload,
                }),
                Vec::new(),
                false,
            )
        })();
        for reference in refs {
            self.0.store.adjust(reference, -1, 0)?;
        }
        result
    }

    pub fn patch(&self, key: &str, fields: &[(String, Value)]) -> Result<(), CompactError> {
        let current = {
            let state = lock(&self.0.state);
            self.0.overlays.get(&state.overrides, key).cloned()
        };
        let mut patch = current
            .as_ref()
            .map(|current| current.patch.clone())
            .unwrap_or_default();
        let mut owned = Vec::new();
        let result = (|| {
            for (name, value) in fields {
                let reference = self.0.values.own(value)?;
                owned.push(reference.owner);
                patch.retain(|(field, _)| field != name);
                patch.push((name.clone(), reference));
            }
            self.put(
                key,
                current.and_then(|current| current.row.clone()),
                patch,
                false,
            )
        })();
        for owner in owned {
            self.0.store.adjust(owner, -1, 0)?;
        }
        result
    }

    pub fn delete(&self, key: &str) -> Result<(), CompactError> {
        self.put(key, None, Vec::new(), true)
    }

    pub fn freeze(&self) -> Result<FrozenIndex, CompactError> {
        self.0.store.assert_readable()?;
        let (base, overrides) = {
            let state = lock(&self.0.state);
            (state.base.clone(), state.overrides.clone())
        };
        self.0.bases.adjust(base.as_ref(), 0, 1)?;
        self.0.overlays.adjust(&overrides, 0, 1)?;
        let (dispose_base, dispose_overrides) = (base.clone(), overrides.clone());
        let inner = self.0.clone();
        let valid_store = self.0.store.clone();
        let lease = CompactReadLease::with_valid(
            move || {
                let _ = inner.bases.adjust(dispose_base.as_ref(), 0, -1);
                let _ = inner.overlays.adjust(&dispose_overrides, 0, -1);
            },
            move || valid_store.assert_readable(),
        );
        Ok(FrozenIndex {
            base,
            overrides,
            id: self.0.next_cut.fetch_add(1, Ordering::SeqCst),
            lease: Arc::new(lease),
        })
    }

    pub fn release(&self, cut: &FrozenIndex) {
        cut.lease.release();
    }

    /// Point descriptor read under a cut lease; never hydrates a message or its block collection.
    pub async fn lookup(
        &self,
        cut: &FrozenIndex,
        key: &str,
    ) -> Result<Option<IndexRow>, CompactError> {
        cut.lease
            .run(async {
                if let Some(change) = self.0.overlays.get(&cut.overrides, key) {
                    if change.deleted {
                        return Ok(None);
                    }
                    if let Some(row) = &change.row {
                        return Ok(Some(row.clone()));
                    }
                }
                let Some(old) = self.0.bases.get(cut.base.as_ref(), key).await? else {
                    return Ok(None);
                };
                let held = self.read_page(&old.page).await?;
                Ok(held.rows.get(old.offset).cloned())
            })
            .await
    }

    /// Compatibility assembly only; recovery descriptors use `page()`, never this whole-row view.
    pub async fn entries(
        &self,
        cut: &FrozenIndex,
    ) -> Result<BTreeMap<String, Value>, CompactError> {
        cut.lease.run(read_index_entries(self, cut)).await
    }

    pub async fn page(
        &self,
        cut: &FrozenIndex,
        cursor: Option<&IndexCursor>,
    ) -> Result<IndexPageRead, CompactError> {
        if cursor.is_some_and(|cursor| cursor.cut != cut.id) {
            return Err(ownership());
        }
        cut.lease
            .run(read_index_page(self, cut, cursor.map(|c| c.after.as_str())))
            .await
    }

    pub async fn read_page(&self, page: &IndexPage) -> Result<HeldRows, CompactError> {
        if page.bytes > 65_536 {
            return Err(CompactError::read(CompactReadReason::SizeLimit));
        }
        if page.count > 128 {
            return Err(CompactError::read(CompactReadReason::CountLimit));
        }
        let resident = self.0.resident_pages.fetch_add(1, Ordering::SeqCst) + 1;
        self.0
            .peak_resident_pages
            .fetch_max(resident, Ordering::SeqCst);
        let weak: Weak<dyn ResidentPages> = Arc::downgrade(&self.0) as Weak<dyn ResidentPages>;
        let mut held = HeldRows {
            rows: Vec::new(),
            counter: Arc::new(IndexCounter(weak)),
        };
        let data = self
            .0
            .store
            .read_file(
                &page.path,
                usize::try_from(page.bytes).unwrap_or(0),
                CompactStorageOperation::PageRead,
                None,
            )
            .await?;
        let rows: Vec<IndexRow> = serde_json::from_slice(&data)?;
        if rows.len() > 128 {
            return Err(CompactError::read(CompactReadReason::CountLimit));
        }
        if rows.len() != page.count {
            return Err(ownership());
        }
        held.rows = rows;
        Ok(held)
    }

    pub fn status(&self) -> IndexStatus {
        let state = lock(&self.0.state);
        IndexStatus {
            override_slots: state.overrides.as_ref().map_or(0, |root| root.size),
            deletion_masks: state.tombstones,
            deletion_mask_bytes: state.mask_units * 2,
            base_pages: state.pages.len(),
            base_rows: state.base.as_ref().map_or(0, |base| base.size),
            resident_index_nodes: self.0.overlays.resident_nodes(),
            allocated_index_nodes: self.0.bases.allocated_nodes()
                + self.0.overlays.allocated_nodes(),
            resident_pages: self.0.resident_pages.load(Ordering::SeqCst)
                + self.0.values.resident_pages(),
            peak_resident_pages: self.0.peak_resident_pages.load(Ordering::SeqCst)
                + self.0.values.peak_resident_pages(),
        }
    }

    pub fn maybe_compact(&self) {
        CompactIndexCompactor::schedule(self);
    }

    pub(crate) fn drop_key(&self, key: &str) -> Result<(), CompactError> {
        let mut state = lock(&self.0.state);
        let Some(old) = self.0.overlays.get(&state.overrides, key).cloned() else {
            return Ok(());
        };
        if old.deleted {
            state.tombstones -= 1;
            state.mask_units -= crate::compact_content_store::utf16_len(key);
        }
        let next = self.0.overlays.set(&state.overrides, key, None)?;
        self.0.overlays.adjust(&state.overrides, -1, 0)?;
        state.overrides = next;
        state.pending.delete(&key.to_owned());
        Ok(())
    }

    fn put(
        &self,
        key: &str,
        row: Option<IndexRow>,
        patch: Vec<(String, ValueRef)>,
        deleted: bool,
    ) -> Result<(), CompactError> {
        let patch_value = if patch.is_empty() {
            None
        } else {
            Some(self.0.values.record(&patch)?)
        };
        let mut refs: Vec<LeafId> = patch_value.iter().map(|value| value.owner).collect();
        if let Some(row) = &row {
            refs.extend([row.key.owner, row.value.owner, row.payload]);
        }
        let owner = self.0.store.text().own(&refs)?;
        self.drop_key(key)?;
        {
            let mut state = lock(&self.0.state);
            let value = Arc::new(IndexOverride {
                row,
                patch,
                patch_value: patch_value.clone(),
                owner,
                deleted,
            });
            let next = self.0.overlays.set(&state.overrides, key, Some(value))?;
            self.0.overlays.adjust(&state.overrides, -1, 0)?;
            state.overrides = next;
            if deleted {
                state.tombstones += 1;
                state.mask_units += crate::compact_content_store::utf16_len(key);
            }
            let uncommitted: HashSet<LeafId> = refs
                .iter()
                .copied()
                .filter(|id| !self.0.store.is_committed(*id))
                .collect();
            state.pending.set(key.to_owned(), uncommitted);
        }
        self.0.store.adjust(owner, -1, 0)?;
        if let Some(value) = patch_value {
            self.0.store.adjust(value.owner, -1, 0)?;
        }
        self.maybe_compact();
        Ok(())
    }
}

impl FrozenIndex {
    pub fn id(&self) -> u64 {
        self.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_content_store::CompactContentStoreOptions;
    use serde_json::json;

    fn index(root: &tempfile::TempDir, threshold: usize) -> CompactContentIndex<Value> {
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: root.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        CompactContentIndex::new(store, |_| Vec::new(), threshold)
    }

    // senpi 4272-persistent-index-pages / bounded-index-compaction: overlays fold into paged bases.
    #[tokio::test]
    async fn folds_overlays_into_bounded_pages_and_keeps_cuts_stable() {
        let root = tempfile::tempdir().expect("tempdir");
        let index = index(&root, 4);
        for n in 0..10 {
            index
                .set(&format!("k{n:02}"), &json!({"n":n}))
                .expect("set");
        }
        let early = index.freeze().expect("freeze");
        index.store().idle().await;
        let status = index.status();
        assert_eq!(status.base_rows, 10);
        assert_eq!(status.override_slots, 0);
        assert!(status.base_pages >= 1);
        index
            .patch("k03", &[("n".into(), json!(33))])
            .expect("patch");
        index.delete("k04").expect("delete");
        index.store().idle().await;
        let cut = index.freeze().expect("freeze");
        let entries = index.entries(&cut).await.expect("entries");
        assert_eq!(entries.len(), 9);
        assert_eq!(entries["k03"], json!({"n":33}));
        assert!(!entries.contains_key("k04"));
        assert!(index.lookup(&cut, "k04").await.expect("lookup").is_none());
        let early_entries = index.entries(&early).await.expect("early");
        assert_eq!(early_entries.len(), 10);
        index.release(&early);
        assert_eq!(
            index
                .entries(&early)
                .await
                .expect_err("released")
                .read_reason(),
            Some(CompactReadReason::Released)
        );
        let first = index.page(&cut, None).await.expect("page");
        assert_eq!(first.rows.len(), 9);
        assert!(first.next.is_none());
        index.release(&cut);
        index.store().retire().await.expect("retire");
    }
}
