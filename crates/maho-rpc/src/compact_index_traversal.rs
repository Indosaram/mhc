//! senpi `modes/rpc/compact-index-traversal.ts`: at most 128 descriptors per page; the index owns the
//! surrounding operation lease.
use crate::compact_content_index::{
    CompactContentIndex, FrozenIndex, HeldRows, IndexCursor, IndexRow,
};
use crate::compact_read_lease::{CompactError, CompactReadReason};
use crate::compact_values::ValueRef;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize)]
pub struct IndexEntry {
    pub row: IndexRow,
    pub patch: Option<ValueRef>,
}

pub struct IndexPageRead {
    pub rows: Vec<IndexEntry>,
    pub next: Option<IndexCursor>,
}

/// At most 128 descriptors after `after`, merged from the cut's base and overlay.
pub async fn read_index_page<Row>(
    index: &CompactContentIndex<Row>,
    cut: &FrozenIndex,
    after: Option<&str>,
) -> Result<IndexPageRead, CompactError>
where
    Row: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    let inner = &index.0;
    let base = inner.bases.entries(cut.base.as_ref(), after).await?;
    let overlay = inner.overlays.entries(&cut.overrides, after);
    let (mut a, mut b) = (0, 0);
    let mut rows = Vec::new();
    let mut last: Option<String> = None;
    let mut loaded: Option<(String, HeldRows)> = None;
    while (a < base.len() || b < overlay.len()) && rows.len() < 128 {
        let key = match (base.get(a), overlay.get(b)) {
            (Some((x, _)), Some((y, _))) => x.min(y).clone(),
            (Some((x, _)), None) => x.clone(),
            (None, Some((y, _))) => y.clone(),
            (None, None) => break,
        };
        let old = base.get(a).filter(|(k, _)| *k == key).map(|(_, v)| v);
        let change = overlay.get(b).filter(|(k, _)| *k == key).map(|(_, v)| v);
        if !change.is_some_and(|change| change.deleted) {
            let direct = change.and_then(|change| change.row.clone());
            let row = match (direct, old) {
                (Some(row), _) => Some(row),
                (None, Some(old)) => {
                    if loaded
                        .as_ref()
                        .is_none_or(|(path, _)| *path != old.page.path)
                    {
                        loaded = Some((old.page.path.clone(), index.read_page(&old.page).await?));
                    }
                    loaded
                        .as_ref()
                        .and_then(|(_, held)| held.rows.get(old.offset).cloned())
                }
                (None, None) => None,
            };
            if let Some(row) = row {
                rows.push(IndexEntry {
                    row,
                    patch: change.and_then(|change| change.patch_value.clone()),
                });
            }
        }
        if old.is_some() {
            a += 1;
        }
        if change.is_some() {
            b += 1;
        }
        last = Some(key);
    }
    drop(loaded);
    inner.store.assert_readable()?;
    if serde_json::to_string(&rows)?.len() > 65_536 {
        return Err(CompactError::read(CompactReadReason::SizeLimit));
    }
    let done = a >= base.len() && b >= overlay.len();
    Ok(IndexPageRead {
        rows,
        next: match (done, last) {
            (false, Some(after)) => Some(IndexCursor {
                cut: cut.id(),
                after,
            }),
            _ => None,
        },
    })
}

/// Compatibility assembly runs under the index's operation lease; recovery consumes descriptors.
pub async fn read_index_entries<Row>(
    index: &CompactContentIndex<Row>,
    cut: &FrozenIndex,
) -> Result<BTreeMap<String, Value>, CompactError>
where
    Row: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    let mut result = BTreeMap::new();
    let mut after: Option<String> = None;
    loop {
        let page = read_index_page(index, cut, after.as_deref()).await?;
        for entry in &page.rows {
            let mut row = index.values().read(&entry.row.value).await?;
            if let Some(patch) = &entry.patch
                && let (Value::Object(target), Value::Object(fields)) =
                    (&mut row, index.values().read(patch).await?)
            {
                target.extend(fields);
            }
            let Value::String(key) = index.values().read(&entry.row.key).await? else {
                return Err(CompactError::read(CompactReadReason::Ownership));
            };
            result.insert(key, row);
        }
        match page.next {
            Some(next) => after = Some(next.after),
            None => return Ok(result),
        }
    }
}
