//! Failure-streak backoff arithmetic, parking decisions, and failure clearing.

use std::collections::{HashMap, HashSet};

pub use super::failures_schema::{
    FactsFailureReason, FactsFailureRecord, FactsFailureState, sort_failure_records,
};
use crate::support::time::{format_rfc3339_millis, parse_rfc3339};

const BACKOFF_MS: [i64; 4] = [60_000, 5 * 60_000, 30 * 60_000, 120 * 60_000];

const PARK_AT_STREAK: u64 = 5;

const DETAIL_MAX_BYTES: usize = 512;

/// Queue endpoint identifying a failure target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactsFailureTarget {
    pub conversation_id: String,
    pub end_message_id: String,
    pub end_snapshot_line: u64,
}

/// Parameters for applying a new failure occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyFailureInput {
    pub entries: Vec<FactsFailureRecord>,
    pub targets: Vec<FactsFailureTarget>,
    pub failure_id: String,
    pub reason: FactsFailureReason,
    pub detail: Option<String>,
    pub now: String,
}

/// Filter criteria for clearing failure records.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FactsFailureFilter {
    pub conversation_id: Option<String>,
    pub end_message_id: Option<String>,
}

/// Apply a failure to target records, incrementing streak or parking as needed.
pub fn apply_failure(input: &ApplyFailureInput) -> Vec<FactsFailureRecord> {
    let detail = input.detail.as_deref().and_then(sanitize_detail);
    let mut by_key: HashMap<String, FactsFailureRecord> = input
        .entries
        .iter()
        .map(|rec| (record_key(rec), rec.clone()))
        .collect();

    for target in &input.targets {
        let t_key = target_key(target);
        if let Some(prev) = by_key.get(&t_key)
            && prev.last_failure_id == input.failure_id
        {
            continue;
        }
        let previous = by_key.get(&t_key).cloned();
        let next = next_record(previous.as_ref(), target, input, detail.as_deref());
        by_key.insert(t_key, next);
    }

    sort_failure_records(by_key.into_values().collect())
}

/// Clear failure records for endpoints that completed successfully.
pub fn clear_on_success(
    entries: &[FactsFailureRecord],
    targets: &[FactsFailureTarget],
) -> Vec<FactsFailureRecord> {
    let cleared: HashSet<(&str, &str)> = targets
        .iter()
        .map(|t| (t.conversation_id.as_str(), t.end_message_id.as_str()))
        .collect();
    entries
        .iter()
        .filter(|r| !cleared.contains(&(r.conversation_id.as_str(), r.end_message_id.as_str())))
        .cloned()
        .collect()
}

/// Clear failure records matching an optional conversation and message filter.
pub fn clear_for_retry(
    entries: &[FactsFailureRecord],
    filter: &FactsFailureFilter,
) -> Vec<FactsFailureRecord> {
    entries
        .iter()
        .filter(|record| {
            if let Some(cid) = &filter.conversation_id
                && &record.conversation_id != cid
            {
                return true;
            }
            if let Some(mid) = &filter.end_message_id
                && &record.end_message_id != mid
            {
                return true;
            }
            false
        })
        .cloned()
        .collect()
}

fn key(conversation_id: &str, end_message_id: &str) -> String {
    format!("{conversation_id}\0{end_message_id}")
}

fn record_key(record: &FactsFailureRecord) -> String {
    key(&record.conversation_id, &record.end_message_id)
}

fn target_key(target: &FactsFailureTarget) -> String {
    key(&target.conversation_id, &target.end_message_id)
}

fn sanitize_detail(detail: &str) -> Option<String> {
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
        return value.to_string();
    }
    let mut bytes = 0;
    let mut end = 0;
    for ch in value.chars() {
        let ch_len = ch.len_utf8();
        if bytes + ch_len > max_bytes {
            break;
        }
        bytes += ch_len;
        end += ch_len;
    }
    value[..end].to_string()
}

fn delay_for(streak: u64) -> i64 {
    let index = (streak.clamp(1, BACKOFF_MS.len() as u64) - 1) as usize;
    BACKOFF_MS[index]
}

fn next_record(
    previous: Option<&FactsFailureRecord>,
    target: &FactsFailureTarget,
    input: &ApplyFailureInput,
    detail: Option<&str>,
) -> FactsFailureRecord {
    let at = input.now.clone();
    let streak = previous.map(|p| p.streak).unwrap_or(0) + 1;
    let was_parked = previous
        .map(|p| p.state == FactsFailureState::Parked)
        .unwrap_or(false);
    let parks = was_parked
        || input.reason == FactsFailureReason::PayloadEntryOversize
        || streak >= PARK_AT_STREAK;

    let now_millis = parse_rfc3339(&input.now).unwrap_or(0);
    let first_failure_at = previous
        .map(|p| p.first_failure_at.clone())
        .unwrap_or_else(|| at.clone());
    let next_eligible_at = if parks {
        None
    } else {
        Some(format_rfc3339_millis(now_millis + delay_for(streak)))
    };
    let parked_at = if parks {
        Some(
            previous
                .and_then(|p| p.parked_at.clone())
                .unwrap_or_else(|| at.clone()),
        )
    } else {
        None
    };

    FactsFailureRecord {
        conversation_id: target.conversation_id.clone(),
        end_message_id: target.end_message_id.clone(),
        end_snapshot_line: target.end_snapshot_line,
        state: if parks {
            FactsFailureState::Parked
        } else {
            FactsFailureState::Backoff
        },
        streak,
        first_failure_at,
        last_failure_at: at,
        last_reason: input.reason,
        last_detail: detail.map(|s| s.to_string()),
        last_failure_id: input.failure_id.clone(),
        next_eligible_at,
        parked_at,
    }
}

#[cfg(test)]
#[path = "failures_backoff_tests.rs"]
mod tests;
