//! Read-only rendering of the facts failure ledger for `/facts`, `/doctor`, and the advisory line.
//! Port of `components/memory/commands/facts-status.ts` at pin 77f3067f1.

use std::path::Path;

use memory_core::facts::{
    failures_schema::{FactsFailureRecord, FactsFailureState},
    failures_store::{FactsFailureStore, FactsFailureStoreError, FactsFailureStoreOptions},
    schema::facts_queue_paths,
};
use memory_core::identity::layout::MemoryIdentityPaths;

const MAX_LISTED_CONVERSATIONS: usize = 5;
const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 60 * MINUTE_MS;

pub struct FactsOverview {
    pub pending: usize,
    pub records: Vec<FactsFailureRecord>,
    pub now_ms: i64,
    pub corrupt: Option<String>,
}

fn parked(state: &FactsOverview) -> Vec<&FactsFailureRecord> {
    state
        .records
        .iter()
        .filter(|record| record.state == FactsFailureState::Parked)
        .collect()
}

fn backoff(state: &FactsOverview) -> Vec<&FactsFailureRecord> {
    state
        .records
        .iter()
        .filter(|record| record.state == FactsFailureState::Backoff)
        .collect()
}

fn next_eligible_ms(state: &FactsOverview) -> Option<i64> {
    let instants: Vec<i64> = backoff(state)
        .iter()
        .filter_map(|record| record.next_eligible_at.as_deref())
        .filter_map(parse_iso_ms)
        .collect();
    let earliest = instants.into_iter().min()?;
    Some((earliest - state.now_ms).max(0))
}

fn parse_iso_ms(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.timestamp_millis())
}

fn format_wait(delta_ms: i64) -> String {
    if delta_ms < MINUTE_MS {
        return "now".to_owned();
    }
    if delta_ms < HOUR_MS {
        return format!("{}m", round_div(delta_ms, MINUTE_MS));
    }
    format!("{}h", round_div(delta_ms, HOUR_MS))
}

fn round_div(value: i64, divisor: i64) -> i64 {
    let quotient = value / divisor;
    let remainder = value % divisor;
    if remainder.abs() * 2 >= divisor { quotient + value.signum() } else { quotient }
}

fn first_line(detail: Option<&str>) -> Option<String> {
    let line = detail?.split('\n').next()?.trim();
    if line.is_empty() {
        None
    } else {
        Some(line.to_owned())
    }
}

/// One bounded advisory line, or nothing when the ledger is clean.
pub fn format_facts_advisory(state: &FactsOverview) -> Option<String> {
    if state.corrupt.is_some() {
        return Some("facts: failure ledger unreadable (run /facts for detail)".to_owned());
    }
    let parked_count = parked(state).len();
    let backoff_count = backoff(state).len();
    if parked_count == 0 && backoff_count == 0 {
        return None;
    }
    let suffix = next_eligible_ms(state)
        .map(|wait| format!(" (next {})", format_wait(wait)))
        .unwrap_or_default();
    Some(format!(
        "facts: {parked_count} parked / {backoff_count} backoff{suffix}"
    ))
}

/// Advisory remediation: parked state names the one manual remedy, `/facts retry`.
pub fn facts_remediation_hint(state: &FactsOverview) -> Option<String> {
    if let Some(corrupt) = &state.corrupt {
        return Some(format!(
            "facts failure ledger is unreadable ({corrupt}); repair or delete failures.json to resume launches"
        ));
    }
    let parked_count = parked(state).len();
    if parked_count > 0 {
        return Some(format!(
            "{parked_count} parked facts batch{} need{} a manual unpark; run /facts retry after fixing the cause",
            if parked_count == 1 { "" } else { "es" },
            if parked_count == 1 { "s" } else { "" }
        ));
    }
    let backoff_count = backoff(state).len();
    if backoff_count == 0 {
        return None;
    }
    let wait = next_eligible_ms(state)
        .map(|wait| format!("; next attempt in {}", format_wait(wait)))
        .unwrap_or_default();
    Some(format!(
        "{backoff_count} facts batch{} backing off{wait}",
        if backoff_count == 1 { " is" } else { "es are" }
    ))
}

/// The full read-only `/facts` view. Never enters model context; callers notify + return it.
pub fn format_facts_status(identity: &str, state: &FactsOverview) -> String {
    let mut lines = vec![
        format!("# Facts pipeline: {identity}"),
        String::new(),
        format!("queued: {} pending", state.pending),
    ];
    if let Some(corrupt) = &state.corrupt {
        lines.push(String::new());
        lines.push(format!("failure ledger is UNREADABLE: {corrupt}"));
        lines.push(
            "launches are blocked until failures.json is repaired or removed (fail-closed by design)"
                .to_owned(),
        );
        return lines.join("\n");
    }

    let parked_records = parked(state);
    let backoff_records = backoff(state);
    if parked_records.is_empty() && backoff_records.is_empty() {
        lines.push("no failing batches".to_owned());
        return lines.join("\n");
    }

    let wait_suffix = next_eligible_ms(state)
        .map(|wait| format!(" (next eligible {})", format_wait(wait)))
        .unwrap_or_default();
    lines.push(format!("parked: {}", parked_records.len()));
    lines.push(format!("backoff: {}{wait_suffix}", backoff_records.len()));
    lines.push(String::new());

    let mut newest_first: Vec<&FactsFailureRecord> = state.records.iter().collect();
    newest_first.sort_by(|left, right| {
        parse_iso_ms(&right.last_failure_at)
            .unwrap_or(0)
            .cmp(&parse_iso_ms(&left.last_failure_at).unwrap_or(0))
    });
    for record in newest_first.iter().take(MAX_LISTED_CONVERSATIONS) {
        let detail = first_line(record.last_detail.as_deref())
            .map(|detail| format!(" - {detail}"))
            .unwrap_or_default();
        lines.push(format!(
            "- {}: {} after {} ({}){detail}",
            record.conversation_id,
            record.state.as_str(),
            record.streak,
            record.last_reason.as_str()
        ));
    }
    if newest_first.len() > MAX_LISTED_CONVERSATIONS {
        lines.push(format!(
            "... and {} more",
            newest_first.len() - MAX_LISTED_CONVERSATIONS
        ));
    }
    lines.push(String::new());
    lines.push("Unpark: /facts retry [--conversation <id>]".to_owned());
    lines.join("\n")
}

fn count_pending(queue_dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(queue_dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            name.ends_with(".json") && name != "consumed.json" && name != "failures.json"
        })
        .count()
}

pub struct ReadFactsOverviewInput<'a> {
    pub identity_paths: &'a MemoryIdentityPaths,
    pub now_ms: i64,
}

/// Reads the durable ledger + queue depth. A corrupt ledger becomes `corrupt`, not zeros.
pub fn read_facts_overview(input: ReadFactsOverviewInput<'_>) -> FactsOverview {
    let layout = facts_queue_paths(input.identity_paths);
    let pending = count_pending(&layout.queue_dir);
    let now_ms = input.now_ms;
    let store = FactsFailureStore::new(FactsFailureStoreOptions {
        identity_paths: input.identity_paths.clone(),
        now: Some(std::sync::Arc::new(move || now_ms)),
        lock_wait_ms: None,
    });
    match store.read_failures() {
        Ok(failures) => FactsOverview { pending, records: failures.entries, now_ms, corrupt: None },
        Err(error) => {
            let corrupt = match &error {
                FactsFailureStoreError::Corrupt(corrupt) => corrupt.to_string(),
                other => format!("failure ledger unreadable: {other}"),
            };
            FactsOverview { pending, records: Vec::new(), now_ms, corrupt: Some(corrupt) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_core::facts::failures_schema::{FactsFailureReason, FactsFailureState};

    const NOW_MS: i64 = 1_786_881_600_000;

    fn backoff_record(end_message_id: &str) -> FactsFailureRecord {
        FactsFailureRecord {
            conversation_id: "conv-a".to_owned(),
            end_message_id: end_message_id.to_owned(),
            end_snapshot_line: 4,
            state: FactsFailureState::Backoff,
            streak: 2,
            first_failure_at: "2026-08-16T11:00:00.000Z".to_owned(),
            last_failure_at: "2026-08-16T11:50:00.000Z".to_owned(),
            last_reason: FactsFailureReason::ChildExit,
            last_detail: None,
            last_failure_id: "run-1".to_owned(),
            next_eligible_at: Some("2026-08-16T12:12:00.000Z".to_owned()),
            parked_at: None,
        }
    }

    fn parked_record(end_message_id: &str) -> FactsFailureRecord {
        FactsFailureRecord {
            conversation_id: "conv-b".to_owned(),
            end_message_id: end_message_id.to_owned(),
            end_snapshot_line: 9,
            state: FactsFailureState::Parked,
            streak: 5,
            first_failure_at: "2026-08-16T09:00:00.000Z".to_owned(),
            last_failure_at: "2026-08-16T11:00:00.000Z".to_owned(),
            last_reason: FactsFailureReason::DeadlineExceeded,
            last_detail: None,
            last_failure_id: "run-5".to_owned(),
            next_eligible_at: None,
            parked_at: Some("2026-08-16T11:00:00.000Z".to_owned()),
        }
    }

    fn overview(records: Vec<FactsFailureRecord>) -> FactsOverview {
        FactsOverview { pending: 0, records, now_ms: NOW_MS, corrupt: None }
    }

    #[test]
    fn given_parked_and_backoff_records_when_rendering_the_advisory_then_one_bounded_line_names_both_counts() {
        let state = overview(vec![
            parked_record("msg-b"),
            parked_record("msg-c"),
            parked_record("msg-d"),
            backoff_record("msg-a"),
        ]);
        assert_eq!(format_facts_advisory(&state).as_deref(), Some("facts: 3 parked / 1 backoff (next 12m)"));
    }

    #[test]
    fn given_no_failure_records_when_rendering_the_advisory_then_nothing_is_shown() {
        let mut state = overview(Vec::new());
        state.pending = 2;
        assert!(format_facts_advisory(&state).is_none());
    }

    #[test]
    fn given_a_corrupt_ledger_when_rendering_the_advisory_then_it_says_corrupt_instead_of_zeros() {
        let mut state = overview(Vec::new());
        state.corrupt = Some("unexpected token".to_owned());
        assert_eq!(
            format_facts_advisory(&state).as_deref(),
            Some("facts: failure ledger unreadable (run /facts for detail)")
        );
    }

    #[test]
    fn given_a_corrupt_ledger_when_rendering_the_status_view_then_it_reports_corruption_and_no_counts() {
        let mut state = overview(Vec::new());
        state.pending = 3;
        state.corrupt = Some("unexpected token } in JSON at position 4".to_owned());
        let text = format_facts_status("agent-x", &state);
        assert!(text.contains("failure ledger is UNREADABLE"));
        assert!(text.contains("unexpected token"));
        assert!(text.contains("launches are blocked"));
        assert!(!text.contains("parked: 0"));
        assert!(!text.contains("backoff: 0"));
    }

    #[test]
    fn given_failing_conversations_when_rendering_the_status_view_then_counts_and_bounded_reasons_appear() {
        let mut many: Vec<FactsFailureRecord> = Vec::new();
        for index in 0..7i64 {
            let mut record = parked_record(&format!("msg-{index}"));
            record.conversation_id = format!("conv-{index}");
            record.last_failure_at = chrono::DateTime::from_timestamp_millis(NOW_MS - index * 60_000)
                .map(|instant| instant.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
                .unwrap_or_default();
            record.last_detail = Some(format!("boom {index}\nsecond line"));
            many.push(record);
        }
        let mut records = many;
        records.push(backoff_record("msg-a"));
        let mut state = overview(records);
        state.pending = 9;

        let text = format_facts_status("agent-x", &state);
        assert!(text.contains("queued: 9 pending"));
        assert!(text.contains("parked: 7"));
        assert!(text.contains("backoff: 1 (next eligible 12m)"));
        assert_eq!(
            text.lines().filter(|line| line.starts_with("- conv-")).count(),
            5
        );
        assert!(text.contains("boom 0"));
        assert!(!text.contains("second line"));
        assert!(text.contains("/facts retry"));
    }

    #[test]
    fn given_a_healthy_queue_when_rendering_the_status_view_then_no_failure_section_is_shown() {
        let mut state = overview(Vec::new());
        state.pending = 1;
        let text = format_facts_status("agent-x", &state);
        assert!(text.contains("queued: 1 pending"));
        assert!(text.contains("no failing batches"));
        assert!(!text.contains("parked:"));
    }

    #[test]
    fn given_parked_records_when_asking_for_the_hint_then_it_names_the_retry_command_and_count() {
        let state = overview(vec![parked_record("msg-b"), parked_record("msg-z")]);
        assert_eq!(
            facts_remediation_hint(&state).as_deref(),
            Some("2 parked facts batches need a manual unpark; run /facts retry after fixing the cause")
        );
    }

    #[test]
    fn given_only_backoff_records_when_asking_for_the_hint_then_it_reports_the_wait_instead_of_a_remedy() {
        let state = overview(vec![backoff_record("msg-a")]);
        assert_eq!(
            facts_remediation_hint(&state).as_deref(),
            Some("1 facts batch is backing off; next attempt in 12m")
        );
    }

    #[test]
    fn given_a_clean_ledger_when_asking_for_the_hint_then_there_is_no_hint() {
        let state = overview(Vec::new());
        assert!(facts_remediation_hint(&state).is_none());
    }
}
