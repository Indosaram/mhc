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
    pub identity: String,
    pub today: String,
    #[serde(rename = "knownPeople")]
    pub known_people: Vec<FactsKnownPerson>,
    #[serde(rename = "primaryHuman")]
    pub primary_human: FactsPrimaryHuman,
}

impl FactsPayloadEnvelope {
    /// Construct a full payload by attaching entries to this envelope.
    pub fn to_payload(&self, entries: Vec<FactsQueueEntry>) -> FactsPayload {
        FactsPayload {
            version: self.version,
            identity: self.identity.clone(),
            today: self.today.clone(),
            known_people: self.known_people.clone(),
            primary_human: self.primary_human.clone(),
            entries,
        }
    }
}

/// Complete extraction payload serialized to disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsPayload {
    pub version: u32,
    pub identity: String,
    pub today: String,
    #[serde(rename = "knownPeople")]
    pub known_people: Vec<FactsKnownPerson>,
    #[serde(rename = "primaryHuman")]
    pub primary_human: FactsPrimaryHuman,
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

/// Selected batch result with oversize tracking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CappedFactsBatch {
    pub selected: Vec<FactsQueueEntry>,
    pub oversized: Vec<FactsQueueEntry>,
    pub envelope_oversized: bool,
}

/// Serialize payload to formatted, newline-terminated JSON text.
pub fn serialize_facts_payload(payload: &FactsPayload) -> String {
    let mut rendered = serde_json::to_string_pretty(payload).unwrap_or_default();
    rendered.push('\n');
    rendered
}

/// Measure byte size of payload when serialized to disk.
pub fn measure_facts_payload_bytes(payload: &FactsPayload) -> usize {
    serialize_facts_payload(payload).len()
}

/// Select a size-capped batch of queue entries preserving prefix closure.
pub fn select_capped_facts_batch(input: &CappedFactsBatchInput) -> CappedFactsBatch {
    let max_bytes = input.max_bytes.unwrap_or(MAX_FACTS_PAYLOAD_BYTES);
    let starvation_ms = input.starvation_ms.unwrap_or(FACTS_STARVATION_MS);
    let envelope_bytes = measure_facts_payload_bytes(&input.envelope.to_payload(Vec::new()));
    if envelope_bytes > max_bytes {
        return CappedFactsBatch {
            selected: Vec::new(),
            oversized: Vec::new(),
            envelope_oversized: true,
        };
    }

    let groups = group_by_conversation(&input.entries);
    let now_millis = parse_rfc3339(&input.now).unwrap_or(0);
    let ordered_buckets = order_conversations(groups, now_millis, starvation_ms);

    let mut oversized = Vec::new();
    let mut selected = Vec::new();

    for bucket in ordered_buckets {
        for candidate in bucket {
            let single = input.envelope.to_payload(vec![candidate.clone()]);
            if measure_facts_payload_bytes(&single) > max_bytes {
                oversized.push(candidate);
                break;
            }
            let mut next = selected.clone();
            next.push(candidate.clone());
            let next_payload = input.envelope.to_payload(next);
            if measure_facts_payload_bytes(&next_payload) > max_bytes {
                break;
            }
            selected.push(candidate);
        }
    }

    let ordered_selected = group_by_conversation(&selected)
        .into_iter()
        .flat_map(|(_, bucket)| bucket)
        .collect();

    CappedFactsBatch {
        selected: ordered_selected,
        oversized,
        envelope_oversized: false,
    }
}

fn group_by_conversation(entries: &[FactsQueueEntry]) -> Vec<(String, Vec<FactsQueueEntry>)> {
    let mut order = Vec::new();
    let mut map: std::collections::HashMap<String, Vec<FactsQueueEntry>> =
        std::collections::HashMap::new();

    for entry in entries {
        let conv_id = entry.conversation_id.clone();
        if !map.contains_key(&conv_id) {
            order.push(conv_id.clone());
        }
        map.entry(conv_id).or_default().push(entry.clone());
    }

    let mut result = Vec::with_capacity(order.len());
    for conv_id in order {
        if let Some(mut bucket) = map.remove(&conv_id) {
            bucket.sort_by_key(|e| e.range.end_snapshot_line);
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

fn order_conversations(
    groups: Vec<(String, Vec<FactsQueueEntry>)>,
    now_millis: i64,
    starvation_ms: i64,
) -> Vec<Vec<FactsQueueEntry>> {
    let mut buckets: Vec<Vec<FactsQueueEntry>> = groups.into_iter().map(|(_, b)| b).collect();
    buckets.sort_by(|left, right| {
        let left_wait = left.first().map(|e| waited_ms(e, now_millis)).unwrap_or(0);
        let right_wait = right.first().map(|e| waited_ms(e, now_millis)).unwrap_or(0);
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
