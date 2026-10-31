//! senpi `modes/rpc/compact-session-state.ts`: synchronous acceptance; cuts and settlement transfer
//! persistent component roots (senpi#4272).
use crate::compact_active_block::{
    ActiveBlock, ActiveMessage, adjust_block, append_block, delta_block,
};
use crate::compact_cold_operations::{
    CompactColdOperation, CompactColdOperations, CompactColdOutcome,
};
use crate::compact_component_roots::{CompactComponentRoots, ComponentRoot};
use crate::compact_content_index::{CompactContentIndex, IndexStatus};
use crate::compact_content_store::{
    CompactContentStore, CompactContentStoreOptions, CompactStorageStatus, utf16_len, utf16_slice,
};
use crate::compact_read_lease::{CompactError, CompactReadLease, CompactReadReason, lock};
use crate::compact_start_identity::CompactStartIdentity;
use crate::compact_state_view::{
    CompactStateView, Components, FrozenCompactState, MessageRoot, SettledRow, read_compact_state,
};
use crate::compact_text_view::FrozenBlock;
use crate::compact_transition::{
    BlockStart, CompactRejection, CompactTransition, CompactTransitionError,
};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct CompactSessionStateOptions {
    pub storage: CompactContentStoreOptions,
    pub compact_above_rows: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactSessionStorageStatus {
    pub store: CompactStorageStatus,
    pub index: IndexStatus,
    pub resident_tail_bytes: i64,
    pub frozen_tail_bytes: i64,
    pub hot_witness_bytes: i64,
    pub resident_component_nodes: i64,
    pub resident_component_index_bytes: i64,
    pub resident_text_nodes: usize,
    pub resident_text_index_bytes: usize,
}

struct Hot {
    messages: ComponentRoot<MessageRoot>,
    active: HashMap<String, ActiveMessage>,
    identity: CompactStartIdentity,
    sequence: u64,
    resident_tail_bytes: i64,
    hot_witness_bytes: i64,
}

pub struct CompactSessionState {
    store: CompactContentStore,
    settled: CompactContentIndex<SettledRow>,
    cold: CompactColdOperations<SettledRow>,
    block_roots: CompactComponentRoots<FrozenBlock>,
    message_roots: CompactComponentRoots<MessageRoot>,
    hot: Arc<Mutex<Hot>>,
    frozen_tail_bytes: Arc<AtomicI64>,
    cuts: Mutex<BTreeMap<u64, Arc<CompactReadLease>>>,
    next_cut: AtomicU64,
}

fn ownership() -> CompactError {
    CompactError::read(CompactReadReason::Ownership)
}

impl CompactSessionState {
    pub fn new(options: CompactSessionStateOptions) -> Self {
        let store = CompactContentStore::new(options.storage);
        let settled = CompactContentIndex::new(
            store.clone(),
            |row: &SettledRow| vec![row.blocks.as_ref().map_or(0, |blocks| blocks.owner)],
            options.compact_above_rows.unwrap_or(128),
        );
        let hot = Arc::new(Mutex::new(Hot {
            messages: ComponentRoot::default(),
            active: HashMap::new(),
            identity: CompactStartIdentity::new(),
            sequence: 0,
            resident_tail_bytes: 0,
            hot_witness_bytes: 0,
        }));
        let active_hot = Arc::downgrade(&hot);
        Self {
            block_roots: CompactComponentRoots::new(store.clone(), |block: &FrozenBlock| {
                vec![block.root, block.tail_root.unwrap_or(0)]
            }),
            message_roots: CompactComponentRoots::new(store.clone(), |row: &MessageRoot| {
                vec![row.blocks.as_ref().map_or(0, |blocks| blocks.owner)]
            }),
            cold: CompactColdOperations::new(settled.clone(), move |key| {
                active_hot
                    .upgrade()
                    .is_some_and(|hot| lock(&hot).active.contains_key(key))
            }),
            settled,
            store,
            hot,
            frozen_tail_bytes: Arc::new(AtomicI64::new(0)),
            cuts: Mutex::new(BTreeMap::new()),
            next_cut: AtomicU64::new(1),
        }
    }

    pub fn accepted_sequence(&self) -> u64 {
        lock(&self.hot).sequence
    }

    pub fn accept(&self, transition: &CompactTransition) -> Result<u64, CompactError> {
        let mut hot = lock(&self.hot);
        if !matches!(transition, CompactTransition::MessageRemove { .. }) {
            self.store.assert_readable()?;
        }
        if matches!(
            transition,
            CompactTransition::MessageStatus { .. } | CompactTransition::MessageRemove { .. }
        ) && !hot.active.contains_key(transition.message_id())
        {
            return Err(CompactTransitionError::new(
                CompactRejection::ColdOperationRequired,
                "use apply_cold after acceptance",
            )
            .into());
        }
        match transition {
            CompactTransition::BlockStart(start) => self.start(&mut hot, start)?,
            CompactTransition::BlockDelta {
                message_id,
                content_index,
                offset,
                text,
            } => {
                let mut block = Self::block(&hot, message_id, *content_index)?.clone();
                adjust_block(&self.store, &block, 1)?;
                let result = delta_block(
                    &self.store,
                    &mut block,
                    i64::try_from(*offset).unwrap_or(i64::MAX),
                    text,
                )
                .and_then(|()| self.publish(&mut hot, message_id, *content_index, block.clone()));
                adjust_block(&self.store, &block, -1)?;
                result?;
            }
            CompactTransition::BlockReplace {
                message_id,
                content_index,
                text,
            } => {
                let old = Self::block(&hot, message_id, *content_index)?;
                let mut block = ActiveBlock {
                    root: 0,
                    tail: String::new(),
                    tail_root: 0,
                    units: 0,
                    covered: 0,
                    witness: String::new(),
                    ..old.clone()
                };
                append_block(&self.store, &mut block, text)?;
                block.emitted = block.units;
                let result = self.publish(&mut hot, message_id, *content_index, block.clone());
                adjust_block(&self.store, &block, -1)?;
                result?;
            }
            CompactTransition::MessageStatus {
                message_id,
                persistence,
            } => {
                let blocks = hot
                    .active
                    .get(message_id)
                    .and_then(|message| message.root.reference.clone());
                self.settled.set(
                    message_id,
                    &SettledRow {
                        message_id: message_id.clone(),
                        persistence: *persistence,
                        blocks,
                    },
                )?;
                self.drop_active(&mut hot, message_id)?;
            }
            CompactTransition::MessageRemove { message_id } => {
                self.drop_active(&mut hot, message_id)?;
            }
        }
        hot.sequence += 1;
        Ok(hot.sequence)
    }

    pub async fn apply_cold(&self, operation: CompactColdOperation) -> CompactColdOutcome {
        let sequence = self.accepted_sequence();
        self.cold.run(operation, sequence).await
    }

    pub fn freeze(&self) -> Result<FrozenCompactState, CompactError> {
        self.store.assert_readable()?;
        let hot = lock(&self.hot);
        let active = hot.messages.reference.clone();
        let tails = hot.resident_tail_bytes;
        if let Some(active) = &active {
            self.store.adjust(active.owner, 0, 1)?;
        }
        self.frozen_tail_bytes.fetch_add(tails, Ordering::SeqCst);
        let settled = self.settled.freeze()?;
        let id = self.next_cut.fetch_add(1, Ordering::SeqCst);
        let (store, frozen_tails, index, cut) = (
            self.store.clone(),
            self.frozen_tail_bytes.clone(),
            self.settled.clone(),
            settled.clone(),
        );
        let active_owner = active.as_ref().map(|active| active.owner);
        let lease = CompactReadLease::new(move || {
            if let Some(owner) = active_owner {
                let _ = store.adjust(owner, 0, -1);
            }
            frozen_tails.fetch_sub(tails, Ordering::SeqCst);
            index.release(&cut);
        });
        let frozen = FrozenCompactState {
            sequence: hot.sequence,
            active,
            settled,
            id,
        };
        lock(&self.cuts).insert(id, Arc::new(lease));
        Ok(frozen)
    }

    pub fn release(&self, frozen: &FrozenCompactState) -> Result<(), CompactError> {
        self.cut_lease(frozen)?.release();
        Ok(())
    }

    /// Whole-view compatibility assembly; persistent nodes and text ranges are read separately.
    pub async fn read(
        &self,
        frozen: &FrozenCompactState,
    ) -> Result<CompactStateView, CompactError> {
        let lease = self.cut_lease(frozen)?;
        let valid = || -> Result<(), CompactError> {
            lease.assert()?;
            self.store.assert_readable()
        };
        valid()?;
        lease
            .run(read_compact_state(
                frozen,
                Components {
                    store: &self.store,
                    settled: &self.settled,
                    block_roots: &self.block_roots,
                    message_roots: &self.message_roots,
                },
                &valid,
            ))
            .await
    }

    fn cut_lease(
        &self,
        frozen: &FrozenCompactState,
    ) -> Result<Arc<CompactReadLease>, CompactError> {
        lock(&self.cuts)
            .get(&frozen.id)
            .cloned()
            .ok_or_else(ownership)
    }

    pub fn storage_status(&self) -> CompactSessionStorageStatus {
        let hot = lock(&self.hot);
        let component_nodes =
            self.block_roots.resident_nodes() + self.message_roots.resident_nodes();
        let text_nodes = self.store.text().resident_nodes();
        CompactSessionStorageStatus {
            store: self.store.status(),
            index: self.settled.status(),
            resident_tail_bytes: hot.resident_tail_bytes,
            frozen_tail_bytes: self.frozen_tail_bytes.load(Ordering::SeqCst),
            hot_witness_bytes: hot.hot_witness_bytes,
            resident_component_nodes: component_nodes,
            resident_component_index_bytes: component_nodes * 192,
            resident_text_nodes: text_nodes,
            resident_text_index_bytes: text_nodes * 192,
        }
    }

    pub async fn idle(&self) {
        self.cold.idle().await;
        self.store.idle().await;
    }

    pub async fn retire(&self) -> Result<(), CompactError> {
        let retired = self.store.retire().await;
        self.cold.idle().await;
        retired
    }

    fn start(&self, hot: &mut Hot, start: &BlockStart) -> Result<(), CompactError> {
        hot.identity.validate(start)?;
        if hot
            .active
            .get(&start.message_id)
            .is_some_and(|message| message.blocks.contains_key(&start.content_index))
        {
            return Err(CompactTransitionError::new(
                CompactRejection::DuplicateBlock,
                format!("{}#{}", start.message_id, start.content_index),
            )
            .into());
        }
        let covered = start.covered_code_units;
        let seed_units = utf16_len(&start.seed);
        if covered != 0 && covered != seed_units {
            return Err(CompactTransitionError::new(
                CompactRejection::InvalidCoverage,
                format!("{covered} of {seed_units} seed code units"),
            )
            .into());
        }
        let mut block = ActiveBlock {
            kind: start.kind,
            root: 0,
            tail: String::new(),
            tail_root: 0,
            units: 0,
            emitted: if covered > 0 { 0 } else { seed_units },
            covered,
            witness: utf16_slice(&start.seed, 0, usize::try_from(covered).unwrap_or(0)).to_owned(),
        };
        append_block(&self.store, &mut block, &start.seed)?;
        let published = self.publish(hot, &start.message_id, start.content_index, block.clone());
        adjust_block(&self.store, &block, -1)?;
        published?;
        hot.identity.commit(start);
        self.cold.activate(&start.message_id);
        Ok(())
    }

    fn publish(
        &self,
        hot: &mut Hot,
        message_id: &str,
        content_index: usize,
        block: ActiveBlock,
    ) -> Result<(), CompactError> {
        let order = format!("{:016}", hot.sequence);
        let message = hot
            .active
            .remove(message_id)
            .unwrap_or_else(|| ActiveMessage {
                order,
                blocks: BTreeMap::new(),
                root: ComponentRoot::default(),
                tail_bytes: 0,
                witness_bytes: 0,
            });
        let result = self.publish_into(hot, message_id, content_index, block, message);
        match result {
            Ok(message) => {
                hot.active.insert(message_id.to_owned(), message);
                Ok(())
            }
            Err((message, error)) => {
                if !message.blocks.is_empty() {
                    hot.active.insert(message_id.to_owned(), message);
                }
                Err(error)
            }
        }
    }

    fn publish_into(
        &self,
        hot: &mut Hot,
        message_id: &str,
        content_index: usize,
        block: ActiveBlock,
        mut message: ActiveMessage,
    ) -> Result<ActiveMessage, (ActiveMessage, CompactError)> {
        let old = message.blocks.get(&content_index);
        let tails = (utf16_len(&block.tail) - old.map_or(0, |old| utf16_len(&old.tail))) * 2;
        let witness =
            (utf16_len(&block.witness) - old.map_or(0, |old| utf16_len(&old.witness))) * 2;
        let root = match self.block_roots.set(
            &message.root,
            &format!("{content_index:016}"),
            Some(FrozenBlock {
                content_index,
                kind: block.kind,
                root: block.root,
                tail: String::new(),
                tail_root: Some(block.tail_root),
            }),
        ) {
            Ok(root) => root,
            Err(error) => return Err((message, error)),
        };
        message.root = root;
        match self.message_roots.set(
            &hot.messages,
            &message.order,
            Some(MessageRoot {
                message_id: message_id.to_owned(),
                blocks: message.root.reference.clone(),
            }),
        ) {
            Ok(messages) => hot.messages = messages,
            Err(error) => return Err((message, error)),
        }
        // The published copy holds its own references; the caller drops its working copy's.
        if let Err(error) = adjust_block(&self.store, &block, 1) {
            return Err((message, error));
        }
        if let Some(previous) = message.blocks.insert(content_index, block)
            && let Err(error) = adjust_block(&self.store, &previous, -1)
        {
            return Err((message, error));
        }
        message.tail_bytes += tails;
        message.witness_bytes += witness;
        hot.resident_tail_bytes += tails;
        hot.hot_witness_bytes += witness;
        Ok(message)
    }

    fn drop_active(&self, hot: &mut Hot, message_id: &str) -> Result<(), CompactError> {
        let Some(message) = hot.active.remove(message_id) else {
            return Ok(());
        };
        hot.messages = self
            .message_roots
            .set(&hot.messages, &message.order, None)?;
        for block in message.blocks.values() {
            adjust_block(&self.store, block, -1)?;
        }
        self.block_roots.release(message.root);
        hot.resident_tail_bytes -= message.tail_bytes;
        hot.hot_witness_bytes -= message.witness_bytes;
        hot.identity.release(message_id);
        Ok(())
    }

    fn block<'a>(
        hot: &'a Hot,
        message_id: &str,
        content_index: usize,
    ) -> Result<&'a ActiveBlock, CompactError> {
        hot.active
            .get(message_id)
            .and_then(|message| message.blocks.get(&content_index))
            .ok_or_else(|| {
                CompactTransitionError::new(
                    CompactRejection::UnknownBlock,
                    format!("{message_id}#{content_index}"),
                )
                .into()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_transition::{CompactBlockKind, CompactPersistence};

    fn state(root: &tempfile::TempDir) -> CompactSessionState {
        CompactSessionState::new(CompactSessionStateOptions {
            storage: CompactContentStoreOptions {
                dir: root.path().join("recovery/epoch-1").display().to_string(),
                ..Default::default()
            },
            compact_above_rows: None,
        })
    }

    fn start(epoch: &str, ordinal: i64, message_id: &str) -> CompactTransition {
        CompactTransition::BlockStart(BlockStart {
            message_id: message_id.into(),
            content_index: 0,
            epoch: epoch.into(),
            ordinal,
            first_ordinal: ordinal,
            kind: CompactBlockKind::Text,
            seed: "body".into(),
            covered_code_units: 0,
        })
    }

    fn reason(error: CompactError) -> Option<CompactRejection> {
        match error {
            CompactError::Transition(error) => Some(error.reason),
            _ => None,
        }
    }

    // senpi 4272-compact-start-identity / compact-current-state: atomic acceptance and readable cuts.
    #[tokio::test]
    async fn rejects_without_publishing_and_reads_live_and_settled_messages() {
        let root = tempfile::tempdir().expect("tempdir");
        let state = state(&root);
        state.accept(&start("A", 0, "live")).expect("start");
        state.accept(&start("B", 0, "other")).expect("rollover");
        let sequence = state.accepted_sequence();
        let status = state.storage_status();
        assert_eq!(
            reason(state.accept(&start("A", 1, "x")).expect_err("retired")),
            Some(CompactRejection::DuplicateOrStaleStart)
        );
        assert_eq!(
            reason(
                state
                    .accept(&CompactTransition::BlockDelta {
                        message_id: "live".into(),
                        content_index: 0,
                        offset: 9,
                        text: "!".into()
                    })
                    .expect_err("gap")
            ),
            Some(CompactRejection::NonContiguous)
        );
        assert_eq!(state.accepted_sequence(), sequence);
        assert_eq!(state.storage_status(), status);
        state
            .accept(&CompactTransition::BlockDelta {
                message_id: "live".into(),
                content_index: 0,
                offset: 4,
                text: "!".into(),
            })
            .expect("delta");
        state
            .accept(&CompactTransition::MessageStatus {
                message_id: "live".into(),
                persistence: CompactPersistence::Appended,
            })
            .expect("settle");
        state.idle().await;
        let cut = state.freeze().expect("freeze");
        let view = state.read(&cut).await.expect("read");
        assert_eq!(view.sequence, 4);
        let texts: Vec<(String, Option<CompactPersistence>, String)> = view
            .messages
            .iter()
            .map(|m| {
                (
                    m.message_id.clone(),
                    m.persistence,
                    m.blocks.iter().map(|b| b.text.clone()).collect(),
                )
            })
            .collect();
        assert_eq!(
            texts,
            [
                (
                    "live".into(),
                    Some(CompactPersistence::Appended),
                    "body!".into()
                ),
                ("other".into(), None, "body".into()),
            ]
        );
        assert_eq!(
            reason(
                state
                    .accept(&CompactTransition::MessageRemove {
                        message_id: "live".into()
                    })
                    .expect_err("cold")
            ),
            Some(CompactRejection::ColdOperationRequired)
        );
        let outcome = state
            .apply_cold(CompactColdOperation::MessageRemove {
                message_id: "live".into(),
            })
            .await;
        assert!(matches!(outcome, CompactColdOutcome::Applied { .. }));
        state.release(&cut).expect("release");
        assert_eq!(
            state.read(&cut).await.expect_err("released").read_reason(),
            Some(CompactReadReason::Released)
        );
        state.retire().await.expect("retire");
    }

    #[tokio::test]
    async fn replays_a_covered_seed_and_refuses_conflicting_overlap() {
        let root = tempfile::tempdir().expect("tempdir");
        let state = state(&root);
        let CompactTransition::BlockStart(mut covered) = start("A", 0, "m") else {
            unreachable!()
        };
        covered.covered_code_units = 4;
        state
            .accept(&CompactTransition::BlockStart(covered))
            .expect("start");
        let delta = |offset, text: &str| CompactTransition::BlockDelta {
            message_id: "m".into(),
            content_index: 0,
            offset,
            text: text.into(),
        };
        assert_eq!(
            reason(state.accept(&delta(0, "bx")).expect_err("conflict")),
            Some(CompactRejection::ConflictingOverlap)
        );
        state.accept(&delta(0, "bo")).expect("replay");
        state.accept(&delta(2, "dy+")).expect("overlap then append");
        let cut = state.freeze().expect("freeze");
        let view = state.read(&cut).await.expect("read");
        assert_eq!(view.messages[0].blocks[0].text, "body+");
        assert_eq!(state.storage_status().hot_witness_bytes, 0);
        state.release(&cut).expect("release");
        state.retire().await.expect("retire");
    }
}
