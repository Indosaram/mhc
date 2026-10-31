//! senpi `modes/rpc/compact-index-compactor.ts`: one bounded selection and one optional small page,
//! with a single atomic representation swap.
use crate::compact_content_index::{
    BaseRow, CompactContentIndex, IndexOverride, IndexPage, IndexRow,
};
use crate::compact_content_store::LeafId;
use crate::compact_index_pages::pack_pages;
use crate::compact_paged_tree::PagedRoot;
use crate::compact_read_lease::{CompactError, CompactReadReason, lock};
use crate::compact_storage_io::CompactStorageOperation;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct CompactIndexCompactor {
    threshold: usize,
    running: AtomicBool,
}

struct Written {
    page: IndexPage,
    keys: Vec<String>,
    owned: bool,
}

impl CompactIndexCompactor {
    pub fn new(threshold: usize) -> Self {
        Self {
            threshold,
            running: AtomicBool::new(false),
        }
    }

    pub fn schedule<Row>(index: &CompactContentIndex<Row>)
    where
        Row: Serialize + DeserializeOwned + Send + Sync + 'static,
    {
        let compactor = &index.0.compactor;
        if compactor.running.load(Ordering::SeqCst) || !index.store().spill_allowed() {
            return;
        }
        let (slots, pending) = {
            let state = lock(&index.0.state);
            (
                state.overrides.as_ref().map_or(0, |root| root.size),
                state.pending.size(),
            )
        };
        let masks = index.status().deletion_masks;
        let ready = usize::try_from(slots).unwrap_or(0).saturating_sub(pending);
        if ready < compactor.threshold && masks == 0 {
            return;
        }
        if compactor.running.swap(true, Ordering::SeqCst) {
            return;
        }
        let index = index.clone();
        index.store().clone().enqueue(
            CompactStorageOperation::PageWrite,
            crate::compact_spool_storage::job(move |_store| async move {
                let progress = index.0.bases.cached(compact(&index)).await;
                index.0.compactor.running.store(false, Ordering::SeqCst);
                if progress? {
                    Self::schedule(&index);
                }
                Ok(())
            }),
        );
    }
}

async fn compact<Row>(i: &CompactContentIndex<Row>) -> Result<bool, CompactError>
where
    Row: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    let store = i.store().clone();
    if !store.spill_allowed() {
        return Ok(false);
    }
    if lock(&i.0.state).pending.size() > 0 {
        store.flush_values().await?;
    }
    if !store.spill_allowed() {
        return Ok(false);
    }
    let cut = i.freeze()?;
    let mut draft: Option<PagedRoot> = lock(&i.0.state).base.clone();
    i.0.bases.adjust(draft.as_ref(), 1, 0)?;
    let mut merged: Vec<LeafId> = Vec::new();
    let mut written: Vec<Written> = Vec::new();
    let result = compact_round(i, &cut, &mut draft, &mut merged, &mut written).await;
    let _ = i.0.bases.adjust(draft.as_ref(), -1, 0);
    for candidate in &written {
        if !candidate.owned {
            store.discard(&candidate.page.path, candidate.page.bytes);
        }
    }
    for owner in merged {
        let _ = store.adjust(owner, -1, 0);
    }
    i.release(&cut);
    match result {
        Err(error) if error.read_reason() == Some(CompactReadReason::Retired) => Ok(false),
        other => other,
    }
}

async fn compact_round<Row>(
    i: &CompactContentIndex<Row>,
    cut: &crate::compact_content_index::FrozenIndex,
    draft: &mut Option<PagedRoot>,
    merged: &mut Vec<LeafId>,
    written: &mut Vec<Written>,
) -> Result<bool, CompactError>
where
    Row: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    let store = i.store().clone();
    let mut selected: BTreeMap<String, Arc<IndexOverride>> = BTreeMap::new();
    let mut touched: Option<IndexPage> = None;
    let mut rows: BTreeMap<String, IndexRow> = BTreeMap::new();
    let mut removed: BTreeSet<String> = BTreeSet::new();
    let entries = i.0.overlays.entries(&cut.overrides, None);
    for batch in entries.chunks(128) {
        if selected.len() >= 128 {
            break;
        }
        let keys: Vec<String> = batch.iter().map(|(key, _)| key.clone()).collect();
        let locations = i.0.bases.get_many(cut.base.as_ref(), &keys).await?;
        for (key, value) in batch {
            let page = locations.get(key).map(|row| row.page.clone());
            let pending = lock(&i.0.state).pending.has(key);
            let same_page = match (&page, &touched) {
                (None, _) | (_, None) => true,
                (Some(page), Some(touched)) => page.path == touched.path,
            };
            if !pending && same_page {
                selected.insert(key.clone(), value.clone());
                if touched.is_none() {
                    touched = page;
                }
            }
            if selected.len() == 128 {
                break;
            }
        }
    }
    let small = lock(&i.0.state)
        .pages
        .iter()
        .find(|page| page.count < 64 && page.bytes < 32_768)
        .cloned();
    let mut sources: Vec<IndexPage> = small.iter().cloned().collect();
    if let Some(touched) = &touched
        && small
            .as_ref()
            .is_none_or(|small| small.path != touched.path)
    {
        sources.push(touched.clone());
    }
    let mut loaded = Vec::new();
    for page in &sources {
        let data = i.read_page(page).await?;
        let mut keyed = Vec::new();
        for row in &data.rows {
            let Value::String(key) = i.values().read(&row.key).await? else {
                return Err(CompactError::read(CompactReadReason::Ownership));
            };
            keyed.push((key, row.clone()));
        }
        loaded.push(data);
        let keys: Vec<String> = keyed.iter().map(|(key, _)| key.clone()).collect();
        let locations = i.0.bases.get_many(cut.base.as_ref(), &keys).await?;
        for (key, row) in keyed {
            if locations
                .get(&key)
                .is_some_and(|location| location.page.path == page.path)
            {
                rows.insert(key.clone(), row);
                removed.insert(key);
            }
        }
    }
    for (key, change) in &selected {
        removed.insert(key.clone());
        if change.deleted {
            rows.remove(key);
            continue;
        }
        let mut row = change.row.clone().or_else(|| rows.get(key).cloned());
        if let Some(current) = &row
            && !change.patch.is_empty()
        {
            let value = i.values().merge(&current.value, &change.patch).await?;
            merged.push(value.owner);
            row = Some(IndexRow {
                value,
                ..current.clone()
            });
        }
        if let Some(row) = row {
            rows.insert(key.clone(), row);
        }
    }
    let ordered: Vec<(String, IndexRow)> = rows.clone().into_iter().collect();
    let mut offset = 0;
    let only_rows: Vec<IndexRow> = ordered.iter().map(|(_, row)| row.clone()).collect();
    for chunk in pack_pages(&only_rows)? {
        let data = serde_json::to_vec(&chunk)?;
        let bytes = i64::try_from(data.len()).unwrap_or(i64::MAX);
        let resident = i.0.resident_pages.load(Ordering::SeqCst) + 1;
        i.0.peak_resident_pages
            .fetch_max(resident, Ordering::SeqCst);
        let Some(path) = store
            .write_file(data, CompactStorageOperation::PageWrite, bytes)
            .await
        else {
            return Ok(false);
        };
        written.push(Written {
            page: IndexPage {
                path,
                bytes,
                count: chunk.len(),
                owner: 0,
            },
            keys: ordered[offset..offset + chunk.len()]
                .iter()
                .map(|(key, _)| key.clone())
                .collect(),
            owned: false,
        });
        offset += chunk.len();
    }
    drop(loaded);
    if !store.spill_allowed() {
        return Ok(false);
    }
    let mut changes: Vec<(String, Option<BaseRow>)> = Vec::new();
    let mut owners: Vec<LeafId> = Vec::new();
    let built = async {
        for candidate in written.iter_mut() {
            let mut kept = Vec::new();
            let mut refs = Vec::new();
            for (offset, key) in candidate.keys.iter().enumerate() {
                let row = rows
                    .get(key)
                    .ok_or_else(|| CompactError::read(CompactReadReason::Ownership))?;
                // Even a concurrently removed row's on-disk key must survive page traversal.
                refs.push(row.key.owner);
                let folded = selected.get(key);
                let current = {
                    let state = lock(&i.0.state);
                    i.0.overlays.get(&state.overrides, key).cloned()
                };
                if let Some(folded) = folded {
                    let same = current
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, folded));
                    let same_row = current
                        .as_ref()
                        .is_some_and(|current| !current.deleted && current.row == folded.row);
                    if !same && !same_row {
                        continue;
                    }
                }
                refs.extend([row.value.owner, row.payload]);
                kept.push((key.clone(), offset));
            }
            let file = store.own_file(&candidate.page.path, candidate.page.bytes)?;
            refs.push(file);
            let owner = store.text().own(&refs)?;
            owners.push(owner);
            store.adjust(file, -1, 0)?;
            candidate.owned = true;
            let page = IndexPage {
                owner,
                ..candidate.page.clone()
            };
            for (key, offset) in &kept {
                changes.push((
                    key.clone(),
                    Some(BaseRow {
                        page: page.clone(),
                        offset: *offset,
                    }),
                ));
                removed.remove(key);
            }
            if !kept.is_empty() {
                candidate.page = page;
            }
        }
        for key in &removed {
            changes.push((key.clone(), None));
        }
        // One sorted pass builds each changed node once; the pages' owners outlive it.
        let next = i.0.bases.apply(draft.as_ref(), changes).await?;
        i.0.bases.adjust(draft.as_ref(), -1, 0)?;
        *draft = next;
        Ok::<_, CompactError>(())
    }
    .await;
    for owner in owners {
        store.adjust(owner, -1, 0)?;
    }
    built?;
    i.0.bases.flush().await?;
    if !store.spill_allowed() {
        return Ok(false);
    }
    {
        let mut state = lock(&i.0.state);
        let previous = std::mem::replace(&mut state.base, draft.take());
        i.0.bases.adjust(previous.as_ref(), -1, 0)?;
        state.pages.retain(|page| {
            small.as_ref().is_none_or(|small| small.path != page.path)
                && touched
                    .as_ref()
                    .is_none_or(|touched| touched.path != page.path)
        });
        for candidate in written.iter() {
            if candidate.page.owner != 0 {
                state.pages.push(candidate.page.clone());
            }
        }
    }
    for (key, value) in &selected {
        let current = {
            let state = lock(&i.0.state);
            i.0.overlays.get(&state.overrides, key).cloned()
        };
        if current.is_some_and(|current| Arc::ptr_eq(&current, value)) {
            i.drop_key(key)?;
        }
    }
    Ok(!selected.is_empty())
}
