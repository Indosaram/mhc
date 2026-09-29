use pretty_assertions::assert_eq;

use super::*;

const CONVERSATION: &str = "conversation-alpha";
const T0_MILLIS: i64 = 1786838400000;
const T0: &str = "2026-08-16T00:00:00.000Z";

fn test_target(
    conversation_id: &str,
    end_message_id: &str,
    end_snapshot_line: u64,
) -> FactsFailureTarget {
    FactsFailureTarget {
        conversation_id: conversation_id.to_string(),
        end_message_id: end_message_id.to_string(),
        end_snapshot_line,
    }
}

fn apply_streak(count: usize, reason: FactsFailureReason) -> Vec<FactsFailureRecord> {
    let mut entries = Vec::new();
    for index in 0..count {
        let now_millis = T0_MILLIS + (index as i64) * 60_000;
        let now = format_rfc3339_millis(now_millis);
        entries = apply_failure(&ApplyFailureInput {
            entries,
            targets: vec![test_target(CONVERSATION, "m2", 4)],
            failure_id: format!("run-{index}"),
            reason,
            detail: None,
            now,
        });
    }
    entries
}

#[test]
fn test_first_failure_eligible_one_minute_later() {
    // given / when
    let entries = apply_streak(1, FactsFailureReason::ChildExit);

    // then
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].state, FactsFailureState::Backoff);
    assert_eq!(entries[0].streak, 1);
    assert_eq!(entries[0].first_failure_at, T0);
    assert_eq!(entries[0].last_failure_at, T0);
    assert_eq!(
        entries[0].next_eligible_at,
        Some("2026-08-16T00:01:00.000Z".to_string())
    );
    assert_eq!(entries[0].parked_at, None);
}

#[test]
fn test_consecutive_failures_backoff_ladder() {
    // given
    let offsets = [60_000, 5 * 60_000, 30 * 60_000, 120 * 60_000];

    // when
    let observed: Vec<FactsFailureRecord> = offsets
        .iter()
        .enumerate()
        .map(|(index, _)| apply_streak(index + 1, FactsFailureReason::ChildExit).remove(0))
        .collect();

    // then
    for (index, record) in observed.iter().enumerate() {
        let failed_at = T0_MILLIS + (index as i64) * 60_000;
        assert_eq!(record.streak, (index + 1) as u64);
        assert_eq!(record.state, FactsFailureState::Backoff);
        assert_eq!(
            record.next_eligible_at,
            Some(format_rfc3339_millis(failed_at + offsets[index]))
        );
    }
}

#[test]
fn test_fifth_failure_parks_without_eligibility() {
    // given / when
    let entries = apply_streak(5, FactsFailureReason::ChildExit);

    // then
    let record = &entries[0];
    assert_eq!(record.streak, 5);
    assert_eq!(record.state, FactsFailureState::Parked);
    assert_eq!(record.next_eligible_at, None);
    assert_eq!(
        record.parked_at,
        Some(format_rfc3339_millis(T0_MILLIS + 4 * 60_000))
    );
    assert_eq!(record.first_failure_at, T0);
}

#[test]
fn test_parked_entry_stays_parked_with_growing_streak() {
    // given
    let parked = apply_streak(5, FactsFailureReason::ChildExit);

    // when
    let entries = apply_failure(&ApplyFailureInput {
        entries: parked,
        targets: vec![test_target(CONVERSATION, "m2", 4)],
        failure_id: "run-later".to_string(),
        reason: FactsFailureReason::ChildExit,
        detail: None,
        now: format_rfc3339_millis(T0_MILLIS + 10 * 60_000),
    });

    // then
    assert_eq!(entries[0].state, FactsFailureState::Parked);
    assert_eq!(entries[0].streak, 6);
    assert_eq!(entries[0].next_eligible_at, None);
    assert_eq!(
        entries[0].parked_at,
        Some(format_rfc3339_millis(T0_MILLIS + 4 * 60_000))
    );
}

#[test]
fn test_already_parked_record_retains_original_parked_at() {
    // given
    let parked = apply_streak(1, FactsFailureReason::PayloadEntryOversize);

    // when
    let entries = apply_failure(&ApplyFailureInput {
        entries: parked,
        targets: vec![test_target(CONVERSATION, "m2", 4)],
        failure_id: "run-later".to_string(),
        reason: FactsFailureReason::ChildExit,
        detail: None,
        now: format_rfc3339_millis(T0_MILLIS + 60 * 60_000),
    });

    // then
    assert_eq!(entries[0].state, FactsFailureState::Parked);
    assert_eq!(entries[0].next_eligible_at, None);
    assert_eq!(entries[0].parked_at, Some(T0.to_string()));
    assert_eq!(entries[0].streak, 2);
    assert_eq!(entries[0].last_reason, FactsFailureReason::ChildExit);
}

#[test]
fn test_changed_reason_does_not_reset_streak() {
    // given
    let first = apply_failure(&ApplyFailureInput {
        entries: Vec::new(),
        targets: vec![test_target(CONVERSATION, "m2", 4)],
        failure_id: "run-a".to_string(),
        reason: FactsFailureReason::ChildExit,
        detail: None,
        now: T0.to_string(),
    });

    // when
    let second = apply_failure(&ApplyFailureInput {
        entries: first,
        targets: vec![test_target(CONVERSATION, "m2", 4)],
        failure_id: "run-b".to_string(),
        reason: FactsFailureReason::DeadlineExceeded,
        detail: None,
        now: format_rfc3339_millis(T0_MILLIS + 60_000),
    });

    // then
    assert_eq!(second[0].streak, 2);
    assert_eq!(second[0].last_reason, FactsFailureReason::DeadlineExceeded);
    assert_eq!(
        second[0].next_eligible_at,
        Some(format_rfc3339_millis(T0_MILLIS + 60_000 + 5 * 60_000))
    );
}

#[test]
fn test_oversized_single_entry_parks_immediately() {
    // given / when
    let entries = apply_streak(1, FactsFailureReason::PayloadEntryOversize);

    // then
    assert_eq!(entries[0].streak, 1);
    assert_eq!(entries[0].state, FactsFailureState::Parked);
    assert_eq!(entries[0].next_eligible_at, None);
    assert_eq!(entries[0].parked_at, Some(T0.to_string()));
}

#[test]
fn test_replayed_failure_id_leaves_record_unchanged() {
    // given
    let first = apply_failure(&ApplyFailureInput {
        entries: Vec::new(),
        targets: vec![test_target(CONVERSATION, "m2", 4)],
        failure_id: "run-a".to_string(),
        reason: FactsFailureReason::ChildExit,
        detail: None,
        now: T0.to_string(),
    });

    // when
    let replayed = apply_failure(&ApplyFailureInput {
        entries: first.clone(),
        targets: vec![test_target(CONVERSATION, "m2", 4)],
        failure_id: "run-a".to_string(),
        reason: FactsFailureReason::ChildExit,
        detail: None,
        now: format_rfc3339_millis(T0_MILLIS + 60_000),
    });

    // then
    assert_eq!(replayed, first);
}

#[test]
fn test_detail_truncated_to_512_bytes() {
    // given
    let detail = "가".repeat(400);

    // when
    let entries = apply_failure(&ApplyFailureInput {
        entries: Vec::new(),
        targets: vec![test_target(CONVERSATION, "m2", 4)],
        failure_id: "run-a".to_string(),
        reason: FactsFailureReason::Other,
        detail: Some(detail),
        now: T0.to_string(),
    });

    // then
    let stored = entries[0].last_detail.as_deref().unwrap_or("");
    assert_eq!(stored.len() <= 512, true);
    assert_eq!(stored, "가".repeat(170));
}

#[test]
fn test_control_characters_sanitized_to_spaces() {
    // given / when
    let entries = apply_failure(&ApplyFailureInput {
        entries: Vec::new(),
        targets: vec![test_target(CONVERSATION, "m2", 4)],
        failure_id: "run-a".to_string(),
        reason: FactsFailureReason::Other,
        detail: Some("child\nexit\0code 9".to_string()),
        now: T0.to_string(),
    });

    // then
    assert_eq!(entries[0].last_detail.as_deref(), Some("child exit code 9"));
}

#[test]
fn test_clear_on_success_removes_matching_record() {
    // given
    let entries = apply_failure(&ApplyFailureInput {
        entries: apply_streak(2, FactsFailureReason::ChildExit),
        targets: vec![test_target("conversation-beta", "m9", 12)],
        failure_id: "run-beta".to_string(),
        reason: FactsFailureReason::ChildExit,
        detail: None,
        now: T0.to_string(),
    });

    // when
    let cleared = clear_on_success(&entries, &[test_target(CONVERSATION, "m2", 4)]);

    // then
    assert_eq!(cleared.len(), 1);
    assert_eq!(cleared[0].conversation_id, "conversation-beta");
}

#[test]
fn test_clear_for_retry_filters_one_conversation() {
    // given
    let entries = apply_failure(&ApplyFailureInput {
        entries: apply_streak(5, FactsFailureReason::ChildExit),
        targets: vec![test_target("conversation-beta", "m9", 12)],
        failure_id: "run-beta".to_string(),
        reason: FactsFailureReason::ChildExit,
        detail: None,
        now: T0.to_string(),
    });

    // when
    let cleared = clear_for_retry(
        &entries,
        &FactsFailureFilter {
            conversation_id: Some(CONVERSATION.to_string()),
            end_message_id: None,
        },
    );

    // then
    assert_eq!(cleared.len(), 1);
    assert_eq!(cleared[0].conversation_id, "conversation-beta");
}

#[test]
fn test_clear_for_retry_without_filter_clears_all() {
    // given
    let entries = apply_failure(&ApplyFailureInput {
        entries: apply_streak(3, FactsFailureReason::ChildExit),
        targets: vec![test_target("conversation-beta", "m9", 12)],
        failure_id: "run-beta".to_string(),
        reason: FactsFailureReason::ChildExit,
        detail: None,
        now: T0.to_string(),
    });

    // when / then
    assert_eq!(
        clear_for_retry(&entries, &FactsFailureFilter::default()).len(),
        0
    );
}
