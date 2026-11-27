//! Plain-words classification of facts failures for `/doctor` and `/facts` (`commands/facts-failure-cause.ts`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use memory_core::facts::failures_schema::{
    FactsFailureReason, FactsFailureRecord, FactsFailureState,
};
use memory_core::support::time::parse_rfc3339;
use serde_json::Value;

use crate::worker::failure_detail::child_failure_cause;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactsFailureAction {
    Retry,
    FixFirst,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactsFailureCause {
    pub label: Option<String>,
    pub action: FactsFailureAction,
    pub setting: Option<&'static str>,
}

fn classify_facts_failure(record: &FactsFailureRecord) -> FactsFailureCause {
    match record.last_reason {
        FactsFailureReason::Other => FactsFailureCause {
            label: None,
            action: FactsFailureAction::Retry,
            setting: None,
        },
        FactsFailureReason::ChildExit => FactsFailureCause {
            label: Some(
                child_failure_cause(record.last_detail.as_deref())
                    .unwrap_or_else(|| "the facts child exited".to_owned()),
            ),
            action: FactsFailureAction::FixFirst,
            setting: None,
        },
        FactsFailureReason::DeadlineExceeded => FactsFailureCause {
            label: Some("timeout".to_owned()),
            action: FactsFailureAction::Retry,
            setting: None,
        },
        FactsFailureReason::MemoryWriteLockExhausted => FactsFailureCause {
            label: Some("the memory write lock stayed busy".to_owned()),
            action: FactsFailureAction::Retry,
            setting: None,
        },
        FactsFailureReason::UnknownLiveness => FactsFailureCause {
            label: Some("the facts child stopped responding".to_owned()),
            action: FactsFailureAction::Retry,
            setting: None,
        },
        FactsFailureReason::QuickCategoryUnavailable | FactsFailureReason::SandboxUnavailable => {
            FactsFailureCause {
                label: Some("no model is available for facts extraction".to_owned()),
                action: FactsFailureAction::FixFirst,
                setting: Some("memory.facts.model"),
            }
        }
        FactsFailureReason::InvalidExtraction => FactsFailureCause {
            label: Some("parse error".to_owned()),
            action: FactsFailureAction::FixFirst,
            setting: None,
        },
        FactsFailureReason::PayloadEntryOversize => FactsFailureCause {
            label: Some("one entry is too large to extract".to_owned()),
            action: FactsFailureAction::FixFirst,
            setting: None,
        },
        FactsFailureReason::PayloadEnvelopeOversize => FactsFailureCause {
            label: Some("the batch is too large to extract".to_owned()),
            action: FactsFailureAction::FixFirst,
            setting: None,
        },
        FactsFailureReason::SecretLikeContent => FactsFailureCause {
            label: Some("secret-like content in the batch".to_owned()),
            action: FactsFailureAction::FixFirst,
            setting: None,
        },
        FactsFailureReason::ParentDirty => FactsFailureCause {
            label: Some("the memory repository has uncommitted changes".to_owned()),
            action: FactsFailureAction::FixFirst,
            setting: None,
        },
    }
}

fn parked_of(records: &[FactsFailureRecord]) -> Vec<&FactsFailureRecord> {
    records
        .iter()
        .filter(|record| record.state == FactsFailureState::Parked)
        .collect()
}

struct CauseCount {
    cause: FactsFailureCause,
    count: usize,
}

fn cause_counts(records: &[FactsFailureRecord]) -> Vec<CauseCount> {
    let mut by_label: BTreeMap<String, CauseCount> = BTreeMap::new();
    for record in parked_of(records) {
        let cause = classify_facts_failure(record);
        let Some(label) = cause.label.clone() else {
            continue;
        };
        if let Some(entry) = by_label.get_mut(&label) {
            entry.count += 1;
        } else {
            by_label.insert(label, CauseCount { cause, count: 1 });
        }
    }
    let mut entries: Vec<CauseCount> = by_label.into_values().collect();
    entries.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.cause.label.cmp(&right.cause.label))
    });
    entries
}

pub fn dominant_fix_first_cause(records: &[FactsFailureRecord]) -> Option<FactsFailureCause> {
    cause_counts(records)
        .into_iter()
        .find(|entry| entry.cause.action == FactsFailureAction::FixFirst)
        .map(|entry| entry.cause)
}

pub fn parked_run_path(
    records: &[FactsFailureRecord],
    facts_dir: Option<&Path>,
) -> Option<PathBuf> {
    let facts_dir = facts_dir?;
    let mut newest_first = parked_of(records);
    if newest_first.is_empty() {
        return None;
    }
    newest_first.sort_by(|left, right| {
        let l_ts = parse_rfc3339(&left.last_failure_at).unwrap_or(i64::MIN);
        let r_ts = parse_rfc3339(&right.last_failure_at).unwrap_or(i64::MIN);
        r_ts.cmp(&l_ts)
    });

    let runs_dir = facts_dir.join("runs");
    let entries = std::fs::read_dir(&runs_dir).ok()?;
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !name.starts_with(".prune-"))
        .collect();
    names.sort();

    let mut by_batch_id: BTreeMap<String, PathBuf> = BTreeMap::new();
    for name in names {
        let run_dir = runs_dir.join(&name);
        let Ok(raw) = std::fs::read_to_string(run_dir.join("ledger.json")) else {
            continue;
        };
        let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let Some(batch_id) = obj.get("batchId").and_then(Value::as_str) else {
            continue;
        };
        if batch_id.is_empty() {
            continue;
        }
        if let Some(run_id) = obj.get("runId").and_then(Value::as_str)
            && run_id != name
        {
            continue;
        }
        by_batch_id.entry(batch_id.to_owned()).or_insert(run_dir);
    }

    for record in newest_first {
        if let Some(run_dir) = by_batch_id.get(record.last_failure_id.trim())
            && run_dir.exists()
        {
            return Some(run_dir.clone());
        }
    }
    None
}

pub fn facts_parked_cause_summary(
    records: &[FactsFailureRecord],
    facts_dir: Option<&Path>,
) -> Option<String> {
    if parked_of(records).is_empty() {
        return None;
    }
    let mut segments = Vec::new();
    let counts = cause_counts(records);
    if !counts.is_empty() {
        let list = counts
            .iter()
            .filter_map(|entry| {
                entry
                    .cause
                    .label
                    .as_deref()
                    .map(|label| format!("{label} x{}", entry.count))
            })
            .collect::<Vec<_>>()
            .join(", ");
        let prefix = if counts.len() == 1 { "cause" } else { "causes" };
        segments.push(format!("{prefix}: {list}"));
    }
    if let Some(run_path) = parked_run_path(records, facts_dir) {
        segments.push(format!("last run: {}", run_path.display()));
    }
    if segments.is_empty() {
        None
    } else {
        Some(segments.join("; "))
    }
}
