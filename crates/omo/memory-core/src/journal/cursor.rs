//! Reflection transcript cursors, state management, and snapshot logic.

use serde::{Deserialize, Serialize};

use crate::journal::entries::TranscriptEntry;
use crate::journal::store::JournalError;

/// Schema version for reflection transcript state.
pub const REFLECTION_STATE_SCHEMA_VERSION: &str = "v3_assistant_steps";
pub const REFLECTION_SNAPSHOT_MAX_BYTES: usize = 131_072;

fn entry_bytes(entry: &TranscriptEntry) -> usize {
    serde_json::to_vec(entry).map_or(0, |json| json.len() + 1)
}

pub fn reflected_through_byte_offset(entries: &[TranscriptEntry], message_id: &str) -> usize {
    let mut offset = 0;
    let mut reflected = 0;
    for entry in entries {
        offset += entry_bytes(entry);
        if entry.source_message_id() == message_id {
            reflected = offset;
        }
    }
    reflected
}

pub fn journal_byte_offset(entries: &[TranscriptEntry]) -> usize {
    serialized_bytes(entries)
}

/// Persisted state for reflection progress through a transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflectionTranscriptState {
    pub schema_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reflected_through_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reflected_through_byte_offset: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unreflected_bytes: Option<usize>,
    pub total_completed_steps: usize,
    pub reflected_completed_steps: usize,
    pub steps_since_last_successful_reflection: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reflection_started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reflection_succeeded_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_compaction: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consecutive_failures: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_eligible_at: Option<String>,
}

fn reflected_cursor_index(entries: &[TranscriptEntry], message_id: &str) -> Option<usize> {
    entries
        .iter()
        .rposition(|entry| entry.source_message_id() == message_id)
}

fn serialized_bytes(entries: &[TranscriptEntry]) -> usize {
    entries.iter().map(entry_bytes).sum()
}

/// A captured snapshot of transcript entries ready for reflection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflectionSnapshot {
    pub start_message_id: String,
    pub end_message_id: String,
    pub start_line: usize,
    pub end_snapshot_line: usize,
    pub entries: Vec<TranscriptEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backlog_remaining: Option<usize>,
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
    derive_state_with_entries(state, entries)
}

fn derive_state_with_entries(
    state: &ReflectionTranscriptState,
    entries: &[TranscriptEntry],
) -> ReflectionTranscriptState {
    let mut derived = derive_state_with_total(state, count_completed_steps(entries));
    let anchor = state.reflected_through_message_id.as_deref()
        .and_then(|message_id| reflected_cursor_index(entries, message_id));
    derived.unreflected_bytes = Some(serialized_bytes(&entries[anchor.map_or(0, |index| index + 1)..]));
    derived
}

/// [`derive_state`] for an already-counted number of completed steps.
pub(crate) fn derive_state_with_total(
    state: &ReflectionTranscriptState,
    total_completed_steps: usize,
) -> ReflectionTranscriptState {
    let reflected_completed_steps = state.reflected_completed_steps.min(total_completed_steps);
    let steps_since_last_successful_reflection =
        total_completed_steps.saturating_sub(reflected_completed_steps);
    ReflectionTranscriptState {
        schema_version: REFLECTION_STATE_SCHEMA_VERSION.to_string(),
        reflected_through_message_id: state.reflected_through_message_id.clone(),
        reflected_through_byte_offset: state.reflected_through_byte_offset,
        unreflected_bytes: state.unreflected_bytes,
        total_completed_steps,
        reflected_completed_steps,
        steps_since_last_successful_reflection,
        last_reflection_started_at: state.last_reflection_started_at.clone(),
        last_reflection_succeeded_at: state.last_reflection_succeeded_at.clone(),
        pending_compaction: state.pending_compaction,
        consecutive_failures: state.consecutive_failures,
        next_eligible_at: state.next_eligible_at.clone(),
    }
}

/// Capture a slice of unreflected entries bounded by canonical messages.
pub fn capture_cursor_snapshot_with_max_bytes(
    entries: &[TranscriptEntry],
    state: &ReflectionTranscriptState,
    max_bytes: Option<u64>,
) -> Option<ReflectionSnapshot> {
    let max_bytes = max_bytes.map_or(REFLECTION_SNAPSHOT_MAX_BYTES, |value| {
        value.min(usize::MAX as u64) as usize
    });
    let anchor_index = state
        .reflected_through_message_id
        .as_deref()
        .and_then(|message_id| reflected_cursor_index(entries, message_id));

    let search_start = match anchor_index {
        Some(idx) => idx + 1,
        None => 0,
    };

    let start_index = entries[search_start..]
        .iter()
        .position(is_canonical_entry)
        .map(|rel| search_start + rel)?;

    let start = &entries[start_index];
    let mut bytes = serialized_bytes(&entries[search_start..start_index]);
    let mut end_id: Option<String> = None;
    let mut end_line = 0;
    let mut exhausted = false;
    let mut index = start_index;
    while index < entries.len() {
        bytes += entry_bytes(&entries[index]);
        if is_canonical_entry(&entries[index]) {
            let id = entries[index].source_message_id();
            let mut group_end = index;
            let mut group_bytes = bytes;
            while group_end + 1 < entries.len()
                && entries[group_end + 1].source_message_id() == id
            {
                group_end += 1;
                group_bytes += entry_bytes(&entries[group_end]);
            }
            if group_bytes > max_bytes && end_id.is_some() {
                exhausted = true;
                break;
            }
            end_id = Some(id.to_string());
            end_line = group_end + 1;
            bytes = group_bytes;
            index = group_end;
            if group_bytes >= max_bytes { exhausted = true; break; }
        }
        index += 1;
    }
    let end_message_id = end_id?;
    if !exhausted && bytes <= max_bytes {
        end_line = entries.len();
    }
    let slice_start = search_start;
    Some(ReflectionSnapshot {
        start_message_id: start.source_message_id().to_string(),
        end_message_id,
        start_line: slice_start,
        end_snapshot_line: end_line,
        entries: entries[slice_start..end_line].to_vec(),
        backlog_remaining: (entries.len() > end_line).then_some(entries.len() - end_line),
    })
}

pub fn capture_cursor_snapshot(
    entries: &[TranscriptEntry],
    state: &ReflectionTranscriptState,
) -> Option<ReflectionSnapshot> {
    capture_cursor_snapshot_with_max_bytes(entries, state, None)
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
    updated.reflected_through_byte_offset =
        Some(reflected_through_byte_offset(entries, &snapshot.end_message_id));
    updated.consecutive_failures = Some(0);
    updated.next_eligible_at = None;
    updated.reflected_completed_steps = count_completed_steps(snapshot_entries);
    updated.last_reflection_succeeded_at = Some(succeeded_at.to_string());
    derive_state_with_entries(&updated, entries)
}

/// Construct the initial empty reflection state.
pub fn initial_reflection_state() -> ReflectionTranscriptState {
    ReflectionTranscriptState {
        schema_version: REFLECTION_STATE_SCHEMA_VERSION.to_string(),
        reflected_through_message_id: None,
        reflected_through_byte_offset: None,
        unreflected_bytes: None,
        total_completed_steps: 0,
        reflected_completed_steps: 0,
        steps_since_last_successful_reflection: 0,
        last_reflection_started_at: None,
        last_reflection_succeeded_at: None,
        pending_compaction: None,
        consecutive_failures: None,
        next_eligible_at: None,
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
    let reflected_through_byte_offset = obj
        .get("reflected_through_byte_offset")
        .and_then(serde_json::Value::as_u64)
        .map(|value| value as usize);
    let unreflected_bytes = obj
        .get("unreflected_bytes")
        .and_then(serde_json::Value::as_u64)
        .map(|value| value as usize);
    Ok(ReflectionTranscriptState {
        schema_version: REFLECTION_STATE_SCHEMA_VERSION.to_string(),
        reflected_through_message_id,
        reflected_through_byte_offset,
        unreflected_bytes,
        total_completed_steps,
        reflected_completed_steps,
        steps_since_last_successful_reflection,
        last_reflection_started_at,
        last_reflection_succeeded_at,
        pending_compaction,
        consecutive_failures: obj.get("consecutive_failures").and_then(|v| v.as_u64()).and_then(|v| u32::try_from(v).ok()),
        next_eligible_at: obj.get("next_eligible_at").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(String::from),
    })
}

#[cfg(test)]
#[path = "cursor_tests.rs"]
mod tests;
