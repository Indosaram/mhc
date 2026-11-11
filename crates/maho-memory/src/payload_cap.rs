//! Byte-capped selection of a facts-extraction batch.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-payload-cap.md`. A payload that exceeds the cap is
//! rejected by the provider, so the batch has to be cut before it is written. Two invariants make the
//! cut safe: a conversation's entries are never interleaved with another's (the extractor reads each
//! conversation as a contiguous span), and an entry whose own payload alone busts the cap is reported
//! as oversized rather than silently skipped, because it must be parked or failed instead of retried
//! forever at the head of the queue.
use serde::{Deserialize, Serialize};

use crate::facts_queue::FactsBatch;
use crate::time_format::parse_millis;

/// The ceiling for one launched payload (128 KiB).
pub const MAX_FACTS_PAYLOAD_BYTES: usize = 131_072;

/// A conversation waiting this long outranks newer ones (24 hours).
pub const FACTS_STARVATION_MS: i64 = 24 * 60 * 60 * 1000;

/// A conversation past the starvation threshold.
pub const FACTS_STARVATION_HOURS: i64 = 24;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownPerson {
    pub slug: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrimaryHuman {
    pub slug: String,
    #[serde(default)]
    pub aliases: Vec<String>,
}

/// Everything in the payload except the entries themselves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadEnvelope {
    pub version: u32,
    #[serde(
        default,
        rename = "agreementEventVersion",
        skip_serializing_if = "Option::is_none"
    )]
    pub agreement_event_version: Option<u32>,
    pub identity: String,
    pub today: String,
    #[serde(rename = "knownPeople", default)]
    pub known_people: Vec<KnownPerson>,
    #[serde(rename = "primaryHuman")]
    pub primary_human: PrimaryHuman,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agreements: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payload {
    pub version: u32,
    #[serde(
        default,
        rename = "agreementEventVersion",
        skip_serializing_if = "Option::is_none"
    )]
    pub agreement_event_version: Option<u32>,
    pub identity: String,
    pub today: String,
    #[serde(rename = "knownPeople", default)]
    pub known_people: Vec<KnownPerson>,
    #[serde(rename = "primaryHuman")]
    pub primary_human: PrimaryHuman,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agreements: Option<Vec<serde_json::Value>>,
    pub entries: Vec<FactsBatch>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CappedBatchInput {
    pub entries: Vec<FactsBatch>,
    pub envelope: PayloadEnvelope,
    pub now: String,
    pub max_bytes: Option<usize>,
    pub starvation_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OversizedEntry {
    pub header: FactsBatch,
    pub bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CappedBatch {
    pub selected: Vec<FactsBatch>,
    pub oversized: Vec<OversizedEntry>,
    pub envelope_oversized: bool,
}

pub fn serialize_payload(payload: &Payload) -> String {
    let mut rendered = serde_json::to_string_pretty(payload).unwrap_or_default();
    rendered.push('\n');
    rendered
}

pub fn measure_payload_bytes(payload: &Payload) -> usize {
    serialize_payload(payload).len()
}

impl PayloadEnvelope {
    pub fn to_payload(&self, entries: Vec<FactsBatch>) -> Payload {
        Payload {
            version: self.version,
            agreement_event_version: self.agreement_event_version,
            identity: self.identity.clone(),
            today: self.today.clone(),
            known_people: self.known_people.clone(),
            primary_human: self.primary_human.clone(),
            agreements: self.agreements.clone(),
            entries,
        }
    }
}

pub fn select_capped_batch(input: &CappedBatchInput) -> CappedBatch {
    select_capped_batch_with(
        &input.entries,
        &input.envelope,
        &input.now,
        input.max_bytes,
        input.starvation_ms,
        |entry| Some(entry.clone()),
    )
}

/// Select over headers: `load` fetches the full entry, and returning `None` stops the current
/// conversation's bucket so a missing entry cannot reorder the ones after it.
///
/// PERF-DIVERGENCE(upstream facts payload cap): the upstream keeps every oversized entry fully
/// resident until classification; this retains only the header plus its measured byte count, so a
/// queue of pathological entries cannot balloon the process. Parking and failure semantics are
/// identical.
pub fn select_capped_batch_with(
    headers: &[FactsBatch],
    envelope: &PayloadEnvelope,
    now: &str,
    max_bytes: Option<usize>,
    starvation_ms: Option<i64>,
    mut load: impl FnMut(&FactsBatch) -> Option<FactsBatch>,
) -> CappedBatch {
    let max_bytes = max_bytes.unwrap_or(MAX_FACTS_PAYLOAD_BYTES);
    let starvation_ms = starvation_ms.unwrap_or(FACTS_STARVATION_MS);
    let envelope_bytes = measure_entries_bytes(envelope, &[]);
    if envelope_bytes > max_bytes {
        return CappedBatch {
            envelope_oversized: true,
            ..CappedBatch::default()
        };
    }

    let now_millis = parse_millis(now).unwrap_or(0);
    let ordered = order_conversations(group_by_conversation(headers), now_millis, starvation_ms);

    let mut oversized = Vec::new();
    let mut selected: Vec<FactsBatch> = Vec::new();
    let mut selected_bytes = envelope_bytes;

    for bucket in ordered {
        for candidate in bucket {
            let Some(full) = load(candidate) else {
                break;
            };
            let single_bytes = measure_entries_bytes(envelope, std::slice::from_ref(&&full));
            if single_bytes > max_bytes {
                let mut header = full;
                header.entries.clear();
                oversized.push(OversizedEntry {
                    header,
                    bytes: single_bytes,
                });
                break;
            }
            let next_bytes = next_selected_bytes(
                selected_bytes,
                envelope_bytes,
                single_bytes,
                selected.is_empty(),
            );
            if next_bytes > max_bytes {
                break;
            }
            selected.push(full);
            selected_bytes = next_bytes;
        }
    }

    let ordered_selected = group_by_conversation(selected.iter())
        .into_iter()
        .flat_map(|(_, bucket)| bucket)
        .cloned()
        .collect();

    CappedBatch {
        selected: ordered_selected,
        oversized,
        envelope_oversized: false,
    }
}

fn measure_entries_bytes(envelope: &PayloadEnvelope, entries: &[&FactsBatch]) -> usize {
    measure_payload_bytes(
        &envelope.to_payload(entries.iter().map(|entry| (*entry).clone()).collect()),
    )
}

/// Payload size after appending one entry, given that entry's single-entry payload size.
///
/// Pretty-printed array elements render identically at any position: the first entry replaces `[]`
/// with a bracketed block, and every later entry costs two bytes less because it reuses the
/// separator line instead of opening one.
fn next_selected_bytes(
    selected_bytes: usize,
    envelope_bytes: usize,
    single_bytes: usize,
    is_first: bool,
) -> usize {
    let delta = single_bytes.saturating_sub(envelope_bytes);
    if is_first {
        selected_bytes + delta
    } else {
        (selected_bytes + delta).saturating_sub(2)
    }
}

fn group_by_conversation<'a>(
    entries: impl IntoIterator<Item = &'a FactsBatch>,
) -> Vec<(String, Vec<&'a FactsBatch>)> {
    let mut order: Vec<String> = Vec::new();
    let mut map: std::collections::HashMap<String, Vec<&'a FactsBatch>> =
        std::collections::HashMap::new();
    for entry in entries {
        let conversation = entry.conversation_id.clone();
        if !map.contains_key(&conversation) {
            order.push(conversation.clone());
        }
        map.entry(conversation).or_default().push(entry);
    }
    let mut result = Vec::with_capacity(order.len());
    for conversation in order {
        if let Some(mut bucket) = map.remove(&conversation) {
            bucket.sort_by_key(|entry| entry.range.end_snapshot_line);
            result.push((conversation, bucket));
        }
    }
    result
}

fn waited_ms(entry: &FactsBatch, now_millis: i64) -> i64 {
    parse_millis(&entry.enqueued_at).map_or(0, |enqueued| now_millis - enqueued)
}

/// Order conversation buckets for fairness: starved conversations first, longest wait first; then
/// everything else, longest wait first.
///
/// The starved comparison is intentionally reversed. Among starved buckets the *oldest* must win, so
/// the ordering is `right.cmp(left)`, not `left.cmp(right)`.
fn order_conversations(
    groups: Vec<(String, Vec<&FactsBatch>)>,
    now_millis: i64,
    starvation_ms: i64,
) -> Vec<Vec<&FactsBatch>> {
    let mut buckets: Vec<Vec<&FactsBatch>> = groups.into_iter().map(|(_, bucket)| bucket).collect();
    buckets.sort_by(|left, right| {
        let left_wait = left.first().map_or(0, |entry| waited_ms(entry, now_millis));
        let right_wait = right
            .first()
            .map_or(0, |entry| waited_ms(entry, now_millis));
        let left_starved = left_wait >= starvation_ms;
        let right_starved = right_wait >= starvation_ms;

        match (left_starved, right_starved) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            (true, true) => right_wait.cmp(&left_wait),
            (false, false) => left_wait.cmp(&right_wait),
        }
    });
    buckets
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts_queue::{BatchEntry, BatchRange};

    fn entry(conversation: &str, end_line: usize, enqueued_at: &str, body: &str) -> FactsBatch {
        FactsBatch {
            version: 1,
            identity: "maho".to_owned(),
            session_id: "s1".to_owned(),
            conversation_id: conversation.to_owned(),
            range: BatchRange {
                start_message_id: format!("m{end_line}"),
                end_message_id: format!("m{end_line}"),
                start_line: end_line.saturating_sub(1),
                end_snapshot_line: end_line,
            },
            entries: vec![BatchEntry {
                kind: "user".to_owned(),
                text: Some(body.to_owned()),
                name: None,
                args_text: None,
                result_text: None,
                result_ok: None,
                captured_at: None,
                source_line_id: None,
                source_message_id: None,
            }],
            enqueued_at: enqueued_at.to_owned(),
        }
    }

    fn envelope() -> PayloadEnvelope {
        PayloadEnvelope {
            version: 1,
            agreement_event_version: None,
            identity: "maho".to_owned(),
            today: "2026-11-10".to_owned(),
            known_people: Vec::new(),
            primary_human: PrimaryHuman {
                slug: "human".to_owned(),
                aliases: Vec::new(),
            },
            agreements: None,
        }
    }

    fn select(entries: Vec<FactsBatch>, max_bytes: usize) -> CappedBatch {
        select_capped_batch(&CappedBatchInput {
            entries,
            envelope: envelope(),
            now: "2026-11-10T00:00:00Z".to_owned(),
            max_bytes: Some(max_bytes),
            starvation_ms: None,
        })
    }

    #[test]
    fn everything_fits_under_a_generous_cap() {
        let batch = select(
            vec![
                entry("c1", 2, "2026-11-09T00:00:00Z", "first"),
                entry("c1", 4, "2026-11-09T00:00:00Z", "second"),
            ],
            MAX_FACTS_PAYLOAD_BYTES,
        );
        assert_eq!(batch.selected.len(), 2);
        assert!(batch.oversized.is_empty());
        assert!(!batch.envelope_oversized);
    }

    #[test]
    fn the_selected_payload_never_exceeds_the_cap() {
        let entries: Vec<FactsBatch> = (1..40)
            .map(|index| entry("c1", index * 2, "2026-11-09T00:00:00Z", &"x".repeat(400)))
            .collect();
        let cap = 4096;
        let batch = select(entries, cap);
        assert!(
            measure_entries_bytes(&envelope(), &batch.selected.iter().collect::<Vec<_>>()) <= cap
        );
    }

    #[test]
    fn an_envelope_larger_than_the_cap_selects_nothing() {
        let batch = select(vec![entry("c1", 2, "2026-11-09T00:00:00Z", "x")], 16);
        assert!(batch.envelope_oversized);
        assert!(batch.selected.is_empty());
        assert!(batch.oversized.is_empty());
    }

    #[test]
    fn an_entry_larger_than_the_cap_is_reported_as_oversized_without_its_body() {
        let batch = select(
            vec![entry("c1", 2, "2026-11-09T00:00:00Z", &"x".repeat(9000))],
            4096,
        );
        assert!(batch.selected.is_empty());
        assert_eq!(batch.oversized.len(), 1);
        assert!(batch.oversized[0].header.entries.is_empty());
        assert!(batch.oversized[0].bytes > 4096);
    }

    #[test]
    fn an_oversized_entry_stops_its_conversation_bucket() {
        let batch = select(
            vec![
                entry("c1", 2, "2026-11-09T00:00:00Z", &"x".repeat(9000)),
                entry("c1", 4, "2026-11-09T00:00:00Z", "small"),
            ],
            4096,
        );
        assert_eq!(batch.oversized.len(), 1);
        assert!(
            batch.selected.is_empty(),
            "the follower must not leapfrog the oversized head"
        );
    }

    #[test]
    fn a_conversation_is_never_split_across_the_selected_batch() {
        let mut entries = Vec::new();
        for conversation in ["a", "b", "c"] {
            for line in [2usize, 4, 6] {
                entries.push(entry(
                    conversation,
                    line,
                    "2026-11-09T00:00:00Z",
                    &"y".repeat(600),
                ));
            }
        }
        let batch = select(entries, 2600);
        let mut seen: Vec<String> = Vec::new();
        for item in &batch.selected {
            if seen.last() != Some(&item.conversation_id) {
                assert!(
                    !seen.contains(&item.conversation_id),
                    "conversation {} was split",
                    item.conversation_id
                );
                seen.push(item.conversation_id.clone());
            }
        }
    }

    #[test]
    fn entries_within_a_conversation_are_ordered_by_end_line() {
        let batch = select(
            vec![
                entry("c1", 6, "2026-11-09T00:00:00Z", "third"),
                entry("c1", 2, "2026-11-09T00:00:00Z", "first"),
                entry("c1", 4, "2026-11-09T00:00:00Z", "second"),
            ],
            MAX_FACTS_PAYLOAD_BYTES,
        );
        let order: Vec<usize> = batch
            .selected
            .iter()
            .map(|item| item.range.end_snapshot_line)
            .collect();
        assert_eq!(order, vec![2, 4, 6]);
    }

    #[test]
    fn the_longest_waiting_conversation_goes_first() {
        let batch = select(
            vec![
                entry("new", 2, "2026-11-09T23:00:00Z", "x"),
                entry("old", 2, "2026-11-08T00:00:00Z", "x"),
            ],
            MAX_FACTS_PAYLOAD_BYTES,
        );
        assert_eq!(batch.selected[0].conversation_id, "old");
    }

    #[test]
    fn a_starved_conversation_outranks_a_newer_one() {
        let batch = select(
            vec![
                entry("new", 2, "2026-11-09T23:00:00Z", "x"),
                entry("starved", 2, "2026-11-05T00:00:00Z", "x"),
            ],
            MAX_FACTS_PAYLOAD_BYTES,
        );
        assert_eq!(batch.selected[0].conversation_id, "starved");
    }

    #[test]
    fn among_starved_conversations_the_oldest_goes_first() {
        let batch = select(
            vec![
                entry("younger", 2, "2026-11-08T00:00:00Z", "x"),
                entry("oldest", 2, "2026-11-01T00:00:00Z", "x"),
            ],
            MAX_FACTS_PAYLOAD_BYTES,
        );
        assert_eq!(batch.selected[0].conversation_id, "oldest");
    }

    #[test]
    fn an_unparseable_enqueue_time_counts_as_no_wait() {
        let batch = select(
            vec![
                entry("broken", 2, "not a time", "x"),
                entry("valid", 2, "2026-11-09T00:00:00Z", "x"),
            ],
            MAX_FACTS_PAYLOAD_BYTES,
        );
        assert_eq!(batch.selected[0].conversation_id, "valid");
    }

    #[test]
    fn the_payload_ends_with_a_newline() {
        assert!(serialize_payload(&envelope().to_payload(Vec::new())).ends_with('\n'));
    }

    #[test]
    fn a_loader_returning_none_stops_at_that_point() {
        let headers = vec![entry("c1", 2, "2026-11-09T00:00:00Z", "a")];
        let batch = select_capped_batch_with(
            &headers,
            &envelope(),
            "2026-11-10T00:00:00Z",
            None,
            None,
            |_| None,
        );
        assert!(batch.selected.is_empty());
    }

    #[test]
    fn the_capped_selection_includes_the_envelope_in_its_budget() {
        let one =
            measure_entries_bytes(&envelope(), &[&entry("c1", 2, "2026-11-09T00:00:00Z", "x")]);
        let batch = select(vec![entry("c1", 2, "2026-11-09T00:00:00Z", "x")], one);
        assert_eq!(batch.selected.len(), 1);
        let batch = select(vec![entry("c1", 2, "2026-11-09T00:00:00Z", "x")], one - 1);
        assert!(batch.selected.is_empty());
    }
}
