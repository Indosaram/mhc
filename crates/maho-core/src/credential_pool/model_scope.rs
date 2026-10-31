//! Port of senpi `packages/coding-agent/src/core/credential-pool/model-scope.ts`.
//!
//! Model-scoped rate limits (senpi#2555). An Anthropic subscription names the exceeded window in its
//! failure text: `five_hour` and `seven_day` bind the account, while the Opus, Sonnet and overage
//! windows bind one model family ("Opus limit", "Sonnet limit", "Fable limit"). The family name in the
//! limit phrase is the model-scope signal; everything without a family stays account-wide.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;

static FAMILY_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:opus|sonnet|haiku|fable|mythos)$")
        .unwrap_or_else(|e| panic!("family key: {e}"))
});
static MODEL_FAMILY_LIMIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(opus|sonnet|haiku|fable|mythos)(?:\s+\d+(?:\.\d+)?)?(?:\s+(?:weekly|daily|monthly|session|\d+-hour))?\s+limit\b",
    )
    .unwrap_or_else(|e| panic!("model family limit: {e}"))
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelBlock {
    pub blocked_until: i64,
}

/// A model block's expiry, keyed by model family (`fable`) or, failing that, the exact model id.
pub type ModelBlocks = BTreeMap<String, ModelBlock>;

pub fn rate_limit_model_family(text: &str) -> Option<String> {
    MODEL_FAMILY_LIMIT
        .captures(text)
        .and_then(|captures| captures.get(1))
        .map(|family| family.as_str().to_lowercase())
}

fn in_family(model_id: &str, family: &str) -> bool {
    let lower = model_id.to_lowercase();
    let family = family.to_lowercase();
    let bytes = lower.as_bytes();
    lower.match_indices(&family).any(|(start, matched)| {
        let end = start + matched.len();
        let before_ok = start == 0 || !bytes[start - 1].is_ascii_lowercase();
        let after_ok = end == bytes.len() || !bytes[end].is_ascii_lowercase();
        before_ok && after_ok
    })
}

/// Where a model-scoped block is recorded: the family when the requested model belongs to it (one
/// limit covers every Fable model), otherwise the exact model id, so a block never leaks onto a model
/// the response did not name.
pub fn model_block_key(family: &str, model_id: &str) -> String {
    if in_family(model_id, family) {
        family.to_owned()
    } else {
        model_id.to_owned()
    }
}

fn applies(key: &str, model_id: &str) -> bool {
    key == model_id || (FAMILY_KEY.is_match(key) && in_family(model_id, key))
}

/// The latest live expiry among the blocks that bind `model_id`, or `None` when it is free.
pub fn active_model_block_until(
    blocks: Option<&ModelBlocks>,
    model_id: Option<&str>,
    now: i64,
) -> Option<i64> {
    let (blocks, model_id) = (blocks?, model_id?);
    blocks
        .iter()
        .filter(|(key, block)| block.blocked_until > now && applies(key, model_id))
        .map(|(_, block)| block.blocked_until)
        .max()
}

/// Drops expired entries and, when `served_model_id` is given, the entries that bind it (a model that
/// just served is not limited). `None` when empty, so a record never carries an empty map.
pub fn prune_model_blocks(
    blocks: Option<&ModelBlocks>,
    now: i64,
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
    blocked_until: i64,
    now: i64,
) -> ModelBlocks {
    let added = ModelBlocks::from([(key.to_owned(), ModelBlock { blocked_until })]);
    merge_model_blocks(
        prune_model_blocks(blocks, now, None).as_ref(),
        Some(&added),
        now,
    )
    .unwrap_or_default()
}

/// Union of two block maps, the later expiry winning per key: a failure without a reset hint, or a
/// concurrent writer's stale snapshot, never shortens a live block.
pub fn merge_model_blocks(
    left: Option<&ModelBlocks>,
    right: Option<&ModelBlocks>,
    now: i64,
) -> Option<ModelBlocks> {
    let mut merged = prune_model_blocks(left, now, None).unwrap_or_default();
    for (key, block) in prune_model_blocks(right, now, None).unwrap_or_default() {
        let current = merged
            .get(&key)
            .map_or(0, |existing| existing.blocked_until);
        merged.insert(
            key,
            ModelBlock {
                blocked_until: current.max(block.blocked_until),
            },
        );
    }
    (!merged.is_empty()).then_some(merged)
}

fn iso(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|time| time.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_default()
}

pub fn describe_model_blocks(blocks: Option<&ModelBlocks>, now: i64) -> Vec<String> {
    prune_model_blocks(blocks, now, None)
        .unwrap_or_default()
        .iter()
        .map(|(key, block)| format!("blocked for {key} until {}", iso(block.blocked_until)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks(entries: &[(&str, i64)]) -> ModelBlocks {
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
    fn the_family_comes_from_the_limit_phrase() {
        assert_eq!(
            rate_limit_model_family("You've hit your Opus limit").as_deref(),
            Some("opus")
        );
        assert_eq!(
            rate_limit_model_family("Fable 4.5 weekly limit reached").as_deref(),
            Some("fable")
        );
        assert_eq!(
            rate_limit_model_family("Sonnet 5-hour limit").as_deref(),
            Some("sonnet")
        );
        assert_eq!(
            rate_limit_model_family("You've hit your session limit"),
            None
        );
        assert_eq!(rate_limit_model_family("weekly limit"), None);
    }

    #[test]
    fn the_block_key_is_the_family_only_for_its_models() {
        assert_eq!(model_block_key("fable", "claude-fable-4"), "fable");
        assert_eq!(
            model_block_key("opus", "claude-sonnet-4-5"),
            "claude-sonnet-4-5"
        );
        assert_eq!(model_block_key("opus", "opusculum"), "opusculum");
    }

    #[test]
    fn active_blocks_bind_by_family_or_exact_id() {
        let map = blocks(&[("opus", 100), ("gpt-5", 200), ("old", 5)]);
        assert_eq!(
            active_model_block_until(Some(&map), Some("claude-opus-4-1"), 10),
            Some(100)
        );
        assert_eq!(
            active_model_block_until(Some(&map), Some("gpt-5"), 10),
            Some(200)
        );
        assert_eq!(active_model_block_until(Some(&map), Some("old"), 10), None);
        assert_eq!(
            active_model_block_until(Some(&map), Some("claude-sonnet"), 10),
            None
        );
        assert_eq!(active_model_block_until(None, Some("gpt-5"), 10), None);
        assert_eq!(active_model_block_until(Some(&map), None, 10), None);
    }

    #[test]
    fn pruning_drops_expired_and_served_entries() {
        let map = blocks(&[("opus", 100), ("gpt-5", 200), ("old", 5)]);
        assert_eq!(
            prune_model_blocks(Some(&map), 10, None),
            Some(blocks(&[("opus", 100), ("gpt-5", 200)]))
        );
        assert_eq!(
            prune_model_blocks(Some(&map), 10, Some("claude-opus-4")),
            Some(blocks(&[("gpt-5", 200)]))
        );
        assert_eq!(
            prune_model_blocks(Some(&blocks(&[("old", 5)])), 10, None),
            None
        );
    }

    #[test]
    fn merging_keeps_the_later_expiry() {
        let left = blocks(&[("opus", 100)]);
        let right = blocks(&[("opus", 50), ("gpt-5", 300)]);
        assert_eq!(
            merge_model_blocks(Some(&left), Some(&right), 10),
            Some(blocks(&[("opus", 100), ("gpt-5", 300)]))
        );
        assert_eq!(
            with_model_block(Some(&left), "opus", 40, 10),
            blocks(&[("opus", 100)])
        );
        assert_eq!(merge_model_blocks(None, None, 10), None);
    }

    #[test]
    fn descriptions_name_the_iso_expiry() {
        assert_eq!(
            describe_model_blocks(Some(&blocks(&[("opus", 0)])), -1),
            vec!["blocked for opus until 1970-01-01T00:00:00.000Z".to_owned()]
        );
    }
}
