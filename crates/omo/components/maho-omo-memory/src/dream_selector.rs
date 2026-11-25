pub use crate::dream_scoring::*;
use memory_core::{
    journal::{
        cursor::{
            REFLECTION_SNAPSHOT_MAX_BYTES, capture_cursor_snapshot, derive_state,
            initial_reflection_state, is_canonical_entry,
        },
        entries::{TextTranscriptEntry, ToolCallTranscriptEntry, TranscriptEntry},
        store::JournalError,
    },
    search::{
        SearchDocument, SearchOptions, SearchToolCall, TranscriptConversation, TranscriptProvider,
        search_transcripts,
    },
};
use std::{
    borrow::Cow,
    collections::{BTreeSet, HashSet},
    path::Path,
};
pub struct DreamSelectorOptions<'a> {
    pub transcripts_dir: &'a Path,
    pub current_conversation_id: Option<&'a str>,
    pub auto_select_max: usize,
    pub auto_select_max_bytes: usize,
    pub now_ms: f64,
}
struct LoadedConversation {
    conversation_id: String,
    /// The journal rows, kept only when a focus search has to read them.
    entries: Option<Vec<TranscriptEntry>>,
    distinct_sources: usize,
    steps: usize,
    reflected_through: Option<String>,
    newest_message_id: String,
    last_activity: String,
    total_bytes: usize,
}
#[derive(Debug)]
pub enum DreamSelectorError {
    Io(std::io::Error),
    Journal(JournalError),
}
impl std::fmt::Display for DreamSelectorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Journal(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for DreamSelectorError {}
impl From<std::io::Error> for DreamSelectorError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<JournalError> for DreamSelectorError {
    fn from(error: JournalError) -> Self {
        Self::Journal(error)
    }
}
fn activity(value: &str) -> f64 {
    memory_core::support::time::parse_rfc3339(value).map_or(f64::NAN, |millis| millis as f64)
}
/// Length of the non-empty entry texts joined by `\n` (a tool call joins its args and result the same
/// way), counted without building the joined string.
fn conversation_byte_length(entries: &[TranscriptEntry]) -> usize {
    let mut total = 0;
    let mut parts = 0usize;
    for entry in entries {
        let len = match entry {
            TranscriptEntry::Text(entry) => entry.text.len(),
            TranscriptEntry::ToolCall(entry) => {
                match (entry.args_text.as_deref(), entry.result_text.as_deref()) {
                    (Some(args), Some(result)) => args.len() + 1 + result.len(),
                    (Some(text), None) | (None, Some(text)) => text.len(),
                    (None, None) => 0,
                }
            }
        };
        if len > 0 {
            total += len;
            parts += 1;
        }
    }
    total + parts.saturating_sub(1)
}
/// Field summary for single-pass unlocked journal scanning without allocating message bodies.
enum FieldProbe {
    Str { blank: bool },
    Bool,
    Null,
    Other,
}
impl<'de> serde::Deserialize<'de> for FieldProbe {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ProbeVisitor;
        impl<'v> serde::de::Visitor<'v> for ProbeVisitor {
            type Value = FieldProbe;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<FieldProbe, E> {
                Ok(FieldProbe::Str {
                    blank: v.trim().is_empty(),
                })
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<FieldProbe, E> {
                Ok(FieldProbe::Bool)
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<FieldProbe, E> {
                Ok(FieldProbe::Other)
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<FieldProbe, E> {
                Ok(FieldProbe::Other)
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<FieldProbe, E> {
                Ok(FieldProbe::Other)
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<FieldProbe, E> {
                Ok(FieldProbe::Null)
            }
            fn visit_seq<A: serde::de::SeqAccess<'v>>(
                self,
                mut seq: A,
            ) -> Result<FieldProbe, A::Error> {
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(FieldProbe::Other)
            }
            fn visit_map<A: serde::de::MapAccess<'v>>(
                self,
                mut map: A,
            ) -> Result<FieldProbe, A::Error> {
                while map
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(FieldProbe::Other)
            }
        }
        deserializer.deserialize_any(ProbeVisitor)
    }
}
#[derive(serde::Deserialize)]
struct RowProbe<'a> {
    #[serde(borrow)]
    kind: Cow<'a, str>,
    #[serde(borrow)]
    captured_at: Cow<'a, str>,
    #[serde(borrow)]
    source_line_id: Cow<'a, str>,
    #[serde(borrow)]
    source_message_id: Cow<'a, str>,
    #[serde(default)]
    text: Option<FieldProbe>,
    #[serde(default)]
    name: Option<FieldProbe>,
    #[serde(rename = "argsText", default)]
    args_text: Option<FieldProbe>,
    #[serde(rename = "resultText", default)]
    result_text: Option<FieldProbe>,
    #[serde(rename = "resultOk", default)]
    result_ok: Option<FieldProbe>,
}
fn parse_row_probe(line: &str) -> Option<RowProbe<'_>> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('{') {
        return None;
    }
    let row: RowProbe<'_> = serde_json::from_str(trimmed).ok()?;
    let _ = &row.source_line_id;
    let opt_str = |field: &Option<FieldProbe>| {
        matches!(
            field,
            None | Some(FieldProbe::Null | FieldProbe::Str { .. })
        )
    };
    let valid = if row.kind == "tool_call" {
        opt_str(&row.name)
            && opt_str(&row.args_text)
            && opt_str(&row.result_text)
            && matches!(
                row.result_ok,
                None | Some(FieldProbe::Null | FieldProbe::Bool)
            )
    } else {
        matches!(&*row.kind, "user" | "assistant" | "reasoning" | "error")
            && matches!(row.text, Some(FieldProbe::Str { .. }))
    };
    valid.then_some(row)
}
enum StrOrOther {
    Str(String),
    Null,
    Other,
}
impl<'de> serde::Deserialize<'de> for StrOrOther {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'v> serde::de::Visitor<'v> for Visitor {
            type Value = StrOrOther;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<StrOrOther, E> {
                Ok(StrOrOther::Str(v.to_owned()))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<StrOrOther, E> {
                Ok(StrOrOther::Str(v))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<StrOrOther, E> {
                Ok(StrOrOther::Null)
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<StrOrOther, E> {
                Ok(StrOrOther::Other)
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<StrOrOther, E> {
                Ok(StrOrOther::Other)
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<StrOrOther, E> {
                Ok(StrOrOther::Other)
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<StrOrOther, E> {
                Ok(StrOrOther::Other)
            }
            fn visit_seq<A: serde::de::SeqAccess<'v>>(
                self,
                mut seq: A,
            ) -> Result<StrOrOther, A::Error> {
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(StrOrOther::Other)
            }
            fn visit_map<A: serde::de::MapAccess<'v>>(
                self,
                mut map: A,
            ) -> Result<StrOrOther, A::Error> {
                while map
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(StrOrOther::Other)
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}
enum BoolOrOther {
    Bool(bool),
    Null,
    Other,
}
impl<'de> serde::Deserialize<'de> for BoolOrOther {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'v> serde::de::Visitor<'v> for Visitor {
            type Value = BoolOrOther;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<BoolOrOther, E> {
                Ok(BoolOrOther::Bool(v))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<BoolOrOther, E> {
                Ok(BoolOrOther::Null)
            }
            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<BoolOrOther, E> {
                Ok(BoolOrOther::Other)
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<BoolOrOther, E> {
                Ok(BoolOrOther::Other)
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<BoolOrOther, E> {
                Ok(BoolOrOther::Other)
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<BoolOrOther, E> {
                Ok(BoolOrOther::Other)
            }
            fn visit_seq<A: serde::de::SeqAccess<'v>>(
                self,
                mut seq: A,
            ) -> Result<BoolOrOther, A::Error> {
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(BoolOrOther::Other)
            }
            fn visit_map<A: serde::de::MapAccess<'v>>(
                self,
                mut map: A,
            ) -> Result<BoolOrOther, A::Error> {
                while map
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(BoolOrOther::Other)
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}
#[derive(serde::Deserialize)]
struct RawSnapshotRow {
    kind: String,
    captured_at: String,
    source_line_id: String,
    source_message_id: String,
    #[serde(default)]
    text: Option<StrOrOther>,
    #[serde(default)]
    name: Option<StrOrOther>,
    #[serde(rename = "argsText", default)]
    args_text: Option<StrOrOther>,
    #[serde(rename = "resultText", default)]
    result_text: Option<StrOrOther>,
    #[serde(rename = "resultOk", default)]
    result_ok: Option<BoolOrOther>,
}
/// Single-pass unlocked row parser matching `TranscriptJournal::read_entries_snapshot`.
fn parse_snapshot_entry(line: &str) -> Option<TranscriptEntry> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('{') {
        return None;
    }
    let row: RawSnapshotRow = serde_json::from_str(trimmed).ok()?;
    let opt_str = |field: Option<StrOrOther>| -> Option<Option<String>> {
        match field {
            None | Some(StrOrOther::Null) => Some(None),
            Some(StrOrOther::Str(value)) => Some(Some(value)),
            Some(StrOrOther::Other) => None,
        }
    };
    if row.kind == "tool_call" {
        let name = opt_str(row.name)?;
        let args_text = opt_str(row.args_text)?;
        let result_text = opt_str(row.result_text)?;
        let result_ok = match row.result_ok {
            None | Some(BoolOrOther::Null) => None,
            Some(BoolOrOther::Bool(value)) => Some(value),
            Some(BoolOrOther::Other) => return None,
        };
        Some(TranscriptEntry::ToolCall(ToolCallTranscriptEntry {
            kind: row.kind,
            name,
            args_text,
            result_text,
            result_ok,
            captured_at: row.captured_at,
            source_line_id: row.source_line_id,
            source_message_id: row.source_message_id,
        }))
    } else if matches!(
        row.kind.as_str(),
        "user" | "assistant" | "reasoning" | "error"
    ) {
        let Some(StrOrOther::Str(text)) = row.text else {
            return None;
        };
        Some(TranscriptEntry::Text(TextTranscriptEntry {
            kind: row.kind,
            text,
            captured_at: row.captured_at,
            source_line_id: row.source_line_id,
            source_message_id: row.source_message_id,
        }))
    } else {
        None
    }
}
fn read_journal_raw(journal_dir: &Path) -> Result<Option<String>, DreamSelectorError> {
    match std::fs::read_to_string(journal_dir.join("transcript.jsonl")) {
        Ok(raw) => Ok(Some(raw)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
/// Summarizes one conversation without allocating message bodies outside the bounded 128 KB
/// snapshot window after the reflected anchor.
fn load_conversation_fast(
    conversation_id: String,
    raw: &str,
    reflected_through: Option<String>,
    reflected_completed_steps: usize,
) -> Option<LoadedConversation> {
    let mut distinct_sources: HashSet<Cow<'_, str>> = HashSet::new();
    let mut total_completed_steps = 0usize;
    let mut last_canonical: Option<(Cow<'_, str>, Cow<'_, str>)> = None;
    let mut after_anchor_offset = 0usize;
    let mut has_canonical_after_anchor = false;
    let mut offset = 0usize;
    for segment in raw.split('\n') {
        let next_offset = (offset + segment.len() + 1).min(raw.len());
        let line = segment.strip_suffix('\r').unwrap_or(segment);
        if let Some(row) = parse_row_probe(line) {
            let text_non_blank = matches!(row.text, Some(FieldProbe::Str { blank: false }));
            let has_msg_id = !row.source_message_id.is_empty();
            if row.kind == "assistant" && has_msg_id && text_non_blank {
                total_completed_steps += 1;
            }
            let canonical =
                matches!(&*row.kind, "user" | "assistant") && has_msg_id && text_non_blank;
            if canonical {
                last_canonical = Some((row.source_message_id.clone(), row.captured_at));
            }
            if reflected_through.as_deref() == Some(&*row.source_message_id) {
                after_anchor_offset = next_offset;
                has_canonical_after_anchor = false;
            } else if canonical {
                has_canonical_after_anchor = true;
            }
            distinct_sources.insert(row.source_message_id);
        }
        offset = next_offset;
    }
    if !has_canonical_after_anchor {
        return None;
    }
    let (newest_message_id, last_activity) = last_canonical?;
    let mut tail = Vec::new();
    let mut tail_bytes = 0usize;
    let mut seen_canonical = false;
    let mut cutoff_message_id: Option<String> = None;
    for line in raw[after_anchor_offset..].lines() {
        let Some(entry) = parse_snapshot_entry(line) else {
            continue;
        };
        if let Some(ref cutoff_id) = cutoff_message_id
            && entry.source_message_id() != cutoff_id
        {
            // Keep one entry past the crossing message group so `capture_cursor_snapshot` sees
            // the exact group boundary and total-bytes overflow.
            tail.push(entry);
            break;
        }
        let is_canon = is_canonical_entry(&entry);
        tail_bytes += serde_json::to_vec(&entry).map_or(0, |json| json.len() + 1);
        if is_canon {
            seen_canonical = true;
        }
        if seen_canonical
            && tail_bytes > REFLECTION_SNAPSHOT_MAX_BYTES
            && cutoff_message_id.is_none()
        {
            cutoff_message_id = Some(entry.source_message_id().to_owned());
        }
        tail.push(entry);
    }
    let snapshot = capture_cursor_snapshot(&tail, &initial_reflection_state())?;
    let reflected_completed = reflected_completed_steps.min(total_completed_steps);
    Some(LoadedConversation {
        conversation_id,
        steps: total_completed_steps.saturating_sub(reflected_completed),
        reflected_through,
        newest_message_id: newest_message_id.into_owned(),
        last_activity: last_activity.into_owned(),
        total_bytes: conversation_byte_length(&snapshot.entries),
        distinct_sources: distinct_sources.len(),
        entries: None,
    })
}
/// Reads every conversation once without taking journal locks: a dream decision only ranks
/// conversations, and the selected ones are captured under their lock afterwards. Rows a concurrent
/// append has not finished are skipped. Returns `None` once `stopped` reports true.
fn load_conversations(
    dir: &Path,
    keep_entries: bool,
    stopped: &dyn Fn() -> bool,
) -> Result<Option<Vec<LoadedConversation>>, DreamSelectorError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Some(Vec::new())),
        Err(error) => return Err(error.into()),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort_by(|left, right| compare_conversation_ids(left, right));
    let mut loaded = Vec::new();
    for conversation_id in names {
        if stopped() {
            return Ok(None);
        }
        let journal_dir = dir.join(&conversation_id);
        let Some(raw) = read_journal_raw(&journal_dir)? else {
            continue;
        };
        let value = match std::fs::read(journal_dir.join("state.json")) {
            Ok(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes)
                .unwrap_or(serde_json::Value::Null),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::Value::Null,
            Err(error) => return Err(error.into()),
        };
        let reflected_through = value
            .get("reflected_through_message_id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned);
        let reflected_completed_steps = value
            .get("reflected_completed_steps")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(0);
        if !keep_entries {
            if let Some(conversation) = load_conversation_fast(
                conversation_id,
                &raw,
                reflected_through,
                reflected_completed_steps,
            ) {
                loaded.push(conversation);
            }
            continue;
        }
        let entries: Vec<TranscriptEntry> = raw.lines().filter_map(parse_snapshot_entry).collect();
        let mut state = initial_reflection_state();
        state.reflected_through_message_id = reflected_through;
        state.reflected_completed_steps = reflected_completed_steps;
        let state = derive_state(&state, &entries);
        let Some(snapshot) = capture_cursor_snapshot(&entries, &state) else {
            continue;
        };
        let Some(newest) = entries.iter().rev().find(|entry| is_canonical_entry(entry)) else {
            continue;
        };
        loaded.push(LoadedConversation {
            conversation_id,
            steps: state.steps_since_last_successful_reflection,
            reflected_through: state.reflected_through_message_id,
            newest_message_id: newest.source_message_id().to_owned(),
            last_activity: newest.captured_at().to_owned(),
            total_bytes: conversation_byte_length(&snapshot.entries),
            distinct_sources: entries
                .iter()
                .map(TranscriptEntry::source_message_id)
                .collect::<BTreeSet<_>>()
                .len(),
            entries: Some(entries),
        });
    }
    Ok(Some(loaded))
}
fn search_documents(conversation_id: &str, entries: &[TranscriptEntry]) -> Vec<SearchDocument> {
    let mut grouped: Vec<(String, Vec<&TranscriptEntry>)> = Vec::new();
    for entry in entries {
        if let Some((_, entries)) = grouped
            .iter_mut()
            .find(|(id, _)| id == entry.source_message_id())
        {
            entries.push(entry);
        } else {
            grouped.push((entry.source_message_id().to_owned(), vec![entry]));
        }
    }
    grouped
        .into_iter()
        .map(|(id, entries)| {
            let mut content = Vec::new();
            let mut reasoning = Vec::new();
            let mut tools = Vec::new();
            let mut returns = Vec::new();
            for entry in &entries {
                match entry {
                    TranscriptEntry::Text(text) => {
                        if text.kind == "reasoning" {
                            reasoning.push(text.text.as_str());
                        } else {
                            content.push(text.text.as_str());
                        }
                    }
                    TranscriptEntry::ToolCall(tool) => {
                        tools.push(SearchToolCall {
                            name: tool.name.clone(),
                            arguments: tool.args_text.clone(),
                        });
                        if let Some(text) = &tool.result_text {
                            returns.push(text.as_str());
                        }
                    }
                }
            }
            SearchDocument {
                id,
                conversation_id: conversation_id.to_owned(),
                date: entries.last().map(|entry| entry.captured_at().to_owned()),
                message_type: entries.first().map(|entry| entry.kind().to_owned()),
                content: Some(content.join("\n").into()),
                reasoning: Some(reasoning.join("\n")),
                summary: None,
                tool_calls: Some(tools),
                tool_return: Some(returns.join("\n").into()),
                func_response: None,
            }
        })
        .collect()
}
struct OneConversation(TranscriptConversation);
impl TranscriptProvider for OneConversation {
    fn list_conversations(&self) -> Vec<TranscriptConversation> {
        vec![self.0.clone()]
    }
}
fn select_loaded(
    conversations: &[LoadedConversation],
    options: &DreamSelectorOptions<'_>,
    focus: Option<&str>,
) -> DreamSelection {
    let target_bytes = options
        .auto_select_max_bytes
        .to_string()
        .parse::<f64>()
        .unwrap_or(0.0)
        / options
            .auto_select_max
            .to_string()
            .parse::<f64>()
            .unwrap_or(0.0);
    let candidates: Vec<_> = conversations
        .iter()
        .map(|conversation| {
            let hits = focus
                .filter(|focus| !focus.trim().is_empty())
                .zip(conversation.entries.as_deref())
                .map(|(focus, entries)| {
                    search_transcripts(
                        &OneConversation(TranscriptConversation {
                            id: conversation.conversation_id.clone(),
                            hidden: None,
                            messages: search_documents(&conversation.conversation_id, entries),
                        }),
                        focus,
                        &SearchOptions {
                            limit: Some(10),
                            conversation_id: Some(conversation.conversation_id.clone()),
                            ..Default::default()
                        },
                    )
                    .len()
                })
                .unwrap_or(0);
            let sources = conversation.distinct_sources;
            let (operands, score) = score_dream_candidate(&DreamScoreInput {
                search_match_count: hits.to_string().parse().unwrap_or(0.0),
                steps_since_last_successful_reflection: conversation
                    .steps
                    .to_string()
                    .parse()
                    .unwrap_or(0.0),
                last_activity: &conversation.last_activity,
                distinct_source_count: sources.to_string().parse().unwrap_or(0.0),
                total_bytes: conversation.total_bytes.to_string().parse().unwrap_or(0.0),
                target_bytes,
                is_current: options.current_conversation_id
                    == Some(conversation.conversation_id.as_str()),
                newest_message_covered: conversation.reflected_through.as_deref()
                    == Some(conversation.newest_message_id.as_str()),
                now_ms: options.now_ms,
            });
            DreamScoredConversation {
                conversation_id: conversation.conversation_id.clone(),
                total_bytes: conversation.total_bytes,
                last_activity_ms: activity(&conversation.last_activity),
                operands,
                score,
            }
        })
        .collect();
    pack_dream_candidates(
        &rank_dream_candidates(&candidates),
        options.auto_select_max,
        options.auto_select_max_bytes,
    )
}
fn has_focus(focus: Option<&str>) -> bool {
    focus.is_some_and(|focus| !focus.trim().is_empty())
}
fn load_all(dir: &Path, keep_entries: bool) -> Result<Vec<LoadedConversation>, DreamSelectorError> {
    Ok(load_conversations(dir, keep_entries, &|| false)?.unwrap_or_default())
}
/// The unreflected conversations of one identity, read once so the volume gate and the selection of
/// a single dream decision share one scan.
pub struct DreamCandidates {
    conversations: Vec<LoadedConversation>,
}
impl DreamCandidates {
    /// Scans `dir`; `focus` decides whether the rows are kept for a focus search. Returns `None` when
    /// `stopped` reports true between conversations.
    pub fn load(
        dir: &Path,
        focus: Option<&str>,
        stopped: &dyn Fn() -> bool,
    ) -> Result<Option<Self>, DreamSelectorError> {
        Ok(load_conversations(dir, has_focus(focus), stopped)?
            .map(|conversations| Self { conversations }))
    }
    pub fn unreflected_volume(&self) -> usize {
        self.conversations
            .iter()
            .map(|conversation| conversation.total_bytes)
            .sum()
    }
    pub fn select(
        &self,
        options: &DreamSelectorOptions<'_>,
        focus: Option<&str>,
    ) -> DreamSelection {
        select_loaded(&self.conversations, options, focus)
    }
}
pub fn compute_unreflected_volume(
    options: &DreamSelectorOptions<'_>,
) -> Result<usize, DreamSelectorError> {
    Ok(load_all(options.transcripts_dir, false)?
        .iter()
        .map(|conversation| conversation.total_bytes)
        .sum())
}
pub fn select_dream_conversations(
    options: &DreamSelectorOptions<'_>,
    focus: Option<&str>,
) -> Result<DreamSelection, DreamSelectorError> {
    Ok(select_loaded(
        &load_all(options.transcripts_dir, has_focus(focus))?,
        options,
        focus,
    ))
}
pub fn select_recent_dream_conversations(
    options: &DreamSelectorOptions<'_>,
    recent_n: usize,
    focus: Option<&str>,
) -> Result<DreamSelection, DreamSelectorError> {
    let mut recent = load_all(options.transcripts_dir, has_focus(focus))?;
    recent.sort_by(|left, right| {
        activity(&right.last_activity)
            .total_cmp(&activity(&left.last_activity))
            .then_with(|| compare_conversation_ids(&left.conversation_id, &right.conversation_id))
    });
    recent.truncate(recent_n);
    Ok(select_loaded(&recent, options, focus))
}
#[cfg(test)]
mod tests {
    use super::*;
    use memory_core::journal::entries::{TextTranscriptEntry, ToolCallTranscriptEntry};
    fn text(id: &str, kind: &str, value: &str, date: &str) -> TranscriptEntry {
        TranscriptEntry::Text(TextTranscriptEntry::new(
            kind,
            value,
            date,
            format!("{id}:line"),
            format!("{id}:message"),
        ))
    }
    fn journal(dir: &Path, id: &str, entries: &[TranscriptEntry], state: serde_json::Value) {
        let path = dir.join(id);
        std::fs::create_dir_all(&path).unwrap();
        let rows = entries
            .iter()
            .map(|entry| serde_json::to_string(entry).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(path.join("transcript.jsonl"), format!("{rows}\n")).unwrap();
        std::fs::write(path.join("state.json"), serde_json::to_vec(&state).unwrap()).unwrap();
    }
    fn options(dir: &Path) -> DreamSelectorOptions<'_> {
        DreamSelectorOptions {
            transcripts_dir: dir,
            current_conversation_id: None,
            auto_select_max: 1,
            auto_select_max_bytes: 10_000,
            now_ms: activity("2026-08-10T12:00:00.000Z"),
        }
    }
    fn canonical_recency(kind: &str) {
        let dir = tempfile::tempdir().unwrap();
        let tail = if kind == "tool_call" {
            TranscriptEntry::ToolCall(ToolCallTranscriptEntry::new(
                Some("read".into()),
                None,
                None,
                None,
                "2026-08-10T11:30:00.000Z",
                "tail:line",
                "tail:message",
            ))
        } else {
            text("tail", kind, "context tail", "2026-08-10T11:30:00.000Z")
        };
        journal(
            dir.path(),
            "stale-with-tail",
            &[
                text("stale", "user", "same", "2026-08-10T09:00:00.000Z"),
                tail,
            ],
            serde_json::json!({}),
        );
        journal(
            dir.path(),
            "actually-recent",
            &[text("recent", "user", "same", "2026-08-10T10:00:00.000Z")],
            serde_json::json!({}),
        );
        assert_eq!(
            select_recent_dream_conversations(&options(dir.path()), 1, None)
                .unwrap()
                .conversation_ids,
            ["actually-recent"]
        );
        assert_eq!(
            select_dream_conversations(&options(dir.path()), None)
                .unwrap()
                .conversation_ids,
            ["actually-recent"]
        );
    }
    #[test]
    fn reasoning_tail_does_not_set_recency() {
        canonical_recency("reasoning");
    }
    #[test]
    fn error_tail_does_not_set_recency() {
        canonical_recency("error");
    }
    #[test]
    fn tool_tail_does_not_set_recency() {
        canonical_recency("tool_call");
    }
    #[test]
    fn utf8_volume_and_ranking_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let alpha = [
            text("alpha", "user", "focus 😀", "2026-08-10T11:00:00.000Z"),
            TranscriptEntry::ToolCall(ToolCallTranscriptEntry::new(
                None,
                Some("x".into()),
                Some("結果".into()),
                None,
                "2026-08-10T11:30:00.000Z",
                "alpha:tool",
                "alpha:tool-message",
            )),
        ];
        journal(
            dir.path(),
            "alpha",
            &alpha,
            serde_json::json!({"steps_since_last_successful_reflection":20}),
        );
        journal(
            dir.path(),
            "gamma",
            &[text(
                "gamma",
                "user",
                "focus later",
                "2026-08-10T09:00:00.000Z",
            )],
            serde_json::json!({"steps_since_last_successful_reflection":10}),
        );
        journal(
            dir.path(),
            "beta",
            &[text(
                "beta",
                "user",
                "already reflected",
                "2026-08-10T10:00:00.000Z",
            )],
            serde_json::json!({"reflected_through_message_id":"beta:message"}),
        );
        let options = DreamSelectorOptions {
            current_conversation_id: Some("gamma"),
            auto_select_max: 5,
            ..options(dir.path())
        };
        let bytes = "focus 😀\nx\n結果".len() + "focus later".len();
        assert_eq!(compute_unreflected_volume(&options).unwrap(), bytes);
        let first = select_dream_conversations(&options, Some("focus")).unwrap();
        assert_eq!(
            first,
            DreamSelection {
                conversation_ids: vec!["gamma".into(), "alpha".into()],
                total_bytes: bytes
            }
        );
        assert_eq!(
            select_dream_conversations(&options, Some("focus")).unwrap(),
            first
        );
    }
    #[test]
    fn missing_directory_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            compute_unreflected_volume(&options(&dir.path().join("absent"))).unwrap(),
            0
        );
    }
    #[test]
    fn candidates_scan_once_and_stop_between_conversations() {
        let dir = tempfile::tempdir().unwrap();
        for id in ["alpha", "beta"] {
            journal(
                dir.path(),
                id,
                &[text(id, "user", "body", "2026-08-10T11:00:00.000Z")],
                serde_json::json!({}),
            );
        }
        let candidates = DreamCandidates::load(dir.path(), None, &|| false)
            .unwrap()
            .unwrap();
        assert_eq!(
            candidates.unreflected_volume(),
            compute_unreflected_volume(&options(dir.path())).unwrap()
        );
        assert_eq!(
            candidates.select(&options(dir.path()), None),
            select_dream_conversations(&options(dir.path()), None).unwrap()
        );
        let checks = std::cell::Cell::new(0);
        let stop_after_first = || {
            checks.set(checks.get() + 1);
            checks.get() > 1
        };
        assert!(
            DreamCandidates::load(dir.path(), None, &stop_after_first)
                .unwrap()
                .is_none()
        );
        assert_eq!(checks.get(), 2);
    }
    #[test]
    fn scan_reads_a_journal_another_writer_holds() {
        let dir = tempfile::tempdir().unwrap();
        journal(
            dir.path(),
            "held",
            &[text("held", "user", "body", "2026-08-10T11:00:00.000Z")],
            serde_json::json!({}),
        );
        let journal_dir = dir.path().join("held");
        let lock = journal_dir.join("state.lock");
        std::fs::write(&lock, format!("{}\n", std::process::id())).unwrap();
        let started = std::time::Instant::now();
        assert!(
            compute_unreflected_volume(&options(dir.path())).unwrap() > 0,
            "the held journal is still counted"
        );
        assert!(
            started.elapsed()
                < std::time::Duration::from_millis(memory_core::journal::lock::ACQUISITION_WAIT_MS),
            "the scan never waits for the journal lock"
        );
    }
    #[test]
    fn fast_scan_matches_full_parse_across_anchors_and_snapshot_cap() {
        let dir = tempfile::tempdir().unwrap();
        let big = "x".repeat(80_000);
        let entries = vec![
            text(
                "m0",
                "user",
                "reflected question",
                "2026-08-10T08:00:00.000Z",
            ),
            text(
                "m0",
                "assistant",
                "reflected answer",
                "2026-08-10T08:01:00.000Z",
            ),
            text("m1", "user", &big, "2026-08-10T09:00:00.000Z"),
            TranscriptEntry::ToolCall(ToolCallTranscriptEntry::new(
                Some("read".into()),
                Some("a".into()),
                Some(big.clone()),
                Some(true),
                "2026-08-10T09:01:00.000Z",
                "m1:tool",
                "m1:message",
            )),
            text("m2", "assistant", "after cap", "2026-08-10T10:00:00.000Z"),
        ];
        journal(
            dir.path(),
            "large",
            &entries,
            serde_json::json!({
                "reflected_through_message_id": "m0:message",
                "reflected_completed_steps": 1
            }),
        );
        // Append a malformed partial row as a concurrent writer would leave behind.
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join("large/transcript.jsonl"))
            .and_then(|mut f| std::io::Write::write_all(&mut f, b"{\"kind\":\"user\",\"broken"))
            .unwrap();
        let fast = load_conversations(dir.path(), false, &|| false)
            .unwrap()
            .unwrap();
        let full = load_conversations(dir.path(), true, &|| false)
            .unwrap()
            .unwrap();
        assert_eq!(fast.len(), 1);
        assert_eq!(full.len(), 1);
        assert_eq!(fast[0].steps, full[0].steps);
        assert_eq!(fast[0].reflected_through, full[0].reflected_through);
        assert_eq!(fast[0].newest_message_id, full[0].newest_message_id);
        assert_eq!(fast[0].last_activity, full[0].last_activity);
        assert_eq!(fast[0].total_bytes, full[0].total_bytes);
        assert_eq!(fast[0].distinct_sources, full[0].distinct_sources);
    }
}
