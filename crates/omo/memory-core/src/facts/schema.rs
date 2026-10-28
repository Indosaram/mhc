//! Facts queue wire format and path helpers.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::identity::MemoryIdentityPaths;
use crate::journal::cursor::is_canonical_entry;
use crate::journal::entries::TranscriptEntry;
use crate::support::sha256::sha256_hex;

/// Current schema version for facts queue files.
pub const FACTS_QUEUE_VERSION: u32 = 1;

/// Message id range and snapshot boundaries for a queued batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsQueueRange {
    pub start_message_id: String,
    pub end_message_id: String,
    pub start_line: u64,
    pub end_snapshot_line: u64,
}

/// Durable entry in the facts queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsQueueEntry {
    pub version: u32,
    pub identity: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "conversationId")]
    pub conversation_id: String,
    pub range: FactsQueueRange,
    #[serde(rename = "enqueuedAt")]
    pub enqueued_at: String,
    pub entries: Vec<TranscriptEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead: Option<TranscriptEntry>,
}

impl AsRef<FactsQueueEntry> for FactsQueueEntry {
    fn as_ref(&self) -> &FactsQueueEntry {
        self
    }
}

/// Per-conversation watermark cursor for enqueued and consumed messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsCursor {
    pub version: u32,
    pub enqueued_through_message_id: Option<String>,
    pub enqueued_through_snapshot_line: i64,
    pub consumed_through_message_id: Option<String>,
    pub consumed_through_snapshot_line: i64,
}

/// Consumed watermark record for one conversation endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsConsumedRecord {
    pub end_message_id: String,
    pub end_snapshot_line: i64,
    #[serde(rename = "consumedAt")]
    pub consumed_at: String,
}

/// Consumed watermark tracking file contents across conversations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsConsumedWatermark {
    pub version: u32,
    pub consumed: BTreeMap<String, FactsConsumedRecord>,
}

/// Filesystem layout for facts queue, cursors, and metadata files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactsQueueLayout {
    pub queue_dir: PathBuf,
    pub parked_dir: PathBuf,
    pub cursor_dir: PathBuf,
    pub consumed_path: PathBuf,
    pub failures_path: PathBuf,
}

impl FactsQueueLayout {
    /// Construct a layout rooted at `queue_dir`.
    pub fn new(queue_dir: PathBuf) -> Self {
        let cursor_dir = queue_dir.join("cursor");
        let consumed_path = queue_dir.join("consumed.json");
        let failures_path = queue_dir.join("failures.json");
        Self {
            parked_dir: queue_dir.join("parked"),
            queue_dir,
            cursor_dir,
            consumed_path,
            failures_path,
        }
    }

    /// Path to a conversation's cursor file.
    pub fn cursor_path(&self, conversation_id: &str) -> PathBuf {
        let hash_id = hash_prefix(conversation_id, 12);
        self.cursor_dir.join(format!("{hash_id}.json"))
    }

    /// Path to a queue entry file given its conversation, endpoint, and enqueue time.
    pub fn entry_path(&self, conversation_id: &str, end_message_id: &str, at: &str) -> PathBuf {
        let ts = queue_timestamp(at);
        let conv_hash = hash_prefix(conversation_id, 12);
        let end_hash = hash_prefix(end_message_id, 8);
        self.queue_dir
            .join(format!("{ts}-{conv_hash}-{end_hash}.json"))
    }
}

/// Build queue layout from memory identity paths.
pub fn facts_queue_paths(identity_paths: &MemoryIdentityPaths) -> FactsQueueLayout {
    FactsQueueLayout::new(identity_paths.facts_queue.clone())
}

/// Colon-free UTC timestamp string (`YYYYMMDDTHHMMSSmmmZ`).
pub fn queue_timestamp(at: &str) -> String {
    at.replace(['-', ':', '.'], "")
}

/// Default cursor state when uninitialized or corrupted.
pub fn initial_cursor() -> FactsCursor {
    FactsCursor {
        version: FACTS_QUEUE_VERSION,
        enqueued_through_message_id: None,
        enqueued_through_snapshot_line: -1,
        consumed_through_message_id: None,
        consumed_through_snapshot_line: -1,
    }
}

/// Parse a raw cursor file, falling back to initial cursor on corruption.
pub fn parse_cursor(raw: &str) -> FactsCursor {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(raw) else {
        return initial_cursor();
    };
    if !val.is_object()
        || val.get("version").and_then(|v| v.as_u64()) != Some(FACTS_QUEUE_VERSION as u64)
    {
        return initial_cursor();
    }
    let enqueued_through_message_id = val
        .get("enqueued_through_message_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let enqueued_through_snapshot_line = val
        .get("enqueued_through_snapshot_line")
        .and_then(|v| v.as_i64())
        .filter(|&n| n >= 0)
        .unwrap_or(-1);
    let consumed_through_message_id = val
        .get("consumed_through_message_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let consumed_through_snapshot_line = val
        .get("consumed_through_snapshot_line")
        .and_then(|v| v.as_i64())
        .filter(|&n| n >= 0)
        .unwrap_or(-1);

    FactsCursor {
        version: FACTS_QUEUE_VERSION,
        enqueued_through_message_id,
        enqueued_through_snapshot_line,
        consumed_through_message_id,
        consumed_through_snapshot_line,
    }
}

/// Parse consumed watermark file, returning an empty watermark on error.
pub fn parse_consumed(raw: &str) -> FactsConsumedWatermark {
    let empty = FactsConsumedWatermark {
        version: FACTS_QUEUE_VERSION,
        consumed: BTreeMap::new(),
    };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(raw) else {
        return empty;
    };
    if !val.is_object()
        || val.get("version").and_then(|v| v.as_u64()) != Some(FACTS_QUEUE_VERSION as u64)
    {
        return empty;
    }
    let Some(consumed_val) = val.get("consumed").and_then(|v| v.as_object()) else {
        return empty;
    };
    let mut consumed = BTreeMap::new();
    for (conv_id, item) in consumed_val {
        if !item.is_object() {
            continue;
        }
        let Some(end_message_id) = item
            .get("end_message_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let Some(consumed_at) = item
            .get("consumedAt")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let end_snapshot_line = item
            .get("end_snapshot_line")
            .and_then(|v| v.as_i64())
            .filter(|&n| n >= 0)
            .unwrap_or(-1);
        consumed.insert(
            conv_id.clone(),
            FactsConsumedRecord {
                end_message_id: end_message_id.to_string(),
                end_snapshot_line,
                consumed_at: consumed_at.to_string(),
            },
        );
    }
    FactsConsumedWatermark {
        version: FACTS_QUEUE_VERSION,
        consumed,
    }
}

/// Parse a queue entry file, returning None if corrupt or invalid.
pub fn parse_queue_entry(raw: &str) -> Option<FactsQueueEntry> {
    let val: serde_json::Value = serde_json::from_str(raw).ok()?;
    if !val.is_object()
        || val.get("version").and_then(|v| v.as_u64()) != Some(FACTS_QUEUE_VERSION as u64)
    {
        return None;
    }
    let identity = val
        .get("identity")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())?
        .to_string();
    let session_id = val
        .get("sessionId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())?
        .to_string();
    let conversation_id = val
        .get("conversationId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())?
        .to_string();
    let enqueued_at = val
        .get("enqueuedAt")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())?
        .to_string();

    let range_val = val.get("range")?.as_object()?;
    let start_message_id = range_val
        .get("start_message_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())?
        .to_string();
    let end_message_id = range_val
        .get("end_message_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())?
        .to_string();
    let start_line = range_val.get("start_line").and_then(|v| v.as_u64())?;
    let end_snapshot_line = range_val
        .get("end_snapshot_line")
        .and_then(|v| v.as_u64())?;

    let entries_val = val.get("entries")?.as_array()?;
    let mut entries = Vec::with_capacity(entries_val.len());
    for item in entries_val {
        if !is_transcript_entry_json(item) {
            return None;
        }
        let entry = <TranscriptEntry as Deserialize>::deserialize(item).ok()?;
        entries.push(entry);
    }

    Some(FactsQueueEntry {
        version: FACTS_QUEUE_VERSION,
        identity,
        session_id,
        conversation_id,
        range: FactsQueueRange {
            start_message_id,
            end_message_id,
            start_line,
            end_snapshot_line,
        },
        enqueued_at,
        lead: match val.get("lead") {
            Some(value) => {
                if !is_transcript_entry_json(value) {
                    return None;
                }
                Some(serde_json::from_value(value.clone()).ok()?)
            }
            None => None,
        },
        entries,
    })
}

struct ValidatedEntries;

struct EntriesValidatorVisitor;

impl<'de> serde::de::Visitor<'de> for EntriesValidatorVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an array of transcript entries")
    }

    fn visit_seq<S: serde::de::SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
        while let Some(item) = seq.next_element::<serde_json::Value>()? {
            if !is_transcript_entry_json(&item) {
                return Err(serde::de::Error::custom(
                    "invalid transcript entry structure",
                ));
            }
            if <TranscriptEntry as Deserialize>::deserialize(&item).is_err() {
                return Err(serde::de::Error::custom(
                    "failed to deserialize transcript entry",
                ));
            }
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for ValidatedEntries {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_seq(EntriesValidatorVisitor)?;
        Ok(ValidatedEntries)
    }
}

struct QueueHeaderReceiver(FactsQueueEntry);

struct QueueHeaderVisitor;

impl<'de> serde::de::Visitor<'de> for QueueHeaderVisitor {
    type Value = FactsQueueEntry;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a facts queue entry JSON object")
    }

    fn visit_map<M: serde::de::MapAccess<'de>>(
        self,
        mut access: M,
    ) -> Result<Self::Value, M::Error> {
        let mut version: Option<u32> = None;
        let mut identity: Option<String> = None;
        let mut session_id: Option<String> = None;
        let mut conversation_id: Option<String> = None;
        let mut range: Option<FactsQueueRange> = None;
        let mut enqueued_at: Option<String> = None;
        let mut lead = None;
        let mut seen_entries = false;

        while let Some(key) = access.next_key::<String>()? {
            match key.as_str() {
                "version" => {
                    let val: serde_json::Value = access.next_value()?;
                    if val.as_u64() != Some(FACTS_QUEUE_VERSION as u64) {
                        return Err(serde::de::Error::custom("invalid facts queue version"));
                    }
                    version = Some(FACTS_QUEUE_VERSION);
                }
                "identity" => {
                    let val: serde_json::Value = access.next_value()?;
                    let s = val
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| serde::de::Error::custom("invalid identity"))?;
                    identity = Some(s.to_string());
                }
                "sessionId" => {
                    let val: serde_json::Value = access.next_value()?;
                    let s = val
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| serde::de::Error::custom("invalid sessionId"))?;
                    session_id = Some(s.to_string());
                }
                "conversationId" => {
                    let val: serde_json::Value = access.next_value()?;
                    let s = val
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| serde::de::Error::custom("invalid conversationId"))?;
                    conversation_id = Some(s.to_string());
                }
                "range" => {
                    let val: serde_json::Value = access.next_value()?;
                    let range_obj = val
                        .as_object()
                        .ok_or_else(|| serde::de::Error::custom("range must be an object"))?;
                    let start_message_id = range_obj
                        .get("start_message_id")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| serde::de::Error::custom("invalid start_message_id"))?
                        .to_string();
                    let end_message_id = range_obj
                        .get("end_message_id")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| serde::de::Error::custom("invalid end_message_id"))?
                        .to_string();
                    let start_line = range_obj
                        .get("start_line")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| serde::de::Error::custom("invalid start_line"))?;
                    let end_snapshot_line = range_obj
                        .get("end_snapshot_line")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| serde::de::Error::custom("invalid end_snapshot_line"))?;
                    range = Some(FactsQueueRange {
                        start_message_id,
                        end_message_id,
                        start_line,
                        end_snapshot_line,
                    });
                }
                "enqueuedAt" => {
                    let val: serde_json::Value = access.next_value()?;
                    let s = val
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| serde::de::Error::custom("invalid enqueuedAt"))?;
                    enqueued_at = Some(s.to_string());
                }
                "lead" => {
                    let value: serde_json::Value = access.next_value()?;
                    if !is_transcript_entry_json(&value) {
                        return Err(serde::de::Error::custom("invalid lead"));
                    }
                    lead = Some(
                        <TranscriptEntry as Deserialize>::deserialize(&value)
                            .map_err(serde::de::Error::custom)?,
                    );
                }
                "entries" => {
                    access.next_value::<ValidatedEntries>()?;
                    seen_entries = true;
                }
                _ => {
                    access.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }

        if !seen_entries {
            return Err(serde::de::Error::custom("missing entries"));
        }
        let Some(version) = version else {
            return Err(serde::de::Error::custom("missing version"));
        };
        let Some(identity) = identity else {
            return Err(serde::de::Error::custom("missing identity"));
        };
        let Some(session_id) = session_id else {
            return Err(serde::de::Error::custom("missing sessionId"));
        };
        let Some(conversation_id) = conversation_id else {
            return Err(serde::de::Error::custom("missing conversationId"));
        };
        let Some(range) = range else {
            return Err(serde::de::Error::custom("missing range"));
        };
        let Some(enqueued_at) = enqueued_at else {
            return Err(serde::de::Error::custom("missing enqueuedAt"));
        };

        Ok(FactsQueueEntry {
            version,
            identity,
            session_id,
            conversation_id,
            range,
            enqueued_at,
            entries: Vec::new(),
            lead,
        })
    }
}

impl<'de> Deserialize<'de> for QueueHeaderReceiver {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer
            .deserialize_map(QueueHeaderVisitor)
            .map(QueueHeaderReceiver)
    }
}

/// Stream-parse a queue entry file header without retaining transcript payloads in memory.
///
/// Validates the header fields and each transcript entry in the `entries` array using the
/// same rules as [`parse_queue_entry`], returning None if corrupt, invalid, or version mismatch.
///
/// Note: Duplicate JSON keys are rejected (single-pass) while `parse_queue_entry` keeps the
/// last value; publish never writes duplicates.
pub fn parse_queue_header<R: Read>(reader: R) -> Option<FactsQueueEntry> {
    serde_json::from_reader::<R, QueueHeaderReceiver>(reader)
        .ok()
        .map(|r| r.0)
}

/// Positional index of a message id among canonical transcript entries.
pub fn canonical_position(entries: &[TranscriptEntry], message_id: Option<&str>) -> i64 {
    let Some(target_id) = message_id else {
        return -1;
    };
    for (index, entry) in entries.iter().enumerate().rev() {
        if is_canonical_entry(entry) && entry.source_message_id() == target_id {
            return index as i64;
        }
    }
    -1
}

fn hash_prefix(value: &str, length: usize) -> String {
    let full = sha256_hex(value.as_bytes());
    full[..length.min(full.len())].to_string()
}

fn is_transcript_entry_json(val: &serde_json::Value) -> bool {
    let Some(obj) = val.as_object() else {
        return false;
    };
    let Some(kind) = obj.get("kind").and_then(|v| v.as_str()) else {
        return false;
    };
    let has_captured_at = obj.get("captured_at").and_then(|v| v.as_str()).is_some();
    let has_source_line = obj.get("source_line_id").and_then(|v| v.as_str()).is_some();
    let has_source_msg = obj
        .get("source_message_id")
        .and_then(|v| v.as_str())
        .is_some();
    if !has_captured_at || !has_source_line || !has_source_msg {
        return false;
    }
    if kind == "tool_call" {
        return true;
    }
    matches!(kind, "user" | "assistant" | "reasoning" | "error")
        && obj.get("text").and_then(|v| v.as_str()).is_some()
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod tests;

pub fn queue_conversation_hash(conversation_id: &str) -> String {
    hash_prefix(conversation_id, 12)
}
pub fn queue_entry_suffix(conversation_id: &str, end_message_id: &str) -> String {
    format!(
        "-{}-{}.json",
        queue_conversation_hash(conversation_id),
        hash_prefix(end_message_id, 8)
    )
}
