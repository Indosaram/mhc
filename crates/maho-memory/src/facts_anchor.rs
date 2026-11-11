//! Where a new facts batch starts, decided from durable state only.
//!
//! Spec: `docs/cleanroom/specs/memory-facts-anchor.md`. Four sources can each know how far extraction got:
//! two cursor watermarks, the consumed file, and batch files still on disk. Missing one re-extracts stored
//! facts; trusting only a stale one skips facts permanently. So the anchor is the **highest** position any
//! of them names.

use std::collections::BTreeMap;

use crate::facts_schema::FactsCursor;

/// One already-handled range end, retained as a message id.
///
/// Only the id is kept: the queue answers "how far", and positions come from the transcript it is being
/// asked about, which may have been compacted since the id was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedEnd {
    pub conversation_id: String,
    pub end_message_id: String,
}

/// The durable inputs that bound where new material begins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorInputs {
    pub cursor: FactsCursor,
    pub consumed: BTreeMap<String, String>,
    pub retained: Vec<RetainedEnd>,
}

/// Deduplicated because one end can appear in all four sources at once — the cursor's two watermarks,
/// the consumed file, and the retained file of the batch just enqueued — and a caller resolving each
/// candidate against the transcript would otherwise pay per spelling.
pub fn anchor_candidates(inputs: &AnchorInputs, conversation_id: &str) -> Vec<String> {
    let mut candidates: Vec<Option<&str>> = vec![
        inputs.cursor.enqueued_through_message_id.as_deref(),
        inputs.cursor.consumed_through_message_id.as_deref(),
        inputs.consumed.get(conversation_id).map(String::as_str),
    ];
    candidates.extend(
        inputs
            .retained
            .iter()
            .filter(|end| end.conversation_id == conversation_id)
            .map(|end| Some(end.end_message_id.as_str())),
    );
    let mut unique: Vec<String> = Vec::new();
    for candidate in candidates.into_iter().flatten() {
        if candidate.is_empty() || unique.iter().any(|seen| seen == candidate) {
            continue;
        }
        unique.push(candidate.to_owned());
    }
    unique
}

/// Built once per transcript instead of once per candidate: four watermarks plus a thousand retained
/// batches would otherwise walk the same transcript five thousand times.
pub fn last_canonical_positions(entries: &[crate::cursor::TranscriptEntry]) -> BTreeMap<&str, i64> {
    let mut positions = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        if entry.is_canonical() {
            positions.insert(entry.source_message_id.as_str(), index as i64);
        }
    }
    positions
}

/// `-1` when no source is usable. A candidate `>= len` is skipped because that id names a message this
/// snapshot does not extend beyond, so nothing follows it. An absent candidate also contributes nothing:
/// the transcript was compacted past it, and mapping it to `-1` would be indistinguishable from "nothing
/// handled yet", which is exactly the state that re-extracts.
pub fn effective_anchor(
    entries: &[crate::cursor::TranscriptEntry],
    inputs: &AnchorInputs,
    conversation_id: &str,
) -> i64 {
    let positions = last_canonical_positions(entries);
    let length = entries.len() as i64;
    let mut anchor = -1;
    for candidate in anchor_candidates(inputs, conversation_id) {
        let position = positions.get(candidate.as_str()).copied().unwrap_or(-1);
        if position >= length {
            continue;
        }
        anchor = anchor.max(position);
    }
    anchor
}

/// Strictly after, not at: the anchor line is already covered by a batch, and including it would
/// re-enqueue the same message on every run.
pub fn first_new_canonical(
    entries: &[crate::cursor::TranscriptEntry],
    anchor: i64,
) -> Option<usize> {
    entries
        .iter()
        .enumerate()
        .position(|(index, entry)| index as i64 > anchor && entry.is_canonical())
}

/// Searched from the end so the range covers everything after the anchor; a tail left out would re-anchor
/// in the middle and the batch would never reach the newest message.
pub fn last_canonical(entries: &[crate::cursor::TranscriptEntry]) -> Option<usize> {
    entries
        .iter()
        .rposition(crate::cursor::TranscriptEntry::is_canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::TranscriptEntry;

    fn entry(kind: &str, message: &str, text: &str) -> TranscriptEntry {
        TranscriptEntry {
            kind: kind.to_owned(),
            text: Some(text.to_owned()),
            captured_at: "2026-01-01T00:00:00Z".to_owned(),
            source_line_id: format!("line-{message}"),
            source_message_id: message.to_owned(),
        }
    }

    fn transcript() -> Vec<TranscriptEntry> {
        vec![
            entry("user", "m1", "hi"),
            entry("assistant", "m2", "one"),
            entry("assistant", "m3", "two"),
            entry("assistant", "m4", "three"),
        ]
    }

    fn inputs(enqueued: Option<&str>, consumed_cursor: Option<&str>) -> AnchorInputs {
        AnchorInputs {
            cursor: FactsCursor {
                version: 1,
                enqueued_through_message_id: enqueued.map(str::to_owned),
                enqueued_through_snapshot_line: -1,
                consumed_through_message_id: consumed_cursor.map(str::to_owned),
                consumed_through_snapshot_line: -1,
            },
            consumed: BTreeMap::new(),
            retained: Vec::new(),
        }
    }

    #[test]
    fn nothing_handled_yet_anchors_before_the_first_line() {
        assert_eq!(
            effective_anchor(&transcript(), &inputs(None, None), "c1"),
            -1
        );
    }

    #[test]
    fn the_enqueued_watermark_is_an_anchor() {
        assert_eq!(
            effective_anchor(&transcript(), &inputs(Some("m2"), None), "c1"),
            1
        );
    }

    #[test]
    fn the_consumed_cursor_is_an_anchor() {
        assert_eq!(
            effective_anchor(&transcript(), &inputs(None, Some("m3")), "c1"),
            2
        );
    }

    #[test]
    fn the_consumed_file_is_an_anchor() {
        let mut anchored = inputs(None, None);
        anchored.consumed.insert("c1".to_owned(), "m3".to_owned());
        assert_eq!(effective_anchor(&transcript(), &anchored, "c1"), 2);
    }

    #[test]
    fn a_retained_batch_is_an_anchor() {
        let mut anchored = inputs(None, None);
        anchored.retained.push(RetainedEnd {
            conversation_id: "c1".to_owned(),
            end_message_id: "m4".to_owned(),
        });
        assert_eq!(effective_anchor(&transcript(), &anchored, "c1"), 3);
    }

    #[test]
    fn the_highest_source_wins() {
        let mut anchored = inputs(Some("m2"), Some("m1"));
        anchored.consumed.insert("c1".to_owned(), "m3".to_owned());
        anchored.retained.push(RetainedEnd {
            conversation_id: "c1".to_owned(),
            end_message_id: "m4".to_owned(),
        });
        assert_eq!(effective_anchor(&transcript(), &anchored, "c1"), 3);
    }

    #[test]
    fn another_conversations_sources_never_move_this_anchor() {
        let mut anchored = inputs(None, None);
        anchored.consumed.insert("c2".to_owned(), "m4".to_owned());
        anchored.retained.push(RetainedEnd {
            conversation_id: "c2".to_owned(),
            end_message_id: "m4".to_owned(),
        });
        assert_eq!(effective_anchor(&transcript(), &anchored, "c1"), -1);
    }

    #[test]
    fn a_compacted_away_message_does_not_anchor() {
        assert_eq!(
            effective_anchor(&transcript(), &inputs(Some("gone"), None), "c1"),
            -1
        );
    }

    #[test]
    fn a_message_id_at_the_edge_contributes_nothing_further() {
        let mut anchored = inputs(Some("m4"), None);
        let mut one_more = transcript();
        one_more.push(entry("assistant", "m5", "four"));
        assert_eq!(effective_anchor(&one_more, &anchored, "c1"), 3);
        anchored.cursor.enqueued_through_message_id = Some("m5".to_owned());
        assert_eq!(effective_anchor(&one_more, &anchored, "c1"), 4);
    }

    #[test]
    fn duplicates_across_sources_appear_once() {
        let mut anchored = inputs(Some("m2"), Some("m2"));
        anchored.consumed.insert("c1".to_owned(), "m2".to_owned());
        let candidates = anchor_candidates(&anchored, "c1");
        assert_eq!(candidates, vec!["m2".to_owned()]);
    }

    #[test]
    fn an_empty_message_id_is_not_a_candidate() {
        let anchored = inputs(Some(""), Some(""));
        assert!(anchor_candidates(&anchored, "c1").is_empty());
        assert_eq!(effective_anchor(&transcript(), &anchored, "c1"), -1);
    }

    #[test]
    fn the_new_range_starts_strictly_after_the_anchor() {
        assert_eq!(first_new_canonical(&transcript(), -1), Some(1));
        assert_eq!(first_new_canonical(&transcript(), 1), Some(2));
    }

    #[test]
    fn a_non_canonical_entry_cannot_start_a_range() {
        let entries = vec![
            entry("assistant", "m1", "one"),
            entry("user", "m2", "hi"),
            entry("assistant", "m3", "two"),
        ];
        assert_eq!(first_new_canonical(&entries, 0), Some(2));
    }

    #[test]
    fn a_snapshot_with_nothing_new_has_no_start() {
        assert_eq!(first_new_canonical(&transcript(), 3), None);
        assert_eq!(first_new_canonical(&[], -1), None);
    }

    #[test]
    fn the_range_ends_at_the_last_canonical_entry() {
        let entries = vec![entry("assistant", "m1", "one"), entry("user", "m2", "hi")];
        assert_eq!(last_canonical(&entries), Some(0));
        assert_eq!(last_canonical(&[]), None);
    }

    #[test]
    fn the_last_position_wins_when_a_message_spans_lines() {
        let entries = vec![
            entry("assistant", "m1", "one"),
            entry("assistant", "m1", "one continued"),
        ];
        assert_eq!(last_canonical_positions(&entries).get("m1"), Some(&1));
    }
}
