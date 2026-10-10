//! Port of senpi packages/agent/src/harness/session/jsonl/legacy-v3.ts.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use maho_ai::types::{ModelThinkingLevel as ThinkingLevel, Usage};
use maho_ai::utils::uuid::uuidv7;
use serde::Deserialize;
use serde_json::Value as JsonValue;

use crate::harness::context::Context;
use crate::harness::messages::{
    create_branch_summary_message, create_compaction_summary_message, CustomMessage,
    CustomMessageContent,
};
use crate::harness::session::commit::{
    CommittedEntryWrite, CommittedValueSetWrite, CommittedWrite, WriteKind,
};
use crate::harness::session::session::{SessionError, SessionErrorKind};
use crate::harness::session::types::{
    EntryKind, LaneConfiguration, LaneModelRef, LaneState,
};
use crate::harness::session::values::{
    branch_tip, entry_label, lane_config, lane_state, session_name, set_value,
};
use crate::harness::types::{FileSystem, TextLineReader};
use crate::harness::utils::usage::{add_usage, empty_usage};
use crate::types::{AgentMessage, CustomAgentMessage};

use super::codec::{parse_jsonl_session_header, JsonlParsedSessionHeader, LegacyV3SessionHeader};
use super::io::{file_value, read_jsonl_header};
use super::types::{
    JsonlSessionMetadata, JsonlStorageHeader, JSONL_FORMAT_VERSION, JSONL_STORAGE_VERSION,
};

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
enum LegacyV3Entry {
    #[serde(rename = "message")]
    Message {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        message: AgentMessage,
    },
    #[serde(rename = "custom")]
    Custom {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(rename = "customType")]
        custom_type: String,
        #[serde(default)]
        data: Option<JsonValue>,
    },
    #[serde(rename = "custom_message")]
    CustomMessage {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(rename = "customType")]
        custom_type: String,
        content: CustomMessageContent,
        #[serde(default)]
        details: Option<JsonValue>,
        display: bool,
    },
    #[serde(rename = "branch_summary")]
    BranchSummary {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(rename = "fromId")]
        from_id: String,
        summary: String,
        #[serde(default)]
        details: Option<JsonValue>,
        #[serde(default)]
        usage: Option<Usage>,
        #[serde(rename = "fromHook", default)]
        from_hook: bool,
    },
    #[serde(rename = "compaction")]
    Compaction {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        summary: String,
        #[serde(rename = "firstKeptEntryId")]
        first_kept_entry_id: String,
        #[serde(rename = "tokensBefore")]
        tokens_before: i64,
        #[serde(default)]
        details: Option<JsonValue>,
        #[serde(default)]
        usage: Option<Usage>,
        #[serde(rename = "fromHook", default)]
        from_hook: bool,
    },
    #[serde(rename = "model_change")]
    ModelChange {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        #[allow(dead_code)]
        timestamp: String,
        provider: String,
        #[serde(rename = "modelId")]
        model_id: String,
    },
    #[serde(rename = "thinking_level_change")]
    ThinkingLevelChange {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        #[allow(dead_code)]
        timestamp: String,
        #[serde(rename = "thinkingLevel")]
        thinking_level: ThinkingLevel,
    },
    #[serde(rename = "active_tools_change")]
    ActiveToolsChange {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        #[allow(dead_code)]
        timestamp: String,
        #[serde(rename = "activeToolNames")]
        active_tool_names: Vec<String>,
    },
    #[serde(rename = "session_info")]
    SessionInfo {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        #[allow(dead_code)]
        timestamp: String,
        #[serde(default)]
        name: Option<String>,
    },
    #[serde(rename = "label")]
    Label {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        #[allow(dead_code)]
        timestamp: String,
        #[serde(rename = "targetId")]
        target_id: String,
        #[serde(default)]
        label: Option<String>,
    },
}

impl LegacyV3Entry {
    fn id(&self) -> &str {
        match self {
            LegacyV3Entry::Message { id, .. }
            | LegacyV3Entry::Custom { id, .. }
            | LegacyV3Entry::CustomMessage { id, .. }
            | LegacyV3Entry::BranchSummary { id, .. }
            | LegacyV3Entry::Compaction { id, .. }
            | LegacyV3Entry::ModelChange { id, .. }
            | LegacyV3Entry::ThinkingLevelChange { id, .. }
            | LegacyV3Entry::ActiveToolsChange { id, .. }
            | LegacyV3Entry::SessionInfo { id, .. }
            | LegacyV3Entry::Label { id, .. } => id,
        }
    }

    fn parent_id(&self) -> Option<&str> {
        match self {
            LegacyV3Entry::Message { parent_id, .. }
            | LegacyV3Entry::Custom { parent_id, .. }
            | LegacyV3Entry::CustomMessage { parent_id, .. }
            | LegacyV3Entry::BranchSummary { parent_id, .. }
            | LegacyV3Entry::Compaction { parent_id, .. }
            | LegacyV3Entry::ModelChange { parent_id, .. }
            | LegacyV3Entry::ThinkingLevelChange { parent_id, .. }
            | LegacyV3Entry::ActiveToolsChange { parent_id, .. }
            | LegacyV3Entry::SessionInfo { parent_id, .. }
            | LegacyV3Entry::Label { parent_id, .. } => parent_id.as_deref(),
        }
    }

    fn entry_type_str(&self) -> &'static str {
        match self {
            LegacyV3Entry::Message { .. } => "message",
            LegacyV3Entry::Custom { .. } => "custom",
            LegacyV3Entry::CustomMessage { .. } => "custom_message",
            LegacyV3Entry::BranchSummary { .. } => "branch_summary",
            LegacyV3Entry::Compaction { .. } => "compaction",
            LegacyV3Entry::ModelChange { .. } => "model_change",
            LegacyV3Entry::ThinkingLevelChange { .. } => "thinking_level_change",
            LegacyV3Entry::ActiveToolsChange { .. } => "active_tools_change",
            LegacyV3Entry::SessionInfo { .. } => "session_info",
            LegacyV3Entry::Label { .. } => "label",
        }
    }

    fn is_retained(&self) -> bool {
        matches!(
            self,
            LegacyV3Entry::Message { .. }
                | LegacyV3Entry::Custom { .. }
                | LegacyV3Entry::CustomMessage { .. }
                | LegacyV3Entry::BranchSummary { .. }
                | LegacyV3Entry::Compaction { .. }
        )
    }
}

#[derive(Debug, Clone)]
enum LegacyV3IndexKind {
    Message,
    Custom,
    CustomMessage,
    BranchSummary { from_id: String, from_hook: bool },
    Compaction { first_kept_entry_id: String, from_hook: bool },
    Label { target_id: String, label: Option<String> },
    ModelChange { provider: String, model_id: String },
    ThinkingLevelChange { thinking_level: ThinkingLevel },
    ActiveToolsChange { active_tool_names: Vec<String> },
    SessionInfo,
}

#[derive(Debug, Clone)]
struct LegacyV3IndexEntry {
    id: String,
    parent_id: Option<String>,
    mapped_id: Option<String>,
    seq: Option<i64>,
    kind: LegacyV3IndexKind,
}

impl LegacyV3IndexEntry {
    fn is_retained(&self) -> bool {
        matches!(
            self.kind,
            LegacyV3IndexKind::Message
                | LegacyV3IndexKind::Custom
                | LegacyV3IndexKind::CustomMessage
                | LegacyV3IndexKind::BranchSummary { .. }
                | LegacyV3IndexKind::Compaction { .. }
        )
    }
}

pub fn parse_iso_millis(value: &str) -> i64 {
    time_from_iso(value).unwrap_or_default()
}

fn time_from_iso(value: &str) -> Option<i64> {
    let trimmed = value.trim();
    let normalized = trimmed.strip_suffix('Z').unwrap_or(trimmed);
    let (date, rest) = match normalized.split_once(['T', 't']) {
        Some(parts) => parts,
        None => (normalized, ""),
    };
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;

    let mut time_parts = rest.split(':');
    let hour: i64 = time_parts.next().unwrap_or("0").parse().ok()?;
    let minute: i64 = time_parts.next().unwrap_or("0").parse().ok()?;
    let second_part = time_parts.next().unwrap_or("0");
    let (second, fraction) = second_part.split_once('.').unwrap_or((second_part, ""));
    let second: i64 = second.parse().ok()?;
    let millis: i64 = {
        let digits: String = fraction.chars().take(3).collect();
        let padded = format!("{digits:0<3}");
        padded.parse().unwrap_or(0)
    };

    let days = days_from_civil(year, month, day);
    Some(((days * 86_400 + hour * 3_600 + minute * 60 + second) * 1_000) + millis)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

async fn resolve_legacy_v3_parent_session_id(
    file_system: &Arc<dyn FileSystem>,
    parent_session_path: &str,
    context: &Context,
) -> Option<String> {
    let lines = file_system
        .read_text_lines(parent_session_path, Some(1), context)
        .await
        .ok()?;
    let first = lines.first()?;
    let parsed = parse_jsonl_session_header(first).ok()?;
    match parsed {
        JsonlParsedSessionHeader::V4(header) => Some(header.id),
        JsonlParsedSessionHeader::V3Legacy(header) => Some(header.id),
    }
}

pub async fn metadata_from_legacy_v3_header(
    file_system: &Arc<dyn FileSystem>,
    header: &LegacyV3SessionHeader,
    context: &Context,
) -> JsonlSessionMetadata {
    let mut metadata = JsonlSessionMetadata {
        id: header.id.clone(),
        created_at: parse_iso_millis(&header.timestamp),
        storage_version: JSONL_STORAGE_VERSION,
        cwd: header.cwd.clone(),
        path: String::new(),
        modified_at: 0,
        parent_session_id: None,
        legacy_parent_session_path: None,
    };
    if let Some(parent) = &header.parent_session {
        if let Some(parent_id) = resolve_legacy_v3_parent_session_id(file_system, parent, context).await {
            metadata.parent_session_id = Some(parent_id);
        } else {
            metadata.legacy_parent_session_path = Some(parent.clone());
        }
    }
    metadata
}

pub async fn normalize_legacy_v3_header(
    file_system: &Arc<dyn FileSystem>,
    header: &LegacyV3SessionHeader,
    context: &Context,
) -> JsonlStorageHeader {
    let meta = metadata_from_legacy_v3_header(file_system, header, context).await;
    JsonlStorageHeader {
        v: JSONL_FORMAT_VERSION,
        kind: "header".to_owned(),
        id: meta.id,
        created_at: meta.created_at,
        storage_version: meta.storage_version,
        cwd: meta.cwd,
        next_seq: None,
        parent_session_id: meta.parent_session_id,
        legacy_parent_session_path: meta.legacy_parent_session_path,
    }
}

fn parse_legacy_v3_entry(line: &str) -> Result<LegacyV3Entry, SessionError> {
    let val: JsonValue = serde_json::from_str(line).map_err(|e| {
        SessionError::new(
            SessionErrorKind::Io,
            format!("Invalid legacy v3 JSONL record: not valid JSON: {e}"),
        )
    })?;
    let record_type = val.get("type").and_then(JsonValue::as_str).unwrap_or("");
    let from_hook = val.get("fromHook").and_then(JsonValue::as_bool).unwrap_or(false);
    match record_type {
        "message" | "custom" | "custom_message" | "branch_summary" | "compaction"
        | "model_change" | "thinking_level_change" | "active_tools_change"
        | "session_info" | "label" => {
            let mut val = val;
            if matches!(record_type, "branch_summary" | "compaction") {
                val["fromHook"] = JsonValue::Bool(from_hook);
            }
            let entry = serde_json::from_value::<LegacyV3Entry>(val).map_err(|e| {
                SessionError::new(
                    SessionErrorKind::Io,
                    format!("Invalid legacy v3 JSONL record: {e}"),
                )
            })?;
            Ok(entry)
        }
        other => Err(SessionError::new(
            SessionErrorKind::Io,
            format!("Unsupported legacy v3 record type: {other}"),
        )),
    }
}

fn imported_custom_message(
    custom_type: String,
    content: CustomMessageContent,
    details: Option<JsonValue>,
    display: bool,
    timestamp: i64,
) -> AgentMessage {
    AgentMessage::Custom(CustomAgentMessage::Custom(CustomMessage {
        role: "custom".to_owned(),
        custom_type,
        content,
        display,
        details,
        timestamp,
    }))
}

fn resolve_legacy_id(
    entries: &HashMap<String, LegacyV3IndexEntry>,
    legacy_id: Option<&str>,
) -> Result<Option<String>, SessionError> {
    let Some(id) = legacy_id else {
        return Ok(None);
    };
    let entry = entries.get(id).ok_or_else(|| {
        SessionError::new(
            SessionErrorKind::Io,
            format!("Missing legacy v3 entry reference: {id}"),
        )
    })?;
    Ok(entry.mapped_id.clone())
}

fn resolve_branch_summary_from_id(
    entries: &HashMap<String, LegacyV3IndexEntry>,
    legacy_from_id: &str,
) -> Result<Option<String>, SessionError> {
    if legacy_from_id == "root" {
        Ok(None)
    } else {
        resolve_legacy_id(entries, Some(legacy_from_id))
    }
}

fn project_context_message(
    entry: &LegacyV3Entry,
    entries: &HashMap<String, LegacyV3IndexEntry>,
) -> Result<Option<AgentMessage>, SessionError> {
    match entry {
        LegacyV3Entry::Message { message, .. } => Ok(Some(message.clone())),
        LegacyV3Entry::CustomMessage {
            custom_type,
            content,
            details,
            display,
            timestamp,
            ..
        } => Ok(Some(imported_custom_message(
            custom_type.clone(),
            content.clone(),
            details.clone(),
            *display,
            parse_iso_millis(timestamp),
        ))),
        LegacyV3Entry::BranchSummary {
            summary,
            from_id,
            timestamp,
            ..
        } => {
            if summary.is_empty() {
                Ok(None)
            } else {
                let resolved_from_id = resolve_branch_summary_from_id(entries, from_id)?;
                let ts = parse_iso_millis(timestamp);
                Ok(Some(AgentMessage::Custom(CustomAgentMessage::BranchSummary(
                    create_branch_summary_message(summary.clone(), resolved_from_id, ts),
                ))))
            }
        }
        LegacyV3Entry::Compaction {
            summary,
            tokens_before,
            timestamp,
            ..
        } => {
            let ts = parse_iso_millis(timestamp);
            Ok(Some(AgentMessage::Custom(CustomAgentMessage::CompactionSummary(
                create_compaction_summary_message(summary.clone(), *tokens_before, ts),
            ))))
        }
        LegacyV3Entry::Custom { .. }
        | LegacyV3Entry::ModelChange { .. }
        | LegacyV3Entry::ThinkingLevelChange { .. }
        | LegacyV3Entry::ActiveToolsChange { .. }
        | LegacyV3Entry::SessionInfo { .. }
        | LegacyV3Entry::Label { .. } => Ok(None),
    }
}

fn retained_tail_structure<'a>(
    compaction_id: &str,
    first_kept_entry_id: &str,
    parent_id: Option<&str>,
    entries_by_id: &'a HashMap<String, LegacyV3IndexEntry>,
) -> Result<Vec<&'a LegacyV3IndexEntry>, SessionError> {
    let mut tail = Vec::new();
    let mut current_id = parent_id;
    while let Some(cid) = current_id {
        let entry = entries_by_id.get(cid).ok_or_else(|| {
            SessionError::new(
                SessionErrorKind::Io,
                format!("Missing legacy v3 entry reference: {cid}"),
            )
        })?;
        tail.push(entry);
        if cid == first_kept_entry_id {
            return Ok(tail);
        }
        current_id = entry.parent_id.as_deref();
    }
    Err(SessionError::new(
        SessionErrorKind::Io,
        format!(
            "Legacy v3 compaction {compaction_id} firstKeptEntryId is not on its parent branch: {first_kept_entry_id}"
        ),
    ))
}

fn normalize_retained_entry(
    entry: &LegacyV3Entry,
    indexed: &LegacyV3IndexEntry,
    retained_tail: Vec<AgentMessage>,
    entries: &HashMap<String, LegacyV3IndexEntry>,
) -> Result<CommittedEntryWrite, SessionError> {
    let mapped_id = indexed.mapped_id.clone().ok_or_else(|| {
        SessionError::new(SessionErrorKind::Io, "Unmapped retained entry")
    })?;
    let parent_id = resolve_legacy_id(entries, indexed.parent_id.as_deref())?;
    let seq = indexed.seq.unwrap_or(0);
    let ts = parse_iso_millis(match entry {
        LegacyV3Entry::Message { timestamp, .. }
        | LegacyV3Entry::Custom { timestamp, .. }
        | LegacyV3Entry::CustomMessage { timestamp, .. }
        | LegacyV3Entry::BranchSummary { timestamp, .. }
        | LegacyV3Entry::Compaction { timestamp, .. } => timestamp,
        _ => "",
    });

    let body = match entry {
        LegacyV3Entry::Message { message, .. } => EntryKind::Message {
            message: message.clone(),
            terminate: None,
        },
        LegacyV3Entry::CustomMessage {
            custom_type,
            content,
            details,
            display,
            ..
        } => EntryKind::Message {
            message: imported_custom_message(
                custom_type.clone(),
                content.clone(),
                details.clone(),
                *display,
                ts,
            ),
            terminate: None,
        },
        LegacyV3Entry::BranchSummary { summary, from_id, details, usage, .. } => EntryKind::BranchSummary {
            from_id: resolve_branch_summary_from_id(entries, from_id)?,
            summary: summary.clone(),
            details: details.clone(),
            usage: *usage,
            from_hook: match &indexed.kind {
                LegacyV3IndexKind::BranchSummary { from_hook, .. } => *from_hook,
                _ => false,
            },
        },
        LegacyV3Entry::Compaction { summary, tokens_before, details, usage, .. } => EntryKind::Compaction {
            summary: summary.clone(),
            retained_tail,
            tokens_before: *tokens_before,
            details: details.clone(),
            usage: *usage,
            from_hook: match &indexed.kind {
                LegacyV3IndexKind::Compaction { from_hook, .. } => *from_hook,
                _ => false,
            },
        },
        LegacyV3Entry::Custom {
            custom_type,
            data,
            ..
        } => EntryKind::Custom {
            custom_type: custom_type.clone(),
            data: data.clone(),
        },
        _ => {
            return Err(SessionError::new(
                SessionErrorKind::Io,
                "Cannot normalize discarded entry as retained",
            ));
        }
    };

    Ok(CommittedEntryWrite {
        kind: WriteKind::Entry,
        id: mapped_id,
        parent_id,
        seq,
        timestamp: ts,
        body,
    })
}

fn selected_configuration(
    entries_by_id: &HashMap<String, LegacyV3IndexEntry>,
    selected_id: Option<&str>,
) -> Option<LaneConfiguration> {
    let mut remaining = HashSet::new();
    remaining.insert("model_change");
    remaining.insert("thinking_level_change");
    remaining.insert("active_tools_change");

    let mut model: Option<LaneModelRef> = None;
    let mut thinking_level: Option<ThinkingLevel> = None;
    let mut active_tool_names: Option<Vec<String>> = None;

    let mut current_id = selected_id;
    while let Some(cid) = current_id {
        if remaining.is_empty() {
            break;
        }
        let entry = entries_by_id.get(cid)?;
        match &entry.kind {
            LegacyV3IndexKind::ModelChange { provider, model_id } => {
                if remaining.remove("model_change") {
                    model = Some(LaneModelRef {
                        provider: provider.clone(),
                        model_id: model_id.clone(),
                    });
                }
            }
            LegacyV3IndexKind::ThinkingLevelChange { thinking_level: tl } => {
                if remaining.remove("thinking_level_change") {
                    thinking_level = Some(*tl);
                }
            }
            LegacyV3IndexKind::ActiveToolsChange { active_tool_names: atn } => {
                if remaining.remove("active_tools_change") {
                    active_tool_names = Some(atn.clone());
                }
            }
            _ => {}
        }
        current_id = entry.parent_id.as_deref();
    }

    let model = model?;
    let thinking_level = thinking_level?;
    Some(LaneConfiguration {
        model,
        thinking_level,
        active_tool_names: active_tool_names.unwrap_or_default(),
    })
}

fn index_legacy_v3_entry(
    entry: &LegacyV3Entry,
    line_number: usize,
    seq: i64,
    entries: &HashMap<String, LegacyV3IndexEntry>,
) -> Result<LegacyV3IndexEntry, SessionError> {
    let mapped_parent_id = match entry.parent_id() {
        None => None,
        Some(pid) => {
            let parent_entry = entries.get(pid).ok_or_else(|| {
                SessionError::new(
                    SessionErrorKind::Io,
                    format!(
                        "Legacy v3 entry {} has a missing or forward parent at line {line_number}: {pid}",
                        entry.id()
                    ),
                )
            })?;
            parent_entry.mapped_id.clone()
        }
    };

    if !entry.is_retained() {
        let kind = match entry {
            LegacyV3Entry::Label { target_id, label, .. } => LegacyV3IndexKind::Label {
                target_id: target_id.clone(),
                label: label.clone(),
            },
            LegacyV3Entry::ModelChange { provider, model_id, .. } => {
                LegacyV3IndexKind::ModelChange {
                    provider: provider.clone(),
                    model_id: model_id.clone(),
                }
            }
            LegacyV3Entry::ThinkingLevelChange { thinking_level, .. } => {
                LegacyV3IndexKind::ThinkingLevelChange {
                    thinking_level: *thinking_level,
                }
            }
            LegacyV3Entry::ActiveToolsChange { active_tool_names, .. } => {
                LegacyV3IndexKind::ActiveToolsChange {
                    active_tool_names: active_tool_names.clone(),
                }
            }
            LegacyV3Entry::SessionInfo { .. } => LegacyV3IndexKind::SessionInfo,
            _ => unreachable!(),
        };
        return Ok(LegacyV3IndexEntry {
            id: entry.id().to_owned(),
            parent_id: entry.parent_id().map(str::to_owned),
            mapped_id: mapped_parent_id,
            seq: None,
            kind,
        });
    }

    let ts_millis = parse_iso_millis(match entry {
        LegacyV3Entry::Message { timestamp, .. }
        | LegacyV3Entry::Custom { timestamp, .. }
        | LegacyV3Entry::CustomMessage { timestamp, .. }
        | LegacyV3Entry::BranchSummary { timestamp, .. }
        | LegacyV3Entry::Compaction { timestamp, .. } => timestamp,
        _ => "",
    });
    let mapped_id = uuidv7(Some(ts_millis as f64)).map_err(|e| {
        SessionError::new(SessionErrorKind::Io, format!("UUIDv7 generation failed: {e}"))
    })?;

    let kind = match entry {
        LegacyV3Entry::BranchSummary { from_id, from_hook, .. } => LegacyV3IndexKind::BranchSummary {
            from_id: from_id.clone(),
            from_hook: *from_hook,
        },
        LegacyV3Entry::Compaction { first_kept_entry_id, from_hook, .. } => LegacyV3IndexKind::Compaction {
            first_kept_entry_id: first_kept_entry_id.clone(),
            from_hook: *from_hook,
        },
        LegacyV3Entry::Message { .. } => LegacyV3IndexKind::Message,
        LegacyV3Entry::Custom { .. } => LegacyV3IndexKind::Custom,
        LegacyV3Entry::CustomMessage { .. } => LegacyV3IndexKind::CustomMessage,
        _ => unreachable!(),
    };

    Ok(LegacyV3IndexEntry {
        id: entry.id().to_owned(),
        parent_id: entry.parent_id().map(str::to_owned),
        mapped_id: Some(mapped_id),
        seq: Some(seq),
        kind,
    })
}

fn legacy_entry_usage(entry: &LegacyV3Entry) -> Option<Usage> {
    match entry {
        LegacyV3Entry::Message { message, .. } => match message {
            AgentMessage::Llm(maho_ai::types::Message::Assistant(assistant)) => Some(assistant.usage),
            AgentMessage::Llm(maho_ai::types::Message::ToolResult(tr)) => tr.usage,
            _ => None,
        },
        LegacyV3Entry::Compaction { usage, .. } | LegacyV3Entry::BranchSummary { usage, .. } =>
            usage.as_ref().copied(),
        _ => None,
    }
}

struct LegacyV3Inventory {
    entries: HashMap<String, LegacyV3IndexEntry>,
    entry_order: Vec<String>,
    next_seq: i64,
    imported_usage: Usage,
    name: Option<String>,
    final_id: Option<String>,
}

async fn read_legacy_v3_inventory(
    reader: &dyn TextLineReader,
    context: &Context,
) -> Result<LegacyV3Inventory, SessionError> {
    let mut entries = HashMap::new();
    let mut entry_order = Vec::new();
    let mut next_seq = 1i64;
    let mut imported_usage = empty_usage();
    let mut name = None;
    let mut final_id = None;

    loop {
        let line = file_value(reader.read_line(context).await, "Failed to read legacy v3 source")?;
        let Some(line) = line else { break };
        if !line.terminated {
            break;
        }
        let line_number = entries.len() + 2;
        let entry = parse_legacy_v3_entry(&line.text)?;
        let eid = entry.id().to_owned();
        if entries.contains_key(&eid) {
            return Err(SessionError::new(
                SessionErrorKind::DuplicateId,
                format!("Duplicate legacy v3 entry id: {eid}"),
            ));
        }
        let indexed = index_legacy_v3_entry(&entry, line_number, next_seq, &entries)?;
        if indexed.is_retained() {
            next_seq += 1;
        }
        final_id = Some(eid.clone());
        if let LegacyV3Entry::SessionInfo { name: entry_name, .. } = &entry {
            name = entry_name.clone().filter(|n| !n.is_empty());
        }
        if let Some(usage) = legacy_entry_usage(&entry) {
            imported_usage = add_usage(&imported_usage, &usage);
        }
        entries.insert(eid.clone(), indexed);
        entry_order.push(eid);
    }

    Ok(LegacyV3Inventory {
        entries,
        entry_order,
        next_seq,
        imported_usage,
        name,
        final_id,
    })
}

fn normalize_legacy_v3_values(
    inventory: &LegacyV3Inventory,
) -> Result<Vec<CommittedValueSetWrite>, SessionError> {
    let entries = &inventory.entries;
    let mut next_seq = inventory.next_seq;
    let mut values = Vec::new();

    // 1. session name
    if let Some(name) = &inventory.name {
        let op = set_value(&session_name(), JsonValue::String(name.clone()));
        if let crate::harness::session::values::ValueWrite::Set(set) = op {
            values.push(CommittedValueSetWrite {
                kind: WriteKind::Value,
                op: crate::harness::session::commit::ValueOp::Set,
                seq: next_seq,
                namespace: set.namespace,
                key: set.key,
                value: set.value,
            });
            next_seq += 1;
        }
    }

    // 2. labels
    let mut labels = HashMap::new();
    for eid in &inventory.entry_order {
        let entry = &entries[eid];
        if let LegacyV3IndexKind::Label { target_id, label } = &entry.kind {
            if let Some(mapped_target_id) = resolve_legacy_id(entries, Some(target_id))? {
                match label {
                    Some(l) if !l.is_empty() => {
                        labels.insert(mapped_target_id, l.clone());
                    }
                    _ => {
                        labels.remove(&mapped_target_id);
                    }
                }
            }
        }
    }

    for (target_id, label) in labels {
        let op = set_value(&entry_label(&target_id), JsonValue::String(label));
        if let crate::harness::session::values::ValueWrite::Set(set) = op {
            values.push(CommittedValueSetWrite {
                kind: WriteKind::Value,
                op: crate::harness::session::commit::ValueOp::Set,
                seq: next_seq,
                namespace: set.namespace,
                key: set.key,
                value: set.value,
            });
            next_seq += 1;
        }
    }

    // 3. branch tip
    let final_mapped_id = resolve_legacy_id(entries, inventory.final_id.as_deref())?;
    let op = set_value(
        &branch_tip("main"),
        match final_mapped_id {
            Some(id) => JsonValue::String(id),
            None => JsonValue::Null,
        },
    );
    if let crate::harness::session::values::ValueWrite::Set(set) = op {
        values.push(CommittedValueSetWrite {
            kind: WriteKind::Value,
            op: crate::harness::session::commit::ValueOp::Set,
            seq: next_seq,
            namespace: set.namespace,
            key: set.key,
            value: set.value,
        });
        next_seq += 1;
    }

    // 4. configuration
    if let Some(config) = selected_configuration(entries, inventory.final_id.as_deref()) {
        let op = set_value(
            &lane_config("main"),
            serde_json::to_value(&config).unwrap_or(JsonValue::Null),
        );
        if let crate::harness::session::values::ValueWrite::Set(set) = op {
            values.push(CommittedValueSetWrite {
                kind: WriteKind::Value,
                op: crate::harness::session::commit::ValueOp::Set,
                seq: next_seq,
                namespace: set.namespace,
                key: set.key,
                value: set.value,
            });
            next_seq += 1;
        }

        let default_state = LaneState {
            current_operation_id: None,
            last_operation_id: None,
            inbox: Vec::new(),
        };
        let op = set_value(
            &lane_state("main"),
            serde_json::to_value(&default_state).unwrap_or(JsonValue::Null),
        );
        if let crate::harness::session::values::ValueWrite::Set(set) = op {
            values.push(CommittedValueSetWrite {
                kind: WriteKind::Value,
                op: crate::harness::session::commit::ValueOp::Set,
                seq: next_seq,
                namespace: set.namespace,
                key: set.key,
                value: set.value,
            });
            #[allow(unused_assignments)]
            {
                next_seq += 1;
            }
        }
    }

    Ok(values)
}

/// A captured legacy file exposed as repeatable logical v4 writes.
#[derive(Clone)]
pub struct LegacyV3Source {
    pub header: JsonlStorageHeader,
    pub imported_usage: Usage,
    pub next_seq: i64,
    pub values: Vec<CommittedValueSetWrite>,
    file_system: Arc<dyn FileSystem>,
    path: String,
    entries: HashMap<String, LegacyV3IndexEntry>,
    entry_order: Vec<String>,
}

impl LegacyV3Source {
    pub async fn read(
        file_system: Arc<dyn FileSystem>,
        path: &str,
        context: &Context,
    ) -> Result<Self, SessionError> {
        let reader = file_value(
            file_system.open_text_line_reader(path, context).await,
            &format!("Failed to open legacy v3 source {path}"),
        )?;
        let parsed = read_jsonl_header(reader.as_ref(), path, context).await;
        let result = async {
            let parsed = parsed?;
            let v3_header = match parsed {
                JsonlParsedSessionHeader::V3Legacy(header) => header,
                _ => {
                    return Err(SessionError::new(
                        SessionErrorKind::Io,
                        format!("Invalid legacy v3 JSONL storage {path}: expected format 3 header"),
                    ));
                }
            };
            let inventory = read_legacy_v3_inventory(reader.as_ref(), context).await?;
            let values = normalize_legacy_v3_values(&inventory)?;
            let total_next_seq = inventory.next_seq + values.len() as i64;
            let header = normalize_legacy_v3_header(&file_system, &v3_header, context).await;
            Ok::<_, SessionError>(Self {
                header,
                imported_usage: inventory.imported_usage,
                next_seq: total_next_seq,
                values,
                file_system: file_system.clone(),
                path: path.to_owned(),
                entries: inventory.entries,
                entry_order: inventory.entry_order,
            })
        }
        .await;
        reader.close(context).await;
        result
    }

    pub fn entry_structures(&self) -> Vec<crate::harness::session::types::EntryStructure> {
        let mut structures = Vec::new();
        for eid in &self.entry_order {
            let entry = &self.entries[eid];
            if !entry.is_retained() {
                continue;
            }
            let Some(mapped_id) = &entry.mapped_id else { continue };
            let parent_id = resolve_legacy_id(&self.entries, entry.parent_id.as_deref()).unwrap_or(None);
            let entry_type = match &entry.kind {
                LegacyV3IndexKind::Message | LegacyV3IndexKind::CustomMessage => {
                    crate::harness::session::types::EntryType::Message
                }
                LegacyV3IndexKind::Custom => crate::harness::session::types::EntryType::Custom,
                LegacyV3IndexKind::BranchSummary { .. } => {
                    crate::harness::session::types::EntryType::BranchSummary
                }
                LegacyV3IndexKind::Compaction { .. } => {
                    crate::harness::session::types::EntryType::Compaction
                }
                _ => continue,
            };
            structures.push(crate::harness::session::types::EntryStructure {
                id: mapped_id.clone(),
                parent_id,
                seq: entry.seq.unwrap_or(0),
                timestamp: 0,
                entry_type,
                custom_type: None,
            });
        }
        structures
    }

    pub fn translate_fork_entry_id(&self, legacy_id: &str) -> Result<String, SessionError> {
        let entry = self.entries.get(legacy_id).ok_or_else(|| {
            SessionError::new(
                SessionErrorKind::Io,
                format!("Legacy v3 fork entry does not exist: {legacy_id}"),
            )
        })?;
        if !entry.is_retained() {
            return Err(SessionError::new(
                SessionErrorKind::Io,
                format!("Legacy v3 fork entry is not a retained entry: {legacy_id}"),
            ));
        }
        entry.mapped_id.clone().ok_or_else(|| {
            SessionError::new(SessionErrorKind::Io, "Unmapped retained entry")
        })
    }

    fn collect_required_tail_message_ids(
        &self,
        is_entry_selected: Option<&Arc<dyn Fn(&str) -> bool + Send + Sync>>,
    ) -> Result<HashSet<String>, SessionError> {
        let mut required_ids = HashSet::new();
        for eid in &self.entry_order {
            let entry = &self.entries[eid];
                if let LegacyV3IndexKind::Compaction { first_kept_entry_id, .. } = &entry.kind {
                let mapped_id = entry.mapped_id.as_deref().unwrap_or("");
                let compaction_is_selected = is_entry_selected.map_or(true, |f| f(mapped_id));
                if !compaction_is_selected {
                    continue;
                }
                let tail = retained_tail_structure(
                    &entry.id,
                    first_kept_entry_id,
                    entry.parent_id.as_deref(),
                    &self.entries,
                )?;
                for tail_entry in tail {
                    let can_produce_context_message =
                        tail_entry.is_retained() && !matches!(tail_entry.kind, LegacyV3IndexKind::Custom);
                    if can_produce_context_message {
                        required_ids.insert(tail_entry.id.clone());
                    }
                }
            }
        }
        Ok(required_ids)
    }

    pub async fn writes(
        &self,
        context: Context,
        is_entry_selected: Option<Arc<dyn Fn(&str) -> bool + Send + Sync>>,
    ) -> Result<Vec<CommittedWrite>, SessionError> {
        let required_tail_ids = self.collect_required_tail_message_ids(is_entry_selected.as_ref())?;
        let mut tail_messages = HashMap::new();

        let reader = file_value(
            self.file_system.open_text_line_reader(&self.path, &context).await,
            &format!("Failed to reopen legacy v3 source {}", self.path),
        )?;
        let result = async {
            let parsed = read_jsonl_header(reader.as_ref(), &self.path, &context).await?;
            let v3_header = match parsed {
                JsonlParsedSessionHeader::V3Legacy(header) => header,
                _ => return Err(SessionError::new(SessionErrorKind::Io, "Legacy v3 source header changed")),
            };
            if v3_header.id != self.header.id || v3_header.cwd != self.header.cwd {
                return Err(SessionError::new(SessionErrorKind::Io, "Legacy v3 source header changed"));
            }

            let mut out_writes = Vec::new();

            for eid in &self.entry_order {
                let indexed = &self.entries[eid];
                let line = file_value(reader.read_line(&context).await, "Failed to reread legacy v3 source")?;
                let Some(line) = line else {
                    return Err(SessionError::new(
                        SessionErrorKind::Io,
                        "Legacy v3 source ended before captured entries",
                    ));
                };
                if !line.terminated {
                    return Err(SessionError::new(
                        SessionErrorKind::Io,
                        "Legacy v3 source ended before captured entries",
                    ));
                }
                let raw_entry = parse_legacy_v3_entry(&line.text)?;
                if raw_entry.id() != indexed.id || raw_entry.entry_type_str() != indexed.kind.entry_type_str() {
                    return Err(SessionError::new(SessionErrorKind::Io, "Legacy v3 source changed"));
                }

                if required_tail_ids.contains(raw_entry.id()) {
                    if let Some(msg) = project_context_message(&raw_entry, &self.entries)? {
                        tail_messages.insert(raw_entry.id().to_owned(), msg);
                    }
                }

                if !indexed.is_retained() || !raw_entry.is_retained() {
                    continue;
                }
                let mapped_id = indexed.mapped_id.as_deref().unwrap_or("");
                if let Some(f) = is_entry_selected.as_ref() {
                    if !f(mapped_id) {
                        continue;
                    }
                }

                let mut retained_tail = Vec::new();
                if let LegacyV3IndexKind::Compaction { first_kept_entry_id, .. } = &indexed.kind {
                    let ancestors = retained_tail_structure(
                        &indexed.id,
                        first_kept_entry_id,
                        indexed.parent_id.as_deref(),
                        &self.entries,
                    )?;
                    for ancestor in ancestors {
                        if let Some(msg) = tail_messages.get(&ancestor.id) {
                            retained_tail.push(msg.clone());
                        }
                    }
                    retained_tail.reverse();
                }

                let write = normalize_retained_entry(&raw_entry, indexed, retained_tail, &self.entries)?;
                out_writes.push(CommittedWrite::Entry(write));
            }

            for val in &self.values {
                out_writes.push(CommittedWrite::Value(
                    crate::harness::session::commit::CommittedValueWrite::Set(val.clone()),
                ));
            }

            Ok::<_, SessionError>(out_writes)
        }
        .await;
        reader.close(&context).await;
        result
    }
}

impl LegacyV3IndexKind {
    fn entry_type_str(&self) -> &'static str {
        match self {
            LegacyV3IndexKind::Message => "message",
            LegacyV3IndexKind::Custom => "custom",
            LegacyV3IndexKind::CustomMessage => "custom_message",
            LegacyV3IndexKind::BranchSummary { .. } => "branch_summary",
            LegacyV3IndexKind::Compaction { .. } => "compaction",
            LegacyV3IndexKind::Label { .. } => "label",
            LegacyV3IndexKind::ModelChange { .. } => "model_change",
            LegacyV3IndexKind::ThinkingLevelChange { .. } => "thinking_level_change",
            LegacyV3IndexKind::ActiveToolsChange { .. } => "active_tools_change",
            LegacyV3IndexKind::SessionInfo => "session_info",
        }
    }
}


