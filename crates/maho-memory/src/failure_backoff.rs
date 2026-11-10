//! Retry backoff for failed facts extractions.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-failure-backoff.md`. A failing extraction must not be
//! retried at full speed: the retry itself can be the thing that keeps the failure alive (a provider
//! quota, a sandbox that is still booting). Each occurrence stretches the wait, and some reasons stop
//! the retry permanently because waiting cannot help.
use std::collections::HashSet;

/// Wait after the first, second, third, and fourth consecutive failure.
///
/// The schedule stops growing at the fourth step: an entry that reaches the fifth failure is parked
/// rather than waiting two hours and trying again.
pub const BACKOFF_MS: [i64; 4] = [60_000, 5 * 60_000, 30 * 60_000, 120 * 60_000];

/// The streak at which an entry is parked instead of retried.
pub const PARK_AT_STREAK: u32 = 5;

/// The cap on a stored failure detail, in bytes.
pub const DETAIL_MAX_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureReason {
    QuickCategoryUnavailable,
    SandboxUnavailable,
    ChildExit,
    DeadlineExceeded,
    InvalidExtraction,
    MemoryWriteLockExhausted,
    ParentDirty,
    UnknownLiveness,
    PayloadEnvelopeOversize,
    PayloadEntryOversize,
    SecretLikeContent,
    Other,
}

impl FailureReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QuickCategoryUnavailable => "quick_category_unavailable",
            Self::SandboxUnavailable => "sandbox_unavailable",
            Self::ChildExit => "child_exit",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::InvalidExtraction => "invalid_extraction",
            Self::MemoryWriteLockExhausted => "memory_write_lock_exhausted",
            Self::ParentDirty => "parent_dirty",
            Self::UnknownLiveness => "unknown_liveness",
            Self::PayloadEnvelopeOversize => "payload_envelope_oversize",
            Self::PayloadEntryOversize => "payload_entry_oversize",
            Self::SecretLikeContent => "secret_like_content",
            Self::Other => "other",
        }
    }

    /// Reasons that park immediately: retrying cannot change the outcome.
    ///
    /// An oversized entry stays oversized, and content that looks like a secret stays rejected no
    /// matter how often it is resubmitted. Backing off would only delay the conclusion and burn
    /// provider calls.
    pub fn parks_immediately(self) -> bool {
        matches!(self, Self::PayloadEntryOversize | Self::SecretLikeContent)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureState {
    Backoff,
    Parked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureRecord {
    pub conversation_id: String,
    pub end_message_id: String,
    pub end_snapshot_line: usize,
    pub state: FailureState,
    pub streak: u32,
    pub first_failure_at: String,
    pub last_failure_at: String,
    pub last_reason: FailureReason,
    pub last_detail: Option<String>,
    pub last_failure_id: String,
    pub next_eligible_at: Option<String>,
    pub parked_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureTarget {
    pub conversation_id: String,
    pub end_message_id: String,
    pub end_snapshot_line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyFailureInput {
    pub entries: Vec<FailureRecord>,
    pub targets: Vec<FailureTarget>,
    pub failure_id: String,
    pub reason: FailureReason,
    pub detail: Option<String>,
    pub now: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FailureFilter {
    pub conversation_id: Option<String>,
    pub end_message_id: Option<String>,
}

/// /// An entry already carrying `failure_id` is left untouched: applying the same failed attempt twice
/// must not advance the streak, or a retried write would park a healthy range early.
pub fn apply_failure(input: &ApplyFailureInput) -> Vec<FailureRecord> {
    let detail = input.detail.as_deref().and_then(sanitize_detail);
    let mut ledger: Vec<FailureRecord> = input.entries.clone();

    for target in &input.targets {
        let key = target_key(target);
        match ledger.iter_mut().find(|record| record_key(record) == key) {
            Some(record) if record.last_failure_id == input.failure_id => continue,
            Some(record) => {
                let next = next_record(Some(record), target, input, detail.as_deref());
                *record = next;
            }
            None => {
                let next = next_record(None, target, input, detail.as_deref());
                ledger.push(next);
            }
        }
    }

    sort_records(ledger)
}

pub fn clear_on_success(
    entries: &[FailureRecord],
    targets: &[FailureTarget],
) -> Vec<FailureRecord> {
    let cleared: HashSet<(&str, &str)> = targets
        .iter()
        .map(|target| {
            (
                target.conversation_id.as_str(),
                target.end_message_id.as_str(),
            )
        })
        .collect();
    entries
        .iter()
        .filter(|record| {
            !cleared.contains(&(
                record.conversation_id.as_str(),
                record.end_message_id.as_str(),
            ))
        })
        .cloned()
        .collect()
}

/// Drop the records matching both filters that were supplied.
///
/// An unset filter matches everything, so `clear_for_retry` with no filter clears the whole ledger —
/// that is the deliberate "forget all failures" path, not an accident.
pub fn clear_for_retry(entries: &[FailureRecord], filter: &FailureFilter) -> Vec<FailureRecord> {
    entries
        .iter()
        .filter(|record| {
            if let Some(conversation) = &filter.conversation_id
                && &record.conversation_id != conversation
            {
                return true;
            }
            if let Some(message) = &filter.end_message_id
                && &record.end_message_id != message
            {
                return true;
            }
            false
        })
        .cloned()
        .collect()
}

pub fn sort_records(mut records: Vec<FailureRecord>) -> Vec<FailureRecord> {
    records.sort_by(|left, right| {
        left.conversation_id
            .cmp(&right.conversation_id)
            .then_with(|| left.end_snapshot_line.cmp(&right.end_snapshot_line))
            .then_with(|| left.end_message_id.cmp(&right.end_message_id))
    });
    records
}

pub fn delay_for(streak: u32) -> i64 {
    let index = (streak.clamp(1, BACKOFF_MS.len() as u32) - 1) as usize;
    BACKOFF_MS[index]
}

/// Flatten control characters to single spaces, trim, and cap at [`DETAIL_MAX_BYTES`].
///
/// Runs of control characters collapse to one space so a multi-line provider error does not become a
/// wall of blanks, and the truncation respects character boundaries so a stored detail is always valid
/// UTF-8.
pub fn sanitize_detail(detail: &str) -> Option<String> {
    let mut flattened = String::with_capacity(detail.len());
    let mut in_control = false;
    for ch in detail.chars() {
        if ch.is_control() {
            if !in_control {
                flattened.push(' ');
                in_control = true;
            }
        } else {
            in_control = false;
            flattened.push(ch);
        }
    }
    let trimmed = flattened.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(truncate_to_bytes(trimmed, DETAIL_MAX_BYTES))
}

fn truncate_to_bytes(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = 0;
    let mut bytes = 0;
    for ch in value.chars() {
        let length = ch.len_utf8();
        if bytes + length > max_bytes {
            break;
        }
        bytes += length;
        end += length;
    }
    value[..end].to_owned()
}

fn key(conversation_id: &str, end_message_id: &str) -> String {
    format!("{conversation_id}\u{0}{end_message_id}")
}

fn record_key(record: &FailureRecord) -> String {
    key(&record.conversation_id, &record.end_message_id)
}

fn target_key(target: &FailureTarget) -> String {
    key(&target.conversation_id, &target.end_message_id)
}

fn next_record(
    previous: Option<&FailureRecord>,
    target: &FailureTarget,
    input: &ApplyFailureInput,
    detail: Option<&str>,
) -> FailureRecord {
    let streak = previous.map_or(1, |record| record.streak.saturating_add(1));
    // Parking is sticky: an entry that was already parked is never moved back into backoff by a new
    // failure. Only an explicit clear (a retry request or a success) leaves the parked state, so a
    // failure that arrives after that is a fresh streak.
    let was_parked = previous.is_some_and(|record| record.state == FailureState::Parked);
    let parks = was_parked || input.reason.parks_immediately() || streak >= PARK_AT_STREAK;

    let now_millis = parse_millis(&input.now).unwrap_or(0);
    let first_failure_at = previous.map_or_else(
        || input.now.clone(),
        |record| record.first_failure_at.clone(),
    );
    let next_eligible_at = if parks {
        None
    } else {
        Some(format_millis(now_millis + delay_for(streak)))
    };
    let parked_at = if parks {
        Some(
            previous
                .and_then(|record| record.parked_at.clone())
                .unwrap_or_else(|| input.now.clone()),
        )
    } else {
        None
    };

    FailureRecord {
        conversation_id: target.conversation_id.clone(),
        end_message_id: target.end_message_id.clone(),
        end_snapshot_line: target.end_snapshot_line,
        state: if parks {
            FailureState::Parked
        } else {
            FailureState::Backoff
        },
        streak,
        first_failure_at,
        last_failure_at: input.now.clone(),
        last_reason: input.reason,
        last_detail: detail.map(str::to_owned),
        last_failure_id: input.failure_id.clone(),
        next_eligible_at,
        parked_at,
    }
}

fn parse_millis(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.timestamp_millis())
}

fn format_millis(millis: i64) -> String {
    chrono::DateTime::from_timestamp_millis(millis)
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(conversation: &str, message: &str, line: usize) -> FailureTarget {
        FailureTarget {
            conversation_id: conversation.to_owned(),
            end_message_id: message.to_owned(),
            end_snapshot_line: line,
        }
    }

    fn apply(
        entries: Vec<FailureRecord>,
        reason: FailureReason,
        now: &str,
        failure_id: &str,
    ) -> Vec<FailureRecord> {
        apply_failure(&ApplyFailureInput {
            entries,
            targets: vec![target("c1", "m1", 10)],
            failure_id: failure_id.to_owned(),
            reason,
            detail: None,
            now: now.to_owned(),
        })
    }

    #[test]
    fn the_first_failure_waits_one_minute() {
        let ledger = apply(
            Vec::new(),
            FailureReason::ChildExit,
            "2026-11-10T00:00:00.000Z",
            "f1",
        );
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].state, FailureState::Backoff);
        assert_eq!(ledger[0].streak, 1);
        assert_eq!(
            ledger[0].next_eligible_at.as_deref(),
            Some("2026-11-10T00:01:00.000Z")
        );
    }

    #[test]
    fn each_streak_step_extends_the_wait() {
        let mut ledger = Vec::new();
        let expected = [
            "2026-11-10T00:01:00.000Z",
            "2026-11-10T00:05:00.000Z",
            "2026-11-10T00:30:00.000Z",
            "2026-11-10T02:00:00.000Z",
        ];
        for (index, want) in expected.iter().enumerate() {
            ledger = apply(
                ledger,
                FailureReason::ChildExit,
                "2026-11-10T00:00:00.000Z",
                &format!("f{index}"),
            );
            assert_eq!(
                ledger[0].next_eligible_at.as_deref(),
                Some(*want),
                "streak {}",
                index + 1
            );
        }
    }

    #[test]
    fn the_fifth_failure_parks_instead_of_waiting() {
        let mut ledger = Vec::new();
        for index in 0..4 {
            ledger = apply(
                ledger,
                FailureReason::ChildExit,
                "2026-11-10T00:00:00.000Z",
                &format!("f{index}"),
            );
        }
        assert_eq!(ledger[0].state, FailureState::Backoff);
        ledger = apply(
            ledger,
            FailureReason::ChildExit,
            "2026-11-10T01:00:00.000Z",
            "f4",
        );
        assert_eq!(ledger[0].state, FailureState::Parked);
        assert_eq!(ledger[0].streak, 5);
        assert_eq!(ledger[0].next_eligible_at, None);
        assert_eq!(
            ledger[0].parked_at.as_deref(),
            Some("2026-11-10T01:00:00.000Z")
        );
    }

    #[test]
    fn an_oversized_entry_parks_on_the_first_failure() {
        let ledger = apply(
            Vec::new(),
            FailureReason::PayloadEntryOversize,
            "2026-11-10T00:00:00.000Z",
            "f1",
        );
        assert_eq!(ledger[0].state, FailureState::Parked);
        assert_eq!(ledger[0].streak, 1);
        assert_eq!(ledger[0].next_eligible_at, None);
    }

    #[test]
    fn secret_like_content_parks_on_the_first_failure() {
        let ledger = apply(
            Vec::new(),
            FailureReason::SecretLikeContent,
            "2026-11-10T00:00:00.000Z",
            "f1",
        );
        assert_eq!(ledger[0].state, FailureState::Parked);
    }

    #[test]
    fn a_transient_reason_does_not_park() {
        for reason in [
            FailureReason::SandboxUnavailable,
            FailureReason::DeadlineExceeded,
            FailureReason::ParentDirty,
            FailureReason::InvalidExtraction,
        ] {
            let ledger = apply(Vec::new(), reason, "2026-11-10T00:00:00.000Z", "f1");
            assert_eq!(ledger[0].state, FailureState::Backoff, "{reason:?}");
        }
    }

    #[test]
    fn parking_is_sticky_across_later_failures() {
        let mut ledger = apply(
            Vec::new(),
            FailureReason::PayloadEntryOversize,
            "2026-11-10T00:00:00.000Z",
            "f1",
        );
        let parked_at = ledger[0].parked_at.clone();
        ledger = apply(
            ledger,
            FailureReason::ChildExit,
            "2026-11-10T05:00:00.000Z",
            "f2",
        );
        assert_eq!(ledger[0].state, FailureState::Parked);
        assert_eq!(
            ledger[0].parked_at, parked_at,
            "the original park time stands"
        );
        assert_eq!(ledger[0].streak, 2);
    }

    #[test]
    fn replaying_the_same_failure_id_does_not_advance_the_streak() {
        let ledger = apply(
            Vec::new(),
            FailureReason::ChildExit,
            "2026-11-10T00:00:00.000Z",
            "f1",
        );
        let replayed = apply(
            ledger.clone(),
            FailureReason::ChildExit,
            "2026-11-10T00:00:00.000Z",
            "f1",
        );
        assert_eq!(replayed[0].streak, 1);
        assert_eq!(replayed[0].next_eligible_at, ledger[0].next_eligible_at);
    }

    #[test]
    fn the_first_failure_time_is_preserved_across_later_ones() {
        let ledger = apply(
            Vec::new(),
            FailureReason::ChildExit,
            "2026-11-10T00:00:00.000Z",
            "f1",
        );
        let ledger = apply(
            ledger,
            FailureReason::ChildExit,
            "2026-11-10T01:00:00.000Z",
            "f2",
        );
        assert_eq!(ledger[0].first_failure_at, "2026-11-10T00:00:00.000Z");
        assert_eq!(ledger[0].last_failure_at, "2026-11-10T01:00:00.000Z");
    }

    #[test]
    fn separate_targets_get_separate_records() {
        let ledger = apply_failure(&ApplyFailureInput {
            entries: Vec::new(),
            targets: vec![target("c1", "m1", 10), target("c1", "m2", 20)],
            failure_id: "f1".to_owned(),
            reason: FailureReason::ChildExit,
            detail: None,
            now: "2026-11-10T00:00:00.000Z".to_owned(),
        });
        assert_eq!(ledger.len(), 2);
        assert_eq!(ledger[0].end_message_id, "m1");
        assert_eq!(ledger[1].end_message_id, "m2");
    }

    #[test]
    fn a_success_clears_only_its_own_range() {
        let ledger = apply_failure(&ApplyFailureInput {
            entries: Vec::new(),
            targets: vec![target("c1", "m1", 10), target("c1", "m2", 20)],
            failure_id: "f1".to_owned(),
            reason: FailureReason::ChildExit,
            detail: None,
            now: "2026-11-10T00:00:00.000Z".to_owned(),
        });
        let remaining = clear_on_success(&ledger, &[target("c1", "m1", 10)]);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].end_message_id, "m2");
    }

    #[test]
    fn a_retry_filter_clears_by_conversation() {
        let ledger = apply_failure(&ApplyFailureInput {
            entries: Vec::new(),
            targets: vec![target("c1", "m1", 10), target("c2", "m1", 10)],
            failure_id: "f1".to_owned(),
            reason: FailureReason::ChildExit,
            detail: None,
            now: "2026-11-10T00:00:00.000Z".to_owned(),
        });
        let remaining = clear_for_retry(
            &ledger,
            &FailureFilter {
                conversation_id: Some("c1".to_owned()),
                end_message_id: None,
            },
        );
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].conversation_id, "c2");
    }

    #[test]
    fn a_retry_filter_with_both_fields_requires_both_to_match() {
        let ledger = apply_failure(&ApplyFailureInput {
            entries: Vec::new(),
            targets: vec![target("c1", "m1", 10), target("c1", "m2", 20)],
            failure_id: "f1".to_owned(),
            reason: FailureReason::ChildExit,
            detail: None,
            now: "2026-11-10T00:00:00.000Z".to_owned(),
        });
        let remaining = clear_for_retry(
            &ledger,
            &FailureFilter {
                conversation_id: Some("c1".to_owned()),
                end_message_id: Some("m2".to_owned()),
            },
        );
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].end_message_id, "m1");
    }

    #[test]
    fn an_empty_retry_filter_clears_everything() {
        let ledger = apply(
            Vec::new(),
            FailureReason::ChildExit,
            "2026-11-10T00:00:00.000Z",
            "f1",
        );
        assert!(clear_for_retry(&ledger, &FailureFilter::default()).is_empty());
    }

    #[test]
    fn the_ledger_sorts_by_conversation_then_line() {
        let ledger = apply_failure(&ApplyFailureInput {
            entries: Vec::new(),
            targets: vec![
                target("c2", "m1", 5),
                target("c1", "m9", 30),
                target("c1", "m1", 10),
            ],
            failure_id: "f1".to_owned(),
            reason: FailureReason::ChildExit,
            detail: None,
            now: "2026-11-10T00:00:00.000Z".to_owned(),
        });
        let order: Vec<(&str, usize)> = ledger
            .iter()
            .map(|record| (record.conversation_id.as_str(), record.end_snapshot_line))
            .collect();
        assert_eq!(order, vec![("c1", 10), ("c1", 30), ("c2", 5)]);
    }

    #[test]
    fn a_detail_drops_control_characters() {
        let detail = sanitize_detail("line one\nline two\ttabbed").expect("detail");
        assert_eq!(detail, "line one line two tabbed");
    }

    #[test]
    fn a_run_of_control_characters_collapses_to_one_space() {
        assert_eq!(sanitize_detail("a\n\n\n\r\nb").as_deref(), Some("a b"));
    }

    #[test]
    fn an_empty_or_control_only_detail_is_dropped() {
        assert_eq!(sanitize_detail(""), None);
        assert_eq!(sanitize_detail("   "), None);
        assert_eq!(sanitize_detail("\n\t\r"), None);
    }

    #[test]
    fn a_detail_is_truncated_on_a_character_boundary() {
        let detail = sanitize_detail(&"가".repeat(400)).expect("detail");
        assert!(detail.len() <= DETAIL_MAX_BYTES);
        assert_eq!(detail.chars().count(), DETAIL_MAX_BYTES / 3);
    }

    #[test]
    fn a_detail_exactly_at_the_cap_is_kept_whole() {
        let exact = "a".repeat(DETAIL_MAX_BYTES);
        assert_eq!(sanitize_detail(&exact).as_deref(), Some(exact.as_str()));
    }

    #[test]
    fn the_backoff_schedule_is_clamped_at_both_ends() {
        assert_eq!(delay_for(0), 60_000);
        assert_eq!(delay_for(1), 60_000);
        assert_eq!(delay_for(4), 7_200_000);
        assert_eq!(delay_for(99), 7_200_000);
    }

    #[test]
    fn an_unparseable_now_still_produces_a_record() {
        let ledger = apply(Vec::new(), FailureReason::ChildExit, "not a time", "f1");
        assert_eq!(ledger[0].streak, 1);
        assert!(ledger[0].next_eligible_at.is_some());
    }

    #[test]
    fn every_reason_has_a_distinct_wire_name() {
        let reasons = [
            FailureReason::QuickCategoryUnavailable,
            FailureReason::SandboxUnavailable,
            FailureReason::ChildExit,
            FailureReason::DeadlineExceeded,
            FailureReason::InvalidExtraction,
            FailureReason::MemoryWriteLockExhausted,
            FailureReason::ParentDirty,
            FailureReason::UnknownLiveness,
            FailureReason::PayloadEnvelopeOversize,
            FailureReason::PayloadEntryOversize,
            FailureReason::SecretLikeContent,
            FailureReason::Other,
        ];
        let mut names: Vec<&str> = reasons.iter().map(|reason| reason.as_str()).collect();
        names.sort_unstable();
        let unique = names.len();
        names.dedup();
        assert_eq!(names.len(), unique);
    }
}
