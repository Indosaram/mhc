//! senpi `modes/rpc/compact-state-view.ts`: frozen compact state and its whole-view compatibility
//! assembly, called only while the owner holds its cut operation lease.
use crate::compact_component_roots::CompactComponentRoots;
use crate::compact_content_index::{CompactContentIndex, FrozenIndex};
use crate::compact_content_store::CompactContentStore;
use crate::compact_read_lease::CompactError;
use crate::compact_text_view::{CompactMessageView, FrozenBlock, read_compact_blocks};
use crate::compact_transition::CompactPersistence;
use crate::compact_values::ValueRef;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageRoot {
    pub message_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks: Option<ValueRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettledRow {
    pub message_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks: Option<ValueRef>,
    pub persistence: CompactPersistence,
}

pub struct FrozenCompactState {
    pub sequence: u64,
    pub active: Option<ValueRef>,
    pub settled: FrozenIndex,
    pub(crate) id: u64,
}

pub struct Components<'a> {
    pub store: &'a CompactContentStore,
    pub settled: &'a CompactContentIndex<SettledRow>,
    pub block_roots: &'a CompactComponentRoots<FrozenBlock>,
    pub message_roots: &'a CompactComponentRoots<MessageRoot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactStateView {
    pub sequence: u64,
    pub messages: Vec<CompactMessageView>,
}

/// Whole-view compatibility assembly, called only while the owner holds its cut operation lease.
pub async fn read_compact_state(
    frozen: &FrozenCompactState,
    components: Components<'_>,
    valid: &(dyn Fn() -> Result<(), CompactError> + Sync),
) -> Result<CompactStateView, CompactError> {
    let read_blocks = |reference: Option<ValueRef>| async move {
        let blocks: Vec<FrozenBlock> = components
            .block_roots
            .entries(reference.as_ref(), valid)
            .await?
            .into_iter()
            .map(|(_, block)| block)
            .collect();
        read_compact_blocks(&components.store.text(), &blocks, valid).await
    };
    let mut messages = Vec::new();
    for (message_id, row) in components.settled.entries(&frozen.settled).await? {
        let row: SettledRow = serde_json::from_value(row)?;
        messages.push(CompactMessageView {
            message_id,
            persistence: Some(row.persistence),
            blocks: read_blocks(row.blocks).await?,
        });
    }
    for (_, row) in components
        .message_roots
        .entries(frozen.active.as_ref(), valid)
        .await?
    {
        messages.push(CompactMessageView {
            message_id: row.message_id,
            persistence: None,
            blocks: read_blocks(row.blocks).await?,
        });
    }
    valid()?;
    Ok(CompactStateView {
        sequence: frozen.sequence,
        messages,
    })
}
