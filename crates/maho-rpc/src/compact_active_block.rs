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
        // A surrogate pair straddling the leaf limit: flush the tail alone rather than overfill the
        // leaf; the pair starts the next one. With an empty tail `take` is a whole leaf, so progress holds.
        let split = utf16_boundary(rest, take);
        let split = if split == 0 && block.tail.is_empty() {
            rest.chars().next().map_or(0, char::len_utf8)
        } else {
            split
        };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_content_store::CompactContentStoreOptions;
    use crate::compact_text_view::read_compact_blocks;

    fn store(root: &tempfile::TempDir) -> CompactContentStore {
        CompactContentStore::new(CompactContentStoreOptions {
            dir: root.path().join("epoch").display().to_string(),
            ..Default::default()
        })
    }

    fn block() -> ActiveBlock {
        ActiveBlock {
            kind: CompactBlockKind::Text,
            root: 0,
            tail: String::new(),
            tail_root: 0,
            units: 0,
            emitted: 0,
            covered: 0,
            witness: String::new(),
        }
    }

    fn ok() -> Result<(), CompactError> {
        Ok(())
    }

    async fn leaves(store: &CompactContentStore, root: LeafId) -> Vec<String> {
        store.text().chunks(root, &ok).await.expect("chunks")
    }

    // A surrogate pair straddling the leaf limit: the tail is flushed alone, never a 32769-unit leaf.
    #[tokio::test]
    async fn a_pair_straddling_the_leaf_limit_flushes_the_tail_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store(&dir);
        let mut block = block();
        let filler = "a".repeat(COMPACT_LEAF_CODE_UNITS - 1);
        append_block(&store, &mut block, &filler).expect("tail at 32767 units");
        assert_eq!(block.root, 0, "below the leaf limit nothing is flushed");
        assert_eq!(utf16_len(&block.tail), (COMPACT_LEAF_CODE_UNITS - 1) as i64);
        append_block(&store, &mut block, "\u{1F600}b").expect("the emoji appends");
        assert_eq!(leaves(&store, block.root).await, vec![filler.clone()]);
        assert_eq!(block.tail, "\u{1F600}b");
        assert_eq!(leaves(&store, block.tail_root).await.concat(), "\u{1F600}b");
        assert_eq!(block.units, COMPACT_LEAF_CODE_UNITS as i64 + 2);
        let blocks = [FrozenBlock {
            content_index: 0,
            kind: block.kind,
            root: block.root,
            tail: block.tail.clone(),
            tail_root: None,
        }];
        let views = read_compact_blocks(&store.text(), &blocks, &ok)
            .await
            .expect("view");
        assert_eq!(views[0].text, format!("{filler}\u{1F600}b"));
        adjust_block(&store, &block, -1).expect("release");
        store.retire().await.expect("retire");
    }

    // A pair at the very start of a whole leaf still makes progress, and no leaf exceeds the limit.
    #[tokio::test]
    async fn long_appends_split_into_bounded_leaves_without_splitting_a_pair() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store(&dir);
        let mut block = block();
        let text = format!(
            "{}{}",
            "x".repeat(COMPACT_LEAF_CODE_UNITS - 1),
            "\u{1F600}".repeat(COMPACT_LEAF_CODE_UNITS)
        );
        append_block(&store, &mut block, &text).expect("append");
        let flushed = leaves(&store, block.root).await;
        assert!(flushed.len() >= 2);
        for leaf in &flushed {
            assert!(utf16_len(leaf) <= COMPACT_LEAF_CODE_UNITS as i64);
        }
        assert_eq!(format!("{}{}", flushed.concat(), block.tail), text);
        assert!(utf16_len(&block.tail) < COMPACT_LEAF_CODE_UNITS as i64);
        assert_eq!(block.units, utf16_len(&text));
        adjust_block(&store, &block, -1).expect("release");
        store.retire().await.expect("retire");
    }

    #[tokio::test]
    async fn deltas_must_be_contiguous_and_overlaps_must_match_the_witness() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store(&dir);
        let mut block = block();
        block.witness = "hello".into();
        block.covered = 5;
        append_block(&store, &mut block, "hello").expect("covered prefix");
        let rejection = |error: CompactError| match error {
            CompactError::Transition(error) => error.reason,
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(
            rejection(delta_block(&store, &mut block, 1, "x").expect_err("gap")),
            CompactRejection::NonContiguous
        );
        assert_eq!(
            rejection(delta_block(&store, &mut block, 0, "help").expect_err("conflict")),
            CompactRejection::ConflictingOverlap
        );
        assert_eq!(block.units, 5, "a refused delta touches nothing");
        delta_block(&store, &mut block, 0, "hel").expect("matching overlap");
        assert_eq!((block.units, block.emitted), (5, 3));
        assert_eq!(
            block.witness, "hello",
            "the witness stays until coverage is reached"
        );
        delta_block(&store, &mut block, 3, "lo, world").expect("overlap then new text");
        assert_eq!(block.tail, "hello, world");
        assert_eq!((block.units, block.emitted), (12, 12));
        assert!(block.witness.is_empty());
        adjust_block(&store, &block, -1).expect("release");
        store.retire().await.expect("retire");
    }
}
