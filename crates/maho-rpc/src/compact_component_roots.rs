//! senpi `modes/rpc/compact-component-roots.ts`: active AVL headers project only changed paths into
//! bounded, spillable nodes. Frozen and settled owners retain the disk-addressable root, not the hot
//! navigation tree or an array of its values.
use crate::compact_content_store::{CompactContentStore, LeafId};
use crate::compact_index_tree::{CompactIndexTree, IndexNode, IndexRoot};
use crate::compact_read_lease::{CompactError, CompactReadReason, lock};
use crate::compact_values::{CompactValues, ValueRef};
use futures::future::BoxFuture;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct Stored<T> {
    pub value: T,
    pub reference: ValueRef,
    pub owner: LeafId,
}

#[derive(Serialize, Deserialize)]
struct Page {
    key: ValueRef,
    value: ValueRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    left: Option<ValueRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    right: Option<ValueRef>,
}

pub struct ComponentRoot<T> {
    pub header: IndexRoot<Stored<T>>,
    pub reference: Option<ValueRef>,
}

impl<T> Default for ComponentRoot<T> {
    fn default() -> Self {
        Self {
            header: None,
            reference: None,
        }
    }
}

impl<T> Clone for ComponentRoot<T> {
    fn clone(&self) -> Self {
        Self {
            header: self.header.clone(),
            reference: self.reference.clone(),
        }
    }
}

type LeavesOf<T> = Box<dyn Fn(&T) -> Vec<LeafId> + Send + Sync>;

pub struct CompactComponentRoots<T> {
    store: CompactContentStore,
    values: CompactValues,
    tree: Arc<CompactIndexTree<Stored<T>>>,
    /// Projected pages by node identity (upstream WeakMap); an entry dies with its node.
    refs: Mutex<HashMap<usize, (std::sync::Weak<IndexNode<Stored<T>>>, ValueRef)>>,
    leaves_of: LeavesOf<T>,
}

type Valid<'a> = &'a (dyn Fn() -> Result<(), CompactError> + Sync);

impl<T> CompactComponentRoots<T>
where
    T: Clone + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    pub fn new(
        store: CompactContentStore,
        leaves_of: impl Fn(&T) -> Vec<LeafId> + Send + Sync + 'static,
    ) -> Self {
        let retain_store = store.clone();
        Self {
            values: CompactValues::new(store.clone()),
            tree: Arc::new(CompactIndexTree::new(
                move |value: &Stored<T>, live, pins| retain_store.adjust(value.owner, live, pins),
            )),
            store,
            refs: Mutex::new(HashMap::new()),
            leaves_of: Box::new(leaves_of),
        }
    }

    pub fn resident_nodes(&self) -> i64 {
        self.tree.resident_nodes()
    }

    pub fn set(
        &self,
        root: &ComponentRoot<T>,
        key: &str,
        value: Option<T>,
    ) -> Result<ComponentRoot<T>, CompactError> {
        let stored = match value {
            Some(value) => {
                let reference = self.values.own(&serde_json::to_value(&value)?)?;
                let mut refs = vec![reference.owner];
                refs.extend((self.leaves_of)(&value));
                let owner = self.store.text().own(&refs)?;
                Some(Stored {
                    value,
                    reference,
                    owner,
                })
            }
            None => None,
        };
        let header = self.tree.set(&root.header, key, stored.clone())?;
        if let Some(stored) = &stored {
            self.store.adjust(stored.reference.owner, -1, 0)?;
            self.store.adjust(stored.owner, -1, 0)?;
        }
        let mut created = Vec::new();
        let next = match &header {
            Some(node) => Some(self.project(node, &mut created)?),
            None => None,
        };
        if let Some(next) = &next {
            self.store.adjust(next.owner, 1, 0)?;
        }
        for reference in created {
            self.store.adjust(reference.owner, -1, 0)?;
        }
        self.tree.adjust(&root.header, -1, 0)?;
        if let Some(reference) = &root.reference {
            self.store.adjust(reference.owner, -1, 0)?;
        }
        Ok(ComponentRoot {
            header,
            reference: next,
        })
    }

    /// Header reclamation is off the acceptance path; the transferred payload root already owns every value.
    pub fn release(&self, root: ComponentRoot<T>) {
        let tree = self.tree.clone();
        self.store.enqueue(
            crate::compact_storage_io::CompactStorageOperation::Reclaim,
            crate::compact_spool_storage::job(move |store| async move {
                tree.adjust(&root.header, -1, 0)?;
                if let Some(reference) = &root.reference {
                    store.adjust(reference.owner, -1, 0)?;
                }
                Ok(())
            }),
        );
        self.store.start_pump();
    }

    /// In-order entries of a projected root.
    pub fn entries<'a>(
        &'a self,
        reference: Option<&'a ValueRef>,
        valid: Valid<'a>,
    ) -> BoxFuture<'a, Result<Vec<(String, T)>, CompactError>> {
        Box::pin(async move {
            let Some(reference) = reference else {
                return Ok(Vec::new());
            };
            valid()?;
            let page: Page = serde_json::from_str(&self.store.read_leaf(reference.node).await?)?;
            valid()?;
            let mut out = self.entries(page.left.as_ref(), valid).await?;
            let Value::String(key) = self.values.read(&page.key).await? else {
                return Err(CompactError::read(CompactReadReason::Ownership));
            };
            let value: T = serde_json::from_value(self.values.read(&page.value).await?)?;
            valid()?;
            out.push((key, value));
            out.extend(self.entries(page.right.as_ref(), valid).await?);
            Ok(out)
        })
    }

    fn project(
        &self,
        node: &Arc<IndexNode<Stored<T>>>,
        created: &mut Vec<ValueRef>,
    ) -> Result<ValueRef, CompactError> {
        let identity = Arc::as_ptr(node) as usize;
        {
            let mut refs = lock(&self.refs);
            refs.retain(|_, (weak, _)| weak.strong_count() > 0);
            if let Some((_, known)) = refs.get(&identity) {
                return Ok(known.clone());
            }
        }
        let left = match &node.left {
            Some(left) => Some(self.project(left, created)?),
            None => None,
        };
        let right = match &node.right {
            Some(right) => Some(self.project(right, created)?),
            None => None,
        };
        // Component keys are fixed-width ordinals; unchanged path keys need no new value owner.
        let key = ValueRef::inline(Value::String(node.key.clone()));
        let text = serde_json::to_string(&Page {
            key,
            value: node.value.reference.clone(),
            left: left.clone(),
            right: right.clone(),
        })?;
        let refs = [
            0,
            node.value.owner,
            left.as_ref().map_or(0, |left| left.owner),
            right.as_ref().map_or(0, |right| right.owner),
        ];
        let reference = self.store.with(|state| {
            let page = state.create_leaf(&text, true)?;
            let mut refs = refs;
            refs[0] = page;
            let owner = state.text_own(&refs)?;
            state.adjust(page, -1, 0)?;
            Ok::<_, CompactError>(ValueRef {
                node: page,
                owner,
                value: None,
            })
        })?;
        lock(&self.refs).insert(identity, (Arc::downgrade(node), reference.clone()));
        created.push(reference.clone());
        Ok(reference)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_content_store::CompactContentStoreOptions;

    // senpi 4272-active-component-roots: projected roots read back in key order and release to zero.
    #[tokio::test]
    async fn projects_changed_paths_and_reads_back_in_order() {
        let root_dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: root_dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let roots = CompactComponentRoots::<i64>::new(store.clone(), |_| Vec::new());
        let mut root = ComponentRoot::default();
        for n in [3i64, 1, 2, 0] {
            root = roots
                .set(&root, &format!("{n:016}"), Some(n * 10))
                .expect("set");
        }
        root = roots
            .set(&root, &format!("{:016}", 2), None)
            .expect("delete");
        let ok = || -> Result<(), CompactError> { Ok(()) };
        let entries = roots
            .entries(root.reference.as_ref(), &ok)
            .await
            .expect("entries");
        assert_eq!(
            entries.iter().map(|(_, v)| *v).collect::<Vec<_>>(),
            [0, 10, 30]
        );
        roots.release(root);
        store.idle().await;
        assert_eq!(roots.resident_nodes(), 0);
        assert_eq!(store.status().structured_bytes.resident, 0);
        assert_eq!(store.status().structured_bytes.spilled, 0);
        store.retire().await.expect("retire");
    }
}
