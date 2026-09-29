//! Launch gating logic evaluating queue entries against the failure ledger.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use super::failures_schema::{FactsFailureRecord, FactsFailureState, FactsFailuresFile};
use super::schema::FactsQueueEntry;
use crate::support::time::parse_rfc3339;

/// Reason why a queue entry was excluded from the launch set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FactsSkipReason {
    #[serde(rename = "parked")]
    Parked,
    #[serde(rename = "backoff")]
    Backoff,
    #[serde(rename = "blocked-by-predecessor")]
    BlockedByPredecessor,
}

impl FactsSkipReason {
    /// Return the wire string representation of this skip reason.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Parked => "parked",
            Self::Backoff => "backoff",
            Self::BlockedByPredecessor => "blocked-by-predecessor",
        }
    }
}

/// Result of evaluating candidate entries against the failure ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactsLaunchSelection {
    pub selected: Vec<FactsQueueEntry>,
    pub skipped: BTreeMap<String, FactsSkipReason>,
}

/// Pair key matching the queue deduplication and failure record key.
pub fn facts_selection_key(conversation_id: &str, end_message_id: &str) -> String {
    format!("{conversation_id}\0{end_message_id}")
}

/// Select launchable queue entries, gating by backoff, parking, and prefix closure.
pub fn select_launchable(
    entries: &[FactsQueueEntry],
    failures: Option<&FactsFailuresFile>,
    now: &str,
) -> FactsLaunchSelection {
    let now_millis = parse_rfc3339(now).unwrap_or(0);
    let records: HashMap<String, &FactsFailureRecord> = failures
        .map(|f| f.entries.iter().map(|rec| (record_key(rec), rec)).collect())
        .unwrap_or_default();

    if records.is_empty() {
        return FactsLaunchSelection {
            selected: entries.to_vec(),
            skipped: BTreeMap::new(),
        };
    }

    let mut by_conversation: HashMap<String, Vec<&FactsQueueEntry>> = HashMap::new();
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
            let key = entry_key(entry);
            let reason = if blocked {
                Some(FactsSkipReason::BlockedByPredecessor)
            } else {
                drop_reason(records.get(&key).copied(), now_millis)
            };
            if let Some(reason) = reason {
                skipped.insert(key, reason);
                blocked = true;
            }
        }
    }

    let selected = entries
        .iter()
        .filter(|entry| !skipped.contains_key(&entry_key(entry)))
        .cloned()
        .collect();

    FactsLaunchSelection { selected, skipped }
}

fn entry_key(entry: &FactsQueueEntry) -> String {
    facts_selection_key(&entry.conversation_id, &entry.range.end_message_id)
}

fn record_key(record: &FactsFailureRecord) -> String {
    facts_selection_key(&record.conversation_id, &record.end_message_id)
}

fn drop_reason(record: Option<&FactsFailureRecord>, now_millis: i64) -> Option<FactsSkipReason> {
    let record = record?;
    if record.state == FactsFailureState::Parked {
        return Some(FactsSkipReason::Parked);
    }
    let Some(next_eligible) = &record.next_eligible_at else {
        return Some(FactsSkipReason::Backoff);
    };
    let next_millis = parse_rfc3339(next_eligible).unwrap_or(0);
    if now_millis < next_millis {
        Some(FactsSkipReason::Backoff)
    } else {
        None
    }
}

#[cfg(test)]
#[path = "failures_selection_tests.rs"]
mod tests;
