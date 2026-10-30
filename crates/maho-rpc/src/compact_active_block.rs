//! senpi `modes/rpc/compact-active-block.ts`: append-only active tail deltas.
use crate::compact_component_roots::ComponentRoot;
use crate::compact_content_store::{
    COMPACT_LEAF_CODE_UNITS, CompactContentStore, LeafId, utf16_boundary, utf16_len, utf16_slice,
};
use crate::compact_read_lease::CompactError;
use crate::compact_text_view::FrozenBlock;
use crate::compact_transition::{CompactBlockKind, CompactRejection, CompactTransitionError};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct ActiveBlock {
    pub kind: CompactBlockKind,
    pub root: LeafId,
    pub tail: String,
    pub tail_root: LeafId,
    pub units: i64,
    pub emitted: i64,
    pub covered: i64,
    pub witness: String,
}

pub struct ActiveMessage {
    pub order: String,
    pub blocks: BTreeMap<usize, ActiveBlock>,
    pub root: ComponentRoot<FrozenBlock>,
    pub tail_bytes: i64,
    pub witness_bytes: i64,
}

pub fn append_block(
    store: &CompactContentStore,
    block: &mut ActiveBlock,
    text: &str,
) -> Result<(), CompactError> {
    block.units += utf16_len(text);
    let mut rest = text;
    while utf16_len(&block.tail) + utf16_len(rest) >= COMPACT_LEAF_CODE_UNITS as i64 {
        let take = COMPACT_LEAF_CODE_UNITS - usize::try_from(utf16_len(&block.tail)).unwrap_or(0);
        let split = utf16_boundary(rest, take).max(rest.chars().next().map_or(0, char::len_utf8));
        let mut leaf = std::mem::take(&mut block.tail);
        leaf.push_str(&rest[..split]);
        block.root = store.text().append(block.root, &leaf, false)?;
        store.adjust(block.tail_root, -1, 0)?;
        block.tail_root = 0;
        rest = &rest[split..];
    }
    // Only the suffix enters structural storage; sealing coalesces it into one payload leaf.
    if !rest.is_empty() {
        block.tail_root = store.text().append(block.tail_root, rest, true)?;
    }
    block.tail.push_str(rest);
    Ok(())
}

pub fn adjust_block(
    store: &CompactContentStore,
    block: &ActiveBlock,
    live: i64,
) -> Result<(), CompactError> {
    store.adjust(block.root, live, 0)?;
    store.adjust(block.tail_root, live, 0)
}

/// Validate before touching the live text owner or its replay witness.
pub fn delta_block(
    store: &CompactContentStore,
    block: &mut ActiveBlock,
    offset: i64,
    text: &str,
) -> Result<(), CompactError> {
    if offset != block.emitted {
        return Err(CompactTransitionError::new(
            CompactRejection::NonContiguous,
            format!("offset {offset}, expected {}", block.emitted),
        )
        .into());
    }
    let length = utf16_len(text);
    let overlap_end = (offset + length).min(block.units);
    let overlap = (overlap_end - offset).max(0);
    let to = |n: i64| usize::try_from(n).unwrap_or(0);
    if overlap > 0
        && utf16_slice(text, 0, to(overlap))
            != utf16_slice(&block.witness, to(offset), to(overlap_end))
    {
        return Err(CompactTransitionError::new(
            CompactRejection::ConflictingOverlap,
            format!("[{offset}, {overlap_end})"),
        )
        .into());
    }
    let rest = &text[utf16_boundary(text, to(overlap))..];
    append_block(store, block, rest)?;
    block.emitted = offset + length;
    if block.emitted >= block.covered {
        block.witness.clear();
    }
    Ok(())
}
