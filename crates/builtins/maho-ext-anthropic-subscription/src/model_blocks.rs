//! Port of senpi `core/credential-pool/model-scope.ts` (pin 903b8336) over this crate's `f64` epoch-ms times.
//!
//! deviation: `maho_core::credential_pool::model_scope` (lane L-core) carries the same rules over `i64`; it is not on
//! this lane's base, so the slot fields use this mirror. Integration may switch the slot type to the core module.
//!
//! Model-scoped rate limits (senpi#2555): a limit naming a model family ("Opus limit") blocks that family on the
//! account, keyed by the family when the requested model belongs to it, else by the exact model id.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelBlock {
    pub blocked_until: f64,
}

/// A model block's expiry, keyed by model family (`fable`) or, failing that, the exact model id.
pub type ModelBlocks = BTreeMap<String, ModelBlock>;

const FAMILIES: [&str; 5] = ["opus", "sonnet", "haiku", "fable", "mythos"];

fn in_family(model_id: &str, family: &str) -> bool {
    let lower = model_id.to_lowercase();
    let family = family.to_lowercase();
    let bytes = lower.as_bytes();
    lower.match_indices(&family).any(|(start, matched)| {
        let end = start + matched.len();
        (start == 0 || !bytes[start - 1].is_ascii_lowercase())
            && (end == bytes.len() || !bytes[end].is_ascii_lowercase())
    })
}

/// Where a model-scoped block is recorded: the family when the requested model belongs to it, otherwise the exact id.
pub fn model_block_key(family: &str, model_id: &str) -> String {
    if in_family(model_id, family) {
        family.to_owned()
    } else {
        model_id.to_owned()
    }
}

fn applies(key: &str, model_id: &str) -> bool {
    key == model_id || (FAMILIES.contains(&key) && in_family(model_id, key))
}

/// The latest live expiry among the blocks that bind `model_id`, or `None` when it is free.
pub fn active_model_block_until(
    blocks: Option<&ModelBlocks>,
    model_id: Option<&str>,
    now: f64,
) -> Option<f64> {
    let (blocks, model_id) = (blocks?, model_id?);
    blocks
        .iter()
        .filter(|(key, block)| block.blocked_until > now && applies(key, model_id))
        .map(|(_, block)| block.blocked_until)
        .max_by(f64::total_cmp)
}

/// Drops expired entries and, when `served_model_id` is given, the entries that bind it. `None` when empty.
pub fn prune_model_blocks(
    blocks: Option<&ModelBlocks>,
    now: f64,
    served_model_id: Option<&str>,
) -> Option<ModelBlocks> {
    let kept: ModelBlocks = blocks?
        .iter()
        .filter(|(key, block)| {
            block.blocked_until > now && served_model_id.is_none_or(|served| !applies(key, served))
        })
        .map(|(key, block)| (key.clone(), *block))
        .collect();
    (!kept.is_empty()).then_some(kept)
}

pub fn with_model_block(
    blocks: Option<&ModelBlocks>,
    key: &str,
    blocked_until: f64,
    now: f64,
) -> ModelBlocks {
    let added = ModelBlocks::from([(key.to_owned(), ModelBlock { blocked_until })]);
    merge_model_blocks(
        prune_model_blocks(blocks, now, None).as_ref(),
        Some(&added),
        now,
    )
    .unwrap_or_default()
}

/// Union of two block maps, the later expiry winning per key.
pub fn merge_model_blocks(
    left: Option<&ModelBlocks>,
    right: Option<&ModelBlocks>,
    now: f64,
) -> Option<ModelBlocks> {
    let mut merged = prune_model_blocks(left, now, None).unwrap_or_default();
    for (key, block) in prune_model_blocks(right, now, None).unwrap_or_default() {
        let current = merged
            .get(&key)
            .map_or(0.0, |existing| existing.blocked_until);
        merged.insert(
            key,
            ModelBlock {
                blocked_until: current.max(block.blocked_until),
            },
        );
    }
    (!merged.is_empty()).then_some(merged)
}

pub(crate) fn iso_ms(ms: f64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .map(|time| time.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_default()
}

pub fn describe_model_blocks(blocks: Option<&ModelBlocks>, now: f64) -> Vec<String> {
    prune_model_blocks(blocks, now, None)
        .unwrap_or_default()
        .iter()
        .map(|(key, block)| format!("blocked for {key} until {}", iso_ms(block.blocked_until)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks(entries: &[(&str, f64)]) -> ModelBlocks {
        entries
            .iter()
            .map(|(key, until)| {
                (
                    (*key).to_owned(),
                    ModelBlock {
                        blocked_until: *until,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn family_blocks_bind_the_family_and_exact_ids_only_themselves() {
        assert_eq!(model_block_key("fable", "claude-fable-4"), "fable");
        assert_eq!(
            model_block_key("opus", "claude-sonnet-4-5"),
            "claude-sonnet-4-5"
        );
        let map = blocks(&[("opus", 100.0), ("gpt-5", 200.0), ("old", 5.0)]);
        assert_eq!(
            active_model_block_until(Some(&map), Some("claude-opus-4-1"), 10.0),
            Some(100.0)
        );
        assert_eq!(
            active_model_block_until(Some(&map), Some("claude-sonnet"), 10.0),
            None
        );
        assert_eq!(
            active_model_block_until(Some(&map), Some("old"), 10.0),
            None
        );
        assert_eq!(
            merge_model_blocks(
                Some(&blocks(&[("opus", 100.0)])),
                Some(&blocks(&[("opus", 50.0), ("gpt-5", 300.0)])),
                10.0
            ),
            Some(blocks(&[("opus", 100.0), ("gpt-5", 300.0)]))
        );
        assert_eq!(
            describe_model_blocks(Some(&blocks(&[("opus", 0.0)])), -1.0),
            vec!["blocked for opus until 1970-01-01T00:00:00.000Z".to_owned()]
        );
    }
}
