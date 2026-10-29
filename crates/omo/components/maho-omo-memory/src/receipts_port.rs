//! Memory receipts port: writes one receipt after the durable artifact it describes.
//! Port of `components/memory/receipts-port.ts` at adb672532.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use memory_core::receipts::{
    append_memory_receipt_once, MemoryReceiptInput,
};
use memory_core::reflection::ReflectionOutcome;

pub trait MemoryReceiptsPort: Send + Sync {
    fn append(&self, runtime_dir: &Path, input: &MemoryReceiptInput) -> Result<bool, String>;
}

#[derive(Clone, Default)]
pub struct DefaultReceiptsPort;

impl MemoryReceiptsPort for DefaultReceiptsPort {
    fn append(&self, runtime_dir: &Path, input: &MemoryReceiptInput) -> Result<bool, String> {
        append_memory_receipt_once(runtime_dir, input)
    }
}

pub type ReceiptWarn = Arc<dyn Fn(&str, &BTreeMap<String, String>) + Send + Sync>;
type ReceiptWarning<'a> = dyn Fn(&str, &BTreeMap<String, String>) + 'a;

#[derive(Clone, Debug)]
pub struct RunReceiptLedger {
    pub kind: String, // "reflection" | "dream"
    pub run_id: String,
    pub trigger: String,
    pub origin: Option<String>,
    pub started_at: String,
}

#[derive(Clone, Debug, Default)]
pub struct ReceiptExtra {
    pub sha: Option<String>,
    pub reason: Option<String>,
    pub detail: Option<String>,
}

/// Writes one receipt after the durable artifact it describes. The artifact is the source of truth and
/// the receipt is the readable view, so a failed write is reported and never fails the run.
pub fn emit_memory_receipt(
    runtime_dir: &Path,
    input: &MemoryReceiptInput,
    port: Option<&dyn MemoryReceiptsPort>,
    warn: Option<&ReceiptWarning<'_>>,
) {
    let default_port = DefaultReceiptsPort;
    let effective_port = port.unwrap_or(&default_port);
    if let Err(error) = effective_port.append(runtime_dir, input)
        && let Some(warn) = warn {
            let mut fields = BTreeMap::new();
            fields.insert("kind".to_string(), input.kind().to_string());
            fields.insert("event".to_string(), input.event().to_string());
            fields.insert("error".to_string(), error);
            warn(
                "memory receipt write failed; the run's own artifact still records its outcome",
                &fields,
            );
    }
}

/// Generation is `ledger.started_at`, the one value both a live emitter and a later backfill can read.
pub fn run_receipt(
    ledger: &RunReceiptLedger,
    event: &str,
    extra: ReceiptExtra,
) -> MemoryReceiptInput {
    MemoryReceiptInput::Run {
        kind: ledger.kind.clone(),
        run_id: ledger.run_id.clone(),
        trigger: ledger.origin.clone().unwrap_or_else(|| ledger.trigger.clone()),
        event: event.to_string(),
        generation: ledger.started_at.clone(),
        sha: extra.sha,
        reason: extra.reason,
        detail: extra.detail,
    }
}

pub fn run_outcome_receipt(
    ledger: &RunReceiptLedger,
    outcome: ReflectionOutcome,
    extra: ReceiptExtra,
) -> MemoryReceiptInput {
    match outcome {
        ReflectionOutcome::Merged => run_receipt(
            ledger,
            "merged",
            ReceiptExtra {
                sha: extra.sha,
                reason: None,
                detail: extra.detail,
            },
        ),
        ReflectionOutcome::NoChanges => run_receipt(
            ledger,
            "no_changes",
            ReceiptExtra {
                sha: None,
                reason: None,
                detail: extra.detail,
            },
        ),
        _ => {
            let outcome_str = match outcome {
                ReflectionOutcome::ParentDirty => "parent_dirty",
                ReflectionOutcome::DirtyUncommitted => "dirty_uncommitted",
                ReflectionOutcome::MergeConflict => "merge_conflict",
                ReflectionOutcome::TimedOut => "timed_out",
                ReflectionOutcome::Failed => "failed",
                _ => "failed",
            };
            run_receipt(
                ledger,
                "failed",
                ReceiptExtra {
                    sha: None,
                    reason: Some(extra.reason.unwrap_or_else(|| outcome_str.to_string())),
                    detail: extra.detail,
                },
            )
        }
    }
}

pub fn facts_receipt(batch_id: &str, event: &str, extra: ReceiptExtra) -> MemoryReceiptInput {
    MemoryReceiptInput::Facts {
        kind: "facts".to_string(),
        batch_id: batch_id.to_string(),
        trigger: "settle".to_string(),
        event: event.to_string(),
        sha: extra.sha,
        reason: extra.reason,
        detail: extra.detail,
    }
}
