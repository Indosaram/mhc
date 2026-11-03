//! senpi `modes/rpc/compact-text-roots.ts`: persistent append tree - positive refs are leaves,
//! negative refs are nodes, zero is empty.
//!
//! Rust shape: the node map lives in the store state (`TextRootsState`), so a node's reference moves
//! happen in the same critical section as its leaves'; `CompactContentStore::text()` is the upstream
//! `store.text` member.
use crate::compact_commit_dependencies::CompactCommitDependencies;
use crate::compact_content_store::{
    COMPACT_LEAF_CODE_UNITS, CompactContentStore, LeafId, StoreState, utf16_len, utf16_slice,
};
use crate::compact_read_lease::{CompactError, CompactReadReason};
use std::collections::HashMap;

struct Node {
    left: LeafId,
    right: LeafId,
    units: i64,
    height: i64,
    full: bool,
    live: i64,
    pins: i64,
}

#[derive(Default)]
pub(crate) struct TextRootsState {
    nodes: HashMap<LeafId, Node>,
    pub(crate) pending: CompactCommitDependencies<LeafId>,
    next: Option<LeafId>,
}

fn ownership() -> CompactError {
    CompactError::read(CompactReadReason::Ownership)
}

impl StoreState {
    fn node(&self, id: LeafId) -> Result<&Node, CompactError> {
        self.text.nodes.get(&id).ok_or_else(ownership)
    }

    pub(crate) fn text_units(&self, id: LeafId) -> Result<i64, CompactError> {
        match id {
            0 => Ok(0),
            id if id > 0 => self.leaf_units(id),
            id => Ok(self.node(id)?.units),
        }
    }

    pub(crate) fn text_is_committed(&self, id: LeafId) -> bool {
        self.text.nodes.contains_key(&id) && !self.text.pending.has(&id)
    }

    /// One owned dependency root; callers retain their existing references.
    pub(crate) fn text_own(&mut self, refs: &[LeafId]) -> Result<LeafId, CompactError> {
        match refs {
            [] => Ok(0),
            [one] => {
                self.adjust(*one, 1, 0)?;
                Ok(*one)
            }
            _ => {
                let middle = refs.len() / 2;
                let left = self.text_own(&refs[..middle])?;
                let right = self.text_own(&refs[middle..])?;
                if left == 0 {
                    return Ok(right);
                }
                if right == 0 {
                    return Ok(left);
                }
                let root = self.join(left, right)?;
                self.adjust(left, -1, 0)?;
                self.adjust(right, -1, 0)?;
                Ok(root)
            }
        }
    }

    /// Transfers one live root and one newly owned bounded leaf into the new root.
    pub(crate) fn text_append(
        &mut self,
        root: LeafId,
        text: &str,
        structured: bool,
    ) -> Result<LeafId, CompactError> {
        self.text_units(root)?;
        let leaf = self.create_leaf(text, structured)?;
        if root == 0 {
            return Ok(leaf);
        }
        let result = self.append_leaf(root, leaf)?;
        self.adjust(root, -1, 0)?;
        self.adjust(leaf, -1, 0)?;
        Ok(result)
    }

    pub(crate) fn text_adjust(
        &mut self,
        id: LeafId,
        live: i64,
        pins: i64,
    ) -> Result<(), CompactError> {
        let node = self.text.nodes.get_mut(&id).ok_or_else(ownership)?;
        let before_live = i64::from(node.live > 0);
        let before_pin = i64::from(node.live == 0 && node.pins > 0);
        if node.live + live < 0 || node.pins + pins < 0 {
            return Err(ownership());
        }
        node.live += live;
        node.pins += pins;
        let live_change = i64::from(node.live > 0) - before_live;
        let pin_change = i64::from(node.live == 0 && node.pins > 0) - before_pin;
        let (left, right, dead) = (node.left, node.right, node.live == 0 && node.pins == 0);
        // Extra cuts on a live subtree do not visit children. Only category transitions propagate.
        if live_change != 0 || pin_change != 0 {
            self.adjust(left, live_change, pin_change)?;
            self.adjust(right, live_change, pin_change)?;
        }
        if dead {
            self.text.pending.delete(&id);
            self.text.nodes.remove(&id);
        }
        Ok(())
    }

    fn append_leaf(&mut self, root: LeafId, leaf: LeafId) -> Result<LeafId, CompactError> {
        let Some(node) = self.text.nodes.get(&root).filter(|node| !node.full) else {
            return self.join(root, leaf);
        };
        let (left, right) = (node.left, node.right);
        let right = self.append_leaf(right, leaf)?;
        let result = self.join(left, right)?;
        self.adjust(right, -1, 0)?;
        Ok(result)
    }

    fn height(&self, id: LeafId) -> Result<i64, CompactError> {
        if id > 0 {
            Ok(0)
        } else {
            Ok(self.node(id)?.height)
        }
    }

    fn join(&mut self, left: LeafId, right: LeafId) -> Result<LeafId, CompactError> {
        let id = self.text.next.unwrap_or(-1);
        self.text.next = Some(id - 1);
        let (lh, rh) = (self.height(left)?, self.height(right)?);
        let full_of = |state: &Self, id: LeafId| state.text.nodes.get(&id).is_none_or(|n| n.full);
        let node = Node {
            left,
            right,
            units: self.text_units(left)? + self.text_units(right)?,
            height: 1 + lh.max(rh),
            full: lh == rh && full_of(self, left) && full_of(self, right),
            live: 1,
            pins: 0,
        };
        self.text.nodes.insert(id, node);
        self.adjust(left, 1, 0)?;
        self.adjust(right, 1, 0)?;
        let uncommitted: Vec<LeafId> = [left, right]
            .into_iter()
            .filter(|child| !self.is_committed(*child))
            .collect();
        self.text.pending.set(id, uncommitted);
        Ok(id)
    }

    fn select(
        &self,
        id: LeafId,
        start: i64,
        end: i64,
        max_leaves: usize,
        selected: &mut Vec<(LeafId, i64, i64)>,
    ) -> Result<(), CompactError> {
        if start >= end || id == 0 {
            return Ok(());
        }
        if id > 0 {
            if selected.len() == max_leaves {
                return Err(CompactError::read(CompactReadReason::CountLimit));
            }
            selected.push((id, start, end - start));
            return Ok(());
        }
        let node = self.node(id)?;
        let left_units = self.text_units(node.left)?;
        let (left, right) = (node.left, node.right);
        self.select(left, start, end.min(left_units), max_leaves, selected)?;
        self.select(
            right,
            (start - left_units).max(0),
            end - left_units,
            max_leaves,
            selected,
        )
    }
}

/// The upstream `store.text` member: a view of the store's persistent text roots.
#[derive(Clone)]
pub struct CompactTextRoots {
    store: CompactContentStore,
}

impl CompactContentStore {
    pub fn text(&self) -> CompactTextRoots {
        CompactTextRoots {
            store: self.clone(),
        }
    }
}

type Valid<'a> = &'a (dyn Fn() -> Result<(), CompactError> + Sync);

impl CompactTextRoots {
    pub fn resident_nodes(&self) -> usize {
        self.store.lock().text.nodes.len()
    }

    pub fn own(&self, refs: &[LeafId]) -> Result<LeafId, CompactError> {
        self.store.with(|state| state.text_own(refs))
    }

    pub fn units(&self, id: LeafId) -> Result<i64, CompactError> {
        self.store.lock().text_units(id)
    }

    pub fn is_committed(&self, id: LeafId) -> bool {
        self.store.lock().text_is_committed(id)
    }

    pub fn append(
        &self,
        root: LeafId,
        text: &str,
        structured: bool,
    ) -> Result<LeafId, CompactError> {
        self.store
            .with(|state| state.text_append(root, text, structured))
    }

    pub fn adjust(&self, id: LeafId, live: i64, pins: i64) -> Result<(), CompactError> {
        self.store.with(|state| state.text_adjust(id, live, pins))
    }

    /// Bounded UTF-16 result and bounded leaf/tail count, checked before any I/O.
    pub async fn read(
        &self,
        root: LeafId,
        tail: &str,
        offset: i64,
        units: i64,
        max_leaves: usize,
        valid: Option<Valid<'_>>,
    ) -> Result<String, CompactError> {
        let store_valid = || self.store.assert_readable();
        let valid: Valid<'_> = match valid {
            Some(valid) => valid,
            None => &store_valid,
        };
        self.store.assert_readable()?;
        valid()?;
        let (selected, length) = {
            let state = self.store.lock();
            let length = state.text_units(root)?;
            if offset < 0 || units < 0 || offset + units > length + utf16_len(tail) {
                return Err(CompactError::read(CompactReadReason::Range));
            }
            if units > COMPACT_LEAF_CODE_UNITS as i64 {
                return Err(CompactError::read(CompactReadReason::SizeLimit));
            }
            if !(1..=2).contains(&max_leaves) {
                return Err(CompactError::read(CompactReadReason::CountLimit));
            }
            let mut selected = Vec::new();
            state.select(
                root,
                offset,
                length.min(offset + units),
                max_leaves,
                &mut selected,
            )?;
            (selected, length)
        };
        let tail_units = if units == 0 {
            0
        } else {
            (offset + units - offset.max(length)).max(0)
        };
        if selected.len() + usize::from(tail_units > 0) > max_leaves {
            return Err(CompactError::read(CompactReadReason::CountLimit));
        }
        self.store.adjust(root, 0, 1)?;
        let read: Result<String, CompactError> = async {
            let _reading = self.store.begin_read()?;
            let mut result = String::new();
            for (id, from, count) in &selected {
                let text = self.store.read_leaf(*id).await?;
                valid()?;
                let from = usize::try_from(*from).unwrap_or(0);
                result.push_str(utf16_slice(
                    &text,
                    from,
                    from + usize::try_from(*count).unwrap_or(0),
                ));
            }
            valid()?;
            let start = usize::try_from((offset - length).max(0)).unwrap_or(0);
            result.push_str(utf16_slice(
                tail,
                start,
                start + usize::try_from(tail_units).unwrap_or(0),
            ));
            Ok(result)
        }
        .await;
        let unpinned = self.store.adjust(root, 0, -1);
        let result = read?;
        unpinned?;
        Ok(result)
    }

    /// Compatibility assembly visits actual leaves, including short append-only delta leaves.
    pub async fn chunks(
        &self,
        root: LeafId,
        valid: Valid<'_>,
    ) -> Result<Vec<String>, CompactError> {
        valid()?;
        let mut chunks = Vec::new();
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            if id == 0 {
                continue;
            }
            if id > 0 {
                let units = self.units(id)?;
                chunks.push(self.read(id, "", 0, units, 1, Some(valid)).await?);
                valid()?;
            } else {
                let (left, right) = {
                    let state = self.store.lock();
                    let node = state.node(id)?;
                    (node.left, node.right)
                };
                stack.push(right);
                stack.push(left);
            }
        }
        Ok(chunks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_content_store::CompactContentStoreOptions;

    fn store(root: &tempfile::TempDir) -> CompactContentStore {
        CompactContentStore::new(CompactContentStoreOptions {
            dir: root.path().join("epoch").display().to_string(),
            ..Default::default()
        })
    }

    // senpi 4272-text-root-height / bounded-text-roots: appends stay logarithmic and reads stay bounded.
    #[tokio::test]
    async fn append_tree_stays_balanced_and_bounded_reads_span_two_leaves() {
        let root_dir = tempfile::tempdir().expect("tempdir");
        let store = store(&root_dir);
        let text = store.text();
        let mut root = 0;
        for n in 0..64 {
            root = text
                .append(root, &format!("{n:02}"), false)
                .expect("append");
        }
        assert_eq!(text.units(root).expect("units"), 128);
        let height = store.lock().height(root).expect("height");
        assert!(height <= 7, "height {height}");
        assert_eq!(
            text.read(root, "!", 3, 2, 2, None).await.expect("read"),
            "10"
        );
        assert_eq!(
            text.read(root, "!", 127, 2, 2, None).await.expect("tail"),
            "3!"
        );
        assert_eq!(
            text.read(root, "", 0, 6, 2, None)
                .await
                .expect_err("three leaves")
                .read_reason(),
            Some(CompactReadReason::CountLimit)
        );
        assert_eq!(
            text.read(root, "", 127, 2, 2, None)
                .await
                .expect_err("range")
                .read_reason(),
            Some(CompactReadReason::Range)
        );
        let chunks = text
            .chunks(root, &|| -> Result<(), CompactError> { Ok(()) })
            .await
            .expect("chunks")
            .concat();
        assert_eq!(chunks.len(), 128);
        assert!(chunks.starts_with("000102"));
        text.adjust(root, -1, 0).expect("release");
        assert_eq!(text.resident_nodes(), 0);
        assert_eq!(store.status().resident_payload_bytes, 0);
        store.retire().await.expect("retire");
    }

    #[tokio::test]
    async fn a_node_commits_once_both_children_are_committed() {
        let root_dir = tempfile::tempdir().expect("tempdir");
        let store = store(&root_dir);
        let a = store.create_leaf("a", true).expect("a");
        let b = store.create_leaf("b", true).expect("b");
        let root = store.text().own(&[a, b]).expect("own");
        assert!(!store.is_committed(root));
        store.flush_leaves(vec![a, b]).await.expect("flush");
        assert!(store.is_committed(root));
        for id in [a, b, root] {
            store.adjust(id, -1, 0).expect("release");
        }
        store.idle().await;
        assert_eq!(store.status().structured_bytes.spilled, 0);
        store.retire().await.expect("retire");
    }
}
