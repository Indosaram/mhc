//! Append-only, human-readable record of what memory maintenance did: one JSON line per lifecycle event
//! in `<runtime>/receipts.jsonl`, outside the memory git repo. Appends serialise under the `receipts`
//! lock so concurrent processes never interleave a line; the file is never rotated or truncated.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fs::resilient::{create_dir_all, read_to_string};
use crate::locks::{
    AcquireLockOptions, CreateLockRecordOptions, acquire_lock, create_lock_record,
    receipts_lock_path, release_lock,
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MemoryReceipt {
    pub v: u32,
    pub at: String,
    pub host: String,
    pub pid: u32,
    #[serde(flatten)]
    pub input: MemoryReceiptInput,
    #[serde(skip)]
    pub raw: Option<serde_json::Value>,
}

impl Serialize for MemoryReceipt {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if let Some(raw) = &self.raw {
            return raw.serialize(serializer);
        }
        let mut value = serde_json::to_value(&self.input).map_err(serde::ser::Error::custom)?;
        value["v"] = self.v.into();
        value["at"] = self.at.clone().into();
        value["host"] = self.host.clone().into();
        value["pid"] = self.pid.into();
        value.serialize(serializer)
    }
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
    // The file is append-only and never truncated, so a receipt seen once stays recorded: answer
    // that case without the lock (and its fsync). Reconcile re-offers every terminal run's receipt
    // on each start and shutdown, so this is the common path.
    let identity = receipt_identity_from_input(input);
    if contains_receipt_identity(&read_text(runtime_dir)?, &identity) {
        return Ok(false);
    }
    under_receipts_lock(runtime_dir, || {
        let text = read_text(runtime_dir)?;
        if contains_receipt_identity(&text, &identity) {
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
        raw: None,
    }
}

fn bound_detail(detail: &str) -> String {
    let masked = crate::sync::redact::redact_secret_like_material(detail);
    if masked.encode_utf16().count() <= DETAIL_LIMIT {
        masked
    } else {
        let prefix = masked
            .encode_utf16()
            .take(DETAIL_LIMIT - 3)
            .collect::<Vec<_>>();
        format!("{}...", String::from_utf16_lossy(&prefix))
    }
}

fn write_line(runtime_dir: &Path, receipt: &MemoryReceipt) -> Result<(), String> {
    write_line_with_permissions(runtime_dir, receipt, |path| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        #[cfg(not(unix))]
        let _ = path;
        Ok(())
    })
}

fn write_line_with_permissions(
    runtime_dir: &Path,
    receipt: &MemoryReceipt,
    set_permissions: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<(), String> {
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

    set_permissions(&path).map_err(|e| e.to_string())?;

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

/// Whether a stored line carries `identity`. Only each line's identity fields are decoded; building
/// every full receipt made each append cost a whole-file JSON-tree parse.
fn contains_receipt_identity(text: &str, identity: &str) -> bool {
    text.split('\n')
        .filter(|line| !line.is_empty())
        .any(|line| receipt_identity_from_line(line).as_deref() == Some(identity))
}

/// The identity of a stored line, under exactly the validity rules of [`parse_receipt`], without
/// materializing the receipt.
fn receipt_identity_from_line(line: &str) -> Option<String> {
    // Only these fields are materialized; every other key is skipped without building a tree.
    #[derive(Deserialize)]
    struct IdentityLine {
        v: Option<serde_json::Value>,
        at: Option<serde_json::Value>,
        event: Option<serde_json::Value>,
        kind: Option<serde_json::Value>,
        #[serde(rename = "batchId")]
        batch_id: Option<serde_json::Value>,
        #[serde(rename = "runId")]
        run_id: Option<serde_json::Value>,
        generation: Option<serde_json::Value>,
    }
    let line: IdentityLine = serde_json::from_str(line).ok()?;
    if line.v?.as_u64()? != 1 {
        return None;
    }
    line.at?.as_str()?;
    let event = line.event?;
    let event = event.as_str()?;
    let kind = line.kind?;
    let kind = kind.as_str()?;
    let identity = if kind == "facts" {
        let batch_id = line.batch_id?;
        receipt_identity(&MemoryReceiptIdentityInput::Facts {
            kind,
            batch_id: batch_id.as_str()?,
            event,
        })
    } else if kind == "reflection" || kind == "dream" {
        let run_id = line.run_id?;
        let generation = line.generation?;
        receipt_identity(&MemoryReceiptIdentityInput::Run {
            kind,
            run_id: run_id.as_str()?,
            event,
            generation: generation.as_str()?,
        })
    } else {
        return None;
    };
    Some(identity)
}

fn parse_receipt(line: &str) -> Option<MemoryReceipt> {
    let val: serde_json::Value = serde_json::from_str(line).ok()?;
    let obj = val.as_object()?;

    let v = obj.get("v")?.as_u64()?;
    if v != 1 {
        return None;
    }
    let at = obj.get("at")?.as_str()?;
    let event = obj.get("event")?.as_str()?;
    let host = obj
        .get("host")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let pid = obj
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or_default();

    let kind = obj.get("kind")?.as_str()?;
    let input = if kind == "facts" {
        let batch_id = obj.get("batchId")?.as_str()?;
        let trigger = obj
            .get("trigger")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let sha = obj.get("sha").and_then(|v| v.as_str()).map(str::to_string);
        let reason = obj
            .get("reason")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let detail = obj
            .get("detail")
            .and_then(|v| v.as_str())
            .map(str::to_string);
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
        let trigger = obj
            .get("trigger")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let sha = obj.get("sha").and_then(|v| v.as_str()).map(str::to_string);
        let reason = obj
            .get("reason")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let detail = obj
            .get("detail")
            .and_then(|v| v.as_str())
            .map(str::to_string);
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
        raw: Some(val),
    })
}

#[cfg(all(test, unix))]
mod permission_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn input() -> MemoryReceiptInput {
        MemoryReceiptInput::Facts {
            kind: "facts".into(),
            batch_id: "permissions".into(),
            trigger: "settle".into(),
            event: "committed".into(),
            sha: None,
            reason: None,
            detail: None,
        }
    }

    #[test]
    fn append_restores_private_permissions_when_receipts_already_exist() {
        // given
        let root = tempfile::tempdir().expect("temporary runtime directory");
        let path = receipts_path(root.path());
        std::fs::write(&path, "").expect("existing receipts file");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666))
            .expect("public receipt permissions");

        // when
        append_memory_receipt(root.path(), &input()).expect("append receipt");

        // then
        let mode = std::fs::metadata(&path)
            .expect("receipt metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn write_line_reports_permission_failure_after_appending() {
        // given
        let root = tempfile::tempdir().expect("temporary runtime directory");
        let receipt = to_receipt(&input());
        let error = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let expected = error.to_string();

        // when: only the permission syscall is replaced, not append or flush.
        let result = write_line_with_permissions(root.path(), &receipt, |_| Err(error));

        // then: as upstream, the append remains durable but the operation reports failure.
        assert_eq!(result, Err(expected));
        let text = std::fs::read_to_string(receipts_path(root.path())).expect("appended receipt");
        assert!(text.ends_with('\n'));
        let stored: MemoryReceipt = serde_json::from_str(text.trim_end()).expect("valid receipt");
        assert_eq!(stored, receipt);
    }
}
