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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_content_store::{CompactContentStore, CompactContentStoreOptions};
    use crate::compact_read_lease::CompactReadReason;

    fn ok() -> Result<(), CompactError> {
        Ok(())
    }

    #[tokio::test]
    async fn assembles_root_then_structural_tail_then_hot_tail_per_block() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let texts = store.text();
        let mut root = texts.append(0, "ab", false).expect("a");
        root = texts.append(root, "cd", false).expect("b");
        let tail_root = texts.append(0, "ef", true).expect("tail");
        let blocks = [
            FrozenBlock {
                content_index: 0,
                kind: CompactBlockKind::Thinking,
                root,
                tail: "g".into(),
                tail_root: Some(tail_root),
            },
            FrozenBlock {
                content_index: 2,
                kind: CompactBlockKind::Text,
                root: 0,
                tail: "only tail".into(),
                tail_root: None,
            },
        ];
        let views = read_compact_blocks(&texts, &blocks, &ok)
            .await
            .expect("views");
        assert_eq!(
            views,
            vec![
                CompactBlockView {
                    content_index: 0,
                    kind: CompactBlockKind::Thinking,
                    text: "abcdefg".into()
                },
                CompactBlockView {
                    content_index: 2,
                    kind: CompactBlockKind::Text,
                    text: "only tail".into()
                },
            ]
        );
        let json = serde_json::to_value(&blocks[1]).expect("json");
        assert_eq!(
            json,
            serde_json::json!({"contentIndex": 2, "kind": "text", "root": 0, "tail": "only tail"})
        );
        store.adjust(root, -1, 0).expect("release root");
        store.adjust(tail_root, -1, 0).expect("release tail");
        store.retire().await.expect("retire");
    }

    #[tokio::test]
    async fn an_invalid_read_stops_the_assembly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let blocks = [FrozenBlock {
            content_index: 0,
            kind: CompactBlockKind::Text,
            root: 0,
            tail: "x".into(),
            tail_root: None,
        }];
        store.retire().await.expect("retire");
        let invalid = || -> Result<(), CompactError> {
            Err(crate::compact_read_lease::CompactReadError::new(CompactReadReason::Retired).into())
        };
        let error = read_compact_blocks(&store.text(), &blocks, &invalid)
            .await
            .expect_err("invalid");
        assert_eq!(error.read_reason(), Some(CompactReadReason::Retired));
    }
}
