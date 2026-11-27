//! senpi `modes/rpc/compact-seeded-draft.ts`: candidate payloads remain owned until
//! publication acquires them or rejection releases them.

use crate::compact_active_block::{ActiveBlock, adjust_block, append_block};
use crate::compact_content_store::{CompactContentStore, LeafId};
use crate::compact_read_lease::CompactError;
use crate::compact_transition::CompactBlockKind;
use crate::compact_values::{CompactValues, ValueRef};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub type SeededFields = BTreeMap<String, ValueRef>;

enum DraftRelease {
    Leaf(LeafId),
    Block { root: LeafId, tail_root: LeafId },
}

/// Candidate payloads remain owned until publication acquires them or rejection/drop releases them.
pub struct CompactSeededDraft {
    store: CompactContentStore,
    values: CompactValues,
    releases: Vec<DraftRelease>,
}

impl CompactSeededDraft {
    pub fn new(store: CompactContentStore) -> Self {
        let values = CompactValues::new(store.clone());
        Self {
            store,
            values,
            releases: Vec::new(),
        }
    }

    pub fn store(&self) -> &CompactContentStore {
        &self.store
    }

    /// `remove_null` reads a message-metadata change, where `null` removes the field.
    pub fn fields(
        &mut self,
        value: &Map<String, Value>,
        previous: Option<&SeededFields>,
        remove_null: bool,
    ) -> Result<SeededFields, CompactError> {
        let mut result = previous.cloned().unwrap_or_default();
        for (key, item) in value {
            if item.is_null() && remove_null {
                result.remove(key);
                continue;
            }
            let reference = self.values.own(item)?;
            self.releases.push(DraftRelease::Leaf(reference.owner));
            result.insert(key.clone(), reference);
        }
        Ok(result)
    }

    pub fn text(
        &mut self,
        kind: CompactBlockKind,
        text: &str,
    ) -> Result<ActiveBlock, CompactError> {
        let mut block = ActiveBlock {
            kind,
            root: 0,
            tail: String::new(),
            tail_root: 0,
            units: 0,
            emitted: 0,
            covered: 0,
            witness: String::new(),
        };
        if let Err(error) = append_block(&self.store, &mut block, text) {
            let _ = adjust_block(&self.store, &block, -1);
            return Err(error);
        }
        block.emitted = block.units;
        self.releases.push(DraftRelease::Block {
            root: block.root,
            tail_root: block.tail_root,
        });
        Ok(block)
    }

    pub fn clone_block(&mut self, block: &ActiveBlock) -> Result<ActiveBlock, CompactError> {
        adjust_block(&self.store, block, 1)?;
        self.releases.push(DraftRelease::Block {
            root: block.root,
            tail_root: block.tail_root,
        });
        Ok(block.clone())
    }
}

impl Drop for CompactSeededDraft {
    fn drop(&mut self) {
        for release in self.releases.drain(..).rev() {
            match release {
                DraftRelease::Leaf(owner) => {
                    let _ = self.store.adjust(owner, -1, 0);
                }
                DraftRelease::Block { root, tail_root } => {
                    let _ = self.store.adjust(root, -1, 0);
                    let _ = self.store.adjust(tail_root, -1, 0);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_content_store::CompactContentStoreOptions;
    use serde_json::json;

    #[test]
    fn draft_releases_candidate_leaves_and_blocks_on_drop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        {
            let mut draft = CompactSeededDraft::new(store.clone());
            let mut map = Map::new();
            map.insert("a".into(), json!({"nested": [1, 2, 3]}));
            map.insert("b".into(), json!("text"));
            let fields = draft.fields(&map, None, false).expect("fields");
            assert!(fields.contains_key("a"));
            let mut remove = Map::new();
            remove.insert("b".into(), Value::Null);
            let updated = draft.fields(&remove, Some(&fields), true).expect("remove");
            assert!(!updated.contains_key("b"));
            let block = draft.text(CompactBlockKind::Text, "hello").expect("text");
            let _cloned = draft.clone_block(&block).expect("clone");
            assert!(store.status().structured_bytes.resident > 0);
        }
        let status = store.status();
        assert_eq!(status.structured_bytes.resident, 0);
        assert_eq!(status.resident_payload_bytes, 0);
    }
}
