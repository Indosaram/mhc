//! Transcript cursors: how far reflection has read, and which slice to hand it next.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-cursor.md`. Reflection is incremental, so the store has
//! to remember a position rather than a set of entries. The position is a message id, not a line number:
//! the transcript is rewritten by compaction, so a line number would silently point at different content
//! afterwards and a reflection could skip a whole conversation or read one twice.

use serde::Serialize;

/// Marks which step-counting rule produced a stored state.
///
/// Bumped because the rule changed: an older value means the counters were computed by the previous
/// rule and must not be compared against these.
pub const REFLECTION_STATE_SCHEMA_VERSION: &str = "v3_assistant_steps";

pub const REFLECTION_SNAPSHOT_MAX_BYTES: usize = 131_072;

/// Serialized as a JSON object, which is what the journal on disk holds. Assistant text lines are the
/// unit of progress: one assistant message with content is one completed step.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TranscriptEntry {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub captured_at: String,
    pub source_line_id: String,
    pub source_message_id: String,
}

impl TranscriptEntry {
    pub fn is_canonical(&self) -> bool {
        self.kind == "assistant"
            && !self.source_message_id.is_empty()
            && self
                .text
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty())
    }
}

fn entry_bytes(entry: &TranscriptEntry) -> usize {
    serde_json::to_vec(entry).map_or(0, |json| json.len() + 1)
}

fn serialized_bytes(entries: &[TranscriptEntry]) -> usize {
    entries.iter().map(entry_bytes).sum()
}

fn reflected_cursor_index(entries: &[TranscriptEntry], message_id: &str) -> Option<usize> {
    entries
        .iter()
        .rposition(|entry| entry.source_message_id == message_id)
}

/// Bytes from the start of the transcript through the last entry of `message_id`.
///
/// Zero when the id is absent, which is the honest answer: nothing is known to be reflected.
pub fn reflected_through_byte_offset(entries: &[TranscriptEntry], message_id: &str) -> usize {
    let mut offset = 0;
    let mut reflected = 0;
    for entry in entries {
        offset += entry_bytes(entry);
        if entry.source_message_id == message_id {
            reflected = offset;
        }
    }
    reflected
}

pub fn journal_byte_offset(entries: &[TranscriptEntry]) -> usize {
    serialized_bytes(entries)
}

pub fn count_completed_steps(entries: &[TranscriptEntry]) -> usize {
    entries.iter().filter(|entry| entry.is_canonical()).count()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct ReflectionSnapshot {
    pub start_message_id: String,
    pub end_message_id: String,
    pub start_line: usize,
    pub end_snapshot_line: usize,
    pub entries: Vec<TranscriptEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backlog_remaining: Option<usize>,
}

/// Recompute the counters after the transcript changed.
///
/// `total_completed_steps` is derived from the entries, never incremented, so a retried write or a
/// replanted journal cannot inflate progress. `reflected_completed_steps` is clamped to the total
/// because compaction can remove reflected messages, and a cursor pointing past the end would make
/// `steps_since_last_successful_reflection` underflow.
pub fn derive_state(
    state: &ReflectionTranscriptState,
    entries: &[TranscriptEntry],
) -> ReflectionTranscriptState {
    let mut derived = derive_state_with_total(state, count_completed_steps(entries));
    let anchor = state
        .reflected_through_message_id
        .as_deref()
        .and_then(|message_id| reflected_cursor_index(entries, message_id));
    derived.unreflected_bytes = Some(serialized_bytes(
        &entries[anchor.map_or(0, |index| index + 1)..],
    ));
    derived
}

pub fn derive_state_with_total(
    state: &ReflectionTranscriptState,
    total_completed_steps: usize,
) -> ReflectionTranscriptState {
    let reflected_completed_steps = state.reflected_completed_steps.min(total_completed_steps);
    ReflectionTranscriptState {
        schema_version: REFLECTION_STATE_SCHEMA_VERSION.to_owned(),
        reflected_through_message_id: state.reflected_through_message_id.clone(),
        reflected_through_byte_offset: state.reflected_through_byte_offset,
        unreflected_bytes: state.unreflected_bytes,
        total_completed_steps,
        reflected_completed_steps,
        steps_since_last_successful_reflection: total_completed_steps - reflected_completed_steps,
        last_reflection_started_at: state.last_reflection_started_at.clone(),
        last_reflection_succeeded_at: state.last_reflection_succeeded_at.clone(),
        pending_compaction: state.pending_compaction,
        consecutive_failures: state.consecutive_failures,
        next_eligible_at: state.next_eligible_at.clone(),
    }
}

pub fn capture_cursor_snapshot(
    entries: &[TranscriptEntry],
    state: &ReflectionTranscriptState,
) -> Option<ReflectionSnapshot> {
    capture_cursor_snapshot_with_max_bytes(entries, state, None)
}

/// Capture the next slice of unreflected work, never splitting an assistant message's own lines.
///
/// The cut is at a message boundary because the extractor reads each captured conversation as one
/// contiguous span; splitting one message across two runs would show the reader half a message and
/// attribute steps to the wrong run. A single group larger than the budget is still returned when it is
/// the first group — refusing it would stall reflection forever on one long message.
pub fn capture_cursor_snapshot_with_max_bytes(
    entries: &[TranscriptEntry],
    state: &ReflectionTranscriptState,
    max_bytes: Option<u64>,
) -> Option<ReflectionSnapshot> {
    let max_bytes = max_bytes.map_or(REFLECTION_SNAPSHOT_MAX_BYTES, |value| {
        value.min(usize::MAX as u64) as usize
    });
    let search_start = match state
        .reflected_through_message_id
        .as_deref()
        .and_then(|message_id| reflected_cursor_index(entries, message_id))
    {
        Some(index) => index + 1,
        None => 0,
    };

    let start_index = entries[search_start..]
        .iter()
        .position(TranscriptEntry::is_canonical)
        .map(|relative| search_start + relative)?;
    let start = &entries[start_index];

    let mut bytes = serialized_bytes(&entries[search_start..start_index]);
    let mut end_id: Option<String> = None;
    let mut end_line = 0;
    let mut exhausted = false;
    let mut index = start_index;
    while index < entries.len() {
        bytes += entry_bytes(&entries[index]);
        if entries[index].is_canonical() {
            let id = entries[index].source_message_id.clone();
            let mut group_end = index;
            let mut group_bytes = bytes;
            while group_end + 1 < entries.len() && entries[group_end + 1].source_message_id == id {
                group_end += 1;
                group_bytes += entry_bytes(&entries[group_end]);
            }
            if group_bytes > max_bytes && end_id.is_some() {
                exhausted = true;
                break;
            }
            end_id = Some(id);
            end_line = group_end + 1;
            bytes = group_bytes;
            index = group_end;
            if group_bytes >= max_bytes {
                exhausted = true;
                break;
            }
        }
        index += 1;
    }
    let end_message_id = end_id?;
    if !exhausted && bytes <= max_bytes {
        end_line = entries.len();
    }
    Some(ReflectionSnapshot {
        start_message_id: start.source_message_id.clone(),
        end_message_id,
        start_line: search_start,
        end_snapshot_line: end_line,
        entries: entries[search_start..end_line].to_vec(),
        backlog_remaining: (entries.len() > end_line).then(|| entries.len() - end_line),
    })
}

/// Advance the cursor after a run, or only recount after a failure.
///
/// A failed run must not move the cursor: the same slice has to be readable again on the retry, and
/// moving past it would mean the work is lost with the failure. Success also zeroes the failure counters
/// so a recovered conversation is not held back by its own history.
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
    let mut updated = state.clone();
    updated.reflected_through_message_id = Some(snapshot.end_message_id.clone());
    updated.reflected_through_byte_offset = Some(reflected_through_byte_offset(
        entries,
        &snapshot.end_message_id,
    ));
    updated.consecutive_failures = Some(0);
    updated.next_eligible_at = None;
    updated.reflected_completed_steps = count_completed_steps(&entries[..end_line]);
    updated.last_reflection_succeeded_at = Some(succeeded_at.to_owned());
    derive_state(&updated, entries)
}

pub fn initial_reflection_state() -> ReflectionTranscriptState {
    ReflectionTranscriptState {
        schema_version: REFLECTION_STATE_SCHEMA_VERSION.to_owned(),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorError(pub String);

impl std::fmt::Display for CursorError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for CursorError {}

/// Read a stored state, tolerating a missing or unusable field but not a non-object.
///
/// Every absent field falls back to its empty value because a partial write must not make the whole
/// conversation unreflective; a non-object does fail, because that is not a state at all. Negative and
/// non-numeric counters are coerced to zero rather than failing, since a corrupt counter would otherwise
/// block reflection on that conversation permanently.
pub fn parse_state(raw: &str) -> Result<ReflectionTranscriptState, CursorError> {
    let value: serde_json::Value = serde_json::from_str(raw)
        .map_err(|error| CursorError(format!("corrupt reflection state: {error}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| CursorError("state is not a JSON object".to_owned()))?;

    let text = |key: &str| {
        object
            .get(key)
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let count = |key: &str| {
        object
            .get(key)
            .and_then(|value| value.as_i64())
            .filter(|value| *value >= 0)
            .unwrap_or(0) as usize
    };

    Ok(ReflectionTranscriptState {
        schema_version: REFLECTION_STATE_SCHEMA_VERSION.to_owned(),
        reflected_through_message_id: text("reflected_through_message_id"),
        reflected_through_byte_offset: object
            .get("reflected_through_byte_offset")
            .and_then(serde_json::Value::as_u64)
            .map(|value| value as usize),
        unreflected_bytes: object
            .get("unreflected_bytes")
            .and_then(serde_json::Value::as_u64)
            .map(|value| value as usize),
        total_completed_steps: count("total_completed_steps"),
        reflected_completed_steps: count("reflected_completed_steps"),
        steps_since_last_successful_reflection: count("steps_since_last_successful_reflection"),
        last_reflection_started_at: text("last_reflection_started_at"),
        last_reflection_succeeded_at: text("last_reflection_succeeded_at"),
        pending_compaction: object
            .get("pending_compaction")
            .and_then(serde_json::Value::as_bool),
        consecutive_failures: object
            .get("consecutive_failures")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok()),
        next_eligible_at: text("next_eligible_at"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(message: &str, text: &str) -> TranscriptEntry {
        TranscriptEntry {
            kind: "assistant".to_owned(),
            text: Some(text.to_owned()),
            captured_at: "2026-01-01T00:00:00Z".to_owned(),
            source_line_id: format!("line-{message}"),
            source_message_id: message.to_owned(),
        }
    }

    fn user(message: &str) -> TranscriptEntry {
        TranscriptEntry {
            kind: "user".to_owned(),
            text: Some("hello".to_owned()),
            captured_at: "2026-01-01T00:00:00Z".to_owned(),
            source_line_id: format!("line-{message}"),
            source_message_id: message.to_owned(),
        }
    }

    fn tool_call(message: &str) -> TranscriptEntry {
        TranscriptEntry {
            kind: "tool_call".to_owned(),
            text: None,
            captured_at: "2026-01-01T00:00:00Z".to_owned(),
            source_line_id: format!("line-{message}"),
            source_message_id: message.to_owned(),
        }
    }

    #[test]
    fn only_assistant_text_is_a_completed_step() {
        let entries = vec![
            user("m1"),
            assistant("m2", "done"),
            tool_call("m2"),
            assistant("m3", "   "),
            assistant("m4", "another"),
        ];
        assert_eq!(count_completed_steps(&entries), 2);
    }

    #[test]
    fn a_failure_does_not_move_the_cursor() {
        let entries = vec![assistant("m1", "one"), assistant("m2", "two")];
        let state = initial_reflection_state();
        let snapshot = capture_cursor_snapshot(&entries, &state).expect("captures");
        let after = finalize_cursor(&state, &entries, &snapshot, false, "2026-01-01T00:00:00Z");
        assert_eq!(after.reflected_through_message_id, None);
        assert_eq!(after.reflected_completed_steps, 0);
    }

    #[test]
    fn a_success_advances_past_the_captured_slice() {
        let entries = vec![assistant("m1", "one"), assistant("m2", "two")];
        let state = initial_reflection_state();
        let snapshot = capture_cursor_snapshot(&entries, &state).expect("captures");
        let after = finalize_cursor(&state, &entries, &snapshot, true, "2026-01-01T00:00:00Z");
        assert_eq!(after.reflected_through_message_id.as_deref(), Some("m2"));
        assert_eq!(after.reflected_completed_steps, 2);
        assert_eq!(after.steps_since_last_successful_reflection, 0);
        assert_eq!(after.consecutive_failures, Some(0));
        assert_eq!(after.next_eligible_at, None);
    }

    #[test]
    fn a_later_capture_starts_after_the_reflected_message() {
        let entries = vec![
            assistant("m1", "one"),
            assistant("m2", "two"),
            assistant("m3", "three"),
        ];
        let mut state = initial_reflection_state();
        state.reflected_through_message_id = Some("m2".to_owned());
        let snapshot = capture_cursor_snapshot(&entries, &state).expect("captures");
        assert_eq!(snapshot.start_message_id, "m3");
        assert_eq!(snapshot.entries.len(), 1);
    }

    #[test]
    fn nothing_unreflected_captures_nothing() {
        let entries = vec![assistant("m1", "one")];
        let mut state = initial_reflection_state();
        state.reflected_through_message_id = Some("m1".to_owned());
        assert!(capture_cursor_snapshot(&entries, &state).is_none());
    }

    #[test]
    fn an_empty_transcript_captures_nothing() {
        assert!(capture_cursor_snapshot(&[], &initial_reflection_state()).is_none());
    }

    #[test]
    fn the_capture_starts_at_the_first_assistant_message() {
        let entries = vec![user("m1"), tool_call("m1"), assistant("m2", "done")];
        let snapshot =
            capture_cursor_snapshot(&entries, &initial_reflection_state()).expect("captures");
        assert_eq!(snapshot.start_message_id, "m2");
        assert_eq!(snapshot.start_line, 0);
        assert!(snapshot.backlog_remaining.is_none());
    }

    #[test]
    fn the_budget_cuts_between_messages_not_inside_one() {
        let long = "x".repeat(400);
        let entries = vec![
            assistant("m1", &long),
            assistant("m2", &long),
            assistant("m3", &long),
        ];
        let snapshot = capture_cursor_snapshot_with_max_bytes(
            &entries,
            &initial_reflection_state(),
            Some(600),
        )
        .expect("captures");
        assert_eq!(snapshot.end_message_id, "m1");
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.backlog_remaining, Some(2));
    }

    #[test]
    fn one_oversized_first_message_is_still_captured() {
        let huge = "x".repeat(5000);
        let entries = vec![assistant("m1", &huge)];
        let snapshot =
            capture_cursor_snapshot_with_max_bytes(&entries, &initial_reflection_state(), Some(64))
                .expect("captures");
        assert_eq!(snapshot.end_message_id, "m1");
    }

    #[test]
    fn a_messages_own_lines_stay_in_one_group() {
        let entries = vec![
            assistant("m1", "one"),
            tool_call("m1"),
            assistant("m2", "two"),
        ];
        let snapshot =
            capture_cursor_snapshot_with_max_bytes(&entries, &initial_reflection_state(), Some(1))
                .expect("captures");
        assert_eq!(snapshot.end_message_id, "m1");
        assert!(snapshot.entries.len() >= 2);
    }

    #[test]
    fn a_fitting_transcript_is_captured_to_the_end() {
        let entries = vec![assistant("m1", "one"), assistant("m2", "two")];
        let snapshot =
            capture_cursor_snapshot(&entries, &initial_reflection_state()).expect("captures");
        assert_eq!(snapshot.end_snapshot_line, entries.len());
        assert!(snapshot.backlog_remaining.is_none());
    }

    #[test]
    fn deriving_clamps_a_cursor_past_the_end() {
        let entries = vec![assistant("m1", "one")];
        let mut state = initial_reflection_state();
        state.reflected_completed_steps = 9;
        let derived = derive_state(&state, &entries);
        assert_eq!(derived.total_completed_steps, 1);
        assert_eq!(derived.reflected_completed_steps, 1);
        assert_eq!(derived.steps_since_last_successful_reflection, 0);
    }

    #[test]
    fn deriving_reports_unreflected_bytes_after_the_anchor() {
        let entries = vec![assistant("m1", "one"), assistant("m2", "two")];
        let mut state = initial_reflection_state();
        state.reflected_through_message_id = Some("m1".to_owned());
        let derived = derive_state(&state, &entries);
        assert_eq!(derived.unreflected_bytes, Some(entry_bytes(&entries[1])));
    }

    #[test]
    fn an_unknown_anchor_means_all_bytes_are_unreflected() {
        let entries = vec![assistant("m1", "one")];
        let mut state = initial_reflection_state();
        state.reflected_through_message_id = Some("gone".to_owned());
        let derived = derive_state(&state, &entries);
        assert_eq!(derived.unreflected_bytes, Some(serialized_bytes(&entries)));
    }

    #[test]
    fn a_stored_state_round_trips() {
        let entries = vec![assistant("m1", "one")];
        let snapshot =
            capture_cursor_snapshot(&entries, &initial_reflection_state()).expect("captures");
        let state = finalize_cursor(
            &initial_reflection_state(),
            &entries,
            &snapshot,
            true,
            "then",
        );
        let raw = serde_json::to_string(&state).expect("serializes");
        let parsed = parse_state(&raw).expect("parses");
        assert_eq!(parsed.reflected_through_message_id.as_deref(), Some("m1"));
        assert_eq!(parsed.reflected_completed_steps, 1);
        assert_eq!(parsed.last_reflection_succeeded_at.as_deref(), Some("then"));
    }

    #[test]
    fn parsing_tolerates_missing_and_negative_counters() {
        let parsed = parse_state("{}").expect("parses");
        assert_eq!(parsed.total_completed_steps, 0);
        assert_eq!(parsed.reflected_completed_steps, 0);
        let parsed = parse_state(r#"{"total_completed_steps":-4}"#).expect("parses");
        assert_eq!(parsed.total_completed_steps, 0);
    }

    #[test]
    fn parsing_rejects_a_non_object() {
        assert!(parse_state("[]").is_err());
        assert!(parse_state("3").is_err());
        assert!(parse_state("not json").is_err());
    }

    #[test]
    fn parsing_ignores_an_empty_message_id() {
        let parsed = parse_state(r#"{"reflected_through_message_id":""}"#).expect("parses");
        assert_eq!(parsed.reflected_through_message_id, None);
    }

    #[test]
    fn the_stored_schema_version_is_the_current_rule() {
        assert_eq!(
            initial_reflection_state().schema_version,
            REFLECTION_STATE_SCHEMA_VERSION
        );
    }
}
