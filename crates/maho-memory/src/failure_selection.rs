//! Which queued facts batches may launch now, given the failure ledger.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-failure-selection.md`. Two different kinds of "not
//! yet" exist and they propagate differently: a range in backoff will be retried soon, so the ranges
//! after it in the same conversation must wait for it or the extractor would read the conversation out
//! of order; a parked range is abandoned, so holding its successors hostage would strand the rest of
//! the conversation forever.
use std::collections::{BTreeMap, HashMap};

use crate::facts_queue::FactsBatch;
use crate::failure_backoff::{FailureRecord, FailureState};
use crate::time_format::parse_millis;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SkipReason {
    Parked,
    Backoff,
    BlockedByPredecessor,
}

impl SkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Parked => "parked",
            Self::Backoff => "backoff",
            Self::BlockedByPredecessor => "blocked-by-predecessor",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LaunchSelection {
    pub selected: Vec<FactsBatch>,
    pub skipped: BTreeMap<String, SkipReason>,
}

pub fn selection_key(conversation_id: &str, end_message_id: &str) -> String {
    format!("{conversation_id}\u{0}{end_message_id}")
}

pub fn select_launchable(
    entries: &[FactsBatch],
    failures: &[FailureRecord],
    now: &str,
) -> LaunchSelection {
    if failures.is_empty() {
        return LaunchSelection {
            selected: entries.to_vec(),
            skipped: BTreeMap::new(),
        };
    }

    let now_millis = parse_millis(now).unwrap_or(0);
    let records: HashMap<String, &FailureRecord> = failures
        .iter()
        .map(|record| {
            (
                selection_key(&record.conversation_id, &record.end_message_id),
                record,
            )
        })
        .collect();

    let mut by_conversation: HashMap<String, Vec<&FactsBatch>> = HashMap::new();
    for entry in entries {
        by_conversation
            .entry(entry.conversation_id.clone())
            .or_default()
            .push(entry);
    }

    let mut skipped = BTreeMap::new();
    for mut bucket in by_conversation.into_values() {
        bucket.sort_by_key(|entry| entry.range.end_snapshot_line);
        let mut blocked = false;
        for entry in bucket {
            let key = selection_key(&entry.conversation_id, &entry.range.end_message_id);
            let reason = if blocked {
                Some(SkipReason::BlockedByPredecessor)
            } else {
                drop_reason(records.get(&key).copied(), now_millis)
            };
            if let Some(reason) = reason {
                skipped.insert(key, reason);
                if reason != SkipReason::Parked {
                    blocked = true;
                }
            }
        }
    }

    let selected = entries
        .iter()
        .filter(|entry| {
            !skipped.contains_key(&selection_key(
                &entry.conversation_id,
                &entry.range.end_message_id,
            ))
        })
        .cloned()
        .collect();

    LaunchSelection { selected, skipped }
}

/// Why a record excludes its range, if it does.
///
/// A record with no `next_eligible_at` but a non-parked state is treated as still backing off: the
/// absent timestamp means "not known to be eligible", and guessing eligibility would launch a range the
/// ledger deliberately left undated.
fn drop_reason(record: Option<&FailureRecord>, now_millis: i64) -> Option<SkipReason> {
    let record = record?;
    if record.state == FailureState::Parked {
        return Some(SkipReason::Parked);
    }
    let Some(next_eligible) = &record.next_eligible_at else {
        return Some(SkipReason::Backoff);
    };
    // An unparseable retry time is treated like an absent one rather than as "eligible now": a
    // corrupt timestamp must not turn a deliberate backoff into an immediate relaunch, which is
    // exactly what a zero default would do.
    let Some(next_millis) = parse_millis(next_eligible) else {
        return Some(SkipReason::Backoff);
    };
    (now_millis < next_millis).then_some(SkipReason::Backoff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts_queue::{BatchEntry, BatchRange};

    fn batch(conversation: &str, message: &str, line: usize) -> FactsBatch {
        FactsBatch {
            version: 1,
            identity: "maho".to_owned(),
            session_id: "s1".to_owned(),
            conversation_id: conversation.to_owned(),
            range: BatchRange {
                start_message_id: format!("{message}-start"),
                end_message_id: message.to_owned(),
                start_line: line.saturating_sub(1),
                end_snapshot_line: line,
            },
            enqueued_at: "2026-11-09T00:00:00.000Z".to_owned(),
            entries: vec![BatchEntry {
                kind: "user".to_owned(),
                text: Some("hello".to_owned()),
                name: None,
                args_text: None,
                result_text: None,
                result_ok: None,
                captured_at: None,
                source_line_id: None,
                source_message_id: None,
            }],
        }
    }

    fn record(
        conversation: &str,
        message: &str,
        state: FailureState,
        next_eligible_at: Option<&str>,
    ) -> FailureRecord {
        FailureRecord {
            conversation_id: conversation.to_owned(),
            end_message_id: message.to_owned(),
            end_snapshot_line: 10,
            state,
            streak: 1,
            first_failure_at: "2026-11-09T00:00:00.000Z".to_owned(),
            last_failure_at: "2026-11-09T00:00:00.000Z".to_owned(),
            last_reason: crate::failure_backoff::FailureReason::ChildExit,
            last_detail: None,
            last_failure_id: "f1".to_owned(),
            next_eligible_at: next_eligible_at.map(str::to_owned),
            parked_at: (state == FailureState::Parked)
                .then(|| "2026-11-09T00:00:00.000Z".to_owned()),
        }
    }

    const NOW: &str = "2026-11-10T00:00:00.000Z";

    #[test]
    fn without_failures_everything_launches() {
        let entries = vec![batch("c1", "m1", 10), batch("c2", "m1", 10)];
        let selection = select_launchable(&entries, &[], NOW);
        assert_eq!(selection.selected.len(), 2);
        assert!(selection.skipped.is_empty());
    }

    #[test]
    fn a_range_under_backoff_is_skipped() {
        let entries = vec![batch("c1", "m1", 10)];
        let failures = vec![record(
            "c1",
            "m1",
            FailureState::Backoff,
            Some("2026-11-10T01:00:00.000Z"),
        )];
        let selection = select_launchable(&entries, &failures, NOW);
        assert!(selection.selected.is_empty());
        assert_eq!(
            selection.skipped.get(&selection_key("c1", "m1")),
            Some(&SkipReason::Backoff)
        );
    }

    #[test]
    fn a_range_whose_backoff_expired_launches() {
        let entries = vec![batch("c1", "m1", 10)];
        let failures = vec![record(
            "c1",
            "m1",
            FailureState::Backoff,
            Some("2026-11-09T23:59:00.000Z"),
        )];
        let selection = select_launchable(&entries, &failures, NOW);
        assert_eq!(selection.selected.len(), 1);
    }

    #[test]
    fn a_parked_range_is_skipped_forever() {
        let entries = vec![batch("c1", "m1", 10)];
        let failures = vec![record("c1", "m1", FailureState::Parked, None)];
        let selection = select_launchable(&entries, &failures, "2030-01-01T00:00:00.000Z");
        assert!(selection.selected.is_empty());
        assert_eq!(
            selection.skipped.get(&selection_key("c1", "m1")),
            Some(&SkipReason::Parked)
        );
    }

    #[test]
    fn a_range_in_backoff_blocks_its_successors() {
        let entries = vec![
            batch("c1", "m1", 10),
            batch("c1", "m2", 20),
            batch("c1", "m3", 30),
        ];
        let failures = vec![record(
            "c1",
            "m1",
            FailureState::Backoff,
            Some("2026-11-10T05:00:00.000Z"),
        )];
        let selection = select_launchable(&entries, &failures, NOW);
        assert!(selection.selected.is_empty());
        assert_eq!(
            selection.skipped.get(&selection_key("c1", "m2")),
            Some(&SkipReason::BlockedByPredecessor)
        );
        assert_eq!(
            selection.skipped.get(&selection_key("c1", "m3")),
            Some(&SkipReason::BlockedByPredecessor)
        );
    }

    #[test]
    fn a_parked_range_does_not_block_its_successors() {
        let entries = vec![batch("c1", "m1", 10), batch("c1", "m2", 20)];
        let failures = vec![record("c1", "m1", FailureState::Parked, None)];
        let selection = select_launchable(&entries, &failures, NOW);
        assert_eq!(selection.selected.len(), 1);
        assert_eq!(selection.selected[0].range.end_message_id, "m2");
        assert_eq!(
            selection.skipped.get(&selection_key("c1", "m1")),
            Some(&SkipReason::Parked)
        );
    }

    #[test]
    fn a_blocked_range_keeps_blocking_even_after_a_parked_one() {
        let entries = vec![
            batch("c1", "m1", 10),
            batch("c1", "m2", 20),
            batch("c1", "m3", 30),
        ];
        let failures = vec![
            record("c1", "m1", FailureState::Parked, None),
            record(
                "c1",
                "m2",
                FailureState::Backoff,
                Some("2026-11-10T05:00:00.000Z"),
            ),
        ];
        let selection = select_launchable(&entries, &failures, NOW);
        assert!(selection.selected.is_empty());
        assert_eq!(
            selection.skipped.get(&selection_key("c1", "m3")),
            Some(&SkipReason::BlockedByPredecessor)
        );
    }

    #[test]
    fn a_failure_in_one_conversation_does_not_block_another() {
        let entries = vec![batch("c1", "m1", 10), batch("c2", "m1", 10)];
        let failures = vec![record(
            "c1",
            "m1",
            FailureState::Backoff,
            Some("2026-11-10T05:00:00.000Z"),
        )];
        let selection = select_launchable(&entries, &failures, NOW);
        assert_eq!(selection.selected.len(), 1);
        assert_eq!(selection.selected[0].conversation_id, "c2");
    }

    #[test]
    fn a_backoff_record_without_a_retry_time_still_blocks() {
        let entries = vec![batch("c1", "m1", 10)];
        let failures = vec![record("c1", "m1", FailureState::Backoff, None)];
        let selection = select_launchable(&entries, &failures, NOW);
        assert!(selection.selected.is_empty());
        assert_eq!(
            selection.skipped.get(&selection_key("c1", "m1")),
            Some(&SkipReason::Backoff)
        );
    }

    #[test]
    fn a_backoff_time_that_does_not_parse_blocks() {
        let entries = vec![batch("c1", "m1", 10)];
        let failures = vec![record("c1", "m1", FailureState::Backoff, Some("garbage"))];
        let selection = select_launchable(&entries, &failures, NOW);
        assert!(selection.selected.is_empty());
    }

    #[test]
    fn selection_preserves_input_order() {
        let entries = vec![
            batch("c2", "m1", 10),
            batch("c1", "m1", 10),
            batch("c3", "m1", 10),
        ];
        let selection = select_launchable(&entries, &[], NOW);
        let order: Vec<&str> = selection
            .selected
            .iter()
            .map(|entry| entry.conversation_id.as_str())
            .collect();
        assert_eq!(order, vec!["c2", "c1", "c3"]);
    }

    #[test]
    fn a_skip_key_is_reported_once_per_range() {
        let entries = vec![batch("c1", "m1", 10), batch("c1", "m2", 20)];
        let failures = vec![record(
            "c1",
            "m1",
            FailureState::Backoff,
            Some("2026-11-10T05:00:00.000Z"),
        )];
        let selection = select_launchable(&entries, &failures, NOW);
        assert_eq!(selection.skipped.len(), 2);
        assert_eq!(
            selection.skipped.values().next(),
            Some(&SkipReason::Backoff)
        );
    }

    #[test]
    fn the_selection_key_matches_the_ledger_key() {
        assert_eq!(selection_key("c1", "m1"), selection_key("c1", "m1"));
        assert_ne!(selection_key("c1", "m1"), selection_key("c1", "m2"));
        assert_ne!(selection_key("c1", "m1"), selection_key("c11", "m1"));
    }

    #[test]
    fn every_skip_reason_has_a_distinct_wire_name() {
        let names = [
            SkipReason::Parked.as_str(),
            SkipReason::Backoff.as_str(),
            SkipReason::BlockedByPredecessor.as_str(),
        ];
        let mut sorted = names.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len());
    }
}
