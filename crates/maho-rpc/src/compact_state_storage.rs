//! senpi `modes/rpc/compact-state-storage.ts`: constructs the single storage graph shared by
//! acceptance, settlement, and frozen reads.

use crate::compact_cold_operations::CompactColdOperations;
use crate::compact_component_roots::CompactComponentRoots;
use crate::compact_content_index::CompactContentIndex;
use crate::compact_content_store::{CompactContentStore, CompactContentStoreOptions, LeafId};
use crate::compact_state_view::{MessageRoot, SettledRow};
use crate::compact_text_view::FrozenBlock;

#[derive(Clone, Default)]
pub struct CompactStateStorageOptions {
    pub storage: CompactContentStoreOptions,
    pub compact_above_rows: Option<usize>,
    /// The registry nonce of the epoch this owner serves; the only epoch it accepts (RULING-39).
    pub epoch: Option<String>,
    /// The lane's accepted-sequence epoch, the prefix of every runtime item key it mints (RULING-13 Q1).
    pub session_epoch: Option<String>,
}

fn settled_leaves(row: &SettledRow) -> Vec<LeafId> {
    vec![row.blocks.as_ref().map_or(0, |b| b.owner)]
}

fn message_leaves(row: &MessageRoot) -> Vec<LeafId> {
    vec![row.blocks.as_ref().map_or(0, |b| b.owner)]
}

/// Constructs the single storage graph shared by acceptance, settlement, and frozen reads.
pub struct CompactStateStorage {
    pub store: CompactContentStore,
    pub settled: CompactContentIndex<SettledRow>,
    pub cold: CompactColdOperations<SettledRow>,
    pub block_roots: CompactComponentRoots<FrozenBlock>,
    pub message_roots: CompactComponentRoots<MessageRoot>,
    pub epoch: Option<String>,
    pub session_epoch: String,
}

impl CompactStateStorage {
    pub fn new(
        options: CompactStateStorageOptions,
        is_active_key: impl Fn(&str) -> bool + Send + Sync + 'static,
    ) -> Self {
        let store = CompactContentStore::new(options.storage);
        let block_roots = CompactComponentRoots::new(store.clone(), |block: &FrozenBlock| {
            vec![block.root, block.tail_root.unwrap_or(0)]
        });
        let message_roots = CompactComponentRoots::new(store.clone(), message_leaves);
        let settled = CompactContentIndex::new(
            store.clone(),
            settled_leaves,
            options.compact_above_rows.unwrap_or(128),
        );
        let cold = CompactColdOperations::new(settled.clone(), is_active_key);
        Self {
            store,
            settled,
            cold,
            block_roots,
            message_roots,
            epoch: options.epoch,
            session_epoch: options.session_epoch.unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn builds_shared_storage_graph_and_retires_cleanly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let graph = CompactStateStorage::new(
            CompactStateStorageOptions {
                storage: CompactContentStoreOptions {
                    dir: dir.path().join("epoch").display().to_string(),
                    ..Default::default()
                },
                compact_above_rows: Some(64),
                epoch: Some("nonce-1".into()),
                session_epoch: Some("se-1".into()),
            },
            |_| false,
        );
        assert_eq!(graph.epoch.as_deref(), Some("nonce-1"));
        assert_eq!(graph.session_epoch, "se-1");
        graph.cold.idle().await;
        graph.store.retire().await.expect("retire");
    }
}
