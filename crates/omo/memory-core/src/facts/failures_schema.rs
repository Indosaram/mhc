//! Durable facts failure-streak wire format and validation.

use serde::{Deserialize, Serialize};

use crate::support::time::{format_rfc3339_millis, parse_rfc3339};

/// Current schema version for facts failure ledger files.
pub const FACTS_FAILURES_VERSION: u32 = 1;

/// Canonical failure reason list.
pub const FACTS_FAILURE_REASONS: [&str; 11] = [
    "quick_category_unavailable",
    "sandbox_unavailable",
    "child_exit",
    "deadline_exceeded",
    "invalid_extraction",
    "memory_write_lock_exhausted",
    "parent_dirty",
    "unknown_liveness",
    "payload_envelope_oversize",
    "payload_entry_oversize",
    "other",
];

/// Known error reasons recorded in the failure ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactsFailureReason {
    QuickCategoryUnavailable,
    SandboxUnavailable,
    ChildExit,
    DeadlineExceeded,
    InvalidExtraction,
    MemoryWriteLockExhausted,
    ParentDirty,
    UnknownLiveness,
    PayloadEnvelopeOversize,
    PayloadEntryOversize,
    Other,
}

impl FactsFailureReason {
    /// Return the wire string representation of this failure reason.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::QuickCategoryUnavailable => "quick_category_unavailable",
            Self::SandboxUnavailable => "sandbox_unavailable",
            Self::ChildExit => "child_exit",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::InvalidExtraction => "invalid_extraction",
            Self::MemoryWriteLockExhausted => "memory_write_lock_exhausted",
            Self::ParentDirty => "parent_dirty",
            Self::UnknownLiveness => "unknown_liveness",
            Self::PayloadEnvelopeOversize => "payload_envelope_oversize",
            Self::PayloadEntryOversize => "payload_entry_oversize",
            Self::Other => "other",
        }
    }
}

/// Status of a failing queue endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FactsFailureState {
    Backoff,
    Parked,
}

impl FactsFailureState {
    /// Return the wire string representation of this failure state.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Backoff => "backoff",
            Self::Parked => "parked",
        }
    }
}

/// Durable record of failures for one queue endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsFailureRecord {
    #[serde(rename = "conversationId")]
    pub conversation_id: String,
    pub end_message_id: String,
    pub end_snapshot_line: u64,
    pub state: FactsFailureState,
    pub streak: u64,
    #[serde(rename = "firstFailureAt")]
    pub first_failure_at: String,
    #[serde(rename = "lastFailureAt")]
    pub last_failure_at: String,
    #[serde(rename = "lastReason")]
    pub last_reason: FactsFailureReason,
    #[serde(rename = "lastDetail", skip_serializing_if = "Option::is_none")]
    pub last_detail: Option<String>,
    #[serde(rename = "lastFailureId")]
    pub last_failure_id: String,
    #[serde(rename = "nextEligibleAt")]
    pub next_eligible_at: Option<String>,
    #[serde(rename = "parkedAt", skip_serializing_if = "Option::is_none")]
    pub parked_at: Option<String>,
}

/// Root shape of the failures.json ledger file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsFailuresFile {
    pub version: u32,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    pub entries: Vec<FactsFailureRecord>,
}

/// Typed error returned when the failures ledger file is corrupt or unreadable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactsFailuresCorruptError {
    pub reason: String,
}

impl FactsFailuresCorruptError {
    /// Construct a new corruption error with the given message.
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl std::fmt::Display for FactsFailuresCorruptError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "facts failures file is unusable: {}",
            self.reason
        )
    }
}

impl std::error::Error for FactsFailuresCorruptError {}

/// Construct an empty failure file with the given timestamp.
pub fn empty_failures_file(updated_at: String) -> FactsFailuresFile {
    FactsFailuresFile {
        version: FACTS_FAILURES_VERSION,
        updated_at,
        entries: Vec::new(),
    }
}

/// Parse and validate failures file contents fail-closed.
pub fn parse_failures_file(raw: &str) -> Result<FactsFailuresFile, FactsFailuresCorruptError> {
    let parsed: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| FactsFailuresCorruptError::new(e.to_string()))?;
    if !parsed.is_object() {
        return Err(FactsFailuresCorruptError::new("root must be an object"));
    }
    let version = parsed.get("version").and_then(|v| v.as_u64());
    if version != Some(FACTS_FAILURES_VERSION as u64) {
        let v_str = parsed
            .get("version")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "undefined".to_string());
        return Err(FactsFailuresCorruptError::new(format!(
            "unsupported version {v_str}"
        )));
    }
    let entries_val = parsed
        .get("entries")
        .and_then(|v| v.as_array())
        .ok_or_else(|| FactsFailuresCorruptError::new("entries must be an array"))?;
    let updated_at_val = parsed
        .get("updatedAt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| FactsFailuresCorruptError::new("updatedAt must be a non-empty string"))?;
    let updated_at = require_instant(updated_at_val, "updatedAt")?;

    let mut entries = Vec::with_capacity(entries_val.len());
    for row in entries_val {
        entries.push(parse_record(row)?);
    }
    Ok(FactsFailuresFile {
        version: FACTS_FAILURES_VERSION,
        updated_at,
        entries: sort_failure_records(entries),
    })
}

/// Sort failure records deterministically by conversation, snapshot line, and endpoint id.
pub fn sort_failure_records(mut entries: Vec<FactsFailureRecord>) -> Vec<FactsFailureRecord> {
    entries.sort_by(|left, right| {
        left.conversation_id
            .cmp(&right.conversation_id)
            .then_with(|| left.end_snapshot_line.cmp(&right.end_snapshot_line))
            .then_with(|| left.end_message_id.cmp(&right.end_message_id))
    });
    entries
}

/// Render failure records into formatted, newline-terminated JSON text.
pub fn render_failures_file(entries: &[FactsFailureRecord], updated_at: &str) -> String {
    let file = FactsFailuresFile {
        version: FACTS_FAILURES_VERSION,
        updated_at: updated_at.to_string(),
        entries: sort_failure_records(entries.to_vec()),
    };
    let mut rendered = serde_json::to_string_pretty(&file).unwrap_or_default();
    rendered.push('\n');
    rendered
}

fn require_non_empty_string(
    value: Option<&serde_json::Value>,
    field: &str,
) -> Result<String, FactsFailuresCorruptError> {
    match value.and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => Ok(s.to_string()),
        _ => Err(FactsFailuresCorruptError::new(format!(
            "{field} must be a non-empty string"
        ))),
    }
}

fn require_instant(value: &str, field: &str) -> Result<String, FactsFailuresCorruptError> {
    if value.is_empty() {
        return Err(FactsFailuresCorruptError::new(format!(
            "{field} must be a non-empty string"
        )));
    }
    let Some(millis) = parse_rfc3339(value) else {
        return Err(FactsFailuresCorruptError::new(format!(
            "{field} must be an ISO-8601 instant"
        )));
    };
    if format_rfc3339_millis(millis) != value {
        return Err(FactsFailuresCorruptError::new(format!(
            "{field} must be an ISO-8601 instant"
        )));
    }
    Ok(value.to_string())
}

fn require_non_negative_integer(
    value: Option<&serde_json::Value>,
    field: &str,
) -> Result<u64, FactsFailuresCorruptError> {
    match value.and_then(|v| v.as_i64()) {
        Some(n) if n >= 0 => Ok(n as u64),
        _ => Err(FactsFailuresCorruptError::new(format!(
            "{field} must be a non-negative integer"
        ))),
    }
}

fn parse_record(val: &serde_json::Value) -> Result<FactsFailureRecord, FactsFailuresCorruptError> {
    let Some(obj) = val.as_object() else {
        return Err(FactsFailuresCorruptError::new(
            "entries must contain objects",
        ));
    };
    let state_str = obj
        .get("state")
        .and_then(|v| v.as_str())
        .ok_or_else(|| FactsFailuresCorruptError::new("state must be a string"))?;
    let state = match state_str {
        "backoff" => FactsFailureState::Backoff,
        "parked" => FactsFailureState::Parked,
        other => {
            return Err(FactsFailuresCorruptError::new(format!(
                "state \"{other}\" is not backoff or parked"
            )));
        }
    };

    let next_eligible_at = match obj.get("nextEligibleAt") {
        None => None,
        Some(v) if v.is_null() => None,
        Some(v) => match v.as_str() {
            Some(s) => Some(require_instant(s, "nextEligibleAt")?),
            None => {
                return Err(FactsFailuresCorruptError::new(
                    "nextEligibleAt must be an ISO-8601 instant",
                ));
            }
        },
    };

    let parked_at = match obj.get("parkedAt") {
        None => None,
        Some(v) => match v.as_str() {
            Some(s) => Some(require_instant(s, "parkedAt")?),
            None => {
                return Err(FactsFailuresCorruptError::new(
                    "parkedAt must be an ISO-8601 instant",
                ));
            }
        },
    };

    if state == FactsFailureState::Parked
        && (obj.get("nextEligibleAt") != Some(&serde_json::Value::Null) || parked_at.is_none())
    {
        return Err(FactsFailuresCorruptError::new(
            "a parked record needs parkedAt and a null nextEligibleAt",
        ));
    }
    if state == FactsFailureState::Backoff && (next_eligible_at.is_none() || parked_at.is_some()) {
        return Err(FactsFailuresCorruptError::new(
            "a backoff record needs nextEligibleAt and no parkedAt",
        ));
    }

    let streak = require_non_negative_integer(obj.get("streak"), "streak")?;
    if streak < 1 {
        return Err(FactsFailuresCorruptError::new("streak must be at least 1"));
    }

    let conversation_id = require_non_empty_string(obj.get("conversationId"), "conversationId")?;
    let end_message_id = require_non_empty_string(obj.get("end_message_id"), "end_message_id")?;
    let end_snapshot_line =
        require_non_negative_integer(obj.get("end_snapshot_line"), "end_snapshot_line")?;
    let first_failure_at_str =
        require_non_empty_string(obj.get("firstFailureAt"), "firstFailureAt")?;
    let first_failure_at = require_instant(&first_failure_at_str, "firstFailureAt")?;
    let last_failure_at_str = require_non_empty_string(obj.get("lastFailureAt"), "lastFailureAt")?;
    let last_failure_at = require_instant(&last_failure_at_str, "lastFailureAt")?;

    let last_reason_str = obj
        .get("lastReason")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            FactsFailuresCorruptError::new(format!(
                "lastReason \"{}\" is not a known reason",
                obj.get("lastReason")
                    .map(|v| v.to_string())
                    .unwrap_or_default()
            ))
        })?;
    let last_reason = match last_reason_str {
        "quick_category_unavailable" => FactsFailureReason::QuickCategoryUnavailable,
        "sandbox_unavailable" => FactsFailureReason::SandboxUnavailable,
        "child_exit" => FactsFailureReason::ChildExit,
        "deadline_exceeded" => FactsFailureReason::DeadlineExceeded,
        "invalid_extraction" => FactsFailureReason::InvalidExtraction,
        "memory_write_lock_exhausted" => FactsFailureReason::MemoryWriteLockExhausted,
        "parent_dirty" => FactsFailureReason::ParentDirty,
        "unknown_liveness" => FactsFailureReason::UnknownLiveness,
        "payload_envelope_oversize" => FactsFailureReason::PayloadEnvelopeOversize,
        "payload_entry_oversize" => FactsFailureReason::PayloadEntryOversize,
        "other" => FactsFailureReason::Other,
        other => {
            return Err(FactsFailuresCorruptError::new(format!(
                "lastReason \"{other}\" is not a known reason"
            )));
        }
    };

    let last_failure_id = require_non_empty_string(obj.get("lastFailureId"), "lastFailureId")?;

    let last_detail = match obj.get("lastDetail") {
        None => None,
        Some(v) => Some(require_non_empty_string(Some(v), "lastDetail")?),
    };

    Ok(FactsFailureRecord {
        conversation_id,
        end_message_id,
        end_snapshot_line,
        state,
        streak,
        first_failure_at,
        last_failure_at,
        last_reason,
        last_detail,
        last_failure_id,
        next_eligible_at,
        parked_at,
    })
}

#[cfg(test)]
#[path = "failures_schema_tests.rs"]
mod tests;
