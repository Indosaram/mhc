//! Doctor checks for memory receipts and quarantined runs.
//! Port of `components/memory/commands/doctor-receipts.ts` at adb672532.

use std::path::Path;

use memory_core::receipts::{
    read_memory_receipts, MemoryReceipt, MemoryReceiptInput, ReadMemoryReceiptsOptions,
};
use serde::{Deserialize, Serialize};

use super::doctor_checks::{CheckLevel, DoctorCheck};

pub const QUARANTINE_FILENAME: &str = "quarantined.json";
const TEXT_LIMIT: usize = 120;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptSummary {
    pub event: String,
    pub at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
    pub trigger: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReceiptSummaries {
    pub dream: Option<ReceiptSummary>,
    pub reflection: Option<ReceiptSummary>,
    pub facts: Option<ReceiptSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct QuarantinedRun {
    pub run_id: String,
    pub reason: String,
    pub at: Option<String>,
    pub dir: String,
    pub evidence: Vec<String>,
}

pub fn check_receipts(runtime_dir: &Path, now_ms: i64) -> Result<(DoctorCheck, ReceiptSummaries), String> {
    let read = read_memory_receipts(runtime_dir, &ReadMemoryReceiptsOptions::default())?;
    let (receipts, skipped_partial_lines) = (read.receipts, read.skipped_partial_lines);

    let find_newest = |kind: &str| -> Option<ReceiptSummary> {
        receipts.iter().find_map(|r| {
            if r.input.kind() == kind && r.input.event() != "launched" && r.input.event() != "recovered" {
                Some(summarize(r))
            } else {
                None
            }
        })
    };

    let dream_summary = find_newest("dream");
    let reflection_summary = find_newest("reflection");
    let facts_summary = find_newest("facts");

    let summaries = ReceiptSummaries {
        dream: dream_summary.clone(),
        reflection: reflection_summary.clone(),
        facts: facts_summary.clone(),
    };

    let parts = [
        describe("dream", dream_summary.as_ref(), now_ms),
        describe("reflection", reflection_summary.as_ref(), now_ms),
        describe("facts", facts_summary.as_ref(), now_ms),
    ];

    let skipped = if skipped_partial_lines == 0 {
        String::new()
    } else {
        format!(
            " ({} partial line{} skipped)",
            skipped_partial_lines,
            if skipped_partial_lines == 1 { "" } else { "s" }
        )
    };

    let check = DoctorCheck {
        name: "receipts".to_string(),
        level: if skipped_partial_lines == 0 {
            CheckLevel::Ok
        } else {
            CheckLevel::Warn
        },
        detail: format!("{}{}", parts.join("; "), skipped),
    };

    Ok((check, summaries))
}

pub fn check_quarantined_runs(reflection_dir: &Path) -> (DoctorCheck, Vec<QuarantinedRun>) {
    let runs_dir = reflection_dir.join("runs");
    let mut names = match std::fs::read_dir(&runs_dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        Err(_) => Vec::new(),
    };
    names.sort();

    let mut runs = Vec::new();
    for name in names {
        let dir = runs_dir.join(&name);
        if !dir.join(QUARANTINE_FILENAME).exists() {
            continue;
        }
        runs.push(read_quarantine(&dir, &name));
    }

    if runs.is_empty() {
        return (
            DoctorCheck {
                name: "quarantined-runs".to_string(),
                level: CheckLevel::Ok,
                detail: "no quarantined runs".to_string(),
            },
            runs,
        );
    }

    let listed = runs
        .iter()
        .map(|r| format!("{} ({})", r.dir, r.reason))
        .collect::<Vec<_>>()
        .join("; ");
    let _noun = if runs.len() == 1 {
        "1 run needs"
    } else {
        "runs need" // Will format with count below
    };
    let detail = if runs.len() == 1 {
        format!("1 run needs manual disposal: {listed}")
    } else {
        format!("{} runs need manual disposal: {listed}", runs.len())
    };

    (
        DoctorCheck {
            name: "quarantined-runs".to_string(),
            level: CheckLevel::Warn,
            detail,
        },
        runs,
    )
}

fn read_quarantine(dir: &Path, name: &str) -> QuarantinedRun {
    let path = dir.join(QUARANTINE_FILENAME);
    let Ok(content) = std::fs::read_to_string(&path) else {
        return QuarantinedRun {
            run_id: name.to_string(),
            reason: "unreadable_sentinel".to_string(),
            at: None,
            dir: dir.display().to_string(),
            evidence: Vec::new(),
        };
    };

    let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) else {
        return QuarantinedRun {
            run_id: name.to_string(),
            reason: "unreadable_sentinel".to_string(),
            at: None,
            dir: dir.display().to_string(),
            evidence: Vec::new(),
        };
    };

    let run_id = val
        .get("runId")
        .and_then(|v| v.as_str())
        .unwrap_or(name)
        .to_string();
    let reason = val
        .get("reason")
        .and_then(|v| v.as_str())
        .map(bound)
        .unwrap_or_else(|| "unknown".to_string());
    let at = val
        .get("quarantinedAt")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let evidence = val
        .get("evidence")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    QuarantinedRun {
        run_id,
        reason,
        at,
        dir: dir.display().to_string(),
        evidence,
    }
}

fn summarize(receipt: &MemoryReceipt) -> ReceiptSummary {
    let (run_id, batch_id, trigger, reason, sha, detail) = match &receipt.input {
        MemoryReceiptInput::Run {
            run_id,
            trigger,
            reason,
            sha,
            detail,
            ..
        } => (
            Some(run_id.clone()),
            None,
            trigger.clone(),
            reason.as_deref().map(bound),
            sha.clone(),
            detail.as_deref().map(bound),
        ),
        MemoryReceiptInput::Facts {
            batch_id,
            trigger,
            reason,
            sha,
            detail,
            ..
        } => (
            None,
            Some(batch_id.clone()),
            trigger.clone(),
            reason.as_deref().map(bound),
            sha.clone(),
            detail.as_deref().map(bound),
        ),
    };

    ReceiptSummary {
        event: receipt.input.event().to_string(),
        at: receipt.at.clone(),
        run_id,
        batch_id,
        trigger,
        reason,
        sha,
        detail,
    }
}

fn describe(kind: &str, summary: Option<&ReceiptSummary>, now_ms: i64) -> String {
    let Some(summary) = summary else {
        return format!("{kind} never");
    };

    let label = if let Some(reason) = &summary.reason {
        reason.clone()
    } else if let Some(run_id) = &summary.run_id {
        let prefix = String::from_utf16_lossy(&run_id.encode_utf16().take(8).collect::<Vec<_>>());
        format!("run {prefix}")
    } else if let Some(batch_id) = &summary.batch_id {
        let prefix = String::from_utf16_lossy(&batch_id.encode_utf16().take(8).collect::<Vec<_>>());
        format!("batch {prefix}")
    } else {
        String::new()
    };

    format!(
        "{} {} {} ({label})",
        kind,
        summary.event,
        age(&summary.at, now_ms)
    )
}

fn age(at: &str, now_ms: i64) -> String {
    let parsed_ms = match chrono::DateTime::parse_from_rfc3339(at) {
        Ok(dt) => dt.timestamp_millis(),
        Err(_) => return "at an unknown time".to_string(),
    };

    let elapsed_ms = now_ms - parsed_ms;
    if elapsed_ms < 0 {
        return "0s ago".to_string();
    }
    let seconds = (elapsed_ms / 1000).max(0);
    if seconds < 60 {
        return format!("{seconds}s ago");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", hours / 24)
}

fn bound(text: &str) -> String {
    if text.encode_utf16().count() <= TEXT_LIMIT {
        text.to_string()
    } else {
        format!("{}...", String::from_utf16_lossy(&text.encode_utf16().take(TEXT_LIMIT - 3).collect::<Vec<_>>()))
    }
}
