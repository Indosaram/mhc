//! Durable facts failure-streak ledger: wire format and fail-closed validation.
//!
//! Spec: `docs/cleanroom/specs/memory-failure-ledger.md`. The ledger decides which conversations are retried, so a
//! corrupt file must never be read as "no failures": that would retry every parked conversation at once and
//! recreate the outage that parked them.

use serde::{Deserialize, Serialize};

pub const FACTS_FAILURES_VERSION: u32 = 1;

pub const FACTS_FAILURE_REASONS: [&str; 12] = [
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
    "secret_like_content",
    "other",
];

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
    SecretLikeContent,
    Other,
}

impl FactsFailureReason {
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
            Self::SecretLikeContent => "secret_like_content",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FactsFailureState {
    Backoff,
    Parked,
}

impl FactsFailureState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Backoff => "backoff",
            Self::Parked => "parked",
        }
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsFailuresFile {
    pub version: u32,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    pub entries: Vec<FactsFailureRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactsFailuresCorruptError {
    pub reason: String,
}

impl FactsFailuresCorruptError {
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

pub fn empty_failures_file(updated_at: String) -> FactsFailuresFile {
    FactsFailuresFile {
        version: FACTS_FAILURES_VERSION,
        updated_at,
        entries: Vec::new(),
    }
}

/// Parse and validate a ledger fail-closed.
pub fn parse_failures_file(raw: &str) -> Result<FactsFailuresFile, FactsFailuresCorruptError> {
    let parsed: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| FactsFailuresCorruptError::new(e.to_string()))?;
    if !parsed.is_object() {
        return Err(FactsFailuresCorruptError::new("root must be an object"));
    }
    let version = parsed.get("version").and_then(|v| v.as_u64());
    if version != Some(FACTS_FAILURES_VERSION as u64) {
        let version_text = parsed
            .get("version")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "undefined".to_string());
        return Err(FactsFailuresCorruptError::new(format!(
            "unsupported version {version_text}"
        )));
    }
    let entry_values = parsed
        .get("entries")
        .and_then(|v| v.as_array())
        .ok_or_else(|| FactsFailuresCorruptError::new("entries must be an array"))?;
    let updated_at_value = parsed
        .get("updatedAt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| FactsFailuresCorruptError::new("updatedAt must be a non-empty string"))?;
    let updated_at = require_instant(updated_at_value, "updatedAt")?;

    let mut entries = Vec::with_capacity(entry_values.len());
    for row in entry_values {
        entries.push(parse_record(row)?);
    }
    Ok(FactsFailuresFile {
        version: FACTS_FAILURES_VERSION,
        updated_at,
        entries: sort_failure_records(entries),
    })
}

pub fn sort_failure_records(mut entries: Vec<FactsFailureRecord>) -> Vec<FactsFailureRecord> {
    entries.sort_by(|left, right| {
        left.conversation_id
            .cmp(&right.conversation_id)
            .then_with(|| left.end_snapshot_line.cmp(&right.end_snapshot_line))
            .then_with(|| left.end_message_id.cmp(&right.end_message_id))
    });
    entries
}

/// Render the ledger deterministically so a rewritten-but-unchanged ledger produces no diff.
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
        Some(text) if !text.is_empty() => Ok(text.to_string()),
        _ => Err(FactsFailuresCorruptError::new(format!(
            "{field} must be a non-empty string"
        ))),
    }
}

fn parse_rfc3339_millis(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.timestamp_millis())
}

fn format_rfc3339_millis(millis: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(millis)
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

/// Accept only the exact form this crate writes, so two spellings of one instant can never compare unequal.
fn require_instant(value: &str, field: &str) -> Result<String, FactsFailuresCorruptError> {
    if value.is_empty() {
        return Err(FactsFailuresCorruptError::new(format!(
            "{field} must be a non-empty string"
        )));
    }
    let Some(millis) = parse_rfc3339_millis(value) else {
        return Err(FactsFailuresCorruptError::new(format!(
            "{field} must be an ISO-8601 instant"
        )));
    };
    if format_rfc3339_millis(millis).as_deref() != Some(value) {
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
        Some(number) if number >= 0 => Ok(number as u64),
        _ => Err(FactsFailuresCorruptError::new(format!(
            "{field} must be a non-negative integer"
        ))),
    }
}

fn parse_record(
    value: &serde_json::Value,
) -> Result<FactsFailureRecord, FactsFailuresCorruptError> {
    let Some(object) = value.as_object() else {
        return Err(FactsFailuresCorruptError::new(
            "entries must contain objects",
        ));
    };
    let state_text = object
        .get("state")
        .and_then(|v| v.as_str())
        .ok_or_else(|| FactsFailuresCorruptError::new("state must be a string"))?;
    let state = match state_text {
        "backoff" => FactsFailureState::Backoff,
        "parked" => FactsFailureState::Parked,
        other => {
            return Err(FactsFailuresCorruptError::new(format!(
                "state \"{other}\" is not backoff or parked"
            )));
        }
    };

    let next_eligible_at = match object.get("nextEligibleAt") {
        None => None,
        Some(value) if value.is_null() => None,
        Some(value) => match value.as_str() {
            Some(text) => Some(require_instant(text, "nextEligibleAt")?),
            None => {
                return Err(FactsFailuresCorruptError::new(
                    "nextEligibleAt must be an ISO-8601 instant",
                ));
            }
        },
    };

    let parked_at = match object.get("parkedAt") {
        None => None,
        Some(value) => match value.as_str() {
            Some(text) => Some(require_instant(text, "parkedAt")?),
            None => {
                return Err(FactsFailuresCorruptError::new(
                    "parkedAt must be an ISO-8601 instant",
                ));
            }
        },
    };

    // The two states have disjoint timestamp shapes; enforcing them keeps "when may this retry" answerable from
    // one field instead of from a combination the reader has to reconstruct.
    if state == FactsFailureState::Parked
        && (object.get("nextEligibleAt") != Some(&serde_json::Value::Null) || parked_at.is_none())
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

    let streak = require_non_negative_integer(object.get("streak"), "streak")?;
    if streak < 1 {
        return Err(FactsFailuresCorruptError::new("streak must be at least 1"));
    }

    let conversation_id = require_non_empty_string(object.get("conversationId"), "conversationId")?;
    let end_message_id = require_non_empty_string(object.get("end_message_id"), "end_message_id")?;
    let end_snapshot_line =
        require_non_negative_integer(object.get("end_snapshot_line"), "end_snapshot_line")?;
    let first_failure_text =
        require_non_empty_string(object.get("firstFailureAt"), "firstFailureAt")?;
    let first_failure_at = require_instant(&first_failure_text, "firstFailureAt")?;
    let last_failure_text = require_non_empty_string(object.get("lastFailureAt"), "lastFailureAt")?;
    let last_failure_at = require_instant(&last_failure_text, "lastFailureAt")?;

    let last_reason_text = object
        .get("lastReason")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            FactsFailuresCorruptError::new(format!(
                "lastReason \"{}\" is not a known reason",
                object
                    .get("lastReason")
                    .map(|v| v.to_string())
                    .unwrap_or_default()
            ))
        })?;
    let last_reason = match last_reason_text {
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
        "secret_like_content" => FactsFailureReason::SecretLikeContent,
        "other" => FactsFailureReason::Other,
        other => {
            return Err(FactsFailuresCorruptError::new(format!(
                "lastReason \"{other}\" is not a known reason"
            )));
        }
    };

    let last_failure_id = require_non_empty_string(object.get("lastFailureId"), "lastFailureId")?;

    let last_detail = match object.get("lastDetail") {
        None => None,
        Some(value) => Some(require_non_empty_string(Some(value), "lastDetail")?),
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
mod tests {
    use super::*;

    const T0: &str = "2026-11-11T00:00:00.000Z";
    const T1: &str = "2026-11-11T00:05:00.000Z";

    fn backoff_record() -> FactsFailureRecord {
        FactsFailureRecord {
            conversation_id: "conv-1".into(),
            end_message_id: "msg-1".into(),
            end_snapshot_line: 7,
            state: FactsFailureState::Backoff,
            streak: 2,
            first_failure_at: T0.into(),
            last_failure_at: T1.into(),
            last_reason: FactsFailureReason::ChildExit,
            last_detail: Some("exit status 1".into()),
            last_failure_id: "failure-1".into(),
            next_eligible_at: Some(T1.into()),
            parked_at: None,
        }
    }

    fn parked_record() -> FactsFailureRecord {
        FactsFailureRecord {
            state: FactsFailureState::Parked,
            streak: 5,
            last_reason: FactsFailureReason::SandboxUnavailable,
            last_detail: None,
            next_eligible_at: None,
            parked_at: Some(T1.into()),
            ..backoff_record()
        }
    }

    #[test]
    fn every_reason_round_trips_through_its_wire_name() {
        assert_eq!(FACTS_FAILURE_REASONS.len(), 12);
        for name in FACTS_FAILURE_REASONS {
            let parsed: FactsFailureReason = serde_json::from_str(&format!("\"{name}\"")).unwrap();
            assert_eq!(parsed.as_str(), name);
        }
    }

    #[test]
    fn an_empty_ledger_round_trips() {
        let rendered = render_failures_file(&[], T0);
        let parsed = parse_failures_file(&rendered).unwrap();
        assert_eq!(parsed.version, FACTS_FAILURES_VERSION);
        assert_eq!(parsed.updated_at, T0);
        assert!(parsed.entries.is_empty());
    }

    #[test]
    fn a_backoff_record_round_trips() {
        let rendered = render_failures_file(&[backoff_record()], T0);
        let parsed = parse_failures_file(&rendered).unwrap();
        assert_eq!(parsed.entries, vec![backoff_record()]);
    }

    #[test]
    fn a_parked_record_round_trips() {
        let rendered = render_failures_file(&[parked_record()], T0);
        let parsed = parse_failures_file(&rendered).unwrap();
        assert_eq!(parsed.entries, vec![parked_record()]);
    }

    #[test]
    fn rendering_is_newline_terminated() {
        assert!(render_failures_file(&[], T0).ends_with('\n'));
    }

    #[test]
    fn rendering_sorts_entries() {
        let later = FactsFailureRecord {
            conversation_id: "conv-2".into(),
            ..backoff_record()
        };
        let rendered = render_failures_file(&[later, backoff_record()], T0);
        let parsed = parse_failures_file(&rendered).unwrap();
        assert_eq!(parsed.entries[0].conversation_id, "conv-1");
        assert_eq!(parsed.entries[1].conversation_id, "conv-2");
    }

    #[test]
    fn records_within_one_conversation_sort_by_snapshot_line() {
        let higher = FactsFailureRecord {
            end_snapshot_line: 9,
            ..backoff_record()
        };
        let sorted = sort_failure_records(vec![higher, backoff_record()]);
        assert_eq!(sorted[0].end_snapshot_line, 7);
    }

    #[test]
    fn a_non_object_root_is_rejected() {
        let error = parse_failures_file("[]").unwrap_err();
        assert_eq!(error.reason, "root must be an object");
    }

    #[test]
    fn invalid_json_is_rejected() {
        assert!(parse_failures_file("{").is_err());
    }

    #[test]
    fn an_unsupported_version_is_rejected() {
        let error = parse_failures_file(&format!(
            "{{\"version\":2,\"updatedAt\":\"{T0}\",\"entries\":[]}}"
        ))
        .unwrap_err();
        assert!(error.reason.contains("unsupported version 2"), "{error}");
    }

    #[test]
    fn a_missing_version_is_rejected() {
        let error =
            parse_failures_file(&format!("{{\"updatedAt\":\"{T0}\",\"entries\":[]}}")).unwrap_err();
        assert!(error.reason.contains("undefined"), "{error}");
    }

    #[test]
    fn entries_must_be_an_array() {
        let error = parse_failures_file(&format!(
            "{{\"version\":1,\"updatedAt\":\"{T0}\",\"entries\":{{}}}}"
        ))
        .unwrap_err();
        assert_eq!(error.reason, "entries must be an array");
    }

    #[test]
    fn updated_at_must_be_an_instant() {
        let error =
            parse_failures_file("{\"version\":1,\"updatedAt\":\"yesterday\",\"entries\":[]}")
                .unwrap_err();
        assert!(error.reason.contains("ISO-8601"), "{error}");
    }

    #[test]
    fn an_instant_must_use_the_exact_millisecond_form() {
        let error = parse_failures_file(
            "{\"version\":1,\"updatedAt\":\"2026-11-11T00:00:00Z\",\"entries\":[]}",
        )
        .unwrap_err();
        assert!(error.reason.contains("ISO-8601"), "{error}");
    }

    #[test]
    fn an_unknown_state_is_rejected() {
        let rendered =
            render_failures_file(&[backoff_record()], T0).replace("\"backoff\"", "\"sleeping\"");
        let error = parse_failures_file(&rendered).unwrap_err();
        assert!(error.reason.contains("sleeping"), "{error}");
    }

    #[test]
    fn an_unknown_reason_is_rejected() {
        let rendered =
            render_failures_file(&[backoff_record()], T0).replace("child_exit", "meteor_strike");
        let error = parse_failures_file(&rendered).unwrap_err();
        assert!(error.reason.contains("meteor_strike"), "{error}");
    }

    #[test]
    fn a_parked_record_with_a_next_eligible_at_is_rejected() {
        let rendered = render_failures_file(&[parked_record()], T0).replace(
            "\"nextEligibleAt\": null",
            &format!("\"nextEligibleAt\": \"{T1}\""),
        );
        let error = parse_failures_file(&rendered).unwrap_err();
        assert!(error.reason.contains("parked record"), "{error}");
    }

    #[test]
    fn a_parked_record_without_parked_at_is_rejected() {
        let mut document: serde_json::Value =
            serde_json::from_str(&render_failures_file(&[parked_record()], T0)).unwrap();
        document["entries"][0]
            .as_object_mut()
            .unwrap()
            .remove("parkedAt");
        let error = parse_failures_file(&document.to_string()).unwrap_err();
        assert!(error.reason.contains("parked record"), "{error}");
    }

    #[test]
    fn a_parked_record_with_a_null_parked_at_is_rejected() {
        let rendered = render_failures_file(&[parked_record()], T0)
            .replace(&format!("\"parkedAt\": \"{T1}\""), "\"parkedAt\": null");
        let error = parse_failures_file(&rendered).unwrap_err();
        assert!(error.reason.contains("ISO-8601"), "{error}");
    }

    #[test]
    fn a_backoff_record_without_next_eligible_at_is_rejected() {
        let rendered = render_failures_file(&[backoff_record()], T0).replace(
            &format!("\"nextEligibleAt\": \"{T1}\""),
            "\"nextEligibleAt\": null",
        );
        let error = parse_failures_file(&rendered).unwrap_err();
        assert!(error.reason.contains("backoff record"), "{error}");
    }

    #[test]
    fn a_backoff_record_with_parked_at_is_rejected() {
        let rendered = render_failures_file(&[backoff_record()], T0).replace(
            "\"nextEligibleAt\"",
            &format!("\"parkedAt\": \"{T1}\",\"nextEligibleAt\""),
        );
        let error = parse_failures_file(&rendered).unwrap_err();
        assert!(error.reason.contains("backoff record"), "{error}");
    }

    #[test]
    fn a_zero_streak_is_rejected() {
        let rendered =
            render_failures_file(&[backoff_record()], T0).replace("\"streak\": 2", "\"streak\": 0");
        let error = parse_failures_file(&rendered).unwrap_err();
        assert!(error.reason.contains("at least 1"), "{error}");
    }

    #[test]
    fn a_negative_streak_is_rejected() {
        let rendered = render_failures_file(&[backoff_record()], T0)
            .replace("\"streak\": 2", "\"streak\": -1");
        let error = parse_failures_file(&rendered).unwrap_err();
        assert!(error.reason.contains("non-negative"), "{error}");
    }

    #[test]
    fn an_empty_conversation_id_is_rejected() {
        let rendered = render_failures_file(&[backoff_record()], T0).replace("\"conv-1\"", "\"\"");
        let error = parse_failures_file(&rendered).unwrap_err();
        assert!(error.reason.contains("conversationId"), "{error}");
    }

    #[test]
    fn an_empty_last_detail_is_rejected_rather_than_dropped() {
        let rendered =
            render_failures_file(&[backoff_record()], T0).replace("\"exit status 1\"", "\"\"");
        let error = parse_failures_file(&rendered).unwrap_err();
        assert!(error.reason.contains("lastDetail"), "{error}");
    }

    #[test]
    fn a_record_without_last_detail_is_accepted() {
        let record = FactsFailureRecord {
            last_detail: None,
            ..backoff_record()
        };
        let parsed =
            parse_failures_file(&render_failures_file(std::slice::from_ref(&record), T0)).unwrap();
        assert_eq!(parsed.entries, vec![record]);
    }

    #[test]
    fn a_non_object_entry_is_rejected_with_its_own_reason() {
        let broken = format!("{{\"version\":1,\"updatedAt\":\"{T0}\",\"entries\":[\"conv-1\"]}}");
        let error = parse_failures_file(&broken).unwrap_err();
        assert_eq!(error.reason, "entries must contain objects");
    }

    #[test]
    fn an_empty_file_is_corrupt_not_empty() {
        assert!(parse_failures_file("").is_err());
    }
}
