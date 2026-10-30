//! senpi `modes/rpc/compact-text-view.ts`: a frozen block's text roots and the compatibility
//! assembly of whole block texts.
use crate::compact_content_store::LeafId;
use crate::compact_read_lease::CompactError;
use crate::compact_text_roots::CompactTextRoots;
use crate::compact_transition::{CompactBlockKind, CompactPersistence};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrozenBlock {
    pub content_index: usize,
    pub kind: CompactBlockKind,
    pub root: LeafId,
    /// A cut shares the immutable tail (under one leaf), not a new leaf for each freeze.
    pub tail: String,
    /// Append-only structural tail; the hot string is never re-owned on each delta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_root: Option<LeafId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactBlockView {
    pub content_index: usize,
    pub kind: CompactBlockKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactMessageView {
    pub message_id: String,
    pub persistence: Option<CompactPersistence>,
    pub blocks: Vec<CompactBlockView>,
}

/// Compatibility assembly only; each underlying range is bounded, the resulting legacy view is not.
pub async fn read_compact_blocks(
    texts: &CompactTextRoots,
    blocks: &[FrozenBlock],
    valid: &(dyn Fn() -> Result<(), CompactError> + Sync),
) -> Result<Vec<CompactBlockView>, CompactError> {
    let mut result = Vec::new();
    for block in blocks {
        valid()?;
        let mut text = texts.chunks(block.root, valid).await?.concat();
        text.push_str(
            &texts
                .chunks(block.tail_root.unwrap_or(0), valid)
                .await?
                .concat(),
        );
        text.push_str(&block.tail);
        result.push(CompactBlockView {
            content_index: block.content_index,
            kind: block.kind,
            text,
        });
    }
    Ok(result)
}
