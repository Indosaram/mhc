//! senpi `modes/rpc/compact-commit-dependencies.ts`: per-owner readiness, independent of counted
//! references - one leaf can unblock many rows or trees.
//!
//! Upstream subscribes to the store's commit notifications in its constructor and calls `ready(key)`
//! itself. Here the structure is plain data: its owner feeds each committed leaf to `committed`, which
//! returns the keys that became ready, so it can live inside the store's state (text roots) or behind an
//! index's own lock without a re-entrant callback.
use crate::compact_content_store::LeafId;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;

pub struct CompactCommitDependencies<Key> {
    pending: HashMap<Key, HashSet<LeafId>>,
    dependants: HashMap<LeafId, HashSet<Key>>,
}

impl<Key> Default for CompactCommitDependencies<Key> {
    fn default() -> Self {
        Self {
            pending: HashMap::new(),
            dependants: HashMap::new(),
        }
    }
}

impl<Key: Clone + Eq + Hash> CompactCommitDependencies<Key> {
    pub fn size(&self) -> usize {
        self.pending.len()
    }

    pub fn has(&self, key: &Key) -> bool {
        self.pending.contains_key(key)
    }

    /// `refs` already filtered by the caller to the leaves the store has not committed.
    pub fn set(&mut self, key: Key, uncommitted: impl IntoIterator<Item = LeafId>) {
        self.delete(&key);
        let pending: HashSet<LeafId> = uncommitted.into_iter().collect();
        if pending.is_empty() {
            return;
        }
        for id in &pending {
            self.dependants.entry(*id).or_default().insert(key.clone());
        }
        self.pending.insert(key, pending);
    }

    pub fn delete(&mut self, key: &Key) {
        if let Some(ids) = self.pending.remove(key) {
            for id in ids {
                if let Some(keys) = self.dependants.get_mut(&id) {
                    keys.remove(key);
                    if keys.is_empty() {
                        self.dependants.remove(&id);
                    }
                }
            }
        }
    }

    /// The store committed `id`: the keys whose last pending leaf it was are ready.
    pub fn committed(&mut self, id: LeafId) -> Vec<Key> {
        let mut ready = Vec::new();
        for key in self.dependants.remove(&id).unwrap_or_default() {
            if let Some(pending) = self.pending.get_mut(&key) {
                pending.remove(&id);
                if pending.is_empty() {
                    self.pending.remove(&key);
                    ready.push(key);
                }
            }
        }
        ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_leaf_unblocks_every_dependant_once_all_its_leaves_commit() {
        let mut deps = CompactCommitDependencies::<&str>::default();
        deps.set("a", [1, 2]);
        deps.set("b", [2]);
        deps.set("c", []);
        assert_eq!(deps.size(), 2);
        assert!(!deps.has(&"c"));
        assert!(deps.committed(1).is_empty());
        let mut ready = deps.committed(2);
        ready.sort_unstable();
        assert_eq!(ready, ["a", "b"]);
        assert_eq!(deps.size(), 0);
        deps.set("d", [3]);
        deps.delete(&"d");
        assert!(deps.committed(3).is_empty());
    }
}
