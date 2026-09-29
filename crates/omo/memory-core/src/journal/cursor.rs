//! Reflection transcript cursors, state management, and snapshot logic.

use serde::{Deserialize, Serialize};

use crate::journal::entries::TranscriptEntry;
use crate::journal::store::JournalError;

/// Schema version for reflection transcript state.
pub const REFLECTION_STATE_SCHEMA_VERSION: &str = "v3_assistant_steps";

/// Persisted state for reflection progress through a transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflectionTranscriptState {
    pub schema_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reflected_through_message_id: Option<String>,
    pub total_completed_steps: usize,
    pub reflected_completed_steps: usize,
    pub steps_since_last_successful_reflection: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reflection_started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reflection_succeeded_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_compaction: Option<bool>,
}

/// A captured snapshot of transcript entries ready for reflection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflectionSnapshot {
    pub start_message_id: String,
    pub end_message_id: String,
    pub start_line: usize,
    pub end_snapshot_line: usize,
    pub entries: Vec<TranscriptEntry>,
}

/// Check whether an entry is a canonical user or assistant message with content.
pub fn is_canonical_entry(entry: &TranscriptEntry) -> bool {
    let (kind, source_message_id, text) = match entry {
        TranscriptEntry::Text(e) => (
            e.kind.as_str(),
            e.source_message_id.as_str(),
            e.text.as_str(),
        ),
        TranscriptEntry::ToolCall(_) => return false,
    };
    (kind == "user" || kind == "assistant")
        && !source_message_id.is_empty()
        && !text.trim().is_empty()
}

/// Count the number of completed assistant steps in a sequence of entries.
pub fn count_completed_steps(entries: &[TranscriptEntry]) -> usize {
    entries
        .iter()
        .filter(|entry| match entry {
            TranscriptEntry::Text(e) => {
                e.kind == "assistant"
                    && !e.source_message_id.is_empty()
                    && !e.text.trim().is_empty()
            }
            TranscriptEntry::ToolCall(_) => false,
        })
        .count()
}

/// Recompute step counters and schema version for a reflection state.
pub fn derive_state(
    state: &ReflectionTranscriptState,
    entries: &[TranscriptEntry],
) -> ReflectionTranscriptState {
    let total_completed_steps = count_completed_steps(entries);
    let reflected_completed_steps = state.reflected_completed_steps.min(total_completed_steps);
    let steps_since_last_successful_reflection =
        total_completed_steps.saturating_sub(reflected_completed_steps);
    ReflectionTranscriptState {
        schema_version: REFLECTION_STATE_SCHEMA_VERSION.to_string(),
        reflected_through_message_id: state.reflected_through_message_id.clone(),
        total_completed_steps,
        reflected_completed_steps,
        steps_since_last_successful_reflection,
        last_reflection_started_at: state.last_reflection_started_at.clone(),
        last_reflection_succeeded_at: state.last_reflection_succeeded_at.clone(),
        pending_compaction: state.pending_compaction,
    }
}

/// Capture a slice of unreflected entries bounded by canonical messages.
pub fn capture_cursor_snapshot(
    entries: &[TranscriptEntry],
    state: &ReflectionTranscriptState,
) -> Option<ReflectionSnapshot> {
    let anchor_index = match &state.reflected_through_message_id {
        Some(msg_id) => entries
            .iter()
            .position(|e| is_canonical_entry(e) && e.source_message_id() == msg_id),
        None => None,
    };

    let search_start = match anchor_index {
        Some(idx) => idx + 1,
        None => 0,
    };

    let start_index = entries[search_start..]
        .iter()
        .position(is_canonical_entry)
        .map(|rel| search_start + rel)?;

    let mut end_index = None;
    for (rel, entry) in entries[start_index..].iter().enumerate().rev() {
        if is_canonical_entry(entry) {
            end_index = Some(start_index + rel);
            break;
        }
    }
    let end_index = end_index?;

    let start = &entries[start_index];
    let end = &entries[end_index];
    if !is_canonical_entry(start) || !is_canonical_entry(end) {
        return None;
    }

    let slice_start = search_start;
    Some(ReflectionSnapshot {
        start_message_id: start.source_message_id().to_string(),
        end_message_id: end.source_message_id().to_string(),
        start_line: slice_start,
        end_snapshot_line: entries.len(),
        entries: entries[slice_start..].to_vec(),
    })
}

/// Finalize a reflection run, updating the cursor on success.
pub fn finalize_cursor(
    state: &ReflectionTranscriptState,
    entries: &[TranscriptEntry],
    snapshot: &ReflectionSnapshot,
    success: bool,
    succeeded_at: &str,
) -> ReflectionTranscriptState {
    if !success {
        return derive_state(state, entries);
    }

    let end_line = snapshot.end_snapshot_line.min(entries.len());
    let snapshot_entries = &entries[..end_line];
    let mut updated = state.clone();
    updated.reflected_through_message_id = Some(snapshot.end_message_id.clone());
    updated.reflected_completed_steps = count_completed_steps(snapshot_entries);
    updated.last_reflection_succeeded_at = Some(succeeded_at.to_string());
    derive_state(&updated, entries)
}

/// Construct the initial empty reflection state.
pub fn initial_reflection_state() -> ReflectionTranscriptState {
    ReflectionTranscriptState {
        schema_version: REFLECTION_STATE_SCHEMA_VERSION.to_string(),
        reflected_through_message_id: None,
        total_completed_steps: 0,
        reflected_completed_steps: 0,
        steps_since_last_successful_reflection: 0,
        last_reflection_started_at: None,
        last_reflection_succeeded_at: None,
        pending_compaction: None,
    }
}

/// Parse a raw JSON string into [`ReflectionTranscriptState`], returning an error if corrupt.
pub fn parse_state(raw: &str) -> Result<ReflectionTranscriptState, JournalError> {
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| JournalError::CorruptState(e.to_string()))?;
    let obj = value
        .as_object()
        .ok_or_else(|| JournalError::CorruptState("state is not a JSON object".to_string()))?;

    let reflected_through_message_id = obj
        .get("reflected_through_message_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);

    let total_completed_steps = obj
        .get("total_completed_steps")
        .and_then(|v| v.as_i64())
        .filter(|&n| n >= 0)
        .unwrap_or(0) as usize;

    let reflected_completed_steps = obj
        .get("reflected_completed_steps")
        .and_then(|v| v.as_i64())
        .filter(|&n| n >= 0)
        .unwrap_or(0) as usize;

    let steps_since_last_successful_reflection = obj
        .get("steps_since_last_successful_reflection")
        .and_then(|v| v.as_i64())
        .filter(|&n| n >= 0)
        .unwrap_or(0) as usize;

    let last_reflection_started_at = obj
        .get("last_reflection_started_at")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);

    let last_reflection_succeeded_at = obj
        .get("last_reflection_succeeded_at")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);

    let pending_compaction = obj.get("pending_compaction").and_then(|v| v.as_bool());

    Ok(ReflectionTranscriptState {
        schema_version: REFLECTION_STATE_SCHEMA_VERSION.to_string(),
        reflected_through_message_id,
        total_completed_steps,
        reflected_completed_steps,
        steps_since_last_successful_reflection,
        last_reflection_started_at,
        last_reflection_succeeded_at,
        pending_compaction,
    })
}

#[cfg(test)]
#[path = "cursor_tests.rs"]
mod tests;
