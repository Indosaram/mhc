//! senpi `modes/rpc/compact-seeded-messages.ts`: whole-message seeding, metadata updates, and
//! cold settled message installation.

use crate::compact_active_block::ActiveBlock;
use crate::compact_component_roots::{CompactComponentRoots, ComponentRoot};
use crate::compact_content_index::CompactContentIndex;
use crate::compact_content_store::CompactContentStore;
use crate::compact_read_lease::CompactError;
use crate::compact_seeded_block::{SeededBlock, SeededBlockPhase, frozen_block, seed_block};
use crate::compact_seeded_draft::{CompactSeededDraft, SeededFields};
use crate::compact_state_view::SettledRow;
use crate::compact_text_view::FrozenBlock;
use crate::compact_transition::{CompactPersistence, CompactTransitionError};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashSet};

pub type SeededMessage = Value;

#[derive(Debug, Clone, PartialEq)]
pub enum SeededMessageRecord {
    MessageStart { message: SeededMessage },
    MessageEnd { message: SeededMessage },
    AssistantStart { message: SeededMessage },
    AssistantDone { message: SeededMessage },
    AssistantError { error: SeededMessage },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalCut {
    pub invalidated_blocks: Vec<usize>,
    pub rewritten_from: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct SeededMessageParts {
    pub fields: SeededFields,
    pub blocks: BTreeMap<usize, SeededBlock>,
}

pub fn record_message(record: &SeededMessageRecord) -> (&SeededMessage, bool) {
    match record {
        SeededMessageRecord::MessageStart { message }
        | SeededMessageRecord::AssistantStart { message } => (message, false),
        SeededMessageRecord::MessageEnd { message }
        | SeededMessageRecord::AssistantDone { message }
        | SeededMessageRecord::AssistantError { error: message } => (message, true),
    }
}

/// The whole public message wins: a terminal rebuilds every block from its content, so no index it
/// no longer names survives. Raw tool spelling is kept only from an ENDED block below the
/// producer's cutoff and not invalidated, so an abandoned stream's partial raw text and preview
/// never outlive the terminal.
pub fn seed_message(
    draft: &mut CompactSeededDraft,
    message: &SeededMessage,
    terminal: bool,
    previous: Option<&BTreeMap<usize, ActiveBlock>>,
    previous_seeded: Option<&BTreeMap<usize, SeededBlock>>,
    cut: &TerminalCut,
) -> Result<SeededMessageParts, CompactError> {
    let mut blocks = BTreeMap::new();
    let invalidated: HashSet<usize> = cut.invalidated_blocks.iter().copied().collect();
    let rewritten_from = cut.rewritten_from.unwrap_or(usize::MAX);
    let role = message.get("role").and_then(Value::as_str).unwrap_or("");
    if role != "assistant" {
        let fields_map = message.as_object().cloned().unwrap_or_default();
        return Ok(SeededMessageParts {
            fields: draft.fields(&fields_map, None, false)?,
            blocks,
        });
    }
    let mut metadata = message.as_object().cloned().unwrap_or_default();
    let content = metadata
        .remove("content")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    for (index, public_block) in content.iter().enumerate() {
        let mut block = seed_block(draft, public_block, None)?;
        let old = previous_seeded.and_then(|map| map.get(&index));
        let keep_raw = terminal
            && block.block_type == "toolCall"
            && old.is_some_and(|old| {
                old.block_type == "toolCall" && old.phase == SeededBlockPhase::Ended
            })
            && index < rewritten_from
            && !invalidated.contains(&index);
        if keep_raw && let Some(old) = old {
            let mut body = match previous.and_then(|map| map.get(&index)) {
                Some(prev_body) => draft.clone_block(prev_body)?,
                None => draft.clone_block(&old.body)?,
            };
            body.witness.clear();
            block.body = body;
            block.parsed_through = old.parsed_through;
        }
        block.phase = if terminal {
            SeededBlockPhase::Ended
        } else {
            SeededBlockPhase::Initial
        };
        blocks.insert(index, block);
    }
    Ok(SeededMessageParts {
        fields: draft.fields(&metadata, None, false)?,
        blocks,
    })
}

/// An indexed record's usage and changed metadata fold into the message's own fields in the same
/// step.
pub fn update_fields(
    draft: &mut CompactSeededDraft,
    fields: Option<&SeededFields>,
    usage: Option<&Value>,
    message_metadata: Option<&Map<String, Value>>,
) -> Result<Option<SeededFields>, CompactError> {
    if usage.is_none() && message_metadata.is_none() {
        return Ok(fields.cloned());
    }
    let mut changed = message_metadata.cloned().unwrap_or_default();
    if let Some(usage) = usage {
        changed.insert("usage".into(), usage.clone());
    }
    Ok(Some(draft.fields(&changed, fields, true)?))
}

/// Validate `rewritten_from` / `invalidated_blocks` when constructed from raw integer inputs.
pub fn validate_terminal_cut(
    rewritten_from: Option<i64>,
    invalidated_blocks: &[i64],
) -> Result<TerminalCut, CompactError> {
    if let Some(rf) = rewritten_from
        && rf < 0
    {
        return Err(CompactTransitionError::new(
            crate::compact_transition::CompactRejection::InvalidCoverage,
            format!("invalid_terminal: rewrittenFrom {rf}"),
        )
        .into());
    }
    let mut validated_invalidated = Vec::with_capacity(invalidated_blocks.len());
    for &idx in invalidated_blocks {
        if idx < 0 {
            return Err(CompactTransitionError::new(
                crate::compact_transition::CompactRejection::InvalidCoverage,
                format!("invalid_terminal: invalidatedBlocks {idx}"),
            )
            .into());
        }
        validated_invalidated.push(usize::try_from(idx).unwrap_or(0));
    }
    Ok(TerminalCut {
        invalidated_blocks: validated_invalidated,
        rewritten_from: rewritten_from.map(|rf| usize::try_from(rf).unwrap_or(0)),
    })
}

/// The cold edit/install step: a settled row replaced whole from an authoritative message, never a
/// partial patch.
pub fn install_settled_message(
    store: &CompactContentStore,
    block_roots: &CompactComponentRoots<FrozenBlock>,
    settled: &CompactContentIndex<SettledRow>,
    message_id: &str,
    message: &SeededMessage,
    persistence: CompactPersistence,
) -> Result<(), CompactError> {
    let mut draft = CompactSeededDraft::new(store.clone());
    let SeededMessageParts { blocks, .. } = seed_message(
        &mut draft,
        message,
        true,
        None,
        None,
        &TerminalCut::default(),
    )?;
    let mut root = ComponentRoot::default();
    for (index, block) in &blocks {
        let next = block_roots.set(
            &root,
            &format!("{index:016}"),
            Some(frozen_block(*index, &block.body)),
        );
        match next {
            Ok(next) => root = next,
            Err(error) => {
                block_roots.release(root);
                return Err(error);
            }
        }
    }
    let set_result = settled.set(
        message_id,
        &SettledRow {
            message_id: message_id.to_owned(),
            blocks: root.reference.clone(),
            persistence,
        },
    );
    block_roots.release(root);
    set_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_content_store::CompactContentStoreOptions;
    use crate::compact_seeded_block::{
        SeedBlockInput, SeedFrontier, SeededBlockEvent, fold_seeded_block,
    };
    use serde_json::json;

    #[test]
    fn keeps_ended_tool_raw_below_cutoff_and_discards_invalidated_or_open_raw() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let mut draft = CompactSeededDraft::new(store);
        let ended_tool = fold_seeded_block(
            &mut draft,
            None,
            &SeededBlockEvent::ToolcallStart {
                content_index: 0,
                id: "tc0".into(),
                tool_name: "read".into(),
                seed: SeedBlockInput {
                    block: json!({"type":"toolCall","id":"tc0","name":"read","arguments":{}}),
                    frontier: SeedFrontier {
                        covered_code_units: 0,
                        next_offset: Some(7),
                        raw_argument_prefix: Some("{\"a\":1}".into()),
                        parsed_through: Some(7),
                    },
                },
            },
        )
        .expect("start0");
        let ended_tool = fold_seeded_block(
            &mut draft,
            Some(&ended_tool),
            &SeededBlockEvent::ToolcallEnd {
                content_index: 0,
                tool_call: json!({"type":"toolCall","id":"tc0","name":"read","arguments":{"a":1}}),
            },
        )
        .expect("end0");
        let mut prev_seeded = BTreeMap::new();
        prev_seeded.insert(0, ended_tool.clone());
        prev_seeded.insert(1, ended_tool);
        let msg = json!({
            "role": "assistant",
            "content": [
                {"type": "toolCall", "id": "tc0", "name": "read", "arguments": {"a": 1}},
                {"type": "toolCall", "id": "tc1", "name": "read", "arguments": {"b": 2}}
            ]
        });
        let parts = seed_message(
            &mut draft,
            &msg,
            true,
            None,
            Some(&prev_seeded),
            &TerminalCut {
                invalidated_blocks: vec![1],
                rewritten_from: Some(2),
            },
        )
        .expect("seed");
        assert_eq!(parts.blocks[&0].body.tail, "{\"a\":1}");
        assert_eq!(parts.blocks[&1].body.tail, "");
    }
}
