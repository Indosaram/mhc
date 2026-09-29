use pretty_assertions::assert_eq;

use super::*;
use crate::facts::schema::{FACTS_QUEUE_VERSION, FactsQueueEntry, FactsQueueRange};
use crate::journal::entries::TranscriptEntry;
use crate::support::time::format_rfc3339_millis;

const T0: &str = "2026-08-16T00:00:00.000Z";
const T0_MILLIS: i64 = 1786838400000;

fn test_envelope() -> FactsPayloadEnvelope {
    FactsPayloadEnvelope {
        version: 1,
        identity: "facts-agent".to_string(),
        today: "2026-08-16".to_string(),
        known_people: Vec::new(),
        primary_human: FactsPrimaryHuman {
            slug: "human".to_string(),
            aliases: Vec::new(),
        },
    }
}

fn sample_entry(
    conversation_id: &str,
    end_message_id: &str,
    end_snapshot_line: u64,
    text: Option<&str>,
    enqueued_at: Option<&str>,
) -> FactsQueueEntry {
    let text_val = text
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{conversation_id}/{end_message_id}"));
    let transcript: TranscriptEntry = serde_json::from_value(serde_json::json!({
        "kind": "user",
        "text": text_val,
        "captured_at": T0,
        "source_line_id": format!("{end_message_id}:user"),
        "source_message_id": end_message_id,
    }))
    .expect("valid transcript entry");

    FactsQueueEntry {
        version: FACTS_QUEUE_VERSION,
        identity: "facts-agent".to_string(),
        session_id: conversation_id.to_string(),
        conversation_id: conversation_id.to_string(),
        range: FactsQueueRange {
            start_message_id: format!("{end_message_id}-start"),
            end_message_id: end_message_id.to_string(),
            start_line: end_snapshot_line.saturating_sub(1),
            end_snapshot_line,
        },
        enqueued_at: enqueued_at.unwrap_or(T0).to_string(),
        entries: vec![transcript],
    }
}

fn endpoints(selected: &[FactsQueueEntry]) -> Vec<String> {
    selected
        .iter()
        .map(|c| format!("{}/{}", c.conversation_id, c.range.end_message_id))
        .collect()
}

fn entry_sized_to(
    conversation_id: &str,
    end_message_id: &str,
    end_snapshot_line: u64,
    target: usize,
) -> FactsQueueEntry {
    let envelope = test_envelope();
    let mut padding = 1;
    for _ in 0..64 {
        let text = "x".repeat(padding);
        let candidate = sample_entry(
            conversation_id,
            end_message_id,
            end_snapshot_line,
            Some(&text),
            None,
        );
        let bytes = measure_facts_payload_bytes(&envelope.to_payload(vec![candidate.clone()]));
        if bytes == target {
            return candidate;
        }
        if bytes > target {
            panic!("cannot size entry to {target} bytes (overshot at {bytes})");
        }
        padding += target - bytes;
    }
    panic!("entry sizing did not converge on {target} bytes");
}

#[test]
fn test_payload_measurement_matches_utf8_length() {
    // given
    let entry = sample_entry("alpha", "m1", 1, Some("café ☕"), None);
    let payload = test_envelope().to_payload(vec![entry]);

    // when
    let measured = measure_facts_payload_bytes(&payload);

    // then
    let serialized = serialize_facts_payload(&payload);
    assert_eq!(serialized.ends_with('\n'), true);
    assert_eq!(measured, serialized.len());
}

#[test]
fn test_cap_constant_value() {
    assert_eq!(MAX_FACTS_PAYLOAD_BYTES, 131_072);
}

#[test]
fn test_exact_cap_accepted_and_one_byte_more_rejected() {
    // given
    let exact = entry_sized_to("alpha", "m1", 1, MAX_FACTS_PAYLOAD_BYTES);
    let over = entry_sized_to("beta", "m1", 1, MAX_FACTS_PAYLOAD_BYTES + 1);

    // when
    let accepted = select_capped_facts_batch(&CappedFactsBatchInput {
        entries: vec![exact.clone()],
        envelope: test_envelope(),
        now: T0.to_string(),
        max_bytes: None,
        starvation_ms: None,
    });
    let rejected = select_capped_facts_batch(&CappedFactsBatchInput {
        entries: vec![over],
        envelope: test_envelope(),
        now: T0.to_string(),
        max_bytes: None,
        starvation_ms: None,
    });

    // then
    assert_eq!(
        measure_facts_payload_bytes(&test_envelope().to_payload(vec![exact])),
        MAX_FACTS_PAYLOAD_BYTES
    );
    assert_eq!(endpoints(&accepted.selected), vec!["alpha/m1"]);
    assert_eq!(rejected.selected.is_empty(), true);
    assert_eq!(
        rejected
            .oversized
            .iter()
            .map(|c| c.range.end_message_id.as_str())
            .collect::<Vec<_>>(),
        vec!["m1"]
    );
}

#[test]
fn test_prefix_closure_ships_neither_when_older_does_not_fit() {
    // given
    let older = entry_sized_to("alpha", "m1", 1, MAX_FACTS_PAYLOAD_BYTES - 200);
    let newer = sample_entry("alpha", "m2", 2, None, None);
    let other = sample_entry("beta", "m9", 1, None, None);
    let max_bytes = MAX_FACTS_PAYLOAD_BYTES - 400;

    // when
    let selection = select_capped_facts_batch(&CappedFactsBatchInput {
        entries: vec![older, newer, other],
        envelope: test_envelope(),
        now: T0.to_string(),
        max_bytes: Some(max_bytes),
        starvation_ms: None,
    });

    // then
    assert_eq!(endpoints(&selection.selected), vec!["beta/m9"]);
    assert_eq!(
        measure_facts_payload_bytes(&test_envelope().to_payload(selection.selected.clone()))
            <= max_bytes,
        true
    );
}

#[test]
fn test_middle_entry_not_fitting_holds_back_follower() {
    // given
    let first = sample_entry("alpha", "m1", 1, Some(&"y".repeat(3_000)), None);
    let middle = sample_entry("alpha", "m2", 2, Some(&"z".repeat(3_000)), None);
    let last = sample_entry("alpha", "m3", 3, None, None);
    let budget =
        measure_facts_payload_bytes(&test_envelope().to_payload(vec![first.clone(), last.clone()]))
            + 100;

    // when
    let selection = select_capped_facts_batch(&CappedFactsBatchInput {
        entries: vec![first, middle.clone(), last],
        envelope: test_envelope(),
        now: T0.to_string(),
        max_bytes: Some(budget),
        starvation_ms: None,
    });

    // then
    assert_eq!(
        measure_facts_payload_bytes(&test_envelope().to_payload(vec![middle])) <= budget,
        true
    );
    assert_eq!(endpoints(&selection.selected), vec!["alpha/m1"]);
    assert_eq!(selection.oversized.is_empty(), true);
}

#[test]
fn test_fitting_older_entry_ships_when_newer_does_not_fit() {
    // given
    let older = sample_entry("alpha", "m1", 1, None, None);
    let newer = entry_sized_to("alpha", "m2", 2, MAX_FACTS_PAYLOAD_BYTES);

    // when
    let selection = select_capped_facts_batch(&CappedFactsBatchInput {
        entries: vec![older, newer],
        envelope: test_envelope(),
        now: T0.to_string(),
        max_bytes: None,
        starvation_ms: None,
    });

    // then
    assert_eq!(endpoints(&selection.selected), vec!["alpha/m1"]);
}

#[test]
fn test_starved_conversation_selected_first() {
    // given
    let starved_enqueued = format_rfc3339_millis(T0_MILLIS - 24 * 60 * 60_000);
    let fresh_enqueued = format_rfc3339_millis(T0_MILLIS - 60_000);
    let starved = sample_entry("conv-a", "ma", 1, Some("rival"), Some(&starved_enqueued));
    let fresh = sample_entry("conv-b", "mb", 1, Some("rival"), Some(&fresh_enqueued));
    let budget = measure_facts_payload_bytes(&test_envelope().to_payload(vec![starved.clone()]));

    // when
    let selection = select_capped_facts_batch(&CappedFactsBatchInput {
        entries: vec![fresh.clone(), starved],
        envelope: test_envelope(),
        now: T0.to_string(),
        max_bytes: Some(budget),
        starvation_ms: None,
    });

    // then
    assert_eq!(
        measure_facts_payload_bytes(&test_envelope().to_payload(vec![fresh])),
        budget
    );
    assert_eq!(endpoints(&selection.selected), vec!["conv-a/ma"]);
}

#[test]
fn test_almost_starved_conversation_loses_to_newest() {
    // given
    let almost_starved_enqueued = format_rfc3339_millis(T0_MILLIS - 24 * 60 * 60_000 + 1);
    let fresh_enqueued = format_rfc3339_millis(T0_MILLIS - 60_000);
    let almost_starved = sample_entry(
        "conv-a",
        "ma",
        1,
        Some("rival"),
        Some(&almost_starved_enqueued),
    );
    let fresh = sample_entry("conv-b", "mb", 1, Some("rival"), Some(&fresh_enqueued));
    let budget = measure_facts_payload_bytes(&test_envelope().to_payload(vec![fresh.clone()]));

    // when
    let selection = select_capped_facts_batch(&CappedFactsBatchInput {
        entries: vec![almost_starved.clone(), fresh],
        envelope: test_envelope(),
        now: T0.to_string(),
        max_bytes: Some(budget),
        starvation_ms: None,
    });

    // then
    assert_eq!(
        measure_facts_payload_bytes(&test_envelope().to_payload(vec![almost_starved])),
        budget
    );
    assert_eq!(endpoints(&selection.selected), vec!["conv-b/mb"]);
}

#[test]
fn test_multi_conversation_entries_ship_ascending() {
    // given
    let entries = vec![
        sample_entry("beta", "m9", 2, None, None),
        sample_entry("alpha", "m2", 4, None, None),
        sample_entry("beta", "m8", 1, None, None),
        sample_entry("alpha", "m1", 3, None, None),
    ];

    // when
    let selection = select_capped_facts_batch(&CappedFactsBatchInput {
        entries,
        envelope: test_envelope(),
        now: T0.to_string(),
        max_bytes: None,
        starvation_ms: None,
    });

    // then
    let alpha_lines: Vec<u64> = selection
        .selected
        .iter()
        .filter(|c| c.conversation_id == "alpha")
        .map(|c| c.range.end_snapshot_line)
        .collect();
    let beta_lines: Vec<u64> = selection
        .selected
        .iter()
        .filter(|c| c.conversation_id == "beta")
        .map(|c| c.range.end_snapshot_line)
        .collect();

    assert_eq!(alpha_lines, vec![3, 4]);
    assert_eq!(beta_lines, vec![1, 2]);
    assert_eq!(selection.selected.len(), 4);
    assert_eq!(selection.oversized.is_empty(), true);
}

#[test]
fn test_oversized_entry_reported_and_never_truncated() {
    // given
    let oversized = entry_sized_to("alpha", "m1", 1, MAX_FACTS_PAYLOAD_BYTES + 1);
    let healthy = sample_entry("beta", "m9", 1, None, None);

    // when
    let selection = select_capped_facts_batch(&CappedFactsBatchInput {
        entries: vec![oversized.clone(), healthy],
        envelope: test_envelope(),
        now: T0.to_string(),
        max_bytes: None,
        starvation_ms: None,
    });

    // then
    assert_eq!(endpoints(&selection.selected), vec!["beta/m9"]);
    assert_eq!(selection.oversized, vec![oversized]);
}

#[test]
fn test_oversized_envelope_blocks_all_shipments() {
    // given
    let mut envelope = test_envelope();
    envelope.identity = "x".repeat(MAX_FACTS_PAYLOAD_BYTES);

    // when
    let selection = select_capped_facts_batch(&CappedFactsBatchInput {
        entries: vec![sample_entry("alpha", "m1", 1, None, None)],
        envelope,
        now: T0.to_string(),
        max_bytes: None,
        starvation_ms: None,
    });

    // then
    assert_eq!(selection.selected.is_empty(), true);
    assert_eq!(selection.envelope_oversized, true);
}
