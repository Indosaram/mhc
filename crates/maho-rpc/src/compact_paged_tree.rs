//! senpi `modes/rpc/compact-paged-tree.ts`: cold AVL paths live in owned storage leaves; only an
//! operation's path is hydrated.
use crate::compact_content_store::{CompactContentStore, LeafId};
use crate::compact_paged_batch::CompactPagedBatch;
use crate::compact_read_lease::{CompactError, CompactReadReason, lock};
use crate::compact_values::{CompactValues, ValueRef};
use futures::future::BoxFuture;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PagedRoot {
    #[serde(rename = "ref")]
    pub reference: ValueRef,
    pub height: i64,
    pub size: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PagedNode<T> {
    pub key: ValueRef,
    pub value: T,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<PagedRoot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<PagedRoot>,
}

#[derive(Clone)]
pub(crate) struct Decoded<T> {
    pub(crate) node: PagedNode<T>,
    pub(crate) name: String,
    bytes: usize,
}

const CACHE_BYTES: usize = 32_768;

/// Round-local LRU: least recently used first.
struct Cache<T> {
    entries: Vec<(LeafId, Decoded<T>)>,
    bytes: usize,
}

type LeavesOf<T> = Box<dyn Fn(&T) -> Vec<LeafId> + Send + Sync>;

pub struct CompactPagedTree<T> {
    pub store: CompactContentStore,
    values: CompactValues,
    leaves_of: LeavesOf<T>,
    dirty: Mutex<Vec<LeafId>>,
    cache: Mutex<Option<Cache<T>>>,
    allocated_nodes: AtomicI64,
}

impl<T: Clone + Serialize + DeserializeOwned + Send + Sync + 'static> CompactPagedTree<T> {
    pub fn new(
        store: CompactContentStore,
        leaves_of: impl Fn(&T) -> Vec<LeafId> + Send + Sync + 'static,
    ) -> Self {
        Self {
            values: CompactValues::new(store.clone()),
            store,
            leaves_of: Box::new(leaves_of),
            dirty: Mutex::new(Vec::new()),
            cache: Mutex::new(None),
            allocated_nodes: AtomicI64::new(0),
        }
    }

    pub fn allocated_nodes(&self) -> i64 {
        self.allocated_nodes.load(Ordering::SeqCst)
    }

    pub fn adjust(
        &self,
        root: Option<&PagedRoot>,
        live: i64,
        pins: i64,
    ) -> Result<(), CompactError> {
        match root {
            Some(root) => self.store.adjust(root.reference.owner, live, pins),
            None => Ok(()),
        }
    }

    /// A round-local 32 KiB LRU retains reused upper paths, including newly built ones.
    pub async fn cached<R>(&self, operation: impl std::future::Future<Output = R>) -> R {
        *lock(&self.cache) = Some(Cache {
            entries: Vec::new(),
            bytes: 0,
        });
        let result = operation.await;
        *lock(&self.cache) = None;
        result
    }

    pub(crate) async fn read(&self, root: &PagedRoot) -> Result<Decoded<T>, CompactError> {
        let id = root.reference.node;
        if let Some(cache) = lock(&self.cache).as_mut()
            && let Some(index) = cache.entries.iter().position(|(leaf, _)| *leaf == id)
        {
            let entry = cache.entries.remove(index);
            let decoded = entry.1.clone();
            cache.entries.push(entry);
            return Ok(decoded);
        }
        let text = self.store.read_leaf(id).await?;
        let node: PagedNode<T> = serde_json::from_str(&text)?;
        let Value::String(name) = self.values.read(&node.key).await? else {
            return Err(CompactError::read(CompactReadReason::Ownership));
        };
        Ok(self.remember(id, node, name, text.len()))
    }

    fn remember(&self, id: LeafId, node: PagedNode<T>, name: String, units: usize) -> Decoded<T> {
        let mut cache = lock(&self.cache);
        if let Some(cache) = cache.as_mut()
            && let Some((_, existing)) = cache.entries.iter().find(|(leaf, _)| *leaf == id)
        {
            return existing.clone();
        }
        let bytes = (units + name.len()) * 2 + 128;
        let decoded = Decoded { node, name, bytes };
        if let Some(cache) = cache.as_mut()
            && bytes <= CACHE_BYTES
        {
            while cache.bytes + bytes > CACHE_BYTES && !cache.entries.is_empty() {
                let (_, oldest) = cache.entries.remove(0);
                cache.bytes -= oldest.bytes;
            }
            cache.entries.push((id, decoded.clone()));
            cache.bytes += bytes;
        }
        decoded
    }

    pub async fn get(
        &self,
        root: Option<&PagedRoot>,
        key: &str,
    ) -> Result<Option<T>, CompactError> {
        let mut cursor = root.cloned();
        while let Some(current) = cursor {
            let Decoded { node, name, .. } = self.read(&current).await?;
            if key == name {
                return Ok(Some(node.value));
            }
            cursor = if key < name.as_str() {
                node.left
            } else {
                node.right
            };
        }
        Ok(None)
    }

    /// Sorted requests share each descent instead of performing one root lookup per row.
    pub async fn get_many(
        &self,
        root: Option<&PagedRoot>,
        keys: &[String],
    ) -> Result<BTreeMap<String, T>, CompactError> {
        let mut ordered = keys.to_vec();
        ordered.sort();
        let mut found = BTreeMap::new();
        self.visit_many(root.cloned(), &ordered, &mut found).await?;
        Ok(found)
    }

    fn visit_many<'a>(
        &'a self,
        root: Option<PagedRoot>,
        ordered: &'a [String],
        found: &'a mut BTreeMap<String, T>,
    ) -> BoxFuture<'a, Result<(), CompactError>> {
        Box::pin(async move {
            let Some(root) = root else { return Ok(()) };
            if ordered.is_empty() {
                return Ok(());
            }
            let Decoded { node, name, .. } = self.read(&root).await?;
            let split = ordered.partition_point(|key| *key < name);
            self.visit_many(node.left.clone(), &ordered[..split], found)
                .await?;
            let mut right = split;
            while right < ordered.len() && ordered[right] == name {
                found.insert(ordered[right].clone(), node.value.clone());
                right += 1;
            }
            self.visit_many(node.right.clone(), &ordered[right..], found)
                .await
        })
    }

    /// In-order entries strictly after `after`.
    ///
    /// PERF-DIVERGENCE(senpi async generator): upstream yields lazily; this collects the descent.
    pub async fn entries(
        &self,
        root: Option<&PagedRoot>,
        after: Option<&str>,
    ) -> Result<Vec<(String, T)>, CompactError> {
        let mut out = Vec::new();
        self.collect(root.cloned(), after, &mut out).await?;
        Ok(out)
    }

    fn collect<'a>(
        &'a self,
        root: Option<PagedRoot>,
        after: Option<&'a str>,
        out: &'a mut Vec<(String, T)>,
    ) -> BoxFuture<'a, Result<(), CompactError>> {
        Box::pin(async move {
            let Some(root) = root else { return Ok(()) };
            let Decoded { node, name, .. } = self.read(&root).await?;
            if after.is_none_or(|after| name.as_str() > after) {
                self.collect(node.left.clone(), after, out).await?;
                out.push((name, node.value));
            }
            self.collect(node.right, after, out).await
        })
    }

    /// Applies one round's changes (`None` deletes) in one sorted pass; returns an owned root.
    pub async fn apply(
        &self,
        root: Option<&PagedRoot>,
        changes: Vec<(String, Option<T>)>,
    ) -> Result<Option<PagedRoot>, CompactError> {
        CompactPagedBatch::new(self, changes)
            .apply(root.cloned())
            .await
    }

    pub(crate) fn own_name(&self, name: &str) -> Result<ValueRef, CompactError> {
        self.values.own(&Value::String(name.to_owned()))
    }

    pub(crate) async fn make(&self, node: PagedNode<T>) -> Result<PagedRoot, CompactError> {
        let text = serde_json::to_string(&node)?;
        // Decode before allocating: a failed key read must not strand a new leaf.
        let name = if lock(&self.cache).is_some() {
            Some(self.values.read(&node.key).await?)
        } else {
            None
        };
        let mut refs = vec![0, node.key.owner];
        refs.extend((self.leaves_of)(&node.value));
        refs.push(node.left.as_ref().map_or(0, |left| left.reference.owner));
        refs.push(node.right.as_ref().map_or(0, |right| right.reference.owner));
        let (leaf, owner) = self.store.with(|state| {
            let leaf = state.create_leaf(&text, true)?;
            refs[0] = leaf;
            let owner = state.text_own(&refs)?;
            state.adjust(leaf, -1, 0)?;
            Ok::<_, CompactError>((leaf, owner))
        })?;
        lock(&self.dirty).push(leaf);
        self.allocated_nodes.fetch_add(1, Ordering::SeqCst);
        let height = 1 + node
            .left
            .as_ref()
            .map_or(0, |l| l.height)
            .max(node.right.as_ref().map_or(0, |r| r.height));
        let size = 1
            + node.left.as_ref().map_or(0, |l| l.size)
            + node.right.as_ref().map_or(0, |r| r.size);
        if let Some(Value::String(name)) = name {
            self.remember(leaf, node, name, text.len());
        }
        Ok(PagedRoot {
            reference: ValueRef {
                node: leaf,
                owner,
                value: None,
            },
            height,
            size,
        })
    }

    /// Commits a round as one segment file: nodes replaced within it were released and are skipped, so
    /// only survivors are written. Until then a round's built nodes stay resident.
    pub async fn flush(&self) -> Result<(), CompactError> {
        let dirty = std::mem::take(&mut *lock(&self.dirty));
        self.store.flush_leaves(dirty).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_content_store::CompactContentStoreOptions;

    // senpi 4272-index-publication / bounded-index-compaction: one sorted pass builds a balanced tree.
    #[tokio::test]
    async fn applies_sorted_rounds_and_reads_back_every_key() {
        let root_dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: root_dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let tree = CompactPagedTree::<i64>::new(store.clone(), |_| Vec::new());
        let first: Vec<(String, Option<i64>)> =
            (0..40).map(|n| (format!("{n:03}"), Some(n))).collect();
        let root = tree.cached(tree.apply(None, first)).await.expect("apply");
        tree.flush().await.expect("flush");
        let root = root.expect("root");
        assert_eq!(root.size, 40);
        assert!(root.height <= 6);
        let changes: Vec<(String, Option<i64>)> = (0..40)
            .step_by(2)
            .map(|n| (format!("{n:03}"), None))
            .chain([("100".to_owned(), Some(100))])
            .collect();
        let next = tree
            .apply(Some(&root), changes)
            .await
            .expect("apply")
            .expect("next");
        tree.adjust(Some(&root), -1, 0).expect("release old");
        tree.flush().await.expect("flush");
        let entries = tree.entries(Some(&next), None).await.expect("entries");
        let expected: Vec<(String, i64)> = (1..40)
            .step_by(2)
            .map(|n| (format!("{n:03}"), n))
            .chain([("100".to_owned(), 100)])
            .collect();
        assert_eq!(entries, expected);
        assert_eq!(next.size, 21);
        assert_eq!(tree.get(Some(&next), "007").await.expect("get"), Some(7));
        assert_eq!(tree.get(Some(&next), "008").await.expect("get"), None);
        let many = tree
            .get_many(Some(&next), &["039".into(), "001".into(), "002".into()])
            .await
            .expect("many");
        assert_eq!(many.len(), 2);
        let after = tree.entries(Some(&next), Some("035")).await.expect("after");
        assert_eq!(after.len(), 3);
        tree.adjust(Some(&next), -1, 0).expect("release");
        store.idle().await;
        assert_eq!(store.status().structured_bytes.spilled, 0);
        assert_eq!(store.status().structured_bytes.resident, 0);
        store.retire().await.expect("retire");
    }
}
