//! Ledger file and record reading for ulw-loop.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::domain_types::UlwLoopLedgerEntry;
use crate::paths::{ulw_loop_dir, UlwLoopScope};
use crate::plan_log::{read_optional, read_records, reconcile_plan, PlanCommitRecord};

pub fn read_ledger_at(dir: impl AsRef<Path>) -> Vec<UlwLoopLedgerEntry> {
    let records = read_records(&dir);
    let reset = reconcile_plan(&dir)
        .and_then(|p| p.ledger_reset_revision)
        .unwrap_or(0);
    read_ledger_from_records(dir, records, reset)
}

/// `read_ledger_at` over already-read revision records and an already-reconciled reset revision.
pub(crate) fn read_ledger_from_records(
    dir: impl AsRef<Path>,
    records: Vec<PlanCommitRecord>,
    reset: u64,
) -> Vec<UlwLoopLedgerEntry> {
    let mut entries = BTreeMap::<String, UlwLoopLedgerEntry>::new();
    let path = dir.as_ref().join("ledger.jsonl");
    let content = read_optional(path).unwrap_or_default();
    let lines = content.lines();

    let mut reached = 0u64;
    for (index, line) in lines.enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(mut entry) = serde_json::from_str::<UlwLoopLedgerEntry>(trimmed) {
            let rev = entry.revision.unwrap_or(0);
            let effective_rev = if rev > 0 { rev } else { reached };
            entry.revision = Some(effective_rev);
            reached = reached.max(effective_rev);
            let id = entry.id.unwrap_or_else(|| format!("legacy-{}", index + 1));
            entry.id = Some(id.clone());
            entries.insert(id, entry);
        }
    }

    for record in records {
        for (seq, mut entry) in record.ledger.into_iter().enumerate() {
            let id = entry.id.unwrap_or_else(|| format!("{}-{}", record.revision, seq));
            entry.revision = Some(record.revision);
            entry.id = Some(id.clone());
            entries.insert(id, entry);
        }
    }

    let sequence = |entry: &UlwLoopLedgerEntry| -> u64 {
        entry
            .id
            .as_deref()
            .and_then(|id| id.split('-').last())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    };

    let mut result: Vec<_> = entries
        .into_values()
        .filter(|entry| entry.revision.unwrap_or(0) >= reset)
        .collect();

    result.sort_by(|a, b| {
        let rev_cmp = a.revision.unwrap_or(0).cmp(&b.revision.unwrap_or(0));
        if rev_cmp != std::cmp::Ordering::Equal {
            rev_cmp
        } else {
            sequence(a).cmp(&sequence(b))
        }
    });

    result
}

pub fn read_ledger(repo_root: impl AsRef<Path>, scope: Option<&UlwLoopScope>) -> Vec<UlwLoopLedgerEntry> {
    read_ledger_at(ulw_loop_dir(repo_root, scope))
}
