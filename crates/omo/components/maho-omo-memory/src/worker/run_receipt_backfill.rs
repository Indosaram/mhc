//! Turns a run directory's terminal artifacts back into its receipt when the live write was lost,
//! or the run finished before receipts existed.
//! Port of `components/memory/worker/run-receipt-backfill.ts` at adb672532.

use std::collections::BTreeMap;
use std::path::Path;

use memory_core::receipts::MemoryReceiptInput;
use memory_core::reflection::ReflectionOutcome;

use crate::receipts_port::{
    emit_memory_receipt, run_outcome_receipt, run_receipt, MemoryReceiptsPort, ReceiptExtra,
    RunReceiptLedger,
};
use crate::worker::reservation_run_ledger::parse_reservation_run_ledger;
use crate::worker::run_artifacts::read_run_json;

type ReceiptWarning<'a> = dyn Fn(&str, &BTreeMap<String, String>) + 'a;

pub async fn backfill_run_receipt(
    runtime_dir: &Path,
    run_dir: &Path,
    port: Option<&dyn MemoryReceiptsPort>,
    warn: Option<&ReceiptWarning<'_>>,
) {
    if let Some(receipt) = terminal_receipt(run_dir) {
        emit_memory_receipt(runtime_dir, &receipt, port, warn);
    }
}

pub fn backfill_run_receipt_sync(
    runtime_dir: &Path,
    run_dir: &Path,
    port: Option<&dyn MemoryReceiptsPort>,
    warn: Option<&ReceiptWarning<'_>>,
) {
    if let Some(receipt) = terminal_receipt(run_dir) {
        emit_memory_receipt(runtime_dir, &receipt, port, warn);
    }
}

fn terminal_receipt(run_dir: &Path) -> Option<MemoryReceiptInput> {
    let ledger = read_ledger(run_dir);
    let final_path = run_dir.join("final.json");
    if final_path.exists() {
        let final_val = read_sentinel(&final_path)?;
        let outcome_str = final_val.get("outcome")?.as_str()?;
        let outcome = parse_outcome(outcome_str)?;
        let ledger = ledger?;
        let settled = if let Some(generation_str) = final_val.get("generation").and_then(|v| v.as_str()) {
            RunReceiptLedger {
                started_at: generation_str.to_string(),
                ..ledger
            }
        } else {
            ledger
        };
        let sha = final_val.get("integrationSha").and_then(|v| v.as_str()).map(str::to_string);
        let reason = final_val.get("reason").and_then(|v| v.as_str()).map(str::to_string);
        return Some(run_outcome_receipt(
            &settled,
            outcome,
            ReceiptExtra {
                sha,
                reason,
                detail: None,
            },
        ));
    }

    for (file, event) in [("quarantined.json", "quarantined"), ("abandoned.json", "abandoned")] {
        let path = run_dir.join(file);
        if !path.exists() {
            continue;
        }
        let sentinel = read_sentinel(&path);
        let source = sentinel.as_ref().and_then(sentinel_ledger).or(ledger.clone())?;
        let reason = sentinel
            .as_ref()
            .and_then(|s| s.get("reason").or_else(|| s.get("outcome")))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        return Some(run_receipt(
            &source,
            event,
            ReceiptExtra {
                sha: None,
                reason,
                detail: None,
            },
        ));
    }

    None
}

fn sentinel_ledger(sentinel: &serde_json::Value) -> Option<RunReceiptLedger> {
    let obj = sentinel.as_object()?;
    let run_id = obj.get("runId")?.as_str()?;
    let generation = obj.get("generation")?.as_str()?;
    let kind = obj.get("kind")?.as_str()?;
    if kind != "reflection" && kind != "dream" {
        return None;
    }
    let trigger = obj.get("trigger")?.as_str()?;
    let origin = obj.get("origin").and_then(|v| v.as_str()).map(str::to_string);

    Some(RunReceiptLedger {
        kind: kind.to_string(),
        run_id: run_id.to_string(),
        trigger: trigger.to_string(),
        origin,
        started_at: generation.to_string(),
    })
}

fn read_ledger(run_dir: &Path) -> Option<RunReceiptLedger> {
    let path = run_dir.join("ledger.json");
    if !path.exists() {
        return None;
    }
    let val: serde_json::Value = read_run_json(&path).ok()?;
    let ledger = parse_reservation_run_ledger(val).ok()?;
    let v = ledger.value();
    Some(RunReceiptLedger {
        kind: v.get("kind")?.as_str()?.to_string(),
        run_id: v.get("runId")?.as_str()?.to_string(),
        trigger: v.get("trigger")?.as_str()?.to_string(),
        origin: v.get("origin").and_then(|x| x.as_str()).map(str::to_string),
        started_at: v.get("startedAt")?.as_str()?.to_string(),
    })
}

fn read_sentinel(path: &Path) -> Option<serde_json::Value> {
    read_run_json(path).ok()
}

fn parse_outcome(s: &str) -> Option<ReflectionOutcome> {
    match s {
        "merged" => Some(ReflectionOutcome::Merged),
        "no_changes" => Some(ReflectionOutcome::NoChanges),
        "parent_dirty" => Some(ReflectionOutcome::ParentDirty),
        "dirty_uncommitted" => Some(ReflectionOutcome::DirtyUncommitted),
        "merge_conflict" => Some(ReflectionOutcome::MergeConflict),
        "timed_out" => Some(ReflectionOutcome::TimedOut),
        "failed" | "admin_tamper" => Some(ReflectionOutcome::Failed),
        _ => None,
    }
}
