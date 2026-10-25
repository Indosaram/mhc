//! Byte-capped lossless batch selection for the facts extraction payload.

use serde::{Deserialize, Serialize};

use super::schema::FactsQueueEntry;
use crate::support::time::parse_rfc3339;

/// Hard ceiling for one launched payload in bytes (128 KiB).
pub const MAX_FACTS_PAYLOAD_BYTES: usize = 131_072;

/// Threshold after which waiting queue entries outrank newer entries (24 hours).
pub const FACTS_STARVATION_MS: i64 = 24 * 60 * 60 * 1000;

/// Known person profile passed into the extraction payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsKnownPerson {
    pub slug: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub aliases: Vec<String>,
}

/// Primary human profile anchor in the extraction payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsPrimaryHuman {
    pub slug: String,
    pub aliases: Vec<String>,
}

/// Facts payload envelope excluding entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsPayloadEnvelope {
    pub version: u32,
    #[serde(
        default,
        rename = "agreementEventVersion",
        skip_serializing_if = "Option::is_none"
    )]
    pub agreement_event_version: Option<u32>,
    pub identity: String,
    pub today: String,
    #[serde(rename = "knownPeople")]
    pub known_people: Vec<FactsKnownPerson>,
    #[serde(rename = "primaryHuman")]
    pub primary_human: FactsPrimaryHuman,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agreements: Option<Vec<crate::agreements::FactsKnownAgreement>>,
}

impl FactsPayloadEnvelope {
    /// Construct a full payload by attaching entries to this envelope.
    pub fn to_payload(&self, entries: Vec<FactsQueueEntry>) -> FactsPayload {
        FactsPayload {
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

/// Complete extraction payload serialized to disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsPayload {
    pub version: u32,
    #[serde(
        default,
        rename = "agreementEventVersion",
        skip_serializing_if = "Option::is_none"
    )]
    pub agreement_event_version: Option<u32>,
    pub identity: String,
    pub today: String,
    #[serde(rename = "knownPeople")]
    pub known_people: Vec<FactsKnownPerson>,
    #[serde(rename = "primaryHuman")]
    pub primary_human: FactsPrimaryHuman,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agreements: Option<Vec<crate::agreements::FactsKnownAgreement>>,
    pub entries: Vec<FactsQueueEntry>,
}

/// Input parameters for capped batch selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CappedFactsBatchInput {
    pub entries: Vec<FactsQueueEntry>,
    pub envelope: FactsPayloadEnvelope,
    pub now: String,
    pub max_bytes: Option<usize>,
    pub starvation_ms: Option<i64>,
}

/// An oversized queue entry candidate retained only as a header with its measured payload byte size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OversizedFactsEntry {
    pub header: FactsQueueEntry,
    pub bytes: usize,
}

/// Selected batch result with oversize tracking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CappedFactsBatch {
    pub selected: Vec<FactsQueueEntry>,
    pub oversized: Vec<OversizedFactsEntry>,
    pub envelope_oversized: bool,
}

/// Serialize payload to formatted, newline-terminated JSON text.
pub fn serialize_facts_payload(payload: &FactsPayload) -> String {
    let mut payload = payload.clone();
    for entry in &mut payload.entries {
        entry.lead = None;
    }
    let mut rendered = serde_json::to_string_pretty(&payload).unwrap_or_default();
    rendered.push('\n');
    rendered
}

/// Measure byte size of payload when serialized to disk.
pub fn measure_facts_payload_bytes(payload: &FactsPayload) -> usize {
    serialize_facts_payload(payload).len()
}

/// Select a size-capped batch of queue entries preserving prefix closure.
pub fn select_capped_facts_batch(input: &CappedFactsBatchInput) -> CappedFactsBatch {
    select_capped_facts_batch_with(
        &input.entries,
        &input.envelope,
        &input.now,
        input.max_bytes,
        input.starvation_ms,
        |entry| Some(entry.clone()),
    )
}

/// [`select_capped_facts_batch`] over payload-free headers: `load` returns the full entry for a
/// header (or `None` to drop it) and is called only for candidates the selection inspects, so
/// only the selected batch and oversize candidates stay resident.
pub fn select_capped_facts_batch_with<H>(
    headers: &[H],
    envelope: &FactsPayloadEnvelope,
    now: &str,
    max_bytes: Option<usize>,
    starvation_ms: Option<i64>,
    mut load: impl FnMut(&H) -> Option<FactsQueueEntry>,
) -> CappedFactsBatch
where
    H: AsRef<FactsQueueEntry>,
{
    let max_bytes = max_bytes.unwrap_or(MAX_FACTS_PAYLOAD_BYTES);
    let starvation_ms = starvation_ms.unwrap_or(FACTS_STARVATION_MS);
    let envelope_bytes = measure_entries_bytes(envelope, &[]);
    if envelope_bytes > max_bytes {
        return CappedFactsBatch {
            selected: Vec::new(),
            oversized: Vec::new(),
            envelope_oversized: true,
        };
    }

    let groups = group_by_conversation(headers);
    let now_millis = parse_rfc3339(now).unwrap_or(0);
    let ordered_buckets = order_conversations(groups, now_millis, starvation_ms);

    let mut oversized = Vec::new();
    let mut selected: Vec<FactsQueueEntry> = Vec::new();
    let mut selected_bytes = envelope_bytes;

    for bucket in ordered_buckets {
        for candidate in bucket {
            let Some(full) = load(candidate) else {
                break;
            };
            let single_bytes = measure_entries_bytes(envelope, &[&full]);
            if single_bytes > max_bytes {
                // PERF-DIVERGENCE(senpi facts payload cap): upstream retains full oversized entries in memory until classification; we drop entries immediately and retain only headers with measured byte counts. Parking and failure semantics are identical.
                let mut header = full;
                header.entries.clear();
                oversized.push(OversizedFactsEntry {
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

    CappedFactsBatch {
        selected: ordered_selected,
        oversized,
        envelope_oversized: false,
    }
}

/// Cap sizing uses the same serializer as the on-disk payload writer.
fn measure_entries_bytes(envelope: &FactsPayloadEnvelope, entries: &[&FactsQueueEntry]) -> usize {
    measure_facts_payload_bytes(
        &envelope.to_payload(entries.iter().map(|entry| (**entry).clone()).collect()),
    )
}

/// Payload size after appending one entry, given its single-entry payload size.
///
/// Pretty-printed array elements render identically at any position: the first entry replaces
/// `[]` with `[\n    <entry>\n  ]`, and each later entry adds `,\n    <entry>`, two bytes fewer.
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

fn group_by_conversation<'a, T: AsRef<FactsQueueEntry>>(
    entries: impl IntoIterator<Item = &'a T>,
) -> Vec<(String, Vec<&'a T>)> {
    let mut order = Vec::new();
    let mut map: std::collections::HashMap<String, Vec<&'a T>> = std::collections::HashMap::new();

    for entry in entries {
        let conv_id = entry.as_ref().conversation_id.clone();
        if !map.contains_key(&conv_id) {
            order.push(conv_id.clone());
        }
        map.entry(conv_id).or_default().push(entry);
    }

    let mut result = Vec::with_capacity(order.len());
    for conv_id in order {
        if let Some(mut bucket) = map.remove(&conv_id) {
            bucket.sort_by_key(|e| e.as_ref().range.end_snapshot_line);
            result.push((conv_id, bucket));
        }
    }
    result
}

fn waited_ms(entry: &FactsQueueEntry, now_millis: i64) -> i64 {
    match parse_rfc3339(&entry.enqueued_at) {
        Some(enqueued_millis) => now_millis - enqueued_millis,
        None => 0,
    }
}

fn order_conversations<T: AsRef<FactsQueueEntry>>(
    groups: Vec<(String, Vec<&T>)>,
    now_millis: i64,
    starvation_ms: i64,
) -> Vec<Vec<&T>> {
    let mut buckets: Vec<Vec<&T>> = groups.into_iter().map(|(_, b)| b).collect();
    buckets.sort_by(|left, right| {
        let left_wait = left
            .first()
            .map(|e| waited_ms(e.as_ref(), now_millis))
            .unwrap_or(0);
        let right_wait = right
            .first()
            .map(|e| waited_ms(e.as_ref(), now_millis))
            .unwrap_or(0);
        let left_starved = left_wait >= starvation_ms;
        let right_starved = right_wait >= starvation_ms;

        if left_starved != right_starved {
            if left_starved {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            }
        } else if left_starved {
            right_wait.cmp(&left_wait)
        } else {
            left_wait.cmp(&right_wait)
        }
    });
    buckets
}

#[cfg(test)]
#[path = "payload_cap_tests.rs"]
mod tests;
