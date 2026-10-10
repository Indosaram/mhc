//! Append-only, human-readable record of what memory maintenance did: one JSON line per lifecycle event
//! in `<runtime>/receipts.jsonl`, outside the memory git repo. Appends serialise under the `receipts`
//! lock so concurrent processes never interleave a line; the file is never rotated or truncated.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fs::resilient::{create_dir_all, read_to_string};
use crate::locks::{
    acquire_lock, create_lock_record, receipts_lock_path, release_lock, AcquireLockOptions,
    CreateLockRecordOptions,
};
use crate::support::host::hostname;
use crate::support::time::now_iso;

pub type MemoryReceiptKind = String;
pub type MemoryReceiptEvent = String;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MemoryReceiptInput {
    Run {
        kind: String, // "reflection" | "dream"
        #[serde(rename = "runId")]
        run_id: String,
        trigger: String,
        event: String,
        generation: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        sha: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Facts {
        kind: String, // "facts"
        #[serde(rename = "batchId")]
        batch_id: String,
        trigger: String,
        event: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        sha: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

impl MemoryReceiptInput {
    pub fn kind(&self) -> &str {
        match self {
            Self::Run { kind, .. } => kind,
            Self::Facts { kind, .. } => kind,
        }
    }

    pub fn event(&self) -> &str {
        match self {
            Self::Run { event, .. } => event,
            Self::Facts { event, .. } => event,
        }
    }

    pub fn sha(&self) -> Option<&str> {
        match self {
            Self::Run { sha, .. } => sha.as_deref(),
            Self::Facts { sha, .. } => sha.as_deref(),
        }
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Run { reason, .. } => reason.as_deref(),
            Self::Facts { reason, .. } => reason.as_deref(),
        }
    }

    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::Run { detail, .. } => detail.as_deref(),
            Self::Facts { detail, .. } => detail.as_deref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryReceipt {
    pub v: u32,
    pub at: String,
    pub host: String,
    pub pid: u32,
    #[serde(flatten)]
    pub input: MemoryReceiptInput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryReceiptIdentityInput<'a> {
    Run {
        kind: &'a str,
        run_id: &'a str,
        event: &'a str,
        generation: &'a str,
    },
    Facts {
        kind: &'a str,
        batch_id: &'a str,
        event: &'a str,
    },
}

const RECEIPTS_FILE: &str = "receipts.jsonl";
const DETAIL_LIMIT: usize = 400;
const LOCK_WAIT_MS: u64 = 30_000;

pub fn receipts_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(RECEIPTS_FILE)
}

/// The key two writers of the same receipt (a live emitter and a later backfill) both compute.
pub fn receipt_identity(receipt: &MemoryReceiptIdentityInput<'_>) -> String {
    match receipt {
        MemoryReceiptIdentityInput::Facts {
            kind,
            batch_id,
            event,
        } => serde_json::to_string(&[kind, batch_id, event]).unwrap_or_default(),
        MemoryReceiptIdentityInput::Run {
            kind,
            run_id,
            event,
            generation,
        } => serde_json::to_string(&[kind, run_id, event, generation]).unwrap_or_default(),
    }
}

fn receipt_identity_from_input(input: &MemoryReceiptInput) -> String {
    match input {
        MemoryReceiptInput::Facts {
            kind,
            batch_id,
            event,
            ..
        } => receipt_identity(&MemoryReceiptIdentityInput::Facts {
            kind,
            batch_id,
            event,
        }),
        MemoryReceiptInput::Run {
            kind,
            run_id,
            event,
            generation,
            ..
        } => receipt_identity(&MemoryReceiptIdentityInput::Run {
            kind,
            run_id,
            event,
            generation,
        }),
    }
}

fn receipt_identity_from_receipt(receipt: &MemoryReceipt) -> String {
    receipt_identity_from_input(&receipt.input)
}

pub fn append_memory_receipt(runtime_dir: &Path, input: &MemoryReceiptInput) -> Result<(), String> {
    under_receipts_lock(runtime_dir, || {
        let receipt = to_receipt(input);
        write_line(runtime_dir, &receipt)
    })
}

/// Appends unless a receipt with the same identity is already recorded. The check and the append run under
/// one lock acquisition, so racing writers across processes produce exactly one line. Returns whether a
/// line was written.
pub fn append_memory_receipt_once(
    runtime_dir: &Path,
    input: &MemoryReceiptInput,
) -> Result<bool, String> {
    under_receipts_lock(runtime_dir, || {
        let identity = receipt_identity_from_input(input);
        let text = read_text(runtime_dir)?;
        let parsed = parse_receipts(&text);
        if parsed
            .receipts
            .iter()
            .any(|r| receipt_identity_from_receipt(r) == identity)
        {
            return Ok(false);
        }
        let receipt = to_receipt(input);
        write_line(runtime_dir, &receipt)?;
        Ok(true)
    })
}

#[derive(Debug, Clone, Default)]
pub struct ReadMemoryReceiptsOptions<'a> {
    pub kind: Option<&'a str>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryReceiptsRead {
    /// Newest first.
    pub receipts: Vec<MemoryReceipt>,
    pub skipped_partial_lines: usize,
}

pub fn read_memory_receipts(
    runtime_dir: &Path,
    options: &ReadMemoryReceiptsOptions<'_>,
) -> Result<MemoryReceiptsRead, String> {
    let text = read_text(runtime_dir)?;
    let parsed = parse_receipts(&text);
    let mut matching: Vec<MemoryReceipt> = parsed
        .receipts
        .into_iter()
        .filter(|receipt| {
            options
                .kind
                .map(|k| receipt.input.kind() == k)
                .unwrap_or(true)
        })
        .collect();
    matching.reverse();
    if let Some(limit) = options.limit
        && matching.len() > limit
    {
        matching.truncate(limit);
    }
    Ok(MemoryReceiptsRead {
        receipts: matching,
        skipped_partial_lines: parsed.skipped_partial_lines,
    })
}

fn under_receipts_lock<T, F>(runtime_dir: &Path, operation: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String>,
{
    let locks = runtime_dir.join("locks");
    create_dir_all(&locks).map_err(|e| e.to_string())?;
    let lock_path = receipts_lock_path(&locks);
    let record = create_lock_record("receipts", CreateLockRecordOptions::default())
        .map_err(|e| e.to_string())?;
    let options = AcquireLockOptions {
        wait_timeout_ms: Some(LOCK_WAIT_MS),
        ..Default::default()
    };

    acquire_lock(&lock_path, &record, &options).map_err(|e| e.to_string())?;

    struct LockGuard<'a> {
        path: &'a Path,
        nonce: &'a str,
    }
    impl Drop for LockGuard<'_> {
        fn drop(&mut self) {
            let _ = release_lock(self.path, self.nonce);
        }
    }
    let _guard = LockGuard {
        path: &lock_path,
        nonce: &record.nonce,
    };

    operation()
}

fn to_receipt(input: &MemoryReceiptInput) -> MemoryReceipt {
    let bound_input = match input {
        MemoryReceiptInput::Run {
            kind,
            run_id,
            trigger,
            event,
            generation,
            sha,
            reason,
            detail,
        } => MemoryReceiptInput::Run {
            kind: kind.clone(),
            run_id: run_id.clone(),
            trigger: trigger.clone(),
            event: event.clone(),
            generation: generation.clone(),
            sha: sha.clone(),
            reason: reason.as_deref().map(bound_detail),
            detail: detail.as_deref().map(bound_detail),
        },
        MemoryReceiptInput::Facts {
            kind,
            batch_id,
            trigger,
            event,
            sha,
            reason,
            detail,
        } => MemoryReceiptInput::Facts {
            kind: kind.clone(),
            batch_id: batch_id.clone(),
            trigger: trigger.clone(),
            event: event.clone(),
            sha: sha.clone(),
            reason: reason.as_deref().map(bound_detail),
            detail: detail.as_deref().map(bound_detail),
        },
    };

    MemoryReceipt {
        v: 1,
        at: now_iso(),
        host: hostname(),
        pid: std::process::id(),
        input: bound_input,
    }
}

fn bound_detail(detail: &str) -> String {
    let masked = crate::sync::redact_url(detail);
    if masked.len() <= DETAIL_LIMIT {
        masked
    } else {
        format!("{}...", &masked[..DETAIL_LIMIT - 3])
    }
}

fn write_line(runtime_dir: &Path, receipt: &MemoryReceipt) -> Result<(), String> {
    let path = receipts_path(runtime_dir);
    let mut json = serde_json::to_string(receipt).map_err(|e| e.to_string())?;
    json.push('\n');

    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|e| e.to_string())?;
    file.write_all(json.as_bytes()).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    Ok(())
}

fn read_text(runtime_dir: &Path) -> Result<String, String> {
    let path = receipts_path(runtime_dir);
    match read_to_string(&path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.to_string()),
    }
}

struct ParsedReceipts {
    receipts: Vec<MemoryReceipt>,
    skipped_partial_lines: usize,
}

fn parse_receipts(text: &str) -> ParsedReceipts {
    let mut receipts = Vec::new();
    let mut skipped_partial_lines = 0;
    for line in text.split('\n') {
        if line.is_empty() {
            continue;
        }
        match parse_receipt(line) {
            Some(receipt) => receipts.push(receipt),
            None => skipped_partial_lines += 1,
        }
    }
    ParsedReceipts {
        receipts,
        skipped_partial_lines,
    }
}

fn parse_receipt(line: &str) -> Option<MemoryReceipt> {
    let val: serde_json::Value = serde_json::from_str(line).ok()?;
    let obj = val.as_object()?;

    let v = obj.get("v")?.as_u64()?;
    if v != 1 {
        return None;
    }
    let at = obj.get("at")?.as_str()?;
    if at.is_empty() {
        return None;
    }
    let event = obj.get("event")?.as_str()?;
    if event.is_empty() {
        return None;
    }
    let host = obj.get("host")?.as_str()?.to_string();
    let pid = obj.get("pid")?.as_u64()? as u32;

    let kind = obj.get("kind")?.as_str()?;
    let input = if kind == "facts" {
        let batch_id = obj.get("batchId")?.as_str()?;
        let trigger = obj.get("trigger").and_then(|v| v.as_str()).unwrap_or("settle").to_string();
        let sha = obj.get("sha").and_then(|v| v.as_str()).map(str::to_string);
        let reason = obj.get("reason").and_then(|v| v.as_str()).map(str::to_string);
        let detail = obj.get("detail").and_then(|v| v.as_str()).map(str::to_string);
        MemoryReceiptInput::Facts {
            kind: kind.to_string(),
            batch_id: batch_id.to_string(),
            trigger,
            event: event.to_string(),
            sha,
            reason,
            detail,
        }
    } else if kind == "reflection" || kind == "dream" {
        let run_id = obj.get("runId")?.as_str()?;
        let generation = obj.get("generation")?.as_str()?;
        let trigger = obj.get("trigger").and_then(|v| v.as_str()).unwrap_or("manual").to_string();
        let sha = obj.get("sha").and_then(|v| v.as_str()).map(str::to_string);
        let reason = obj.get("reason").and_then(|v| v.as_str()).map(str::to_string);
        let detail = obj.get("detail").and_then(|v| v.as_str()).map(str::to_string);
        MemoryReceiptInput::Run {
            kind: kind.to_string(),
            run_id: run_id.to_string(),
            trigger,
            event: event.to_string(),
            generation: generation.to_string(),
            sha,
            reason,
            detail,
        }
    } else {
        return None;
    };

    Some(MemoryReceipt {
        v: 1,
        at: at.to_string(),
        host,
        pid,
        input,
    })
}
