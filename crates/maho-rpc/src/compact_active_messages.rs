//! senpi `modes/rpc/compact-active-messages.ts`: live messages and their persistent block
//! roots; every publication runs inside one synchronous acceptance.

use crate::compact_active_block::{ActiveBlock, ActiveMessage, adjust_block};
use crate::compact_cold_operations::CompactColdOperations;
use crate::compact_component_roots::{CompactComponentRoots, ComponentRoot};
use crate::compact_content_index::CompactContentIndex;
use crate::compact_content_store::{CompactContentStore, CompactContentStoreOptions, utf16_len};
use crate::compact_read_lease::{CompactError, lock};
use crate::compact_seeded_block::{SeededBlock, frozen_block};
use crate::compact_seeded_draft::SeededFields;
use crate::compact_start_identity::CompactStartIdentity;
use crate::compact_state_view::{MessageRoot, SettledRow};
use crate::compact_text_view::FrozenBlock;
use crate::compact_transition::{
    BlockAddress, CompactPersistence, CompactRejection, CompactTransitionError,
};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

/// Per-message seeded metadata tracked alongside `ActiveMessage` without modifying
/// `compact_active_block.rs`.
#[derive(Default)]
pub struct TrackedActiveMessage {
    pub message: ActiveMessage,
    pub epoch: Option<String>,
    pub fields: Option<SeededFields>,
    pub correlation: Option<SeededFields>,
    pub seeded_blocks: BTreeMap<usize, SeededBlock>,
}

#[allow(clippy::derivable_impls)]
impl Default for ActiveMessage {
    fn default() -> Self {
        Self {
            order: String::new(),
            blocks: BTreeMap::new(),
            root: ComponentRoot::default(),
            tail_bytes: 0,
            witness_bytes: 0,
        }
    }
}

fn row_of(message_id: &str, message: &ActiveMessage) -> MessageRoot {
    MessageRoot {
        message_id: message_id.to_owned(),
        blocks: message.root.reference.clone(),
    }
}

/// Live messages and their persistent block roots; every publication runs inside one synchronous
/// acceptance.
pub struct CompactActiveMessages {
    pub store: CompactContentStore,
    pub settled: CompactContentIndex<SettledRow>,
    pub cold: CompactColdOperations<SettledRow>,
    pub block_roots: CompactComponentRoots<FrozenBlock>,
    pub message_roots: CompactComponentRoots<MessageRoot>,
    pub messages: ComponentRoot<MessageRoot>,
    pub active: Arc<Mutex<HashMap<String, TrackedActiveMessage>>>,
    pub identity: CompactStartIdentity,
    pub sequence: u64,
    pub resident_tail_bytes: i64,
    pub hot_witness_bytes: i64,
}

impl CompactActiveMessages {
    pub fn new(options: CompactContentStoreOptions, compact_above_rows: Option<usize>) -> Self {
        let store = CompactContentStore::new(options);
        let settled = CompactContentIndex::new(
            store.clone(),
            |row: &SettledRow| vec![row.blocks.as_ref().map_or(0, |b| b.owner)],
            compact_above_rows.unwrap_or(128),
        );
        let active: Arc<Mutex<HashMap<String, TrackedActiveMessage>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let weak_active = Arc::downgrade(&active);
        let cold = CompactColdOperations::new(settled.clone(), move |key| {
            weak_active
                .upgrade()
                .is_some_and(|map| lock(&map).contains_key(key))
        });
        let block_roots = CompactComponentRoots::new(store.clone(), |block: &FrozenBlock| {
            vec![block.root, block.tail_root.unwrap_or(0)]
        });
        let message_roots = CompactComponentRoots::new(store.clone(), |row: &MessageRoot| {
            vec![row.blocks.as_ref().map_or(0, |b| b.owner)]
        });
        Self {
            store,
            settled,
            cold,
            block_roots,
            message_roots,
            messages: ComponentRoot::default(),
            active,
            identity: CompactStartIdentity::new(),
            sequence: 0,
            resident_tail_bytes: 0,
            hot_witness_bytes: 0,
        }
    }

    pub fn is_active_key(&self, key: &str) -> bool {
        lock(&self.active).contains_key(key)
    }

    pub fn publish(
        &mut self,
        address: &BlockAddress,
        block: ActiveBlock,
        epoch: Option<String>,
        fields: Option<SeededFields>,
        seeded: Option<SeededBlock>,
    ) -> Result<(), CompactError> {
        let mut entry = lock(&self.active)
            .remove(&address.message_id)
            .unwrap_or_else(|| TrackedActiveMessage {
                message: self.fresh(None),
                epoch,
                ..TrackedActiveMessage::default()
            });
        if let Some(fields) = fields {
            self.release_fields(entry.fields.take());
            self.retain_fields(&fields)?;
            entry.fields = Some(fields);
        }
        let result = self.put(&mut entry, address.content_index, block, seeded);
        match result {
            Ok(()) => self.publish_row(&address.message_id, entry),
            Err(err) => {
                if !entry.message.blocks.is_empty() {
                    lock(&self.active).insert(address.message_id.clone(), entry);
                }
                Err(err)
            }
        }
    }

    /// An authoritative whole message: a fresh block root, so no index the message no longer names
    /// survives.
    pub fn install(
        &mut self,
        message_id: &str,
        epoch: String,
        fields: Option<SeededFields>,
        correlation: Option<SeededFields>,
        blocks: BTreeMap<usize, SeededBlock>,
    ) -> Result<(), CompactError> {
        let old = lock(&self.active).remove(message_id);
        let order = old.as_ref().map(|entry| entry.message.order.clone());
        let mut entry = TrackedActiveMessage {
            message: self.fresh(order),
            epoch: Some(epoch),
            fields,
            correlation,
            seeded_blocks: BTreeMap::new(),
        };
        if let Some(fields) = &entry.fields {
            self.retain_fields(fields)?;
        }
        if let Some(correlation) = &entry.correlation {
            self.retain_fields(correlation)?;
        }
        for (index, seeded) in blocks {
            let body = seeded.body.clone();
            if let Err(err) = self.put(&mut entry, index, body, Some(seeded)) {
                self.discard(&mut entry);
                if let Some(old) = old {
                    lock(&self.active).insert(message_id.to_owned(), old);
                }
                return Err(err);
            }
        }
        let published = self.publish_row(message_id, entry);
        if let Some(mut old) = old {
            self.discard(&mut old);
        }
        published
    }

    pub fn settle(
        &mut self,
        message_id: &str,
        persistence: CompactPersistence,
    ) -> Result<(), CompactError> {
        let row = {
            let active = lock(&self.active);
            active
                .get(message_id)
                .map(|entry| row_of(message_id, &entry.message))
                .unwrap_or_else(|| MessageRoot {
                    message_id: message_id.to_owned(),
                    blocks: None,
                })
        };
        self.settled.set(
            message_id,
            &SettledRow {
                message_id: row.message_id,
                blocks: row.blocks,
                persistence,
            },
        )?;
        self.drop_active(message_id)
    }

    pub fn drop_active(&mut self, message_id: &str) -> Result<(), CompactError> {
        let Some(mut entry) = lock(&self.active).remove(message_id) else {
            return Ok(());
        };
        self.messages = self
            .message_roots
            .set(&self.messages, &entry.message.order, None)?;
        self.discard(&mut entry);
        self.identity.release(message_id);
        Ok(())
    }

    pub fn block(&self, address: &BlockAddress) -> Result<ActiveBlock, CompactError> {
        lock(&self.active)
            .get(&address.message_id)
            .and_then(|entry| entry.message.blocks.get(&address.content_index))
            .cloned()
            .ok_or_else(|| {
                CompactTransitionError::new(
                    CompactRejection::UnknownBlock,
                    format!("{}#{}", address.message_id, address.content_index),
                )
                .into()
            })
    }

    fn fresh(&self, order: Option<String>) -> ActiveMessage {
        ActiveMessage {
            order: order.unwrap_or_else(|| format!("{:016}", self.sequence)),
            blocks: BTreeMap::new(),
            root: ComponentRoot::default(),
            tail_bytes: 0,
            witness_bytes: 0,
        }
    }

    fn put(
        &mut self,
        entry: &mut TrackedActiveMessage,
        content_index: usize,
        block: ActiveBlock,
        seeded: Option<SeededBlock>,
    ) -> Result<(), CompactError> {
        let old = entry.message.blocks.get(&content_index);
        let old_seeded = entry.seeded_blocks.get(&content_index);
        let new_sig_units = seeded.as_ref().map_or(0, |s| utf16_len(&s.signature.tail));
        let old_sig_units = old_seeded.map_or(0, |s| utf16_len(&s.signature.tail));
        let tails = (utf16_len(&block.tail) + new_sig_units
            - old.map_or(0, |b| utf16_len(&b.tail))
            - old_sig_units)
            * 2;
        let witness = (utf16_len(&block.witness) - old.map_or(0, |b| utf16_len(&b.witness))) * 2;
        entry.message.root = self.block_roots.set(
            &entry.message.root,
            &format!("{content_index:016}"),
            Some(frozen_block(content_index, &block)),
        )?;
        adjust_block(&self.store, &block, 1)?;
        if let Some(previous) = entry.message.blocks.insert(content_index, block) {
            adjust_block(&self.store, &previous, -1)?;
        }
        if let Some(seeded) = seeded {
            entry.seeded_blocks.insert(content_index, seeded);
        } else {
            entry.seeded_blocks.remove(&content_index);
        }
        entry.message.tail_bytes += tails;
        entry.message.witness_bytes += witness;
        self.resident_tail_bytes += tails;
        self.hot_witness_bytes += witness;
        Ok(())
    }

    fn publish_row(
        &mut self,
        message_id: &str,
        entry: TrackedActiveMessage,
    ) -> Result<(), CompactError> {
        let row = row_of(message_id, &entry.message);
        self.messages = self
            .message_roots
            .set(&self.messages, &entry.message.order, Some(row))?;
        let mut active = lock(&self.active);
        if !active.contains_key(message_id) {
            self.cold.activate(message_id);
        }
        active.insert(message_id.to_owned(), entry);
        Ok(())
    }

    fn discard(&mut self, entry: &mut TrackedActiveMessage) {
        for block in entry.message.blocks.values() {
            let _ = adjust_block(&self.store, block, -1);
        }
        entry.message.blocks.clear();
        entry.seeded_blocks.clear();
        self.release_fields(entry.fields.take());
        self.release_fields(entry.correlation.take());
        self.block_roots
            .release(std::mem::take(&mut entry.message.root));
        self.resident_tail_bytes -= entry.message.tail_bytes;
        self.hot_witness_bytes -= entry.message.witness_bytes;
    }

    fn retain_fields(&self, fields: &SeededFields) -> Result<(), CompactError> {
        for reference in fields.values() {
            self.store.adjust(reference.owner, 1, 0)?;
        }
        Ok(())
    }

    fn release_fields(&self, fields: Option<SeededFields>) {
        if let Some(fields) = fields {
            for reference in fields.values() {
                let _ = self.store.adjust(reference.owner, -1, 0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_seeded_draft::CompactSeededDraft;
    use crate::compact_transition::CompactBlockKind;

    #[tokio::test]
    async fn replacing_block_with_shorter_text_reduces_byte_counters() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut active = CompactActiveMessages::new(
            CompactContentStoreOptions {
                dir: dir.path().join("epoch").display().to_string(),
                ..Default::default()
            },
            None,
        );
        let address = BlockAddress {
            message_id: "m1".into(),
            content_index: 0,
        };
        for (text, witness, tail_bytes, witness_bytes) in
            [("abcdef", "wxyz", 12, 8), ("ab", "x", 4, 2)]
        {
            let mut draft = CompactSeededDraft::new(active.store.clone());
            let mut block = draft.text(CompactBlockKind::Text, text).expect("text");
            block.witness = witness.to_owned();
            adjust_block(&active.store, &block, 1).expect("retain");
            active
                .publish(&address, block.clone(), Some("E1".into()), None, None)
                .expect("publish");
            adjust_block(&active.store, &block, -1).expect("drop caller ref");
            assert_eq!(active.resident_tail_bytes, tail_bytes);
            assert_eq!(active.hot_witness_bytes, witness_bytes);
        }
        active.drop_active("m1").expect("drop");
        assert_eq!(active.resident_tail_bytes, 0);
        assert_eq!(active.hot_witness_bytes, 0);
        active.store.retire().await.expect("retire");
    }

    #[tokio::test]
    async fn publishes_installs_and_settles_active_messages_to_zero_leak() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut active = CompactActiveMessages::new(
            CompactContentStoreOptions {
                dir: dir.path().join("epoch").display().to_string(),
                ..Default::default()
            },
            None,
        );
        let block = {
            let mut draft = CompactSeededDraft::new(active.store.clone());
            let b = draft.text(CompactBlockKind::Text, "hello").expect("text");
            adjust_block(&active.store, &b, 1).expect("retain");
            b
        };
        active
            .publish(
                &BlockAddress {
                    message_id: "m1".into(),
                    content_index: 0,
                },
                block.clone(),
                Some("E1".into()),
                None,
                None,
            )
            .expect("publish");
        adjust_block(&active.store, &block, -1).expect("drop caller ref");
        assert!(active.is_active_key("m1"));
        assert_eq!(active.resident_tail_bytes, 10);
        active
            .settle("m1", CompactPersistence::Appended)
            .expect("settle");
        assert!(!active.is_active_key("m1"));
        assert_eq!(active.resident_tail_bytes, 0);
        active.store.retire().await.expect("retire");
    }
}
