//! senpi `modes/rpc/compact-seeded-block.ts`: block-level seeding and delta folding with
//! explicit parse/seed frontier validation.

use crate::compact_active_block::{ActiveBlock, append_block, delta_block};
use crate::compact_content_store::{LeafId, utf16_len};
use crate::compact_read_lease::CompactError;
use crate::compact_seeded_draft::{CompactSeededDraft, SeededFields};
use crate::compact_text_view::FrozenBlock;
use crate::compact_transition::{CompactBlockKind, CompactRejection, CompactTransitionError};
use maho_ai::utils::assistant_seed_frontier::seed_next_offset;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeededBlockPhase {
    Initial,
    Open,
    Ended,
}

impl SeededBlockPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Open => "open",
            Self::Ended => "ended",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeedFrontier {
    pub covered_code_units: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_argument_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parsed_through: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct SeededBlock {
    pub block_type: String,
    pub fields: SeededFields,
    pub body: ActiveBlock,
    pub signature: ActiveBlock,
    /// `Initial`: a slot from a message start's content, taken over by that index's own block start.
    pub phase: SeededBlockPhase,
    pub parsed_through: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureRoots {
    pub root: LeafId,
    pub tail_root: LeafId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrozenSeededBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    pub fields: SeededFields,
    pub phase: SeededBlockPhase,
    pub parsed_through: usize,
    pub signature: SignatureRoots,
}

/// Seeded block events folded by `fold_seeded_block`.
#[derive(Debug, Clone, PartialEq)]
pub enum SeededBlockEvent {
    TextStart {
        content_index: usize,
        seed: SeedBlockInput,
    },
    ThinkingStart {
        content_index: usize,
        seed: SeedBlockInput,
    },
    ToolcallStart {
        content_index: usize,
        id: String,
        tool_name: String,
        seed: SeedBlockInput,
    },
    TextDelta {
        content_index: usize,
        offset: usize,
        delta: String,
    },
    ThinkingDelta {
        content_index: usize,
        offset: usize,
        delta: String,
    },
    ToolcallDelta {
        content_index: usize,
        offset: usize,
        delta: String,
        parsed_through: i64,
    },
    TextEnd {
        content_index: usize,
        block: Value,
    },
    ThinkingEnd {
        content_index: usize,
        block: Value,
    },
    ToolcallEnd {
        content_index: usize,
        tool_call: Value,
    },
    MessageToolArgumentsReset {
        content_index: usize,
        raw_argument_prefix: String,
        parsed_through: i64,
    },
    MessageBlockMetadata {
        content_index: usize,
        thinking_signature_delta: Option<String>,
        thinking_signature: Option<String>,
        text_signature: Option<String>,
        extra_fields: Map<String, Value>,
    },
    MessageBlockState {
        content_index: usize,
        phase: String,
        block: Value,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeedBlockInput {
    pub block: Value,
    pub frontier: SeedFrontier,
}

impl SeededBlockEvent {
    pub fn content_index(&self) -> usize {
        match self {
            Self::TextStart { content_index, .. }
            | Self::ThinkingStart { content_index, .. }
            | Self::ToolcallStart { content_index, .. }
            | Self::TextDelta { content_index, .. }
            | Self::ThinkingDelta { content_index, .. }
            | Self::ToolcallDelta { content_index, .. }
            | Self::TextEnd { content_index, .. }
            | Self::ThinkingEnd { content_index, .. }
            | Self::ToolcallEnd { content_index, .. }
            | Self::MessageToolArgumentsReset { content_index, .. }
            | Self::MessageBlockMetadata { content_index, .. }
            | Self::MessageBlockState { content_index, .. } => *content_index,
        }
    }
}

fn seeded_rejection(reason: &str, detail: impl std::fmt::Display) -> CompactError {
    CompactError::Io(format!("compact transition rejected ({reason}): {detail}"))
}

fn parse_frontier(next: i64, length: usize, previous: usize) -> Result<usize, CompactError> {
    let length_i64 = i64::try_from(length).unwrap_or(i64::MAX);
    let previous_i64 = i64::try_from(previous).unwrap_or(i64::MAX);
    if next < previous_i64 || next > length_i64 {
        return Err(seeded_rejection(
            "invalid_parse_frontier",
            format!("{next} outside [{previous}, {length}]"),
        ));
    }
    Ok(usize::try_from(next).unwrap_or(0))
}

fn seed_offset(next: Option<i64>, length: usize) -> Result<usize, CompactError> {
    seed_next_offset(next, length)
        .map_err(|err| seeded_rejection("invalid_seed_frontier", err.code.as_str()))
}

pub fn seed_block(
    draft: &mut CompactSeededDraft,
    block: &Value,
    frontier: Option<&SeedFrontier>,
) -> Result<SeededBlock, CompactError> {
    let block_type = block
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let text = match block_type.as_str() {
        "text" => block.get("text").and_then(Value::as_str).unwrap_or(""),
        "thinking" => block.get("thinking").and_then(Value::as_str).unwrap_or(""),
        _ => frontier
            .and_then(|f| f.raw_argument_prefix.as_deref())
            .unwrap_or(""),
    };
    let text_units = usize::try_from(utf16_len(text)).unwrap_or(0);
    let next_offset = match frontier {
        Some(frontier) => seed_offset(frontier.next_offset, text_units)?,
        None => text_units,
    };
    let covered = frontier.map_or(0, |f| f.covered_code_units);
    let text_units_i64 = i64::try_from(text_units).unwrap_or(i64::MAX);
    if (covered != 0 && covered != text_units_i64)
        || (covered == 0 && next_offset != text_units)
        || covered < 0
    {
        return Err(CompactTransitionError::new(
            CompactRejection::InvalidCoverage,
            format!("{covered} of {text_units}"),
        )
        .into());
    }
    let parsed_through = parse_frontier(
        frontier.and_then(|f| f.parsed_through).unwrap_or(0),
        text_units,
        0,
    )?;
    let mut fields_map = Map::new();
    if let Some(obj) = block.as_object() {
        for (key, value) in obj {
            if key == "text" || key == "thinking" {
                continue;
            }
            if key == "thinkingSignature" || key == "textSignature" {
                fields_map.insert(key.clone(), Value::String(String::new()));
            } else {
                fields_map.insert(key.clone(), value.clone());
            }
        }
    }
    let kind = match block_type.as_str() {
        "thinking" => CompactBlockKind::Thinking,
        "toolCall" => CompactBlockKind::ToolArguments,
        _ => CompactBlockKind::Text,
    };
    let mut body = draft.text(kind, text)?;
    body.emitted = i64::try_from(next_offset).unwrap_or(0);
    body.covered = covered;
    body.witness = if next_offset < usize::try_from(covered).unwrap_or(0) {
        text.to_owned()
    } else {
        String::new()
    };
    let signature_text = match block_type.as_str() {
        "text" => block
            .get("textSignature")
            .and_then(Value::as_str)
            .unwrap_or(""),
        "thinking" => block
            .get("thinkingSignature")
            .and_then(Value::as_str)
            .unwrap_or(""),
        _ => "",
    };
    Ok(SeededBlock {
        block_type,
        fields: draft.fields(&fields_map, None, false)?,
        body,
        signature: draft.text(CompactBlockKind::Text, signature_text)?,
        phase: SeededBlockPhase::Open,
        parsed_through,
    })
}

/// Validation and candidate ownership precede the session owner's single publication.
pub fn fold_seeded_block(
    draft: &mut CompactSeededDraft,
    old: Option<&SeededBlock>,
    event: &SeededBlockEvent,
) -> Result<SeededBlock, CompactError> {
    match event {
        SeededBlockEvent::TextStart {
            content_index,
            seed,
        }
        | SeededBlockEvent::ThinkingStart {
            content_index,
            seed,
        }
        | SeededBlockEvent::ToolcallStart {
            content_index,
            seed,
            ..
        } => {
            if old.is_some_and(|b| b.phase != SeededBlockPhase::Initial) {
                return Err(CompactTransitionError::new(
                    CompactRejection::DuplicateBlock,
                    content_index.to_string(),
                )
                .into());
            }
            return seed_block(draft, &seed.block, Some(&seed.frontier));
        }
        SeededBlockEvent::MessageBlockState {
            content_index,
            phase,
            block,
        } if phase == "insert" => {
            if old.is_some_and(|b| b.phase != SeededBlockPhase::Initial) {
                return Err(CompactTransitionError::new(
                    CompactRejection::DuplicateBlock,
                    content_index.to_string(),
                )
                .into());
            }
            return seed_block(draft, block, None);
        }
        _ => {}
    }

    let Some(old) = old else {
        return Err(CompactTransitionError::new(
            CompactRejection::UnknownBlock,
            event.content_index().to_string(),
        )
        .into());
    };
    if old.phase != SeededBlockPhase::Open {
        return Err(seeded_rejection("invalid_phase", old.phase.as_str()));
    }

    match event {
        SeededBlockEvent::TextDelta { offset, delta, .. }
        | SeededBlockEvent::ThinkingDelta { offset, delta, .. }
        | SeededBlockEvent::ToolcallDelta { offset, delta, .. } => {
            let kind = match event {
                SeededBlockEvent::TextDelta { .. } => "text",
                SeededBlockEvent::ThinkingDelta { .. } => "thinking",
                _ => "toolCall",
            };
            if old.block_type != kind {
                return Err(seeded_rejection(
                    "wrong_kind",
                    format!("{} / {kind}", old.block_type),
                ));
            }
            let parsed_through_i64 = match event {
                SeededBlockEvent::ToolcallDelta { parsed_through, .. } => *parsed_through,
                _ => 0,
            };
            let max_units = usize::try_from(old.body.units)
                .unwrap_or(0)
                .max(offset.saturating_add(usize::try_from(utf16_len(delta)).unwrap_or(0)));
            let parsed_through = parse_frontier(parsed_through_i64, max_units, old.parsed_through)?;
            let mut body = draft.clone_block(&old.body)?;
            delta_block(
                draft.store(),
                &mut body,
                i64::try_from(*offset).unwrap_or(i64::MAX),
                delta,
            )?;
            Ok(SeededBlock {
                body,
                parsed_through,
                ..old.clone()
            })
        }
        SeededBlockEvent::MessageToolArgumentsReset {
            raw_argument_prefix,
            parsed_through,
            ..
        } => {
            if old.block_type != "toolCall" {
                return Err(seeded_rejection("wrong_kind", &old.block_type));
            }
            let prefix_units = usize::try_from(utf16_len(raw_argument_prefix)).unwrap_or(0);
            let parsed_through = parse_frontier(*parsed_through, prefix_units, 0)?;
            Ok(SeededBlock {
                body: draft.text(CompactBlockKind::ToolArguments, raw_argument_prefix)?,
                parsed_through,
                ..old.clone()
            })
        }
        SeededBlockEvent::MessageBlockMetadata {
            thinking_signature_delta,
            thinking_signature,
            text_signature,
            extra_fields,
            ..
        } => {
            if (thinking_signature_delta.is_some() || thinking_signature.is_some())
                && old.block_type != "thinking"
            {
                return Err(seeded_rejection("wrong_kind", &old.block_type));
            }
            if text_signature.is_some() && old.block_type != "text" {
                return Err(seeded_rejection("wrong_kind", &old.block_type));
            }
            let replacement = thinking_signature.as_deref().or(text_signature.as_deref());
            let mut signature = match replacement {
                Some(text) => draft.text(CompactBlockKind::Text, text)?,
                None => draft.clone_block(&old.signature)?,
            };
            if let Some(delta) = thinking_signature_delta {
                append_block(draft.store(), &mut signature, delta)?;
            }
            let mut changed = extra_fields.clone();
            if thinking_signature.is_some() || thinking_signature_delta.is_some() {
                changed.insert("thinkingSignature".into(), Value::String(String::new()));
            }
            if text_signature.is_some() {
                changed.insert("textSignature".into(), Value::String(String::new()));
            }
            Ok(SeededBlock {
                signature,
                fields: draft.fields(&changed, Some(&old.fields), false)?,
                ..old.clone()
            })
        }
        SeededBlockEvent::MessageBlockState { block, .. }
        | SeededBlockEvent::TextEnd { block, .. }
        | SeededBlockEvent::ThinkingEnd { block, .. } => {
            let kind = match event {
                SeededBlockEvent::MessageBlockState { .. } => {
                    block.get("type").and_then(Value::as_str).unwrap_or("")
                }
                SeededBlockEvent::TextEnd { .. } => "text",
                _ => "thinking",
            };
            if old.block_type != kind {
                return Err(seeded_rejection(
                    "wrong_kind",
                    format!("{} / {kind}", old.block_type),
                ));
            }
            let mut ended = seed_block(draft, block, None)?;
            ended.phase = SeededBlockPhase::Ended;
            Ok(ended)
        }
        SeededBlockEvent::ToolcallEnd { tool_call, .. } => {
            if old.block_type != "toolCall" {
                return Err(seeded_rejection("wrong_kind", &old.block_type));
            }
            let fields_map = tool_call.as_object().cloned().unwrap_or_default();
            Ok(SeededBlock {
                fields: draft.fields(&fields_map, None, false)?,
                phase: SeededBlockPhase::Ended,
                ..old.clone()
            })
        }
        SeededBlockEvent::TextStart { .. }
        | SeededBlockEvent::ThinkingStart { .. }
        | SeededBlockEvent::ToolcallStart { .. } => unreachable!(),
    }
}

pub fn frozen_seeded_block(block: &SeededBlock) -> FrozenSeededBlock {
    FrozenSeededBlock {
        block_type: block.block_type.clone(),
        fields: block.fields.clone(),
        phase: block.phase,
        parsed_through: block.parsed_through,
        signature: SignatureRoots {
            root: block.signature.root,
            tail_root: block.signature.tail_root,
        },
    }
}

/// The published component row of one active block.
pub fn frozen_block(content_index: usize, block: &ActiveBlock) -> FrozenBlock {
    FrozenBlock {
        content_index,
        kind: block.kind,
        root: block.root,
        tail: String::new(),
        tail_root: Some(block.tail_root),
    }
}

pub fn seeded_block_leaves(block: &FrozenSeededBlock) -> Vec<LeafId> {
    let mut leaves = vec![block.signature.root, block.signature.tail_root];
    leaves.extend(block.fields.values().map(|reference| reference.owner));
    leaves
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_content_store::{CompactContentStore, CompactContentStoreOptions};
    use serde_json::json;

    #[test]
    fn seeds_deltas_metadata_and_resets_tool_arguments() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let mut draft = CompactSeededDraft::new(store);
        let started = fold_seeded_block(
            &mut draft,
            None,
            &SeededBlockEvent::ToolcallStart {
                content_index: 0,
                id: "tc1".into(),
                tool_name: "read".into(),
                seed: SeedBlockInput {
                    block: json!({"type":"toolCall","id":"tc1","name":"read","arguments":{}}),
                    frontier: SeedFrontier {
                        covered_code_units: 0,
                        next_offset: Some(1),
                        raw_argument_prefix: Some("{".into()),
                        parsed_through: Some(1),
                    },
                },
            },
        )
        .expect("start");
        assert_eq!(started.phase, SeededBlockPhase::Open);
        assert_eq!(started.body.tail, "{");
        let delta = fold_seeded_block(
            &mut draft,
            Some(&started),
            &SeededBlockEvent::ToolcallDelta {
                content_index: 0,
                offset: 1,
                delta: "\"a\":1}".into(),
                parsed_through: 7,
            },
        )
        .expect("delta");
        assert_eq!(delta.body.tail, "{\"a\":1}");
        assert_eq!(delta.parsed_through, 7);
        let reset = fold_seeded_block(
            &mut draft,
            Some(&delta),
            &SeededBlockEvent::MessageToolArgumentsReset {
                content_index: 0,
                raw_argument_prefix: "{\"b\":".into(),
                parsed_through: 0,
            },
        )
        .expect("reset");
        assert_eq!(reset.body.tail, "{\"b\":");
        assert_eq!(reset.parsed_through, 0);
        let ended = fold_seeded_block(
            &mut draft,
            Some(&reset),
            &SeededBlockEvent::ToolcallEnd {
                content_index: 0,
                tool_call: json!({"type":"toolCall","id":"tc1","name":"read","arguments":{"b":2}}),
            },
        )
        .expect("end");
        assert_eq!(ended.phase, SeededBlockPhase::Ended);
        let frozen = frozen_seeded_block(&ended);
        assert!(!seeded_block_leaves(&frozen).is_empty());
    }
}
