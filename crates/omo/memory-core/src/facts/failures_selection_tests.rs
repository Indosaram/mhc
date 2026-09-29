use pretty_assertions::assert_eq;

use super::*;
use crate::facts::failures_schema::{FactsFailureReason, FactsFailureRecord, FactsFailureState};
use crate::facts::schema::{FACTS_QUEUE_VERSION, FactsQueueEntry, FactsQueueRange};
use crate::support::time::format_rfc3339_millis;

const T0: &str = "2026-08-16T00:00:00.000Z";
const T0_MILLIS: i64 = 1786838400000;

fn test_entry(
    conversation_id: &str,
    end_message_id: &str,
    end_snapshot_line: u64,
) -> FactsQueueEntry {
    FactsQueueEntry {
        version: FACTS_QUEUE_VERSION,
        identity: "agent".to_string(),
        session_id: conversation_id.to_string(),
        conversation_id: conversation_id.to_string(),
        range: FactsQueueRange {
            start_message_id: format!("{end_message_id}-start"),
            end_message_id: end_message_id.to_string(),
            start_line: end_snapshot_line.saturating_sub(1),
            end_snapshot_line,
        },
        enqueued_at: T0.to_string(),
        entries: Vec::new(),
    }
}

fn parked_record(
    conversation_id: &str,
    end_message_id: &str,
    end_snapshot_line: u64,
) -> FactsFailureRecord {
    FactsFailureRecord {
        conversation_id: conversation_id.to_string(),
        end_message_id: end_message_id.to_string(),
        end_snapshot_line,
        state: FactsFailureState::Parked,
        streak: 5,
        first_failure_at: T0.to_string(),
        last_failure_at: T0.to_string(),
        last_reason: FactsFailureReason::ChildExit,
        last_detail: None,
        last_failure_id: "run-parked".to_string(),
        next_eligible_at: None,
        parked_at: Some(T0.to_string()),
    }
}

fn backoff_record(
    conversation_id: &str,
    end_message_id: &str,
    end_snapshot_line: u64,
    next_eligible_at: &str,
) -> FactsFailureRecord {
    FactsFailureRecord {
        conversation_id: conversation_id.to_string(),
        end_message_id: end_message_id.to_string(),
        end_snapshot_line,
        state: FactsFailureState::Backoff,
        streak: 1,
        first_failure_at: T0.to_string(),
        last_failure_at: T0.to_string(),
        last_reason: FactsFailureReason::ChildExit,
        last_detail: None,
        last_failure_id: "run-backoff".to_string(),
        next_eligible_at: Some(next_eligible_at.to_string()),
        parked_at: None,
    }
}

fn wrap_failures(records: Vec<FactsFailureRecord>) -> FactsFailuresFile {
    FactsFailuresFile {
        version: 1,
        updated_at: T0.to_string(),
        entries: records,
    }
}

fn endpoints(selection: &FactsLaunchSelection) -> Vec<String> {
    selection
        .selected
        .iter()
        .map(|s| format!("{}/{}", s.conversation_id, s.range.end_message_id))
        .collect()
}

#[test]
fn test_no_failures_all_launchable() {
    // given
    let entries = vec![test_entry("alpha", "m1", 1), test_entry("beta", "m9", 3)];

    // when
    let selection = select_launchable(&entries, None, T0);

    // then
    assert_eq!(endpoints(&selection), vec!["alpha/m1", "beta/m9"]);
    assert_eq!(selection.skipped.is_empty(), true);
}

#[test]
fn test_parked_endpoint_blocks_later_same_conversation_entries() {
    // given
    let entries = vec![
        test_entry("alpha", "m1", 1),
        test_entry("alpha", "m2", 4),
        test_entry("beta", "m9", 2),
    ];
    let failures = wrap_failures(vec![parked_record("alpha", "m1", 1)]);

    // when
    let selection = select_launchable(&entries, Some(&failures), T0);

    // then
    assert_eq!(endpoints(&selection), vec!["beta/m9"]);
    let mut expected_skipped = BTreeMap::new();
    expected_skipped.insert(facts_selection_key("alpha", "m1"), FactsSkipReason::Parked);
    expected_skipped.insert(
        facts_selection_key("alpha", "m2"),
        FactsSkipReason::BlockedByPredecessor,
    );
    assert_eq!(selection.skipped, expected_skipped);
}

#[test]
fn test_backoff_dropped_one_millisecond_early() {
    // given
    let eligible_millis = T0_MILLIS + 60_000;
    let eligible_at = format_rfc3339_millis(eligible_millis);
    let entries = vec![test_entry("alpha", "m1", 1)];
    let failures = wrap_failures(vec![backoff_record("alpha", "m1", 1, &eligible_at)]);
    let query_time = format_rfc3339_millis(eligible_millis - 1);

    // when
    let selection = select_launchable(&entries, Some(&failures), &query_time);

    // then
    assert_eq!(selection.selected.is_empty(), true);
    let mut expected_skipped = BTreeMap::new();
    expected_skipped.insert(facts_selection_key("alpha", "m1"), FactsSkipReason::Backoff);
    assert_eq!(selection.skipped, expected_skipped);
}

#[test]
fn test_backoff_eligible_at_exact_instant() {
    // given
    let eligible_millis = T0_MILLIS + 60_000;
    let eligible_at = format_rfc3339_millis(eligible_millis);
    let entries = vec![test_entry("alpha", "m1", 1)];
    let failures = wrap_failures(vec![backoff_record("alpha", "m1", 1, &eligible_at)]);

    // when
    let selection = select_launchable(&entries, Some(&failures), &eligible_at);

    // then
    assert_eq!(endpoints(&selection), vec!["alpha/m1"]);
    assert_eq!(selection.skipped.is_empty(), true);
}

#[test]
fn test_legacy_record_anchored_at_zero_still_gates() {
    // given
    let entries = vec![test_entry("alpha", "m1", 7), test_entry("alpha", "m2", 9)];
    let failures = wrap_failures(vec![parked_record("alpha", "m1", 0)]);

    // when
    let selection = select_launchable(&entries, Some(&failures), T0);

    // then
    assert_eq!(selection.selected.is_empty(), true);
    let mut expected_skipped = BTreeMap::new();
    expected_skipped.insert(facts_selection_key("alpha", "m1"), FactsSkipReason::Parked);
    expected_skipped.insert(
        facts_selection_key("alpha", "m2"),
        FactsSkipReason::BlockedByPredecessor,
    );
    assert_eq!(selection.skipped, expected_skipped);
}

#[test]
fn test_unrelated_failure_record_allows_other_entries_to_launch() {
    // given
    let entries = vec![test_entry("alpha", "m1", 1)];
    let failures = wrap_failures(vec![parked_record("beta", "m9", 2)]);

    // when
    let selection = select_launchable(&entries, Some(&failures), T0);

    // then
    assert_eq!(endpoints(&selection), vec!["alpha/m1"]);
    assert_eq!(selection.skipped.is_empty(), true);
}

#[test]
fn test_out_of_order_pending_list_follows_snapshot_boundary() {
    // given
    let entries = vec![test_entry("alpha", "m2", 6), test_entry("alpha", "m1", 2)];
    let failures = wrap_failures(vec![parked_record("alpha", "m1", 2)]);

    // when
    let selection = select_launchable(&entries, Some(&failures), T0);

    // then
    assert_eq!(selection.selected.is_empty(), true);
    let mut expected_skipped = BTreeMap::new();
    expected_skipped.insert(facts_selection_key("alpha", "m1"), FactsSkipReason::Parked);
    expected_skipped.insert(
        facts_selection_key("alpha", "m2"),
        FactsSkipReason::BlockedByPredecessor,
    );
    assert_eq!(selection.skipped, expected_skipped);
}

#[test]
fn test_launchable_predecessor_with_later_parked_entry() {
    // given
    let entries = vec![test_entry("alpha", "m1", 1), test_entry("alpha", "m2", 4)];
    let failures = wrap_failures(vec![parked_record("alpha", "m2", 4)]);

    // when
    let selection = select_launchable(&entries, Some(&failures), T0);

    // then
    assert_eq!(endpoints(&selection), vec!["alpha/m1"]);
    let mut expected_skipped = BTreeMap::new();
    expected_skipped.insert(facts_selection_key("alpha", "m2"), FactsSkipReason::Parked);
    assert_eq!(selection.skipped, expected_skipped);
}
