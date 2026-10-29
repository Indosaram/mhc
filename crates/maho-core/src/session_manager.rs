//! Port of senpi packages/coding-agent/src/core/session-manager.ts.
//!
//! Sessions are append-only JSONL trees. Entries are kept as serde_json::Value so that a file
//! read and re-written is byte-identical (key order preserved, unknown fields kept), which is what
//! makes maho able to read and resume omo/senpi v3 sessions.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};

use crate::config::{app_name, get_agent_dir, get_sessions_dir};
use crate::messages::{
    create_branch_summary_message, create_compaction_summary_message, create_custom_message,
};
use crate::paths::{PathInputOptions, normalize_path, resolve_path};

pub const CURRENT_SESSION_VERSION: i64 = 3;

const SESSION_READ_BUFFER_SIZE: usize = 1024 * 1024;
const SESSION_HEADER_READ_BUFFER_SIZE: usize = 4096;
const MAX_SESSION_HEADER_SCAN_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Default)]
pub struct NewSessionOptions {
    pub id: Option<String>,
    pub parent_session: Option<String>,
    pub forked_from_leaf_id: Option<String>,
    pub spawned_by: Option<crate::session_lifecycle_types::SpawnedBy>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct UsageTotals {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub cost: f64,
    pub cost_unknown: bool,
    pub latest_cache_hit_rate: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionContext {
    pub messages: Vec<Value>,
    /// senpi `getSessionContextEntryId`: the session entry each projected message came from,
    /// index-aligned with `messages`. A typed side channel, never written into message JSON.
    pub entry_ids: Vec<String>,
    pub thinking_level: String,
    pub thinking_selection: Option<Value>,
    pub configuration_update: Option<Value>,
    pub model: Option<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionTreeNode {
    pub entry: Value,
    pub children: Vec<SessionTreeNode>,
    pub label: Option<String>,
    pub label_timestamp: Option<String>,
}

pub fn create_session_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

fn is_valid_session_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    let alnum = |b: u8| b.is_ascii_alphanumeric();
    !bytes.is_empty()
        && alnum(bytes[0])
        && alnum(bytes[bytes.len() - 1])
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'.' || *b == b'_' || *b == b'-')
}

/// The id a session file's name carries (`<timestamp>_<id>.jsonl`), when the name has that shape
/// and the id is valid.
pub fn session_file_name_id(path: &str) -> Option<String> {
    let name = Path::new(path).file_name()?.to_str()?;
    let stem = name.strip_suffix(".jsonl")?;
    // `YYYY-MM-DDTHH-MM-SS-mmmZ_<id>`: a 24-byte timestamp, `_`, then a non-empty id.
    let (timestamp, id) = (stem.get(..24)?, stem.get(25..)?);
    let digits = |range: std::ops::Range<usize>| {
        timestamp
            .get(range)
            .is_some_and(|part| part.bytes().all(|byte| byte.is_ascii_digit()))
    };
    let shaped = digits(0..4)
        && digits(5..7)
        && digits(8..10)
        && digits(11..13)
        && digits(14..16)
        && digits(17..19)
        && digits(20..23)
        && timestamp.as_bytes().get(4) == Some(&b'-')
        && timestamp.as_bytes().get(7) == Some(&b'-')
        && timestamp.as_bytes().get(10) == Some(&b'T')
        && timestamp.as_bytes().get(13) == Some(&b'-')
        && timestamp.as_bytes().get(16) == Some(&b'-')
        && timestamp.as_bytes().get(19) == Some(&b'-')
        && timestamp.as_bytes().get(23) == Some(&b'Z')
        && stem.as_bytes().get(24) == Some(&b'_');
    if !shaped || id.is_empty() {
        return None;
    }
    is_valid_session_id(id).then(|| id.to_owned())
}

/// Opening or creating a session at a path without an explicit id adopts the id its file name carries.
fn with_file_name_id(path: &str, options: Option<NewSessionOptions>) -> Option<NewSessionOptions> {
    if options.as_ref().is_some_and(|options| options.id.is_some()) {
        return options;
    }
    match session_file_name_id(path) {
        Some(id) => Some(NewSessionOptions {
            id: Some(id),
            ..options.unwrap_or_default()
        }),
        None => options,
    }
}

pub fn assert_valid_session_id(id: &str) {
    if !is_valid_session_id(id) {
        panic!(
            "Session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character"
        );
    }
}

fn generate_id(exists: impl Fn(&str) -> bool) -> String {
    for _ in 0..100 {
        let id = uuid::Uuid::new_v4().to_string()[..8].to_owned();
        if !exists(&id) {
            return id;
        }
    }
    uuid::Uuid::new_v4().to_string()
}

fn entry_type(entry: &Value) -> &str {
    entry
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn migrate_v1_to_v2(entries: &mut [Value]) {
    let mut ids: HashMap<String, Value> = HashMap::new();
    let mut prev_id: Option<String> = None;
    for index in 0..entries.len() {
        if entry_type(&entries[index]) == "session" {
            entries[index]["version"] = Value::from(2);
            continue;
        }
        let id = generate_id(|candidate| ids.contains_key(candidate));
        ids.insert(id.clone(), Value::Null);
        entries[index]["id"] = Value::from(id.clone());
        match &prev_id {
            Some(previous) => entries[index]["parentId"] = Value::from(previous.clone()),
            None => entries[index]["parentId"] = Value::Null,
        }
        prev_id = Some(id);

        if entry_type(&entries[index]) == "compaction" {
            let first_kept_index = entries[index]
                .get("firstKeptEntryIndex")
                .and_then(Value::as_i64);
            if let Some(first_kept_index) = first_kept_index {
                if let Some(target) = entries.get(first_kept_index as usize)
                    && entry_type(target) != "session"
                {
                    let target_id = target.get("id").cloned().unwrap_or(Value::Null);
                    entries[index]["firstKeptEntryId"] = target_id;
                }
                if let Some(object) = entries[index].as_object_mut() {
                    object.shift_remove("firstKeptEntryIndex");
                }
            }
        }
    }
}

fn migrate_v2_to_v3(entries: &mut [Value]) {
    for entry in entries.iter_mut() {
        if entry_type(entry) == "session" {
            entry["version"] = Value::from(3);
            continue;
        }
        if entry_type(entry) == "message"
            && let Some(message) = entry.get_mut("message")
            && message.get("role").and_then(Value::as_str) == Some("hookMessage")
        {
            message["role"] = Value::from("custom");
        }
    }
}

pub fn migrate_to_current_version(entries: &mut [Value]) -> bool {
    let version = entries
        .iter()
        .find(|entry| entry_type(entry) == "session")
        .and_then(|header| header.get("version").and_then(Value::as_i64))
        .unwrap_or(1);
    if version >= CURRENT_SESSION_VERSION {
        return false;
    }
    if version < 2 {
        migrate_v1_to_v2(entries);
    }
    if version < 3 {
        migrate_v2_to_v3(entries);
    }
    true
}

pub fn migrate_session_entries(entries: &mut [Value]) {
    migrate_to_current_version(entries);
}

pub fn parse_session_entries(content: &str) -> Vec<Value> {
    content
        .trim()
        .split('\n')
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect()
}

fn parse_session_entry_line(line: &str) -> Option<Value> {
    if line.trim().is_empty() {
        return None;
    }
    serde_json::from_str::<Value>(line).ok()
}

pub fn get_latest_compaction_entry(entries: &[Value]) -> Option<Value> {
    entries
        .iter()
        .rev()
        .find(|entry| entry_type(entry) == "compaction")
        .cloned()
}

fn build_entry_index(entries: &[Value]) -> HashMap<&str, &Value> {
    let mut index = HashMap::new();
    for entry in entries {
        if let Some(id) = entry.get("id").and_then(Value::as_str) {
            index.insert(id, entry);
        }
    }
    index
}

pub fn build_session_path(entries: &[Value], leaf_id: Option<&str>) -> Vec<Value> {
    session_path_refs(entries, leaf_id).into_iter().cloned().collect()
}

// PERF-DIVERGENCE(senpi session-manager.ts): upstream walks the path once keeping object references; the public Vec<Value> wrappers still clone, but internal context building borrows the path instead of deep-cloning every entry on it (twice). Same entries, same order.
fn session_path_refs<'a>(entries: &'a [Value], leaf_id: Option<&str>) -> Vec<&'a Value> {
    let index = build_entry_index(entries);
    let mut leaf = match leaf_id {
        None => entries.last(),
        Some("") => return Vec::new(),
        Some(id) => index.get(id).copied().or_else(|| entries.last()),
    };
    let mut path = Vec::new();
    while let Some(current) = leaf {
        path.push(current);
        leaf = current
            .get("parentId")
            .and_then(Value::as_str)
            .and_then(|parent| index.get(parent).copied());
    }
    path.reverse();
    path
}

#[derive(Debug, Clone, PartialEq)]
struct ContextSettings {
    thinking_level: String,
    thinking_selection: Option<Value>,
    configuration_update: Option<Value>,
    model: Option<(String, String)>,
}

fn normalize_provider(provider: &str) -> String {
    maho_ai::legacy_provider_ids::normalize_provider_id(provider)
}

fn get_session_context_settings(path: &[&Value]) -> ContextSettings {
    let mut thinking_level = "off".to_owned();
    let mut thinking_selection: Option<Value> = None;
    let mut configuration_update: Option<Value> = None;
    let mut model: Option<(String, String)> = None;
    let mut is_model_selection_explicit = false;
    let mut is_in_fallback_window = false;
    let mut pre_fallback_thinking_level = thinking_level.clone();
    let mut pre_fallback_thinking_selection: Option<Value> = None;

    for entry in path.iter().copied() {
        match entry_type(entry) {
            "thinking_level_change" => {
                thinking_level = entry
                    .get("thinkingLevel")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                thinking_selection = entry.get("thinkingSelection").cloned();
            }
            "configuration_update" => {
                configuration_update = Some(json!({
                    "effort": entry.get("reasoning").and_then(|r| r.get("effort")).cloned().unwrap_or(Value::Null)
                }));
            }
            "model_change" => {
                let reason = entry.get("reason").and_then(Value::as_str);
                match reason {
                    Some("fallback") => {
                        if !is_in_fallback_window {
                            pre_fallback_thinking_level = thinking_level.clone();
                            pre_fallback_thinking_selection = thinking_selection.clone();
                            let original_provider =
                                entry.get("originalProvider").and_then(Value::as_str);
                            let original_model_id =
                                entry.get("originalModelId").and_then(Value::as_str);
                            if let (Some(provider), Some(model_id)) =
                                (original_provider, original_model_id)
                            {
                                model = Some((normalize_provider(provider), model_id.to_owned()));
                                is_model_selection_explicit = true;
                            }
                        }
                        is_in_fallback_window = true;
                    }
                    Some("fallback-revert") => {
                        if is_in_fallback_window {
                            thinking_level = pre_fallback_thinking_level.clone();
                            thinking_selection = pre_fallback_thinking_selection.clone();
                        }
                        is_in_fallback_window = false;
                    }
                    _ => {
                        is_in_fallback_window = false;
                        let provider = entry.get("provider").and_then(Value::as_str);
                        let model_id = entry.get("modelId").and_then(Value::as_str);
                        if let (Some(provider), Some(model_id)) = (provider, model_id) {
                            model = Some((normalize_provider(provider), model_id.to_owned()));
                            is_model_selection_explicit = true;
                        }
                    }
                }
            }
            "message" => {
                let message = entry.get("message");
                let is_assistant = message.and_then(|m| m.get("role")).and_then(Value::as_str)
                    == Some("assistant");
                if is_assistant && !is_in_fallback_window {
                    let provider = message
                        .and_then(|m| m.get("provider"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if is_model_selection_explicit
                        && model.as_ref().map(|(p, _)| p.as_str())
                            == Some(normalize_provider(provider).as_str())
                    {
                        continue;
                    }
                    let model_id = message
                        .and_then(|m| m.get("model"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    model = Some((normalize_provider(provider), model_id.to_owned()));
                    is_model_selection_explicit = false;
                }
            }
            _ => {}
        }
    }

    if is_in_fallback_window {
        thinking_level = pre_fallback_thinking_level;
        thinking_selection = pre_fallback_thinking_selection;
    }

    ContextSettings {
        thinking_level,
        thinking_selection,
        configuration_update,
        model,
    }
}

pub fn session_entry_to_context_messages(entry: &Value) -> Vec<Value> {
    match entry_type(entry) {
        "message" => {
            let Some(message) = entry.get("message") else {
                return Vec::new();
            };
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if matches!(role, "user" | "assistant" | "toolResult")
                && message.get("content").is_none_or(Value::is_null)
            {
                let mut patched = message.clone();
                patched["content"] = Value::Array(Vec::new());
                return vec![patched];
            }
            vec![message.clone()]
        }
        "configuration_update" => vec![json!({
            "role": "configurationUpdate",
            "content": [],
            "effort": entry.get("reasoning").and_then(|r| r.get("effort")).cloned().unwrap_or(Value::Null),
            "timestamp": parse_entry_timestamp(entry),
        })],
        "custom_message" => {
            let content = entry
                .get("content")
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new()));
            let timestamp = entry
                .get("timestamp")
                .and_then(Value::as_str)
                .unwrap_or_default();
            vec![create_custom_message(
                entry
                    .get("customType")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                content,
                entry
                    .get("display")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                entry.get("details").cloned(),
                timestamp,
            )]
        }
        "branch_summary" => {
            let summary = entry
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if summary.is_empty() {
                return Vec::new();
            }
            vec![create_branch_summary_message(
                summary,
                entry
                    .get("fromId")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                entry
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            )]
        }
        "compaction" => vec![create_compaction_summary_message(
            entry
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            entry
                .get("tokensBefore")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            entry
                .get("timestamp")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            entry.get("details").cloned(),
        )],
        _ => Vec::new(),
    }
}

fn parse_entry_timestamp(entry: &Value) -> i64 {
    entry
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).ok())
        .map(|value| value.timestamp_millis())
        .unwrap_or(0)
}

pub fn build_context_entries(entries: &[Value], leaf_id: Option<&str>) -> Vec<Value> {
    context_entry_refs(session_path_refs(entries, leaf_id))
        .into_iter()
        .cloned()
        .collect()
}

// PERF-DIVERGENCE(senpi session-manager.ts): upstream selects compaction-aware entries from shared object references; this borrows from the path instead of cloning each selected entry. Same entries, same order.
fn context_entry_refs(path: Vec<&Value>) -> Vec<&Value> {
    let mut compaction: Option<&Value> = None;
    for entry in path.iter().copied() {
        if entry_type(entry) == "compaction" {
            compaction = Some(entry);
        }
    }
    let Some(compaction) = compaction else {
        return path;
    };
    let Some(compaction_index) = path
        .iter()
        .position(|entry| entry.get("id") == compaction.get("id"))
    else {
        return path;
    };
    let first_kept_entry_id = compaction
        .get("firstKeptEntryId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut context_entries = vec![compaction];
    let mut found_first_kept = false;
    for entry in path.iter().copied().take(compaction_index) {
        if entry_type(entry) == "compaction" {
            continue;
        }
        if entry.get("id").and_then(Value::as_str) == Some(first_kept_entry_id) {
            found_first_kept = true;
        }
        if found_first_kept {
            context_entries.push(entry);
        }
    }
    context_entries.extend(path.iter().copied().skip(compaction_index + 1));
    context_entries
}

pub fn build_session_context(entries: &[Value], leaf_id: Option<&str>) -> SessionContext {
    // PERF-DIVERGENCE(senpi session-manager.ts): upstream builds the path once; the previous port built and deep-cloned it twice, this builds it once as references shared by the settings scan and the context-entry selection. Output is identical.
    let path = session_path_refs(entries, leaf_id);
    let settings = get_session_context_settings(&path);
    // senpi projectSession: only entries retained by an `omit-unsafe-v1` checkpoint are repaired;
    // later messages keep their normal semantics and the source transcript is never edited.
    let retained_ids: Option<std::collections::HashSet<&str>> = path
        .iter()
        .rposition(|entry| {
            entry_type(entry) == "compaction"
                && entry
                    .get("details")
                    .and_then(|details| details.get("retainedMessagePolicy"))
                    .and_then(Value::as_str)
                    == Some("omit-unsafe-v1")
        })
        .map(|recovery_index| path[..recovery_index].iter().filter_map(|entry| entry.get("id").and_then(Value::as_str)).collect());
    let projected: Vec<(&Value, Vec<Value>)> = context_entry_refs(path)
        .into_iter()
        .map(|entry| (entry, session_entry_to_context_messages(entry)))
        .collect();
    let mut retained_messages: Vec<Value> = Vec::new();
    if let Some(retained_ids) = &retained_ids {
        for (entry, messages) in &projected {
            if entry.get("id").and_then(Value::as_str).is_some_and(|id| retained_ids.contains(id)) {
                retained_messages.extend(messages.iter().cloned());
            }
        }
        retained_messages = crate::retained_message_projection::project_retained_messages(&retained_messages);
    }
    let mut repaired = retained_messages.into_iter();
    let (entry_ids, messages) = projected
        .into_iter()
        .flat_map(|(entry, messages)| {
            let entry_id = entry.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
            let is_retained = retained_ids
                .as_ref()
                .is_some_and(|ids| ids.contains(entry_id.as_str()));
            messages
                .into_iter()
                .map(|message| {
                    let message = if is_retained { repaired.next().unwrap_or(message) } else { message };
                    (entry_id.clone(), message)
                })
                .collect::<Vec<_>>()
        })
        .unzip();
    SessionContext {
        messages,
        entry_ids,
        thinking_level: settings.thinking_level,
        thinking_selection: settings.thinking_selection,
        configuration_update: settings.configuration_update,
        model: settings.model,
    }
}

pub fn get_default_session_dir_path(cwd: &str, agent_dir: &str) -> String {
    let options = PathInputOptions::default();
    let resolved_cwd = resolve_path(cwd, cwd, &options);
    let resolved_agent_dir = resolve_path(agent_dir, agent_dir, &options);
    let safe_path = format!(
        "--{}--",
        resolved_cwd
            .trim_start_matches(['/', '\\'])
            .replace(['/', '\\', ':'], "-")
    );
    Path::new(&resolved_agent_dir)
        .join("sessions")
        .join(safe_path)
        .to_string_lossy()
        .into_owned()
}

pub fn get_default_session_dir(cwd: &str) -> String {
    get_default_session_dir_with(cwd, &get_agent_dir())
}

pub fn get_default_session_dir_with(cwd: &str, agent_dir: &str) -> String {
    let session_dir = get_default_session_dir_path(cwd, agent_dir);
    let _ = std::fs::create_dir_all(&session_dir);
    session_dir
}

pub fn load_entries_from_file(file_path: &str) -> Vec<Value> {
    let resolved = normalize_path(file_path, &PathInputOptions::default());
    let Ok(file) = std::fs::File::open(&resolved) else {
        return Vec::new();
    };
    let mut reader = BufReader::with_capacity(SESSION_READ_BUFFER_SIZE, file);
    let mut entries = Vec::new();
    let mut buffer: Vec<u8> = Vec::new();
    loop {
        buffer.clear();
        match reader.read_until(b'\n', &mut buffer) {
            Ok(0) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buffer);
                if let Some(entry) = parse_session_entry_line(line.trim_end_matches(['\n', '\r'])) {
                    entries.push(entry);
                }
            }
            Err(_) => break,
        }
    }
    let Some(header) = entries.first() else {
        return entries;
    };
    if entry_type(header) != "session" || header.get("id").and_then(Value::as_str).is_none() {
        return Vec::new();
    }
    entries
}

fn read_session_header(file_path: &str) -> Option<Value> {
    let mut file = std::fs::File::open(file_path).ok()?;
    let mut scanned = 0usize;
    let mut chunk = vec![0u8; SESSION_HEADER_READ_BUFFER_SIZE];
    let mut pending: Vec<u8> = Vec::new();
    while scanned < MAX_SESSION_HEADER_SCAN_BYTES {
        let read_length = chunk.len().min(MAX_SESSION_HEADER_SCAN_BYTES - scanned);
        let bytes_read = file.read(&mut chunk[..read_length]).ok()?;
        if bytes_read == 0 {
            let line = String::from_utf8_lossy(&pending).into_owned();
            return header_candidate(&line).unwrap_or(None);
        }
        scanned += bytes_read;
        pending.extend_from_slice(&chunk[..bytes_read]);
        while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = pending.drain(..=newline).collect();
            let line = String::from_utf8_lossy(&line).into_owned();
            if let Some(candidate) = header_candidate(line.trim_end_matches(['\n', '\r'])) {
                return candidate;
            }
        }
    }
    let mut probe = [0u8; 1];
    if file.read(&mut probe).ok()? == 0 {
        let line = String::from_utf8_lossy(&pending).into_owned();
        return header_candidate(&line).unwrap_or(None);
    }
    None
}

fn header_candidate(line: &str) -> Option<Option<Value>> {
    if line.trim().is_empty() {
        return None;
    }
    let entry = parse_session_entry_line(line)?;
    if entry_type(&entry) != "session" || entry.get("id").and_then(Value::as_str).is_none() {
        return Some(None);
    }
    Some(Some(entry))
}

pub fn find_most_recent_session(session_dir: &str, cwd: Option<&str>) -> Option<String> {
    let resolved_session_dir = normalize_path(session_dir, &PathInputOptions::default());
    let resolved_cwd = cwd.map(|cwd| resolve_path(cwd, cwd, &PathInputOptions::default()));
    let entries = std::fs::read_dir(&resolved_session_dir).ok()?;
    let mut candidates: Vec<(std::time::SystemTime, String)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
            continue;
        }
        let path_string = path.to_string_lossy().into_owned();
        let Some(Some(header)) = header_candidate_file(&path_string) else {
            continue;
        };
        if let Some(resolved_cwd) = &resolved_cwd {
            let header_cwd = header.get("cwd").and_then(Value::as_str);
            let matches = header_cwd
                .filter(|cwd| !cwd.is_empty())
                .map(|cwd| resolve_path(cwd, cwd, &PathInputOptions::default()) == *resolved_cwd)
                .unwrap_or(false);
            if !matches {
                continue;
            }
        }
        let modified = std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok();
        if let Some(modified) = modified {
            candidates.push((modified, path_string));
        }
    }
    candidates.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    candidates.into_iter().next().map(|(_, path)| path)
}

fn header_candidate_file(path: &str) -> Option<Option<Value>> {
    read_session_header(path).map(Some)
}

pub fn resolve_moved_session_path(path: &str) -> String {
    crate::schedule::moved_paths::resolve(path)
}

#[derive(Debug, Clone, Default)]
struct SessionOwnership {
    session_file: Option<String>,
    persisted: bool,
    hold: Option<crate::schedule::runner_lease::SessionHold>,
    hold_error: Option<String>,
    /// Exclusive lock on the session's sidecar lock file; `prune_sessions` skips locked sessions.
    retention_lock: Option<Arc<RetentionLock>>,
    retention_lock_error: Option<String>,
}

/// Exclusive lock on `<session>.jsonl.lock`. The session file itself is never locked: Windows
/// byte-range locks are mandatory and would make other handles' writes to it fail.
#[derive(Debug)]
struct RetentionLock {
    // Held only for its lock; closing it on drop releases the lock.
    _file: std::fs::File,
    path: PathBuf,
}

impl RetentionLock {
    /// Unlinks the sidecar. Only called while this lock is verified to be on the path's current
    /// inode; it is never done on session exit, so a stable inode is what contenders lock.
    /// Not done on non-Unix platforms, where inode identity cannot be verified on stable Rust.
    fn unlink_sidecar(&self) {
        #[cfg(unix)]
        if let Err(error) = std::fs::remove_file(&self.path) {
            crate::hidden_stdout_log::append_debug_note(&format!(
                "session retention: cannot remove {}: {error}",
                self.path.display()
            ));
        }
    }
}

fn retention_lock_path(session_file: &Path) -> PathBuf {
    let mut name = session_file.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// Takes the exclusive lock, then checks the locked handle is still the file at `path`. A lock
/// on an inode that was unlinked (or replaced) after the open protects nothing. `Ok(false)` means
/// the identity no longer matches. On non-Unix platforms sidecars are never unlinked, so the
/// identity cannot change and the check is a no-op.
fn lock_verified(file: &std::fs::File, path: &Path) -> std::io::Result<bool> {
    #[cfg(not(unix))]
    let _ = path;
    file.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "session lock is held by another process",
        ),
        std::fs::TryLockError::Error(error) => error,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let locked = file.metadata()?;
        Ok(match std::fs::metadata(path) {
            Ok(current) => locked.dev() == current.dev() && locked.ino() == current.ino(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error),
        })
    }
    #[cfg(not(unix))]
    Ok(true)
}

/// Opens (creating if needed) the sidecar lock file and takes a verified exclusive lock on it,
/// retrying once when the sidecar was replaced between open and lock. A second mismatch is
/// reported as `WouldBlock` so callers skip the session.
fn open_retention_lock(session_file: &Path) -> std::io::Result<RetentionLock> {
    let path = retention_lock_path(session_file);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    for _ in 0..2 {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        if lock_verified(&file, &path)? {
            return Ok(RetentionLock { _file: file, path });
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        "session lock file keeps being replaced",
    ))
}

#[derive(Debug)]
struct SessionWriterToken(Arc<Mutex<SessionOwnership>>);

impl crate::session_write_reservation::SessionWriterOwner for SessionWriterToken {
    fn get_session_file(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .session_file
            .clone()
    }

    fn is_persisted(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .persisted
    }
}

/// Lazily materialized non-header entries shared by every reader until the next mutation of
/// `file_entries`. A clone starts empty so clones never share a stale view.
#[derive(Debug, Default)]
struct EntriesCache(Mutex<Option<Arc<Vec<Value>>>>);

impl Clone for EntriesCache {
    fn clone(&self) -> Self {
        Self::default()
    }
}

#[derive(Debug, Clone)]
pub struct SessionManager {
    session_id: String,
    session_file: Option<String>,
    session_dir: String,
    cwd: String,
    persist: bool,
    flushed: bool,
    file_entries: Vec<Value>,
    entries_cache: EntriesCache,
    by_id: HashMap<String, usize>,
    labels_by_id: HashMap<String, String>,
    label_timestamps_by_id: HashMap<String, String>,
    leaf_id: Option<String>,
    session_name_cache: Option<String>,
    usage_totals: UsageTotals,
    ownership: Arc<Mutex<SessionOwnership>>,
    writer_token: Arc<SessionWriterToken>,
    resident_store: crate::session_resident_store::ResidentStringStore,
    resident_blobs_dir: Arc<Mutex<Option<String>>>,
    mirror_trimmed: bool,
    /// senpi `fullEntryCount`: non-header entries in the full history, kept across mirror trims.
    full_entry_count: usize,
    /// `full_entry_count` at the latest mirror trim: entries before it may be missing from the mirror.
    trimmed_entry_count: usize,
    /// Refuses writes by a presumed-done owner whose fence another process advanced (senpi#3652).
    write_fence: crate::session_write_fence::SessionWriteFence,
    /// Bumped whenever the history is replaced (not on append or mirror trim); keys `reduce_history`'s memo.
    history_generation: u64,
    /// The mirror's last `full_entry_count - verbatim_from_count` entries are the history's own tail, in order.
    verbatim_from_count: usize,
    history_reductions: crate::session_history_reduce::HistoryReductions,
    last_write_error: Option<String>,
}

impl SessionManager {
    /// senpi's private constructor up to the session-file step: callers then load a file
    /// (`set_session_file`, which can refuse a corrupt one) or start a new session.
    fn build(cwd: &str, session_dir: &str, persist: bool) -> Self {
        let ownership = Arc::new(Mutex::new(SessionOwnership {
            session_file: None,
            persisted: persist,
            ..Default::default()
        }));
        let writer_token = Arc::new(SessionWriterToken(Arc::clone(&ownership)));
        // Lazily resolved: the session id only exists once the header loads or new_session() runs.
        // Unpersisted sessions disable eviction entirely - dropping strings with no backing would lose them.
        let resident_blobs_dir: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let provider_slot = Arc::clone(&resident_blobs_dir);
        let resident_store = crate::session_resident_store::ResidentStringStore::new(
            crate::session_resident_store::ResidentStringStoreOptions {
                blobs_dir: Some(Arc::new(move || {
                    provider_slot
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone()
                })),
                ..Default::default()
            },
        );
        let manager = Self {
            last_write_error: None,
            session_id: String::new(),
            session_file: None,
            session_dir: normalize_path(session_dir, &PathInputOptions::default()),
            cwd: resolve_path(cwd, cwd, &PathInputOptions::default()),
            persist,
            flushed: false,
            file_entries: Vec::new(),
            entries_cache: EntriesCache::default(),
            by_id: HashMap::new(),
            labels_by_id: HashMap::new(),
            label_timestamps_by_id: HashMap::new(),
            leaf_id: None,
            session_name_cache: None,
            usage_totals: UsageTotals::default(),
            ownership,
            writer_token,
            resident_store,
            resident_blobs_dir,
            mirror_trimmed: false,
            full_entry_count: 0,
            trimmed_entry_count: 0,
            write_fence: crate::session_write_fence::SessionWriteFence::new(),
            history_generation: 0,
            verbatim_from_count: 0,
            history_reductions: crate::session_history_reduce::HistoryReductions::new(),
        };
        if persist && !manager.session_dir.is_empty() {
            let _ = std::fs::create_dir_all(&manager.session_dir);
        }
        if persist {
            crate::session_write_reservation::register_session_writer(&manager.writer_owner());
        }
        manager
    }

    pub fn dispose(&mut self) {
        crate::session_write_reservation::unregister_session_writer(&self.writer_owner());
        self.ownership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .hold = None;
        self.release_blobs_dir_unless_shared();
        // Entries stay readable after disposal (callers audit them for late applies); only the
        // derived materialized view is dropped.
        self.invalidate_entries_cache();
    }

    pub fn drop_materialized_cache(&self) {
        *self
            .entries_cache
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    pub fn park_idle_memory(&mut self) -> bool {
        if !self.persist
            || !self.flushed
            || self.resident_store.resolved_blobs_dir().is_none()
            || !self
                .session_file
                .as_ref()
                .is_some_and(|path| Path::new(path).exists())
        {
            return false;
        }
        let store = self.resident_store.clone();
        for entry in &mut self.file_entries {
            store.externalize_in_place_with_min(entry, 1024);
        }
        self.resident_store.spill_resident();
        self.invalidate_entries_cache();
        self.file_entries.shrink_to_fit();
        true
    }

    fn writer_owner(&self) -> Arc<dyn crate::session_write_reservation::SessionWriterOwner> {
        self.writer_token.clone() as Arc<dyn crate::session_write_reservation::SessionWriterOwner>
    }

    fn sync_session_file_ownership(&self) {
        self.sync_session_file_hold(false);
    }

    fn sync_session_file_hold(&self, expect_existing: bool) {
        let hold = if self.persist {
            self.session_file
                .as_ref()
                .map(|file| {
                    crate::schedule::runner_lease::hold_session_file(
                        Path::new(file),
                        &self.session_id,
                        &self.cwd,
                        expect_existing,
                    )
                })
                .transpose()
        } else {
            Ok(None)
        };
        let mut ownership = self
            .ownership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // The sidecar needs no existing session file, so a new session is covered from the start.
        // An unchanged session keeps its lock: taking a second one would conflict with it.
        let wanted = self
            .session_file
            .as_deref()
            .filter(|_| self.persist)
            .map(|file| retention_lock_path(Path::new(file)));
        if wanted.as_ref() != ownership.retention_lock.as_ref().map(|lock| &lock.path) {
            ownership.retention_lock = None;
            ownership.retention_lock_error = None;
            if let Some(file) = self.session_file.as_deref().filter(|_| self.persist) {
                match open_retention_lock(Path::new(file)) {
                    Ok(lock) => ownership.retention_lock = Some(Arc::new(lock)),
                    Err(error) => {
                        let message = format!(
                            "Session retention lock unavailable for {file}: {error}; other processes may prune this session"
                        );
                        crate::hidden_stdout_log::append_debug_note(&message);
                        ownership.retention_lock_error = Some(message);
                    }
                }
            }
        }
        ownership.session_file = self.session_file.clone();
        match hold {
            Ok(hold) => {
                ownership.hold = hold;
                ownership.hold_error = None;
            }
            Err(error) => {
                ownership.hold = None;
                ownership.hold_error = Some(error.to_string());
            }
        }
    }

    /// Why this session could not be protected from other processes' retention pruning, if so.
    pub fn retention_lock_error(&self) -> Option<String> {
        self.ownership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retention_lock_error
            .clone()
    }

    fn check_session_hold(&self) -> Result<(), String> {
        match &self
            .ownership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .hold_error
        {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    fn sync_resident_blobs_dir(&self) {
        *self
            .resident_blobs_dir
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = self.blobs_dir();
    }

    /// Must follow every mutation of `file_entries`: drops the shared materialized view.
    fn invalidate_entries_cache(&mut self) {
        *self.entries_cache.0.get_mut().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    fn externalize_file_entries(&mut self) {
        self.invalidate_entries_cache();
        let store = self.resident_store.clone();
        self.file_entries = self
            .file_entries
            .iter()
            .map(|entry| store.externalize(entry))
            .collect();
    }

    fn blobs_dir(&self) -> Option<String> {
        if !self.persist || self.session_id.is_empty() {
            return None;
        }
        Some(
            Path::new(&self.session_dir)
                .join("resident-blobs")
                .join(&self.session_id)
                .to_string_lossy()
                .into_owned(),
        )
    }

    fn release_blobs_dir_unless_shared(&self) {
        let Some(blobs_dir) = self.blobs_dir() else {
            return;
        };
        if let Some(session_file) = &self.session_file
            && crate::session_write_reservation::has_other_live_session_writer(
                session_file,
                &self.writer_owner(),
            )
        {
            return;
        }
        let _ = std::fs::remove_dir_all(blobs_dir);
    }

    pub fn create(
        cwd: &str,
        session_dir: Option<&str>,
        options: Option<NewSessionOptions>,
    ) -> Self {
        let dir = session_dir
            .map(str::to_owned)
            .unwrap_or_else(|| get_default_session_dir(cwd));
        let mut manager = Self::build(cwd, &dir, true);
        manager.new_session(options);
        manager
    }

    pub fn fork_from(
        source_path: &str,
        target_cwd: &str,
        session_dir: Option<&str>,
        options: Option<NewSessionOptions>,
    ) -> Result<Self, String> {
        let resolved_source_path = resolve_moved_session_path(&resolve_path(
            source_path,
            source_path,
            &PathInputOptions::default(),
        ));
        let resolved_target_cwd = resolve_moved_session_path(&resolve_path(
            target_cwd,
            target_cwd,
            &PathInputOptions::default(),
        ));
        let source_entries = load_entries_from_file(&resolved_source_path);
        if source_entries.is_empty() {
            return Err(format!(
                "Cannot fork: source session file is empty or invalid: {resolved_source_path}"
            ));
        }
        if !source_entries
            .iter()
            .any(|entry| entry_type(entry) == "session")
        {
            return Err(format!(
                "Cannot fork: source session has no header: {resolved_source_path}"
            ));
        }
        let dir = session_dir
            .map(|session_dir| normalize_path(session_dir, &PathInputOptions::default()))
            .unwrap_or_else(|| get_default_session_dir(&resolved_target_cwd));
        let _ = std::fs::create_dir_all(&dir);
        if let Some(id) = options.as_ref().and_then(|options| options.id.as_deref()) {
            assert_valid_session_id(id);
        }
        let new_session_id = options
            .as_ref()
            .and_then(|options| options.id.clone())
            .unwrap_or_else(create_session_id);
        let timestamp = now_iso_millis();
        let file_timestamp = timestamp.replace([':', '.'], "-");
        let new_session_file = Path::new(&dir)
            .join(format!("{file_timestamp}_{new_session_id}.jsonl"))
            .to_string_lossy()
            .into_owned();
        crate::session_write_reservation::reserve_session_write(&new_session_file);
        let fork_hold = crate::schedule::runner_lease::hold_session_file(
            Path::new(&new_session_file),
            &new_session_id,
            &resolved_target_cwd,
            false,
        )
        .map_err(|error| error.to_string())?;
        let last_copied_entry_id = source_entries
            .iter()
            .rev()
            .find(|entry| entry_type(entry) != "session")
            .and_then(|entry| entry.get("id").and_then(Value::as_str));
        let header = json!({
            "type": "session",
            "version": CURRENT_SESSION_VERSION,
            "id": new_session_id,
            "timestamp": timestamp,
            "cwd": resolved_target_cwd,
            "parentSession": resolved_source_path,
            "forkedFromLeafId": last_copied_entry_id,
        });
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&new_session_file)
            .map_err(|error| format!("Cannot fork: {error}"))?;
        file.write_all(serialize_entry(&header).as_bytes())
            .map_err(|error| error.to_string())?;
        file.write_all(b"\n").map_err(|error| error.to_string())?;
        for entry in &source_entries {
            if entry_type(entry) != "session" {
                file.write_all(serialize_entry(entry).as_bytes())
                    .map_err(|error| error.to_string())?;
                file.write_all(b"\n").map_err(|error| error.to_string())?;
            }
        }
        drop(file);
        let manager = Self::open(
            &new_session_file,
            Some(&dir),
            Some(&resolved_target_cwd),
            None,
        );
        drop(fork_hold);
        manager
    }

    /// senpi `SessionManager.open`: fails (rather than panicking) on a non-empty file that does not
    /// parse as a session, with senpi's `Session file is not a valid ... session` message.
    pub fn open(
        path: &str,
        session_dir: Option<&str>,
        cwd_override: Option<&str>,
        options: Option<NewSessionOptions>,
    ) -> Result<Self, String> {
        let process_cwd = std::env::current_dir()
            .map(|cwd| cwd.to_string_lossy().into_owned())
            .unwrap_or_default();
        let resolved_path = resolve_moved_session_path(&resolve_path(
            path,
            &process_cwd,
            &PathInputOptions::default(),
        ));
        crate::session_write_reservation::reserve_session_write(&resolved_path);
        let entries = load_entries_from_file(&resolved_path);
        let header_cwd = entries
            .first()
            .and_then(|header| header.get("cwd"))
            .and_then(Value::as_str)
            .map(resolve_moved_session_path);
        let cwd = cwd_override
            .map(str::to_owned)
            .or(header_cwd)
            .unwrap_or_else(|| process_cwd.clone());
        let opening_hold = if let Some(id) = entries
            .first()
            .and_then(|header| header.get("id"))
            .and_then(Value::as_str)
        {
            Some(
                crate::schedule::runner_lease::hold_session_file(
                    Path::new(&resolved_path),
                    id,
                    &cwd,
                    true,
                )
                .map_err(|error| error.to_string())?,
            )
        } else {
            None
        };
        // This process opens the session for append; normalize a final unterminated JSONL entry
        // here, never from read-only load_entries_from_file callers.
        if Path::new(&resolved_path).exists() {
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .append(true)
                .open(&resolved_path)
                .map_err(|error| error.to_string())?;
            if file.metadata().map_err(|error| error.to_string())?.len() > 0 {
                let mut last = [0u8; 1];
                file.seek(SeekFrom::End(-1))
                    .map_err(|error| error.to_string())?;
                file.read_exact(&mut last)
                    .map_err(|error| error.to_string())?;
                if last[0] != b'\n' {
                    file.write_all(b"\n").map_err(|error| error.to_string())?;
                }
            }
        }
        // If no session_dir is provided, derive it from the file's parent directory.
        let dir = match session_dir {
            Some(session_dir) => normalize_path(session_dir, &PathInputOptions::default()),
            None => Path::new(&resolved_path)
                .parent()
                .map(|parent| parent.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        let mut manager = Self::build(&cwd, &dir, true);
        manager.set_session_file(&resolved_path, options)?;
        drop(opening_hold);
        if !entries.is_empty() {
            manager.file_entries = entries;
            manager.session_id = manager
                .file_entries
                .first()
                .and_then(|header| header.get("id"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(create_session_id);
            manager.sync_resident_blobs_dir();
            if migrate_to_current_version(&mut manager.file_entries) {
                manager.rewrite_file();
            }
            manager.externalize_file_entries();
            manager.resident_store.spill_resident();
            manager.build_index();
            manager.flushed = true;
        }
        Ok(manager)
    }

    /// senpi `continueRecent`: the cwd filter applies only to a custom session dir, and a resumed
    /// session keeps the launch cwd (no `open` newline repair).
    pub fn continue_recent(cwd: &str, session_dir: Option<&str>) -> Result<Self, String> {
        let dir = match session_dir {
            Some(session_dir) => normalize_path(session_dir, &PathInputOptions::default()),
            None => get_default_session_dir(cwd),
        };
        let filter_cwd =
            session_dir.is_some() && dir != get_default_session_dir_path(cwd, &get_agent_dir());
        match find_most_recent_session(&dir, filter_cwd.then_some(cwd)) {
            Some(path) => {
                let mut manager = Self::build(cwd, &dir, true);
                manager.set_session_file(&path, None)?;
                Ok(manager)
            }
            None => Ok(Self::create(cwd, Some(&dir), None)),
        }
    }

    pub fn in_memory(
        cwd: &str,
        options: Option<NewSessionOptions>,
        entries: Option<Vec<Value>>,
    ) -> Self {
        let mut manager = Self::build(cwd, "", false);
        manager.new_session(options);
        if let Some(entries) = entries
            && !entries.is_empty()
        {
            let header = entries
                .iter()
                .find(|entry| entry_type(entry) == "session")
                .cloned();
            if let Some(header) = header {
                manager.file_entries = entries;
                manager.session_id = header
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                migrate_to_current_version(&mut manager.file_entries);
            } else {
                manager.new_session(None);
                manager.file_entries.extend(entries);
            }
            manager.externalize_file_entries();
            manager.build_index();
        }
        manager
    }

    pub fn reload_from_disk(&mut self) -> Result<(), String> {
        let Some(session_file) = self.session_file.clone() else {
            return Ok(());
        };
        if !Path::new(&session_file).exists() {
            return Ok(());
        }
        let file_entries = load_entries_from_file(&session_file);
        if file_entries.is_empty() {
            return Ok(());
        }
        self.set_session_file(&session_file, None)
    }

    pub fn set_session_file(
        &mut self,
        session_file: &str,
        options: Option<NewSessionOptions>,
    ) -> Result<(), String> {
        let session_file = resolve_moved_session_path(session_file);
        let session_file = session_file.as_str();
        if self.persist {
            let resolved = resolve_path(session_file, session_file, &PathInputOptions::default());
            crate::session_write_reservation::reserve_session_write(&resolved);
            self.write_fence
                .opened(&resolved)
                .map_err(|error| error.to_string())?;
        }
        self.session_file = Some(resolve_path(
            session_file,
            session_file,
            &PathInputOptions::default(),
        ));
        self.mirror_trimmed = false;
        self.trimmed_entry_count = 0;
        self.resident_store.clear();
        let Some(current) = self.session_file.clone() else {
            return Ok(());
        };
        if Path::new(&current).exists() {
            let entries = load_entries_from_file(&current);
            if entries.is_empty() {
                let size = std::fs::metadata(&current)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
                if size > 0 {
                    return Err(format!(
                        "Session file is not a valid {} session: {current}",
                        app_name()
                    ));
                }
                // An empty file carries no identity yet, so a caller-supplied id (or the file name's) still applies.
                self.reset_to_new_session(with_file_name_id(&current, options));
                self.sync_session_file_ownership();
                self.check_session_hold()?;
                self.rewrite_file();
                self.flushed = true;
                return Ok(());
            }
            self.file_entries = entries;
            self.invalidate_entries_cache();
            self.session_id = self
                .file_entries
                .iter()
                .find(|entry| entry_type(entry) == "session")
                .and_then(|header| header.get("id"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(create_session_id);
            self.sync_session_file_hold(true);
            self.check_session_hold()?;
            self.sync_resident_blobs_dir();
            // Blobs left under this id by a process that is gone are a disposable cache: the JSONL
            // is the authority for every entry, so clearing them bounds the directory to one process lifetime.
            self.release_blobs_dir_unless_shared();
            if migrate_to_current_version(&mut self.file_entries) {
                self.rewrite_file();
            }
            self.externalize_file_entries();
            self.build_index();
            self.flushed = true;
        } else {
            // A caller-chosen id (or the id the file name carries) is already held in the caller's own
            // records: the file has to answer to it now, or a reopen before the first reply would mint another.
            let created = with_file_name_id(&current, options);
            let explicit_id = created.as_ref().and_then(|options| options.id.clone());
            self.reset_to_new_session(created);
            self.sync_session_file_ownership();
            self.check_session_hold()?;
            if explicit_id.is_some() {
                self.rewrite_file();
                self.flushed = true;
            }
        }
        Ok(())
    }

    pub fn new_session(&mut self, options: Option<NewSessionOptions>) -> Option<String> {
        let timestamp = self.reset_to_new_session(options);
        if self.persist {
            let file_timestamp = timestamp.replace([':', '.'], "-");
            let path = Path::new(&self.session_dir)
                .join(format!("{file_timestamp}_{}.jsonl", self.session_id))
                .to_string_lossy()
                .into_owned();
            crate::session_write_reservation::reserve_session_write(&path);
            self.session_file = Some(path);
        }
        self.sync_session_file_ownership();
        self.session_file.clone()
    }

    fn reset_to_new_session(&mut self, options: Option<NewSessionOptions>) -> String {
        if let Some(id) = options.as_ref().and_then(|options| options.id.as_deref()) {
            assert_valid_session_id(id);
        }
        // Capture the previous backing before the id changes: clear() below resolves against the
        // new id, so it would otherwise leak the old session's blob directory on disk.
        let previous_blobs_dir = self.resident_store.resolved_blobs_dir();
        self.session_id = options
            .as_ref()
            .and_then(|options| options.id.clone())
            .unwrap_or_else(create_session_id);
        self.sync_resident_blobs_dir();
        let timestamp = now_iso_millis();
        let mut header = Map::new();
        header.insert("type".to_owned(), Value::from("session"));
        header.insert("version".to_owned(), Value::from(CURRENT_SESSION_VERSION));
        header.insert("id".to_owned(), Value::from(self.session_id.clone()));
        header.insert("timestamp".to_owned(), Value::from(timestamp.clone()));
        header.insert("cwd".to_owned(), Value::from(self.cwd.clone()));
        if let Some(parent_session) = options
            .as_ref()
            .and_then(|options| options.parent_session.clone())
        {
            header.insert("parentSession".to_owned(), Value::from(parent_session));
        }
        if let Some(forked_from_leaf_id) = options
            .as_ref()
            .and_then(|options| options.forked_from_leaf_id.clone())
        {
            header.insert("forkedFromLeafId".to_owned(), Value::from(forked_from_leaf_id));
        }
        if let Some(spawned_by) = options
            .as_ref()
            .and_then(|options| options.spawned_by.as_ref())
            && let Ok(val) = serde_json::to_value(spawned_by) {
                header.insert("spawnedBy".to_owned(), val);
            }
        self.file_entries = vec![Value::Object(header)];
        self.history_generation += 1;
        self.verbatim_from_count = 0;
        self.invalidate_entries_cache();
        self.mirror_trimmed = false;
        self.trimmed_entry_count = 0;
        self.full_entry_count = 0;
        self.resident_store.clear();
        if let Some(previous_blobs_dir) = previous_blobs_dir {
            let _ = std::fs::remove_dir_all(previous_blobs_dir);
        }
        self.by_id.clear();
        self.labels_by_id.clear();
        self.label_timestamps_by_id.clear();
        self.leaf_id = None;
        self.session_name_cache = None;
        self.usage_totals = UsageTotals::default();
        self.flushed = false;
        timestamp
    }

    fn build_index(&mut self) {
        if !self.mirror_trimmed {
            self.verbatim_from_count = 0;
        }
        self.history_generation += 1;
        self.by_id.clear();
        self.labels_by_id.clear();
        self.label_timestamps_by_id.clear();
        self.leaf_id = None;
        self.session_name_cache = None;
        self.usage_totals = UsageTotals::default();
        let entries = self.file_entries.clone();
        let mut full_entry_count = 0;
        for (order, entry) in entries.iter().enumerate() {
            if entry_type(entry) == "session" {
                continue;
            }
            full_entry_count += 1;
            let Some(id) = entry.get("id").and_then(Value::as_str).map(str::to_owned) else {
                continue;
            };
            self.by_id.insert(id.clone(), order);
            self.leaf_id = Some(id.clone());
            self.accumulate_usage(entry);
            if entry_type(entry) == "session_info" {
                self.session_name_cache = entry
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned);
            }
            if entry_type(entry) == "label" {
                let target = entry
                    .get("targetId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                match entry
                    .get("label")
                    .and_then(Value::as_str)
                    .filter(|label| !label.is_empty())
                {
                    Some(label) => {
                        self.labels_by_id.insert(target.clone(), label.to_owned());
                        if let Some(timestamp) = entry.get("timestamp").and_then(Value::as_str) {
                            self.label_timestamps_by_id
                                .insert(target, timestamp.to_owned());
                        }
                    }
                    None => {
                        self.labels_by_id.remove(&target);
                        self.label_timestamps_by_id.remove(&target);
                    }
                }
            }
        }
        // A trimmed mirror cannot replace the full-history count.
        if !self.mirror_trimmed {
            self.full_entry_count = full_entry_count;
        }
    }

    fn accumulate_usage(&mut self, entry: &Value) {
        if entry_type(entry) != "message" {
            return;
        }
        let Some(message) = entry.get("message") else {
            return;
        };
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            return;
        }
        let Some(usage) = message.get("usage") else {
            return;
        };
        self.usage_totals.input += usage.get("input").and_then(Value::as_i64).unwrap_or(0);
        self.usage_totals.output += usage.get("output").and_then(Value::as_i64).unwrap_or(0);
        self.usage_totals.cache_read += usage.get("cacheRead").and_then(Value::as_i64).unwrap_or(0);
        self.usage_totals.cache_write +=
            usage.get("cacheWrite").and_then(Value::as_i64).unwrap_or(0);
        self.usage_totals.cost += usage
            .get("cost")
            .and_then(|cost| cost.get("total"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        if usage
            .get("costUnknown")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            self.usage_totals.cost_unknown = true;
        }
        let cache_read = usage.get("cacheRead").and_then(Value::as_i64).unwrap_or(0);
        let latest_prompt_tokens = usage.get("input").and_then(Value::as_i64).unwrap_or(0)
            + cache_read
            + usage.get("cacheWrite").and_then(Value::as_i64).unwrap_or(0);
        self.usage_totals.latest_cache_hit_rate = (latest_prompt_tokens > 0)
            .then(|| cache_read as f64 / latest_prompt_tokens as f64 * 100.0);
    }

    fn rewrite_file(&mut self) {
        if let Err(error) = self.check_session_hold() {
            eprintln!("cannot rewrite session: {error}");
            return;
        }
        if !self.persist {
            return;
        }
        let Some(session_file) = self.session_file.clone() else {
            return;
        };
        if let Err(error) = self.write_fence.assert_owned(&session_file) {
            eprintln!("cannot rewrite session: {error}");
            return;
        }
        crate::session_write_reservation::reserve_session_write(&session_file);
        let mut content = String::new();
        for entry in &self.file_entries {
            // PERF-DIVERGENCE(senpi session-manager.ts): upstream serializes each stored object directly; this borrows entries without resident tokens instead of deep-cloning them through materialize. Serialized text is identical.
            content.push_str(&serialize_entry(&self.resident_store.materialize_cow(entry)));
            content.push('\n');
        }
        if let Err(error) = std::fs::write(&session_file, content) {
            self.last_write_error = Some(error.to_string());
            crate::hidden_stdout_log::append_debug_note(&format!(
                "cannot rewrite session {session_file}: {error}"
            ));
        }
    }

    fn persist_entry(&mut self, order: usize) -> std::io::Result<()> {
        self.check_session_hold().map_err(std::io::Error::other)?;
        if !self.persist {
            return Ok(());
        }
        let Some(session_file) = self.session_file.clone() else {
            return Ok(());
        };
        self.write_fence
            .assert_owned(&session_file)
            .map_err(std::io::Error::other)?;
        crate::session_write_reservation::reserve_session_write(&session_file);
        // PERF-DIVERGENCE(senpi session-manager.ts _persist): upstream scans every entry for an assistant message on each append; once flushed both branches below append the line regardless, so this skips the O(n) scan (and the materialize) then. Bytes written are identical.
        if self.flushed {
            let persisted_entry = self.resident_store.materialize_cow(&self.file_entries[order]);
            return append_line(&session_file, &persisted_entry);
        }
        let durable_lifecycle = {
            let entry = &self.file_entries[order];
            entry_type(entry) == "custom"
                && entry
                    .get("customType")
                    .and_then(Value::as_str)
                    .map(|ct| matches!(ct, "session-lifecycle" | "work-custody" | "child-lifecycle"))
                    .unwrap_or(false)
        };
        let has_assistant = durable_lifecycle || self.file_entries.iter().any(|candidate| {
            entry_type(candidate) == "message"
                && candidate
                    .get("message")
                    .and_then(|message| message.get("role"))
                    .and_then(Value::as_str)
                    == Some("assistant")
        });
        if has_assistant {
            self.rewrite_file();
            if let Some(error) = self.last_write_error.take() { return Err(std::io::Error::other(error)); }
            self.flushed = true;
        }
        Ok(())
    }

    fn append_entry(&mut self, entry: Value) {
        // PERF-DIVERGENCE(senpi session-manager.ts): upstream keeps one object per entry; the port deep-cloned each appended entry for externalize, for the push, and again for persist, so this moves the entry through externalize into file_entries and persists from there. Stored and written values are identical.
        let entry = self.resident_store.externalize_owned(entry);
        let id = entry.get("id").and_then(Value::as_str).map(str::to_owned);
        self.invalidate_entries_cache();
        if let Some(id) = id {
            self.by_id.insert(id.clone(), self.file_entries.len());
            self.leaf_id = Some(id);
        }
        self.full_entry_count += 1;
        self.accumulate_usage(&entry);
        if entry_type(&entry) == "session_info" {
            self.session_name_cache = entry
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned);
        }
        if entry_type(&entry) == "label" {
            let target = entry
                .get("targetId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            match entry
                .get("label")
                .and_then(Value::as_str)
                .filter(|label| !label.is_empty())
            {
                Some(label) => {
                    self.labels_by_id.insert(target.clone(), label.to_owned());
                    if let Some(timestamp) = entry.get("timestamp").and_then(Value::as_str) {
                        self.label_timestamps_by_id
                            .insert(target, timestamp.to_owned());
                    }
                }
                None => {
                    self.labels_by_id.remove(&target);
                    self.label_timestamps_by_id.remove(&target);
                }
            }
        }
        // senpi's appendFileSync throws to the appending caller; the append_* surface here returns
        // the entry rather than a Result, so a failed write is reported instead of dropped.
        self.file_entries.push(entry);
        self.last_write_error = None;
        if let Err(error) = self.persist_entry(self.file_entries.len() - 1) {
            self.last_write_error = Some(error.to_string());
            crate::output_guard::maho_write_stderr(&format!(
                "Failed to persist session entry to {}: {error}\n",
                self.session_file.as_deref().unwrap_or_default()
            ));
        }
    }

    pub fn last_persistence_error(&self) -> Option<&str> { self.last_write_error.as_deref() }

    pub fn take_write_error(&mut self) -> Option<String> { self.last_write_error.take() }

    pub fn append_entry_raw(&mut self, entry: Value) {
        self.append_entry(entry);
    }

    fn next_entry_id(&self) -> String {
        generate_id(|id| self.by_id.contains_key(id))
    }

    fn base_entry(&self, entry_type_name: &str) -> Map<String, Value> {
        let mut entry = Map::new();
        entry.insert("type".to_owned(), Value::from(entry_type_name));
        entry.insert("id".to_owned(), Value::from(self.next_entry_id()));
        match &self.leaf_id {
            Some(leaf) => entry.insert("parentId".to_owned(), Value::from(leaf.clone())),
            None => entry.insert("parentId".to_owned(), Value::Null),
        };
        entry.insert("timestamp".to_owned(), Value::from(now_iso_millis()));
        entry
    }

    pub fn append_message(&mut self, message: Value) -> Value {
        let mut entry = self.base_entry("message");
        entry.insert("message".to_owned(), message);
        let entry = Value::Object(entry);
        self.append_entry(entry.clone());
        entry
    }

    /// senpi `appendOwnedMessage`: `input_source` is where a user message came from (the `source` its
    /// `input` event reported). It is stored on the entry only, never on the provider message.
    pub fn append_owned_message(
        &mut self,
        message: Value,
        input_source: Option<maho_ext_api::InputSource>,
    ) -> Value {
        let mut entry = self.base_entry("message");
        entry.insert("message".to_owned(), message);
        if let Some(source) = input_source {
            let name = match source {
                maho_ext_api::InputSource::Interactive => "interactive",
                maho_ext_api::InputSource::Rpc => "rpc",
                maho_ext_api::InputSource::Extension => "extension",
            };
            entry.insert("inputSource".to_owned(), Value::from(name));
        }
        let entry = Value::Object(entry);
        self.append_entry(entry.clone());
        entry
    }

    pub fn append_thinking_level_change(
        &mut self,
        thinking_level: &str,
        thinking_selection: Option<Value>,
    ) -> Value {
        self.append_thinking_level_change_with_model(thinking_level, thinking_selection, None)
    }

    pub fn append_thinking_level_change_with_model(
        &mut self,
        thinking_level: &str,
        thinking_selection: Option<Value>,
        model: Option<(&str, &str)>,
    ) -> Value {
        self.append_thinking_level_change_context(thinking_level, thinking_selection, None, model)
    }

    pub fn append_thinking_level_change_with_context(
        &mut self,
        thinking_level: &str,
        thinking_selection: Option<Value>,
        source: Option<&str>,
        model: Option<&maho_ai::types::Model>,
    ) -> Value {
        self.append_thinking_level_change_context(thinking_level, thinking_selection, source, model.map(|model| (model.provider.as_str(), model.id.as_str())))
    }

    fn append_thinking_level_change_context(
        &mut self,
        thinking_level: &str,
        thinking_selection: Option<Value>,
        source: Option<&str>,
        model: Option<(&str, &str)>,
    ) -> Value {
        let mut entry = self.base_entry("thinking_level_change");
        if let Some(source) = source { entry.insert("source".into(), Value::from(source)); }
        entry.insert("thinkingLevel".to_owned(), Value::from(thinking_level));
        if let Some(selection) = thinking_selection {
            entry.insert("thinkingSelection".to_owned(), selection);
        }
        if let Some((provider, model_id)) = model {
            entry.insert("provider".to_owned(), Value::from(provider));
            entry.insert("modelId".to_owned(), Value::from(model_id));
        }
        let entry = Value::Object(entry);
        self.append_entry(entry.clone());
        entry
    }

    /// senpi `appendConfigurationUpdate`: a durable Responses configuration update.
    pub fn append_configuration_update(&mut self, effort: &str) -> Value {
        let mut entry = self.base_entry("configuration_update");
        entry.insert("reasoning".to_owned(), json!({ "effort": effort }));
        let entry = Value::Object(entry);
        self.append_entry(entry.clone());
        entry
    }

    /// senpi `appendModelChangeRejected` (#1526): record a refused model switch, which otherwise
    /// leaves no trace because no `model_change` is written. Budget numbers are present only for
    /// the context-budget reason.
    pub fn append_model_change_rejected(
        &mut self,
        provider: &str,
        model_id: &str,
        reason: &str,
        detail: &str,
        budget: Option<Map<String, Value>>,
    ) -> Value {
        let mut entry = self.base_entry("model_change_rejected");
        entry.insert("provider".to_owned(), Value::from(provider));
        entry.insert("modelId".to_owned(), Value::from(model_id));
        entry.insert("reason".to_owned(), Value::from(reason));
        entry.insert("detail".to_owned(), Value::from(detail));
        entry.extend(budget.unwrap_or_default());
        let entry = Value::Object(entry);
        self.append_entry(entry.clone());
        entry
    }

    pub fn append_model_change(
        &mut self,
        provider: &str,
        model_id: &str,
        reason: Option<&str>,
        original: Option<(&str, &str)>,
    ) -> Value {
        let mut entry = self.base_entry("model_change");
        entry.insert("provider".to_owned(), Value::from(provider));
        entry.insert("modelId".to_owned(), Value::from(model_id));
        if let Some(reason) = reason {
            entry.insert("reason".to_owned(), Value::from(reason));
        }
        if let Some((original_provider, original_model_id)) = original {
            entry.insert(
                "originalProvider".to_owned(),
                Value::from(original_provider),
            );
            entry.insert("originalModelId".to_owned(), Value::from(original_model_id));
        }
        let entry = Value::Object(entry);
        self.append_entry(entry.clone());
        entry
    }

    pub fn append_compaction(
        &mut self,
        summary: &str,
        first_kept_entry_id: &str,
        tokens_before: i64,
        details: Option<Value>,
        usage: Option<Value>,
        from_hook: Option<bool>,
    ) -> Value {
        let mut entry = self.base_entry("compaction");
        entry.insert("summary".to_owned(), Value::from(summary));
        entry.insert(
            "firstKeptEntryId".to_owned(),
            Value::from(first_kept_entry_id),
        );
        entry.insert("tokensBefore".to_owned(), Value::from(tokens_before));
        if let Some(details) = details {
            entry.insert("details".to_owned(), details);
        }
        if let Some(usage) = usage {
            entry.insert("usage".to_owned(), usage);
        }
        if let Some(from_hook) = from_hook {
            entry.insert("fromHook".to_owned(), Value::from(from_hook));
        }
        let entry = Value::Object(entry);
        self.append_entry(entry.clone());
        self.trim_mirror_after_compaction(&entry);
        entry
    }

    fn trim_mirror_after_compaction(&mut self, compaction: &Value) {
        // An unflushed mirror is the only copy of its entries, so it is never trimmed.
        if !self.persist || !self.flushed {
            return;
        }
        let first_kept_entry_id = compaction.get("firstKeptEntryId").and_then(Value::as_str);
        let compaction_id = compaction.get("id").and_then(Value::as_str);
        let position = |id: Option<&str>| {
            self.file_entries
                .iter()
                .position(|entry| id.is_some() && entry.get("id").and_then(Value::as_str) == id)
        };
        let (Some(first_kept_index), Some(compaction_index)) =
            (position(first_kept_entry_id), position(compaction_id))
        else {
            return;
        };
        if compaction_index < first_kept_index {
            return;
        }
        let verbatim_start = self.file_entries.len().saturating_sub(
            self.full_entry_count
                .saturating_sub(self.verbatim_from_count),
        );
        if first_kept_index >= verbatim_start {
            self.verbatim_from_count = self
                .full_entry_count
                .saturating_sub(compaction_index + 1 - first_kept_index);
        }
        let header = self
            .file_entries
            .iter()
            .find(|entry| entry_type(entry) == "session")
            .cloned();
        let mut retained: Vec<Value> = self.file_entries[..first_kept_index]
            .iter()
            .filter(|entry| !matches!(entry_type(entry), "message" | "session"))
            .cloned()
            .chain(
                self.file_entries[first_kept_index..=compaction_index]
                    .iter()
                    .cloned(),
            )
            .collect();
        let mut parent_id = Value::Null;
        for entry in &mut retained {
            entry["parentId"] = parent_id;
            parent_id = entry.get("id").cloned().unwrap_or(Value::Null);
        }
        self.resident_store.spill_resident();
        self.mirror_trimmed = true;
        self.trimmed_entry_count = self.full_entry_count;
        self.file_entries = header.into_iter().chain(retained).collect();
        self.invalidate_entries_cache();
        self.externalize_file_entries();
        // A trim keeps the history; only a replacement of it changes the reduction memo's generation.
        let generation = self.history_generation;
        self.build_index();
        self.history_generation = generation;
    }

    pub fn append_branch_summary(
        &mut self,
        from_id: &str,
        summary: &str,
        details: Option<Value>,
        usage: Option<Value>,
        from_hook: Option<bool>,
    ) -> Value {
        let mut entry = self.base_entry("branch_summary");
        entry.insert("fromId".to_owned(), Value::from(from_id));
        entry.insert("summary".to_owned(), Value::from(summary));
        if let Some(details) = details {
            entry.insert("details".to_owned(), details);
        }
        if let Some(usage) = usage {
            entry.insert("usage".to_owned(), usage);
        }
        if let Some(from_hook) = from_hook {
            entry.insert("fromHook".to_owned(), Value::from(from_hook));
        }
        let entry = Value::Object(entry);
        self.append_entry(entry.clone());
        entry
    }

    pub fn append_custom(&mut self, custom_type: &str, data: Option<Value>) -> Value {
        let mut entry = self.base_entry("custom");
        entry.insert("customType".to_owned(), Value::from(custom_type));
        if let Some(data) = data {
            entry.insert("data".to_owned(), data);
        }
        let entry = Value::Object(entry);
        self.append_entry(entry.clone());
        entry
    }

    pub fn append_custom_message(
        &mut self,
        custom_type: &str,
        content: Value,
        display: bool,
        details: Option<Value>,
    ) -> Value {
        let mut entry = self.base_entry("custom_message");
        entry.insert("customType".to_owned(), Value::from(custom_type));
        entry.insert("content".to_owned(), content);
        entry.insert("display".to_owned(), Value::from(display));
        if let Some(details) = details {
            entry.insert("details".to_owned(), details);
        }
        let entry = Value::Object(entry);
        self.append_entry(entry.clone());
        entry
    }

    pub fn append_label(&mut self, target_id: &str, label: Option<&str>) -> Result<Value, String> {
        if !self.by_id.contains_key(target_id) {
            return Err(format!("Entry {target_id} not found"));
        }
        let mut entry = self.base_entry("label");
        entry.insert("targetId".to_owned(), Value::from(target_id));
        match label {
            Some(label) => entry.insert("label".to_owned(), Value::from(label)),
            None => entry.insert("label".to_owned(), Value::Null),
        };
        let entry = Value::Object(entry);
        self.append_entry(entry.clone());
        Ok(entry)
    }

    pub fn append_session_info(&mut self, name: Option<&str>) -> Value {
        let mut entry = self.base_entry("session_info");
        if let Some(name) = name {
            let sanitized: String = name
                .split(['\r', '\n'])
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            entry.insert("name".to_owned(), Value::from(sanitized.trim()));
        }
        let entry = Value::Object(entry);
        self.append_entry(entry.clone());
        entry
    }

    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    /// senpi `getEntryCount`: the maintained non-header entry count without loading or
    /// materializing history.
    pub fn get_entry_count(&self) -> usize {
        self.full_entry_count
    }

    pub fn session_dir(&self) -> &str {
        &self.session_dir
    }

    pub fn uses_default_session_dir(&self) -> bool {
        self.session_dir == get_default_session_dir_path(&self.cwd, &get_agent_dir())
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn session_file(&self) -> Option<&str> {
        self.session_file.as_deref()
    }

    pub fn is_persisted(&self) -> bool {
        self.persist
    }

    pub fn is_flushed(&self) -> bool {
        self.flushed
    }

    pub fn leaf_id(&self) -> Option<&str> {
        self.leaf_id.as_deref()
    }

    pub fn usage_totals(&self) -> &UsageTotals {
        &self.usage_totals
    }

    pub fn extension_usage_totals(&self) -> Value {
        let totals = self.usage_totals();
        serde_json::json!({
            "input": totals.input,
            "output": totals.output,
            "cacheRead": totals.cache_read,
            "cacheWrite": totals.cache_write,
            "cost": totals.cost,
            "latestCacheHitRate": totals.latest_cache_hit_rate,
        })
    }

    /// Exposes the resident store for out-of-band string lifecycles such as the agent's runtime
    /// message state (tokenized while idle, hydrated per turn).
    pub fn resident_store(&self) -> &crate::session_resident_store::ResidentStringStore {
        &self.resident_store
    }

    pub fn resident_store_stats(&self) -> crate::session_resident_store::ResidentStoreStats {
        self.resident_store.stats()
    }

    fn load_full_history_entries(&self) -> Vec<Value> {
        match &self.session_file {
            Some(session_file) => load_entries_from_file(session_file),
            None => self.file_entries.clone(),
        }
    }

    fn materialize_entry(&self, entry: &Value) -> Value {
        let missing = std::cell::Cell::new(false);
        let materialized = self.resident_store.materialize_with(
            entry,
            Some(&|_| {
                missing.set(true);
                None
            }),
        );
        // The resident store is a bounded cache; the JSONL stays authoritative for an evicted blob.
        if missing.get() && self.session_file.is_some() {
            let id = entry.get("id");
            if let Some(persisted) = self
                .load_full_history_entries()
                .into_iter()
                .find(|candidate| id.is_some() && candidate.get("id") == id)
            {
                return persisted;
            }
        }
        materialized
    }

    /// The resident mirror's entries, materialized; after a compaction trim this is the bounded
    /// post-compaction view rather than the full history.
    /// Materialized once per mutation and shared; readers borrow the same `Arc`.
    fn compact_entries(&self) -> Arc<Vec<Value>> {
        let mut cache = self.entries_cache.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cached) = cache.as_ref() {
            return Arc::clone(cached);
        }
        let entries = self.file_entries.iter().filter(|entry| entry_type(entry) != "session");
        let materialized = Arc::new(crate::session_entry_materializer::materialize_session_entries(
            entries,
            &self.resident_store,
            || self.load_full_history_entries(),
            |_| {},
        ));
        *cache = Some(Arc::clone(&materialized));
        materialized
    }

    pub fn header(&self) -> Option<Value> {
        self.file_entries
            .iter()
            .find(|entry| entry_type(entry) == "session")
            .map(|header| self.resident_store.materialize(header))
    }

    /// The entries appended after the first `skip` of the full history, materialized from the
    /// resident tail. `None` when `skip` exceeds the history or the tail no longer holds every
    /// entry after it (a compaction trim dropped some), so callers fall back to `entries()`.
    pub fn entries_after(&self, skip: usize) -> Option<Vec<Value>> {
        if self.mirror_trimmed && skip < self.trimmed_entry_count {
            return None;
        }
        let count = self.full_entry_count.checked_sub(skip)?;
        let resident: Vec<&Value> = self
            .file_entries
            .iter()
            .filter(|entry| entry_type(entry) != "session")
            .collect();
        let start = resident.len().checked_sub(count)?;
        Some(
            resident[start..]
                .iter()
                .map(|entry| self.materialize_entry(entry))
                .collect(),
        )
    }

    /// Folds the whole history (every branch, append order, like `entries()`) without keeping a
    /// full-history view (senpi#2537): the history is read once per load, later calls fold only the
    /// entries appended since. See `HistoryReducer` for the reducer contract.
    pub fn reduce_history<S: Clone + Send + 'static>(
        &self,
        reducer: &'static crate::session_history_reduce::HistoryReducer<S>,
    ) -> S {
        self.history_reductions
            .reduce(reducer, &ManagerHistory(self))
    }

    fn appended_since(&self, count: usize) -> Option<Vec<Value>> {
        if count < self.verbatim_from_count {
            return None;
        }
        let wanted = self.full_entry_count.checked_sub(count)?;
        let missing = std::cell::Cell::new(false);
        let mut appended = Vec::new();
        for entry in self.file_entries.iter().rev() {
            if appended.len() >= wanted || entry_type(entry) == "session" {
                break;
            }
            appended.push(self.resident_store.materialize_with(
                entry,
                Some(&|_| {
                    missing.set(true);
                    None
                }),
            ));
        }
        if missing.get() || appended.len() < wanted {
            return None;
        }
        appended.reverse();
        Some(appended)
    }

    pub fn entries(&self) -> Vec<Value> {
        if self.mirror_trimmed && self.session_file.is_some() {
            return self
                .load_full_history_entries()
                .into_iter()
                .filter(|entry| entry_type(entry) != "session")
                .map(|entry| self.resident_store.materialize(&entry))
                .collect();
        }
        self.compact_entries().as_ref().clone()
    }

    pub fn entry(&self, id: &str) -> Option<Value> {
        if let Some(entry) = self
            .by_id
            .get(id)
            .and_then(|order| self.file_entries.get(*order))
        {
            return Some(self.materialize_entry(entry));
        }
        if self.mirror_trimmed && self.session_file.is_some() {
            return self
                .load_full_history_entries()
                .into_iter()
                .find(|candidate| candidate.get("id").and_then(Value::as_str) == Some(id))
                .map(|persisted| self.resident_store.materialize(&persisted));
        }
        None
    }

    pub fn leaf_entry(&self) -> Option<Value> {
        self.leaf_id.as_deref().and_then(|id| self.entry(id))
    }

    /// senpi `getChildren`: direct children of an entry.
    pub fn children(&self, parent_id: &str) -> Vec<Value> {
        self.file_entries
            .iter()
            .filter(|entry| {
                entry_type(entry) != "session"
                    && entry.get("parentId").and_then(Value::as_str) == Some(parent_id)
            })
            .cloned()
            .collect()
    }

    /// senpi `getMessageEntryPosition`: the append position of the persisted entry carrying this exact
    /// message. Native messages are values, so the runtime identity map becomes the latest entry whose
    /// materialized message equals `message`.
    pub fn message_entry_position(&self, message: &Value) -> Option<(String, usize)> {
        self.file_entries
            .iter()
            .enumerate()
            .rev()
            .find_map(|(order, entry)| {
                if entry_type(entry) != "message" {
                    return None;
                }
                // Cheap identity prefilter before materializing resident strings.
                if entry
                    .get("message")
                    .and_then(|stored| stored.get("timestamp"))
                    != message.get("timestamp")
                {
                    return None;
                }
                let materialized = self.materialize_entry(entry);
                (materialized.get("message") == Some(message))
                    .then(|| {
                        materialized
                            .get("id")
                            .and_then(Value::as_str)
                            .map(|id| (id.to_owned(), order))
                    })
                    .flatten()
            })
    }

    /// senpi `getEntryOrder`: the runtime/file append position of an entry.
    pub fn entry_order(&self, entry_id: &str) -> Option<usize> {
        self.by_id.get(entry_id).copied()
    }

    pub fn has_context_messages(&self) -> bool {
        self.has_branch_entry(|entry| match entry_type(entry) {
            "message" | "custom_message" | "compaction" => true,
            "branch_summary" => entry
                .get("summary")
                .and_then(Value::as_str)
                .is_some_and(|summary| !summary.is_empty()),
            _ => false,
        })
    }

    pub fn has_thinking_level_changes(&self) -> bool {
        self.has_branch_entry(|entry| entry_type(entry) == "thinking_level_change")
    }

    pub fn count_compactions(&self) -> usize {
        self.file_entries
            .iter()
            .filter(|entry| entry_type(entry) == "compaction")
            .count()
    }

    fn has_branch_entry(&self, predicate: impl Fn(&Value) -> bool) -> bool {
        let mut current = self
            .leaf_id
            .as_deref()
            .and_then(|id| self.by_id.get(id))
            .and_then(|order| self.file_entries.get(*order));
        while let Some(entry) = current {
            if predicate(entry) {
                return true;
            }
            current = entry
                .get("parentId")
                .and_then(Value::as_str)
                .and_then(|parent| self.by_id.get(parent))
                .and_then(|order| self.file_entries.get(*order));
        }
        false
    }

    /// Id of the entry nearest the current leaf whose `type` is `kind` (and, when `role` is given,
    /// whose `message.role` matches), walking resident entries by reference via `parentId` the
    /// way `current_branch()` does, without materializing anything: `type`, `id`, `parentId` and
    /// `role` are short strings that are never externalized.
    pub fn latest_branch_entry_id_by_type(&self, kind: &str, role: Option<&str>) -> Option<String> {
        let mut current = self
            .leaf_id
            .as_deref()
            .and_then(|id| self.by_id.get(id))
            .and_then(|order| self.file_entries.get(*order));
        while let Some(entry) = current {
            if entry_type(entry) == kind
                && role.is_none_or(|role| {
                    entry
                        .get("message")
                        .and_then(|message| message.get("role"))
                        .and_then(Value::as_str)
                        == Some(role)
                })
            {
                return entry.get("id").and_then(Value::as_str).map(str::to_owned);
            }
            current = entry
                .get("parentId")
                .and_then(Value::as_str)
                .and_then(|parent| self.by_id.get(parent))
                .and_then(|order| self.file_entries.get(*order));
        }
        None
    }

    /// `latest_branch_entry_id_by_type`, falling back to the materialized `current_branch()` when
    /// the leaf is not indexed (a trimmed mirror), so the result always equals
    /// `current_branch().iter().rev().find(..)`.
    pub fn latest_current_branch_entry_id(&self, kind: &str, role: Option<&str>) -> Option<String> {
        let leaf = self.leaf_id.as_deref()?;
        if self.entry_order(leaf).is_none() {
            return self
                .current_branch()
                .into_iter()
                .rev()
                .find(|entry| {
                    entry["type"] == kind
                        && role.is_none_or(|role| entry["message"]["role"] == role)
                })
                .and_then(|entry| entry["id"].as_str().map(str::to_owned));
        }
        self.latest_branch_entry_id_by_type(kind, role)
    }

    /// The newest suffix of `current_branch()` plus the TRUE branch length, without materializing
    /// the whole branch: walks resident entries by reference from the leaf via `parentId`,
    /// materializing only the suffix. The suffix holds at least `min_entries` entries (or the whole
    /// branch) and extends further back until `satisfied` (called on each materialized entry,
    /// newest-first) has returned `true` once. Falls back to the materialized branch when the leaf
    /// is not indexed, so the result always equals the matching suffix of `current_branch()`.
    pub fn current_branch_tail(
        &self,
        min_entries: usize,
        satisfied: &mut dyn FnMut(&Value) -> bool,
    ) -> (usize, Vec<Value>) {
        let Some(leaf) = self.leaf_id.as_deref() else {
            return (0, Vec::new());
        };
        let Some(order) = self.entry_order(leaf) else {
            let mut branch = self.current_branch();
            let len = branch.len();
            let mut start = len;
            let mut done = false;
            while start > 0 && (len - start < min_entries || !done) {
                start -= 1;
                if !done {
                    done = satisfied(&branch[start]);
                }
            }
            return (len, branch.split_off(start));
        };
        let mut tail = Vec::new();
        let mut len = 0;
        let mut done = false;
        let mut current = self.file_entries.get(order);
        while let Some(entry) = current {
            len += 1;
            if tail.len() < min_entries || !done {
                let materialized = self.materialize_entry(entry);
                if !done {
                    done = satisfied(&materialized);
                }
                tail.push(materialized);
            }
            current = entry
                .get("parentId")
                .and_then(Value::as_str)
                .and_then(|parent| self.by_id.get(parent))
                .and_then(|order| self.file_entries.get(*order));
        }
        tail.reverse();
        (len, tail)
    }

    pub fn label(&self, id: &str) -> Option<&str> {
        self.labels_by_id.get(id).map(String::as_str)
    }

    pub fn label_timestamp(&self, id: &str) -> Option<&str> {
        self.label_timestamps_by_id.get(id).map(String::as_str)
    }

    pub fn session_name(&self) -> Option<&str> {
        self.session_name_cache.as_deref()
    }

    pub fn fork_provenance(&self) -> Option<maho_ext_api::ForkProvenance> {
        let header = self.header()?;
        let parent_session = header.get("parentSession").and_then(Value::as_str)?.to_owned();
        if let Some(leaf_val) = header.get("forkedFromLeafId") {
            let inherited_through_entry_id = leaf_val.as_str().map(str::to_owned);
            return Some(maho_ext_api::ForkProvenance {
                parent_session,
                inherited_through_entry_id,
            });
        }
        // Old forks without the field: treat as fork only if parentSession is set and the parent
        // session file is readable; boundary = last entry id present in both; else no seeding.
        let resolved_parent = resolve_moved_session_path(&resolve_path(
            &parent_session,
            &parent_session,
            &PathInputOptions::default(),
        ));
        if !Path::new(&resolved_parent).exists() {
            return None;
        }
        let parent_entries = load_entries_from_file(&resolved_parent);
        if parent_entries.is_empty()
            || !parent_entries.iter().any(|entry| entry_type(entry) == "session")
        {
            return None;
        }
        let parent_ids: std::collections::HashSet<&str> = parent_entries
            .iter()
            .filter(|e| entry_type(e) != "session")
            .filter_map(|e| e.get("id").and_then(Value::as_str))
            .collect();
        if parent_ids.is_empty() {
            return None;
        }
        let boundary = self.entries().iter().rev().find_map(|e| {
            let id = e.get("id").and_then(Value::as_str)?;
            if parent_ids.contains(id) {
                Some(id.to_owned())
            } else {
                None
            }
        })?;
        Some(maho_ext_api::ForkProvenance {
            parent_session,
            inherited_through_entry_id: Some(boundary),
        })
    }

    pub fn branch(&self, leaf_id: Option<&str>) -> Vec<Value> {
        if let Some(id) = leaf_id
            && !self.by_id.contains_key(id)
            && self.mirror_trimmed
            && self.session_file.is_some()
        {
            return build_session_path(&self.entries(), leaf_id);
        }
        build_session_path(&self.compact_entries(), leaf_id)
    }

    /// senpi `getBranch()` with no argument: the path to the current leaf. A null leaf (after
    /// `reset_leaf`) is empty rather than the path of the last-appended entry.
    pub fn current_branch(&self) -> Vec<Value> {
        match self.leaf_id.as_deref() {
            Some(id) => self.branch(Some(id)),
            None => Vec::new(),
        }
    }

    /// senpi `buildContextEntries()`: compaction-aware entries from the current leaf.
    pub fn current_context_entries(&self) -> Vec<Value> {
        self.context_entries(Some(self.leaf_id.as_deref().unwrap_or_default()))
    }

    /// senpi `buildSessionContext()`: the LLM context from the current leaf (empty when null).
    pub fn current_context(&self) -> SessionContext {
        self.build_context(Some(self.leaf_id.as_deref().unwrap_or_default()))
    }

    pub fn context_entries(&self, leaf_id: Option<&str>) -> Vec<Value> {
        build_context_entries(&self.compact_entries(), leaf_id)
    }

    pub fn build_context(&self, leaf_id: Option<&str>) -> SessionContext {
        build_session_context(&self.compact_entries(), leaf_id)
    }

    /// senpi `branch(branchFromId)` (a `None` target is `resetLeaf()`): move the leaf so the
    /// next append starts a new branch. An id unknown to the mirror triggers one reload from disk
    /// and is an error when still missing.
    pub fn set_leaf(&mut self, leaf_id: Option<&str>) -> Result<(), String> {
        let Some(id) = leaf_id else {
            self.reset_leaf();
            return Ok(());
        };
        if !self.by_id.contains_key(id) && self.session_file.is_some() {
            self.reload_from_disk()?;
        }
        if !self.by_id.contains_key(id) {
            return Err(format!("Entry {id} not found"));
        }
        self.leaf_id = Some(id.to_owned());
        Ok(())
    }

    /// senpi `resetLeaf`: the next append creates a new root entry (parentId = null).
    pub fn reset_leaf(&mut self) {
        self.leaf_id = None;
    }

    /// senpi `branchWithSummary`: move the leaf to `branch_from_id` (None = root) and append a
    /// branch_summary there whose `fromId` is the abandoned leaf (or "root").
    pub fn branch_with_summary(
        &mut self,
        branch_from_id: Option<&str>,
        summary: &str,
        details: Option<Value>,
        usage: Option<Value>,
        from_hook: Option<bool>,
    ) -> Result<Value, String> {
        if let Some(id) = branch_from_id
            && !self.by_id.contains_key(id)
        {
            return Err(format!("Entry {id} not found"));
        }
        let from_id = self.leaf_id.clone().unwrap_or_else(|| "root".to_owned());
        self.leaf_id = branch_from_id.map(str::to_owned);
        Ok(self.append_branch_summary(&from_id, summary, details, usage, from_hook))
    }

    /// senpi `createBranchedSession`: replace this manager's session with a new one holding only
    /// the root-to-`leaf_id` path. Label entries are dropped from the path (re-chaining the
    /// retained entries and any compaction `firstKeptEntryId` that pointed at one) and the
    /// resolved labels are re-appended at the tail. Returns the new session file when persisting.
    pub fn create_branched_session(&mut self, leaf_id: &str) -> Result<Option<String>, String> {
        let previous_session_file = self.session_file.clone();
        if !self.by_id.contains_key(leaf_id) {
            return Err(format!("Entry {leaf_id} not found"));
        }
        let path = self.branch(Some(leaf_id));
        let mut path_without_labels: Vec<Value> = Vec::new();
        let mut replacement_by_label_id: HashMap<String, String> = HashMap::new();
        let mut pending_label_ids: Vec<String> = Vec::new();
        let mut path_parent_id = Value::Null;
        for mut entry in path {
            let id = entry
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if entry_type(&entry) == "label" {
                pending_label_ids.push(id);
                continue;
            }
            for label_id in pending_label_ids.drain(..) {
                replacement_by_label_id.insert(label_id, id.clone());
            }
            entry["parentId"] = path_parent_id;
            if entry_type(&entry) == "compaction"
                && let Some(replacement) = entry
                    .get("firstKeptEntryId")
                    .and_then(Value::as_str)
                    .and_then(|first_kept| replacement_by_label_id.get(first_kept))
            {
                entry["firstKeptEntryId"] = Value::from(replacement.clone());
            }
            path_parent_id = Value::from(id);
            path_without_labels.push(entry);
        }

        let new_session_id = create_session_id();
        let timestamp = now_iso_millis();
        let file_timestamp = timestamp.replace([':', '.'], "-");
        let new_session_file = Path::new(&self.session_dir)
            .join(format!("{file_timestamp}_{new_session_id}.jsonl"))
            .to_string_lossy()
            .into_owned();
        let mut header = Map::new();
        header.insert("type".to_owned(), Value::from("session"));
        header.insert("version".to_owned(), Value::from(CURRENT_SESSION_VERSION));
        header.insert("id".to_owned(), Value::from(new_session_id.clone()));
        header.insert("timestamp".to_owned(), Value::from(timestamp));
        header.insert("cwd".to_owned(), Value::from(self.cwd.clone()));
        if self.persist
            && let Some(previous) = &previous_session_file
        {
            header.insert("parentSession".to_owned(), Value::from(previous.clone()));
        }

        let mut path_entry_ids: HashMap<String, Value> = path_without_labels
            .iter()
            .filter_map(|entry| {
                entry
                    .get("id")
                    .and_then(Value::as_str)
                    .map(|id| (id.to_owned(), Value::Null))
            })
            .collect();
        let mut labels_to_write: Vec<(String, String, Option<String>)> = self
            .labels_by_id
            .iter()
            .filter(|(target_id, _)| path_entry_ids.contains_key(*target_id))
            .map(|(target_id, label)| {
                (
                    target_id.clone(),
                    label.clone(),
                    self.label_timestamps_by_id.get(target_id).cloned(),
                )
            })
            .collect();
        // Map iteration order is unspecified; keep label entries in path order.
        let order: HashMap<&str, usize> = path_without_labels
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                entry
                    .get("id")
                    .and_then(Value::as_str)
                    .map(|id| (id, index))
            })
            .collect();
        labels_to_write.sort_by_key(|(target_id, _, _)| order.get(target_id.as_str()).copied());
        let mut parent_id = path_without_labels
            .last()
            .and_then(|entry| entry.get("id"))
            .cloned()
            .unwrap_or(Value::Null);
        let mut label_entries = Vec::new();
        for (target_id, label, label_timestamp) in labels_to_write {
            let id = generate_id(|candidate| path_entry_ids.contains_key(candidate));
            path_entry_ids.insert(id.clone(), Value::Null);
            label_entries.push(json!({
                "type": "label",
                "id": id,
                "parentId": parent_id,
                "timestamp": label_timestamp.unwrap_or_else(now_iso_millis),
                "targetId": target_id,
                "label": label,
            }));
            parent_id = Value::from(id);
        }

        let mut file_entries = vec![Value::Object(header)];
        file_entries.extend(path_without_labels);
        file_entries.extend(label_entries);

        if self.persist {
            crate::session_write_reservation::reserve_session_write(&new_session_file);
            self.release_blobs_dir_unless_shared();
            self.session_id = new_session_id;
            self.session_file = Some(new_session_file.clone());
            self.sync_session_file_ownership();
            self.check_session_hold()?;
            self.file_entries = file_entries;
            self.invalidate_entries_cache();
            self.build_index();
            // Defer the write to the first assistant response when the branch has none, matching
            // the new_session contract.
            let has_assistant = self.file_entries.iter().any(|entry| {
                entry_type(entry) == "message"
                    && entry
                        .get("message")
                        .and_then(|message| message.get("role"))
                        .and_then(Value::as_str)
                        == Some("assistant")
            });
            if has_assistant {
                self.rewrite_file();
                self.flushed = true;
            } else {
                self.flushed = false;
            }
            return Ok(Some(new_session_file));
        }

        self.file_entries = file_entries;
        self.invalidate_entries_cache();
        self.session_id = new_session_id;
        self.build_index();
        Ok(None)
    }

    /// senpi `getTree`: every root, with orphaned entries (broken parent chain) also returned as
    /// roots and children sorted by timestamp (oldest first). Built iteratively so deep sessions
    /// cannot overflow the stack.
    pub fn get_tree(&self) -> Vec<SessionTreeNode> {
        // PERF-DIVERGENCE(senpi session-manager.ts getTree): upstream reads shared entry objects; entries() deep-copied the whole materialized history, so this borrows the shared cached view unless the mirror is trimmed (then the full history is loaded as before). Same entries and order.
        let owned_entries;
        let shared_entries;
        let entries: &[Value] = if self.mirror_trimmed && self.session_file.is_some() {
            owned_entries = self.entries();
            &owned_entries
        } else {
            shared_entries = self.compact_entries();
            shared_entries.as_slice()
        };
        let index_by_id: HashMap<&str, usize> = entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                entry
                    .get("id")
                    .and_then(Value::as_str)
                    .map(|id| (id, index))
            })
            .collect();
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); entries.len()];
        let mut roots = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            let id = entry.get("id").and_then(Value::as_str);
            match entry.get("parentId").and_then(Value::as_str) {
                Some(parent) if Some(parent) != id => match index_by_id.get(parent) {
                    Some(&parent_index) => children[parent_index].push(index),
                    None => roots.push(index),
                },
                _ => roots.push(index),
            }
        }
        for siblings in &mut children {
            siblings.sort_by_key(|&index| parse_entry_timestamp(&entries[index]));
        }
        let mut built: Vec<Option<SessionTreeNode>> = (0..entries.len()).map(|_| None).collect();
        let mut stack: Vec<(usize, bool)> =
            roots.iter().rev().map(|&index| (index, false)).collect();
        while let Some((index, expanded)) = stack.pop() {
            if expanded {
                let child_nodes = children[index]
                    .iter()
                    .filter_map(|&child| built[child].take())
                    .collect();
                let id = entries[index]
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                built[index] = Some(SessionTreeNode {
                    entry: entries[index].clone(),
                    children: child_nodes,
                    label: self.labels_by_id.get(id).cloned(),
                    label_timestamp: self.label_timestamps_by_id.get(id).cloned(),
                });
            } else {
                stack.push((index, true));
                stack.extend(children[index].iter().map(|&child| (child, false)));
            }
        }
        roots
            .iter()
            .filter_map(|&index| built[index].take())
            .collect()
    }
}

impl SessionManager {
    /// senpi `SessionManager.list(cwd, sessionDir)`: one project's sessions, newest first. A custom
    /// session dir that is not this cwd's default dir is filtered to sessions started in `cwd`;
    /// published partial rows are filtered the same way.
    pub fn list(
        cwd: &str,
        session_dir: Option<&str>,
        mut on_progress: Option<crate::session_discovery::SessionListProgress<'_>>,
    ) -> Vec<crate::session_discovery::SessionInfo> {
        let options = PathInputOptions::default();
        let dir = session_dir
            .map(|dir| normalize_path(dir, &options))
            .unwrap_or_else(|| get_default_session_dir(cwd));
        let filter_cwd =
            session_dir.is_some() && dir != get_default_session_dir_path(cwd, &get_agent_dir());
        let resolved_cwd = resolve_path(cwd, cwd, &options);
        let include = |session: &crate::session_discovery::SessionInfo| {
            !filter_cwd
                || (!session.cwd.is_empty()
                    && resolve_path(&session.cwd, &session.cwd, &options) == resolved_cwd)
        };
        let mut forward = |loaded: usize, total: usize, rows: Option<Vec<crate::session_discovery::SessionInfo>>| {
            if let Some(progress) = on_progress.as_deref_mut() {
                progress(loaded, total, rows.map(|rows| rows.into_iter().filter(|row| include(row)).collect()));
            }
        };
        let mut sessions: Vec<_> =
            crate::session_discovery::list_sessions_from_dir(&dir, Some(&mut forward), 0, None)
                .into_iter()
                .filter(|session| include(session))
                .collect();
        sessions.sort_by_key(|session| std::cmp::Reverse(session.modified));
        sessions
    }

    /// senpi `SessionManager.listAll(sessionDir)`: every session in a custom dir, or across every
    /// project directory under the agent sessions dir, newest first. `on_progress` reports
    /// `(loaded, total)` across every directory and publishes partial rows every
    /// [`crate::session_discovery::ALL_SESSION_LIST_PUBLISH_INTERVAL`] files. Files load newest first
    /// across all directories (see [`crate::session_discovery::session_files_newest_first`]).
    pub fn list_all(
        session_dir: Option<&str>,
        on_progress: Option<crate::session_discovery::SessionListProgress<'_>>,
    ) -> Vec<crate::session_discovery::SessionInfo> {
        let mut sessions = match session_dir {
            Some(dir) => crate::session_discovery::list_sessions_from_dir(
                &normalize_path(dir, &PathInputOptions::default()),
                on_progress,
                0,
                None,
            ),
            None => {
                let dirs: Vec<PathBuf> = std::fs::read_dir(get_sessions_dir())
                    .map(|entries| {
                        entries
                            .filter_map(Result::ok)
                            .map(|entry| entry.path())
                            .filter(|path| path.is_dir())
                            .collect()
                    })
                    .unwrap_or_default();
                let files = crate::session_discovery::session_files_newest_first(&dirs);
                crate::session_discovery::list_session_files(
                    &files,
                    on_progress,
                    0,
                    files.len(),
                    crate::session_discovery::ALL_SESSION_LIST_PUBLISH_INTERVAL,
                )
            }
        };
        sessions.sort_by_key(|session| std::cmp::Reverse(session.modified));
        sessions
    }
}

pub fn now_iso_millis() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn serialize_entry(entry: &Value) -> String {
    serde_json::to_string(entry).unwrap_or_else(|_| "null".to_owned())
}

fn append_line(session_file: &str, entry: &Value) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(session_file)?;
    // PERF-DIVERGENCE(senpi session-manager.ts): upstream appendFileSync builds the line once; the port allocated a second String via format!, this appends the newline to the serialized line. Bytes appended are identical.
    let mut line = serialize_entry(entry);
    line.push('\n');
    file.write_all(line.as_bytes())
}

/// Validates the `sessionRetentionDays` setting: unset means keep forever; negative, fractional,
/// non-finite or out-of-range numbers are rejected.
pub fn parse_retention_days(value: Option<f64>) -> Result<Option<u64>, String> {
    match value {
        None => Ok(None),
        Some(days) if days.is_finite() && days >= 1.0 && days.fract() == 0.0 && days <= 36_500.0 => {
            Ok(Some(days as u64))
        }
        Some(days) => Err(format!(
            "Invalid sessionRetentionDays {days}: expected a whole number of days between 1 and 36500; sessions are not pruned"
        )),
    }
}

/// Deletes `*.jsonl` session files under `root` (the root itself and its direct project
/// directories) last modified more than `days` days before `now`. Symlinks are never followed,
/// `keep` is never deleted, and a session whose sidecar lock (`<file>.lock`) another process
/// holds is skipped.
// SAFETY-DIVERGENCE(packages/coding-agent/src/core/session-manager.ts): upstream never deletes session transcripts; mhc opt-in prunes idle *.jsonl older than `sessionRetentionDays` under the default sessions root, skipping symlinks, the open session and any session whose sidecar lock another process holds (no lock is ever taken on the session file itself, so Windows mandatory locks cannot break writes).
pub fn prune_sessions(
    root: &Path,
    days: u64,
    keep: &Path,
    now: std::time::SystemTime,
) -> std::io::Result<usize> {
    let Some(cutoff) = now.checked_sub(std::time::Duration::from_secs(days.saturating_mul(86_400)))
    else {
        return Ok(0);
    };
    let keep = std::fs::canonicalize(keep).unwrap_or_else(|_| keep.to_path_buf());
    let mut removed = 0;
    let mut dirs = vec![root.to_path_buf()];
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            dirs.push(entry.path());
        }
    }
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
            if meta.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".jsonl.lock"))
            {
                collect_orphan_sidecar(&path);
                continue;
            }
            if !meta.is_file() || path.extension().is_none_or(|ext| ext != "jsonl") {
                continue;
            }
            if !meta.modified().is_ok_and(|modified| modified < cutoff) {
                continue;
            }
            if std::fs::canonicalize(&path).is_ok_and(|resolved| resolved == keep) {
                continue;
            }
            // Held across the delete so a session starting now cannot slip in between.
            let lock = match open_retention_lock(&path) {
                Ok(lock) => lock,
                Err(error) => {
                    if error.kind() != std::io::ErrorKind::WouldBlock {
                        crate::hidden_stdout_log::append_debug_note(&format!(
                            "session retention: cannot lock {}: {error}",
                            path.display()
                        ));
                    }
                    continue;
                }
            };
            // A session may have written between the scan and the lock; re-check under the lock.
            if !is_idle_since(&path, cutoff) {
                continue;
            }
            // Session first, then its sidecar, then the lock is released.
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    removed += 1;
                    lock.unlink_sidecar();
                }
                Err(error) => crate::hidden_stdout_log::append_debug_note(&format!(
                    "session retention: cannot remove {}: {error}",
                    path.display()
                )),
            }
            drop(lock);
        }
    }
    Ok(removed)
}

/// Whether `path` is a regular file (not a symlink) last modified before `cutoff`.
fn is_idle_since(path: &Path, cutoff: std::time::SystemTime) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.modified().is_ok_and(|modified| modified < cutoff))
}

/// Removes a `*.jsonl.lock` sidecar whose session file is gone (for example after a crash), but
/// only while holding a verified lock on it, so a live session's sidecar is never touched.
fn collect_orphan_sidecar(sidecar: &Path) {
    let Some(session) = sidecar.to_str().and_then(|path| path.strip_suffix(".lock")) else {
        return;
    };
    let session = Path::new(session);
    if std::fs::symlink_metadata(session).is_ok() {
        return;
    }
    let Ok(lock) = open_retention_lock(session) else { return };
    if std::fs::symlink_metadata(session).is_err() {
        lock.unlink_sidecar();
    }
}

/// Startup retention: prunes the default sessions root when `sessionRetentionDays` is a valid
/// whole number of 1..=36500 days. Returns a warning for an invalid value or a failed prune.
pub fn prune_sessions_at_startup(setting: Option<f64>, keep: Option<&str>) -> Option<String> {
    let days = match parse_retention_days(setting) {
        Ok(Some(days)) => days,
        Ok(None) => return None,
        Err(message) => return Some(message),
    };
    let root = Path::new(&get_agent_dir()).join("sessions");
    let keep = keep.unwrap_or_default();
    match prune_sessions(&root, days, Path::new(keep), std::time::SystemTime::now()) {
        Ok(count) => crate::hidden_stdout_log::append_debug_note(&format!(
            "session retention pruned {count} session file(s) older than {days} day(s)"
        )),
        Err(error) => {
            let message = format!("Session retention failed for {}: {error}", root.display());
            crate::hidden_stdout_log::append_debug_note(&message);
            return Some(message);
        }
    }
    None
}

pub fn sessions_dir() -> String {
    get_sessions_dir()
}

pub fn seek_end_of_file(file: &mut std::fs::File) -> std::io::Result<u64> {
    file.seek(SeekFrom::End(0))
}

struct ManagerHistory<'a>(&'a SessionManager);

impl crate::session_history_reduce::HistorySource for ManagerHistory<'_> {
    fn generation(&self) -> u64 {
        self.0.history_generation
    }

    fn count(&self) -> usize {
        self.0.full_entry_count
    }

    fn appended_since(&self, count: usize) -> Option<Vec<Value>> {
        self.0.appended_since(count)
    }

    fn read_all(&self) -> Vec<Value> {
        self.0.entries()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holder_record(manager: &SessionManager) -> std::path::PathBuf {
        Path::new(manager.session_file().expect("session file"))
            .parent()
            .expect("parent")
            .join("session-holders")
            .join(crate::schedule::runner_lease::encode_session_id(
                manager.session_id(),
            ))
            .join(format!("{}.json", std::process::id()))
    }

    #[test]
    fn persisted_manager_publishes_holder_before_first_flush_and_releases_on_drop() {
        // Given
        let dir = tempfile::tempdir().expect("session dir");
        let dir = dir.path().to_string_lossy();
        // When
        let manager = SessionManager::create(&dir, Some(&dir), None);
        let record = holder_record(&manager);
        // Then
        assert!(!Path::new(manager.session_file().expect("session file")).exists());
        let holder: Value = serde_json::from_slice(&std::fs::read(&record).expect("holder record"))
            .expect("holder JSON");
        assert_eq!(holder["pid"], std::process::id());
        assert!(
            holder["processStartedAtMs"]
                .as_u64()
                .is_some_and(|start| start > 0)
        );
        assert!(holder["bootAtMs"].as_u64().is_some_and(|boot| boot > 0));
        drop(manager);
        assert!(!record.exists());
    }

    #[test]
    fn switching_session_releases_old_holder_and_dispose_releases_new_holder() {
        // Given
        let dir = tempfile::tempdir().expect("session dir");
        let dir = dir.path().to_string_lossy();
        let mut manager = SessionManager::create(&dir, Some(&dir), None);
        let old = holder_record(&manager);
        // When
        manager.new_session(None);
        // Then
        let new = holder_record(&manager);
        assert_ne!(old, new);
        assert!(!old.exists());
        assert!(new.exists());
        manager.dispose();
        assert!(!new.exists());
    }

    #[test]
    fn separate_managers_share_holder_until_the_last_writer_disposes() {
        // Given
        let dir = tempfile::tempdir().expect("session dir");
        let dir = dir.path().to_string_lossy();
        let mut first = SessionManager::create(&dir, Some(&dir), None);
        first.append_message(json!({"role":"assistant", "content":"ready", "timestamp":1}));
        let record = holder_record(&first);
        let mut second = SessionManager::open(
            first.session_file().expect("file"),
            Some(&dir),
            Some(&dir),
            None,
        )
        .expect("open");
        // When
        first.dispose();
        // Then
        assert!(record.exists());
        second.dispose();
        assert!(!record.exists());
    }

    fn header(id: &str, version: Option<i64>, cwd: &str) -> Value {
        let mut header = json!({
            "type": "session",
            "id": id,
            "timestamp": "2026-09-24T01:49:15.521Z",
            "cwd": cwd,
        });
        if let Some(version) = version {
            header["version"] = Value::from(version);
        }
        header
    }

    #[test]
    fn round_trips_a_session_file_byte_for_byte() {
        let lines = [
            r#"{"type":"session","version":3,"id":"01a0d11a-4780-7ed0-962e-cf60e634e699","timestamp":"2026-09-24T01:49:15.521Z","cwd":"/home/indo/piratetalk"}"#,
            r#"{"type":"message","id":"ab12cd34","parentId":null,"timestamp":"2026-09-24T01:49:16.000Z","message":{"role":"user","content":"hi","timestamp":1784935756000}}"#,
            r#"{"type":"message","id":"ef56ab78","parentId":"ab12cd34","timestamp":"2026-09-24T01:49:17.000Z","message":{"role":"assistant","content":[{"type":"text","text":"hello"}],"api":"anthropic","provider":"anthropic","model":"claude","usage":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0,"cost":{"total":0.01}},"stopReason":"stop","timestamp":1784935757000,"unknownField":{"kept":true}}}"#,
            r#"{"type":"label","id":"99887766","parentId":"ef56ab78","timestamp":"2026-09-24T01:49:18.000Z","targetId":"ab12cd34","label":"start"}"#,
            r#"{"type":"custom","id":"11223344","parentId":"99887766","timestamp":"2026-09-24T01:49:19.000Z","customType":"demo","data":{"n":[1,2,3],"f":1.5}}"#,
        ];
        let content = format!("{}\n", lines.join("\n"));
        let entries = parse_session_entries(&content);
        assert_eq!(entries.len(), 5);
        let reserialized: String = entries
            .iter()
            .map(|entry| format!("{}\n", serialize_entry(entry)))
            .collect();
        assert_eq!(reserialized, content);
    }

    #[test]
    fn appending_after_a_read_invalidates_the_shared_entries_cache() {
        let mut manager = SessionManager::in_memory("/tmp", None, None);
        manager.append_message(json!({ "role": "user", "content": "one", "timestamp": 1 }));
        assert_eq!(manager.entries().len(), 1);
        let second = manager.append_message(json!({ "role": "user", "content": "two", "timestamp": 2 }));
        let entries = manager.entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1]["id"], second["id"]);
        assert_eq!(manager.current_branch().last().map(|entry| &entry["id"]), Some(&second["id"]));
    }

    #[test]
    fn current_branch_tail_is_a_suffix_of_the_materialized_branch_with_its_true_length() {
        let mut manager = SessionManager::in_memory("/tmp", None, None);
        assert_eq!(
            manager.current_branch_tail(3, &mut |_: &Value| true),
            (0, Vec::new())
        );
        manager.append_message(json!({ "role": "assistant", "content": "old", "timestamp": 1 }));
        for n in 0..6 {
            manager.append_message(
                json!({ "role": "user", "content": format!("u{n}"), "timestamp": n + 2 }),
            );
        }
        let full = manager.current_branch();
        assert_eq!(full.len(), 7);
        let (len, tail) = manager.current_branch_tail(2, &mut |_: &Value| true);
        assert_eq!((len, tail.as_slice()), (7, &full[5..]));
        // The tail extends past `min_entries` until the predicate (here: an assistant) is met.
        let (len, tail) = manager.current_branch_tail(2, &mut |entry: &Value| {
            entry["message"]["role"] == "assistant"
        });
        assert_eq!((len, tail.as_slice()), (7, full.as_slice()));
        let (len, tail) = manager.current_branch_tail(50, &mut |_: &Value| false);
        assert_eq!((len, tail), (7, full));
    }

    #[test]
    fn latest_branch_entry_id_by_type_matches_materialized_branch_lookup() {
        let mut manager = SessionManager::in_memory("/tmp", None, None);
        assert_eq!(
            manager.latest_branch_entry_id_by_type("compaction", None),
            None
        );
        let user =
            manager.append_message(json!({ "role": "user", "content": "hi", "timestamp": 1 }));
        let assistant =
            manager.append_message(json!({ "role": "assistant", "content": [], "timestamp": 2 }));
        let compaction = manager.append_compaction(
            "summary",
            user["id"].as_str().expect("id"),
            10,
            None,
            None,
            None,
        );
        manager.append_message(json!({ "role": "user", "content": "again", "timestamp": 3 }));
        let expected = |kind: &str, role: Option<&str>| {
            manager
                .current_branch()
                .into_iter()
                .rev()
                .find(|entry| {
                    entry["type"] == kind
                        && role.is_none_or(|role| entry["message"]["role"] == role)
                })
                .and_then(|entry| entry["id"].as_str().map(str::to_owned))
        };
        assert_eq!(
            manager.latest_branch_entry_id_by_type("compaction", None),
            compaction["id"].as_str().map(str::to_owned)
        );
        assert_eq!(
            manager.latest_branch_entry_id_by_type("message", Some("assistant")),
            assistant["id"].as_str().map(str::to_owned)
        );
        for (kind, role) in [
            ("compaction", None),
            ("message", Some("assistant")),
            ("message", Some("user")),
            ("label", None),
        ] {
            assert_eq!(
                manager.latest_branch_entry_id_by_type(kind, role),
                expected(kind, role)
            );
        }
    }

    #[test]
    fn entry_count_tracks_non_header_entries_and_list_reports_progress() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let mut manager = SessionManager::create(&dir, Some(&dir), None);
        assert_eq!(manager.get_entry_count(), 0);
        manager.append_message(json!({ "role": "user", "content": "hi", "timestamp": 1 }));
        manager.append_message(json!({ "role": "assistant", "content": [{"type": "text", "text": "hello"}], "stopReason": "stop", "timestamp": 2 }));
        assert_eq!(manager.get_entry_count(), 2);
        let path = manager.session_file().expect("session file").to_owned();
        assert_eq!(
            SessionManager::open(&path, Some(&dir), None, None)
                .expect("open")
                .get_entry_count(),
            2
        );
        let mut updates = Vec::new();
        let sessions = SessionManager::list(
            &dir,
            Some(&dir),
            Some(&mut |loaded, total, _rows| updates.push((loaded, total))),
        );
        assert_eq!(sessions.len(), 1);
        assert_eq!(updates, vec![(1, 1)]);
        let mut all_updates = Vec::new();
        SessionManager::list_all(
            Some(&dir),
            Some(&mut |loaded, total, _rows| all_updates.push((loaded, total))),
        );
        assert_eq!(all_updates, vec![(1, 1)]);
    }

    #[test]
    fn round_trips_through_the_manager_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let mut manager = SessionManager::create(
            &dir,
            Some(&dir),
            Some(NewSessionOptions {
                id: Some("sess-1".to_owned()),
                ..Default::default()
            }),
        );
        manager.append_message(json!({ "role": "user", "content": "hi", "timestamp": 1 }));
        let assistant = manager.append_message(json!({
            "role": "assistant",
            "content": [{"type": "text", "text": "hello"}],
            "usage": { "input": 3, "output": 4, "cacheRead": 5, "cacheWrite": 6, "cost": { "total": 0.5 } },
            "stopReason": "stop",
            "timestamp": 2,
        }));
        let session_file = manager.session_file().expect("session file").to_owned();
        let first_read = std::fs::read_to_string(&session_file).expect("read");
        let reloaded = load_entries_from_file(&session_file);
        let reserialized: String = reloaded
            .iter()
            .map(|entry| format!("{}\n", serialize_entry(entry)))
            .collect();
        assert_eq!(reserialized, first_read);
        assert_eq!(reloaded.len(), 3);
        assert_eq!(reloaded[0]["id"], "sess-1");
        assert_eq!(reloaded[2]["id"], assistant["id"]);
        let totals = manager.usage_totals();
        assert_eq!(
            (
                totals.input,
                totals.output,
                totals.cache_read,
                totals.cache_write
            ),
            (3, 4, 5, 6)
        );
        assert!((totals.cost - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn fork_from_copies_source_entries_into_a_new_session_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let source_dir = tmp.path().join("source");
        let target_dir = tmp.path().join("target");
        std::fs::create_dir_all(&source_dir).expect("source dir");
        std::fs::create_dir_all(&target_dir).expect("target dir");
        let source_file = source_dir.join("source-session.jsonl");
        std::fs::write(
            &source_file,
            format!(
                "{}\n{}\n",
                serialize_entry(&header("source", Some(3), dir.as_str())),
                serialize_entry(
                    &json!({ "type": "message", "id": "entry-1", "message": { "role": "user" } })
                )
            ),
        )
        .expect("write source");
        let mut forked = SessionManager::fork_from(
            source_file.to_str().expect("source path"),
            target_dir.to_str().expect("target path"),
            Some(target_dir.to_str().expect("target dir")),
            Some(NewSessionOptions {
                id: Some("forked-session".to_owned()),
                ..Default::default()
            }),
        )
        .expect("fork");
        let forked_file = forked.session_file().expect("forked session file");
        let entries = load_entries_from_file(forked_file);
        assert_eq!(
            entries
                .first()
                .and_then(|entry| entry.get("parentSession"))
                .and_then(Value::as_str),
            Some(source_file.to_str().expect("source path"))
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.get("id").and_then(Value::as_str) == Some("entry-1"))
        );
        assert_eq!(
            entries
                .first()
                .and_then(|entry| entry.get("cwd"))
                .and_then(Value::as_str),
            Some(target_dir.to_str().expect("target path"))
        );
        forked.dispose();
    }

    #[test]
    fn fork_from_writes_boundary_and_fork_provenance_returns_it_after_reopen() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let source_dir = tmp.path().join("source");
        let target_dir = tmp.path().join("target");
        std::fs::create_dir_all(&source_dir).expect("source dir");
        std::fs::create_dir_all(&target_dir).expect("target dir");
        let source_file = source_dir.join("source-session.jsonl");
        std::fs::write(
            &source_file,
            format!(
                "{}\n{}\n{}\n",
                serialize_entry(&header("source", Some(3), dir.as_str())),
                serialize_entry(&json!({ "type": "message", "id": "entry-1", "message": { "role": "user" } })),
                serialize_entry(&json!({ "type": "message", "id": "entry-2", "message": { "role": "assistant" } })),
            ),
        )
        .expect("write source");

        let mut forked = SessionManager::fork_from(
            source_file.to_str().expect("source path"),
            target_dir.to_str().expect("target path"),
            Some(target_dir.to_str().expect("target dir")),
            Some(NewSessionOptions {
                id: Some("forked-session".to_owned()),
                ..Default::default()
            }),
        )
        .expect("fork");
        let forked_file = forked.session_file().expect("forked session file").to_owned();

        // Check header directly
        let entries = load_entries_from_file(&forked_file);
        assert_eq!(
            entries.first().and_then(|entry| entry.get("forkedFromLeafId")).and_then(Value::as_str),
            Some("entry-2")
        );

        // Check fork_provenance on live manager
        let prov = forked.fork_provenance().expect("fork provenance");
        assert_eq!(prov.parent_session, source_file.to_str().expect("source path"));
        assert_eq!(prov.inherited_through_entry_id.as_deref(), Some("entry-2"));
        forked.dispose();

        // Reopen and check fork_provenance again
        let mut reopened = SessionManager::open(&forked_file, Some(target_dir.to_str().expect("target dir")), Some(target_dir.to_str().expect("target path")), None).expect("reopen");
        let prov_reopened = reopened.fork_provenance().expect("reopened fork provenance");
        assert_eq!(prov_reopened.parent_session, source_file.to_str().expect("source path"));
        assert_eq!(prov_reopened.inherited_through_entry_id.as_deref(), Some("entry-2"));
        reopened.dispose();

        // Old fork without forkedFromLeafId:
        let old_fork_file = target_dir.join("old-fork.jsonl");
        std::fs::write(
            &old_fork_file,
            format!(
                "{}\n{}\n",
                serialize_entry(&json!({
                    "type": "session",
                    "version": 3,
                    "id": "old-fork",
                    "timestamp": "2026-09-24T01:49:15.521Z",
                    "cwd": target_dir.to_str().expect("target path"),
                    "parentSession": source_file.to_str().expect("source path"),
                })),
                serialize_entry(&json!({ "type": "message", "id": "entry-1", "message": { "role": "user" } })),
            ),
        ).expect("write old fork");
        let mut old_mgr = SessionManager::open(old_fork_file.to_str().expect("old fork file"), Some(target_dir.to_str().expect("target dir")), Some(target_dir.to_str().expect("target path")), None).expect("open old");
        let old_prov = old_mgr.fork_provenance().expect("old fork provenance");
        assert_eq!(old_prov.inherited_through_entry_id.as_deref(), Some("entry-1"));
        old_mgr.dispose();

        // Old fork with unreadable parent:
        let broken_parent_fork_file = target_dir.join("broken-parent-fork.jsonl");
        std::fs::write(
            &broken_parent_fork_file,
            format!(
                "{}\n{}\n",
                serialize_entry(&json!({
                    "type": "session",
                    "version": 3,
                    "id": "broken-fork",
                    "timestamp": "2026-09-24T01:49:15.521Z",
                    "cwd": target_dir.to_str().expect("target path"),
                    "parentSession": "/nonexistent/parent/session.jsonl",
                })),
                serialize_entry(&json!({ "type": "message", "id": "entry-1", "message": { "role": "user" } })),
            ),
        ).expect("write broken fork");
        let mut broken_mgr = SessionManager::open(broken_parent_fork_file.to_str().expect("broken fork file"), Some(target_dir.to_str().expect("target dir")), Some(target_dir.to_str().expect("target path")), None).expect("open broken");
        assert!(broken_mgr.fork_provenance().is_none());
        broken_mgr.dispose();
    }

    #[test]
    fn dispose_keeps_blobs_while_another_writer_is_live_and_last_writer_cleans_them() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let options = Some(NewSessionOptions {
            id: Some("shared-session".to_owned()),
            ..Default::default()
        });
        let mut first = SessionManager::create(&dir, Some(&dir), options.clone());
        first.append_message(json!({"role":"assistant","content":[]}));
        let session_file = first.session_file().expect("session file").to_owned();
        let mut second =
            SessionManager::open(&session_file, Some(&dir), Some(&dir), None).expect("open");
        let blobs = Path::new(&dir)
            .join("resident-blobs")
            .join("shared-session");
        std::fs::create_dir_all(&blobs).expect("blob directory");
        std::fs::write(blobs.join("blob"), b"resident").expect("blob");
        first.dispose();
        assert!(blobs.join("blob").exists());
        second.dispose();
        assert!(!blobs.exists());
    }

    #[test]
    fn buffers_entries_until_the_first_assistant_message() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let mut manager = SessionManager::create(&dir, Some(&dir), None);
        manager.append_message(json!({ "role": "user", "content": "hi", "timestamp": 1 }));
        let session_file = manager.session_file().expect("session file").to_owned();
        assert!(!Path::new(&session_file).exists());
        manager.append_message(
            json!({ "role": "assistant", "content": [], "stopReason": "stop", "timestamp": 2 }),
        );
        let content = std::fs::read_to_string(&session_file).expect("read");
        assert_eq!(content.lines().count(), 3);
        assert!(manager.is_flushed());
    }

    #[test]
    fn migrates_v1_to_v2_with_tree_ids_and_first_kept_entry_id() {
        let mut entries = vec![
            header("sess", None, "/w"),
            json!({ "type": "message", "timestamp": "2026-09-24T01:00:00.000Z", "message": { "role": "user", "content": "a" } }),
            json!({ "type": "message", "timestamp": "2026-09-24T01:00:01.000Z", "message": { "role": "assistant", "content": [] } }),
            json!({ "type": "compaction", "timestamp": "2026-09-24T01:00:02.000Z", "summary": "s", "firstKeptEntryIndex": 2, "tokensBefore": 10 }),
        ];
        assert!(migrate_to_current_version(&mut entries));
        assert_eq!(entries[0]["version"], 3);
        assert!(entries[1]["id"].is_string());
        assert_eq!(entries[1]["parentId"], Value::Null);
        assert_eq!(entries[2]["parentId"], entries[1]["id"]);
        assert_eq!(entries[3]["parentId"], entries[2]["id"]);
        assert_eq!(entries[3]["firstKeptEntryId"], entries[2]["id"]);
        assert!(entries[3].get("firstKeptEntryIndex").is_none());
        assert_eq!(entries[3]["version"], Value::Null);
    }

    #[test]
    fn migrates_v2_to_v3_and_renames_hook_message() {
        let mut entries = vec![
            header("sess", Some(2), "/w"),
            json!({ "type": "message", "id": "a", "parentId": null, "timestamp": "t", "message": { "role": "hookMessage", "content": "x" } }),
        ];
        assert!(migrate_to_current_version(&mut entries));
        assert_eq!(entries[0]["version"], 3);
        assert_eq!(entries[1]["message"]["role"], "custom");
    }

    #[test]
    fn a_v3_file_is_not_migrated_again() {
        let mut entries = vec![header("sess", Some(3), "/w")];
        assert!(!migrate_to_current_version(&mut entries));
        assert_eq!(entries[0]["version"], 3);
    }

    #[test]
    fn opening_a_v2_file_rewrites_it_at_version_3() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("2026-09-24T01-00-00-000Z_sess.jsonl");
        let lines = [
            r#"{"type":"session","version":2,"id":"sess","timestamp":"2026-09-24T01:00:00.000Z","cwd":"/w"}"#,
            r#"{"type":"message","id":"a","parentId":null,"timestamp":"2026-09-24T01:00:01.000Z","message":{"role":"hookMessage","content":"x"}}"#,
        ];
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).expect("write");
        let manager = SessionManager::open(
            &path.to_string_lossy(),
            Some(&tmp.path().to_string_lossy()),
            None,
            None,
        )
        .expect("open");
        assert_eq!(manager.header().expect("header")["version"], 3);
        let rewritten = std::fs::read_to_string(&path).expect("read");
        assert!(rewritten.contains("\"version\":3"));
        assert!(rewritten.contains("\"role\":\"custom\""));
        assert_eq!(manager.session_id(), "sess");
        let expected_cwd = if cfg!(windows) { "W:\\" } else { "/w" };
        assert_eq!(manager.cwd(), expected_cwd);
    }

    #[test]
    fn build_session_context_follows_the_leaf_path_and_the_latest_compaction() {
        let entries = vec![
            json!({ "type": "message", "id": "a", "parentId": null, "timestamp": "t1", "message": { "role": "user", "content": "one" } }),
            json!({ "type": "message", "id": "b", "parentId": "a", "timestamp": "t2", "message": { "role": "assistant", "content": [], "provider": "anthropic", "model": "claude", "stopReason": "stop" } }),
            json!({ "type": "message", "id": "c", "parentId": "b", "timestamp": "t3", "message": { "role": "user", "content": "two" } }),
            json!({ "type": "compaction", "id": "d", "parentId": "c", "timestamp": "t4", "summary": "sum", "firstKeptEntryId": "c", "tokensBefore": 7 }),
            json!({ "type": "message", "id": "e", "parentId": "d", "timestamp": "t5", "message": { "role": "user", "content": "three" } }),
        ];
        let context = build_session_context(&entries, None);
        assert_eq!(context.messages.len(), 3);
        assert_eq!(context.messages[0]["role"], "compactionSummary");
        assert_eq!(context.messages[0]["summary"], "sum");
        assert_eq!(context.messages[1]["content"], "two");
        assert_eq!(context.messages[2]["content"], "three");
        assert_eq!(
            context.model,
            Some(("anthropic".to_owned(), "claude".to_owned()))
        );
        let branch = build_context_entries(&entries, Some("b"));
        assert_eq!(branch.len(), 2);
        assert_eq!(branch[0]["id"], "a");
        assert!(build_context_entries(&entries, Some("")).is_empty());
    }

    #[test]
    fn build_session_context_stamps_each_message_with_its_entry_id() {
        let entries = vec![
            json!({ "type": "message", "id": "a", "parentId": null, "timestamp": "t1", "message": { "role": "user", "content": "one" } }),
            json!({ "type": "model_change", "id": "b", "parentId": "a", "timestamp": "t2", "provider": "anthropic", "modelId": "opus" }),
            json!({ "type": "compaction", "id": "c", "parentId": "b", "timestamp": "t3", "summary": "sum", "firstKeptEntryId": "a", "tokensBefore": 7 }),
            json!({ "type": "message", "id": "d", "parentId": "c", "timestamp": "t4", "message": { "role": "user", "content": "two" } }),
        ];
        let context = build_session_context(&entries, None);
        assert_eq!(
            context.entry_ids,
            vec!["c".to_owned(), "a".to_owned(), "d".to_owned()]
        );
        assert!(
            context
                .messages
                .iter()
                .all(|message| message.get("id").is_none())
        );
    }

    #[test]
    fn omit_unsafe_checkpoint_repairs_only_its_retained_messages() {
        let unsafe_result = json!({ "role": "toolResult", "toolCallId": "t", "toolName": "read", "content": [{ "type": "image", "mimeType": "image/png" }], "isError": false, "timestamp": 2 });
        let call = json!({ "role": "assistant", "content": [{ "type": "toolCall", "id": "t", "name": "read", "arguments": {} }], "timestamp": 1 });
        let build = |policy: Option<&str>| {
            let details = policy.map_or(Value::Null, |policy| json!({ "retainedMessagePolicy": policy }));
            let entries = vec![
                json!({ "type": "message", "id": "a", "parentId": null, "timestamp": "t1", "message": { "role": "user", "content": "task", "timestamp": 0 } }),
                json!({ "type": "message", "id": "b", "parentId": "a", "timestamp": "t2", "message": call }),
                json!({ "type": "message", "id": "c", "parentId": "b", "timestamp": "t3", "message": unsafe_result }),
                json!({ "type": "compaction", "id": "d", "parentId": "c", "timestamp": "t4", "summary": "s", "firstKeptEntryId": "a", "tokensBefore": 1, "details": details }),
                json!({ "type": "message", "id": "e", "parentId": "d", "timestamp": "t5", "message": { "role": "toolResult", "toolCallId": "later", "toolName": "read", "content": [{ "type": "image", "mimeType": "image/png" }], "isError": false, "timestamp": 9 } }),
            ];
            build_session_context(&entries, None)
        };
        let plain = build(None);
        assert_eq!(plain.messages[3], unsafe_result);
        let repaired = build(Some("omit-unsafe-v1"));
        assert_eq!(repaired.entry_ids, plain.entry_ids);
        // The retained assistant is unsafe (bare envelope), so it and its result are omitted together.
        assert_eq!(repaired.messages[2]["role"], "user");
        assert_eq!(repaired.messages[3]["role"], "user");
        // Messages after the checkpoint keep their normal semantics.
        assert_eq!(repaired.messages[4], plain.messages[4]);
    }

    #[test]
    fn a_fallback_window_restores_the_primary_model_and_thinking_level() {
        let entries = vec![
            json!({ "type": "thinking_level_change", "id": "a", "parentId": null, "timestamp": "t", "thinkingLevel": "high" }),
            json!({ "type": "model_change", "id": "b", "parentId": "a", "timestamp": "t", "provider": "anthropic", "modelId": "opus" }),
            json!({ "type": "model_change", "id": "c", "parentId": "b", "timestamp": "t", "provider": "google", "modelId": "flash", "reason": "fallback", "originalProvider": "anthropic", "originalModelId": "opus" }),
            json!({ "type": "thinking_level_change", "id": "d", "parentId": "c", "timestamp": "t", "thinkingLevel": "low" }),
        ];
        let context = build_session_context(&entries, None);
        assert_eq!(
            context.model,
            Some(("anthropic".to_owned(), "opus".to_owned()))
        );
        assert_eq!(context.thinking_level, "high");
        let mut reverted = entries.clone();
        reverted.push(json!({ "type": "model_change", "id": "e", "parentId": "d", "timestamp": "t", "provider": "anthropic", "modelId": "opus", "reason": "fallback-revert" }));
        assert_eq!(
            build_session_context(&reverted, None).thinking_level,
            "high"
        );
    }

    #[test]
    fn an_assistant_message_from_another_provider_restores_the_model() {
        let entries = vec![
            json!({ "type": "model_change", "id": "a", "parentId": null, "timestamp": "t", "provider": "anthropic", "modelId": "opus" }),
            json!({ "type": "message", "id": "b", "parentId": "a", "timestamp": "t", "message": { "role": "assistant", "content": [], "provider": "google", "model": "flash", "stopReason": "stop" } }),
            json!({ "type": "message", "id": "c", "parentId": "b", "timestamp": "t", "message": { "role": "assistant", "content": [], "provider": "anthropic", "model": "opus-wire", "stopReason": "stop" } }),
        ];
        assert_eq!(
            build_session_context(&entries, None).model,
            Some(("anthropic".to_owned(), "opus-wire".to_owned()))
        );
    }

    #[test]
    fn entries_missing_their_content_project_an_empty_array() {
        let entry = json!({ "type": "message", "id": "a", "parentId": null, "timestamp": "t", "message": { "role": "user", "content": null } });
        let messages = session_entry_to_context_messages(&entry);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["content"], Value::Array(Vec::new()));
        let custom = json!({ "type": "custom", "id": "b", "parentId": null, "timestamp": "t", "customType": "x" });
        assert!(session_entry_to_context_messages(&custom).is_empty());
    }

    #[test]
    fn labels_and_session_names_track_the_latest_entry() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let mut manager = SessionManager::create(&dir, Some(&dir), None);
        let first =
            manager.append_message(json!({ "role": "user", "content": "a", "timestamp": 1 }));
        manager
            .append_label(first["id"].as_str().unwrap_or_default(), Some("start"))
            .expect("label");
        assert_eq!(
            manager.label(first["id"].as_str().unwrap_or_default()),
            Some("start")
        );
        manager
            .append_label(first["id"].as_str().unwrap_or_default(), None)
            .expect("label");
        assert_eq!(
            manager.label(first["id"].as_str().unwrap_or_default()),
            None
        );
        manager.append_session_info(Some("  named  "));
        assert_eq!(manager.session_name(), Some("named"));
        manager.append_session_info(Some("   "));
        assert_eq!(manager.session_name(), None);
    }

    #[test]
    fn an_empty_label_clears_like_no_label_and_stays_cleared_after_reload() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let mut manager = SessionManager::create(&dir, Some(&dir), None);
        let first =
            manager.append_message(json!({ "role": "user", "content": "a", "timestamp": 1 }));
        manager.append_message(json!({ "role": "assistant", "content": [], "timestamp": 2 }));
        let id = first["id"].as_str().unwrap_or_default().to_owned();
        manager.append_label(&id, Some("start")).expect("label");
        manager.append_label(&id, Some("")).expect("clear");
        assert_eq!(manager.label(&id), None);
        let session_file = manager.session_file().expect("session file").to_owned();
        let reopened = SessionManager::open(&session_file, Some(&dir), None, None).expect("open");
        assert_eq!(reopened.label(&id), None);
    }

    #[test]
    fn open_derives_the_session_dir_from_the_file_and_terminates_the_last_line() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let custom = tmp.path().join("custom");
        std::fs::create_dir_all(&custom).expect("dir");
        let path = custom.join("s.jsonl");
        let header = serialize_entry(
            &json!({ "type": "session", "version": CURRENT_SESSION_VERSION, "id": "s", "timestamp": "2026-01-01T00:00:00.000Z", "cwd": "/tmp" }),
        );
        std::fs::write(&path, &header).expect("unterminated header");
        let mut manager =
            SessionManager::open(&path.to_string_lossy(), None, None, None).expect("open");
        assert_eq!(manager.session_dir(), custom.to_string_lossy());
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            format!("{header}\n")
        );
        manager.append_message(json!({ "role": "user", "content": "a", "timestamp": 1 }));
        manager.append_message(json!({ "role": "assistant", "content": [], "timestamp": 2 }));
        assert_eq!(load_entries_from_file(&path.to_string_lossy()).len(), 3);
    }

    #[test]
    fn session_names_collapse_line_breaks_and_labels_require_an_existing_target() {
        let mut manager = SessionManager::in_memory("/tmp", None, None);
        let entry = manager.append_session_info(Some(" first\r\n\nsecond "));
        assert_eq!(entry["name"], "first second");
        assert_eq!(manager.session_name(), Some("first second"));
        assert_eq!(
            manager.append_label("missing", Some("x")),
            Err("Entry missing not found".to_owned())
        );
    }

    #[test]
    fn tree_children_follow_parent_ids() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let mut manager = SessionManager::create(&dir, Some(&dir), None);
        let root =
            manager.append_message(json!({ "role": "user", "content": "a", "timestamp": 1 }));
        let root_id = root["id"].as_str().unwrap_or_default().to_owned();
        manager.set_leaf(Some(&root_id)).expect("leaf");
        let left =
            manager.append_message(json!({ "role": "assistant", "content": [], "timestamp": 2 }));
        manager.set_leaf(Some(&root_id)).expect("leaf");
        let right =
            manager.append_message(json!({ "role": "assistant", "content": [], "timestamp": 3 }));
        let trees = manager.get_tree();
        assert_eq!(trees.len(), 1);
        let tree = &trees[0];
        assert_eq!(tree.entry["id"], root_id);
        assert_eq!(tree.children.len(), 2);
        assert_eq!(tree.children[0].entry["id"], left["id"]);
        assert_eq!(tree.children[1].entry["id"], right["id"]);
        assert_eq!(
            manager.leaf_id(),
            Some(right["id"].as_str().unwrap_or_default())
        );
    }

    #[test]
    fn branching_rejects_unknown_ids_and_summaries_record_the_abandoned_leaf() {
        let mut manager = SessionManager::in_memory("/tmp", None, None);
        let root =
            manager.append_message(json!({ "role": "user", "content": "a", "timestamp": 1 }));
        let root_id = root["id"].as_str().unwrap_or_default().to_owned();
        let leaf =
            manager.append_message(json!({ "role": "user", "content": "b", "timestamp": 2 }));
        assert_eq!(
            manager.set_leaf(Some("missing")),
            Err("Entry missing not found".to_owned())
        );
        assert_eq!(manager.leaf_id(), leaf["id"].as_str());
        assert!(
            manager
                .branch_with_summary(Some("missing"), "s", None, None, None)
                .is_err()
        );
        let summary = manager
            .branch_with_summary(Some(&root_id), "s", None, None, None)
            .expect("summary");
        assert_eq!(summary["fromId"], leaf["id"]);
        assert_eq!(summary["parentId"], json!(root_id));
        manager.reset_leaf();
        let at_root = manager
            .branch_with_summary(None, "t", None, None, None)
            .expect("root summary");
        assert_eq!(at_root["fromId"], "root");
        assert_eq!(at_root["parentId"], Value::Null);
    }

    #[test]
    fn branched_sessions_keep_only_the_leaf_path_and_rechain_labels() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let mut manager = SessionManager::create(&dir, Some(&dir), None);
        let previous = manager.session_file().unwrap_or_default().to_owned();
        let root =
            manager.append_message(json!({ "role": "user", "content": "a", "timestamp": 1 }));
        let root_id = root["id"].as_str().unwrap_or_default().to_owned();
        let label = manager
            .append_label(&root_id, Some("start"))
            .expect("label");
        let answer =
            manager.append_message(json!({ "role": "assistant", "content": [], "timestamp": 2 }));
        let answer_id = answer["id"].as_str().unwrap_or_default().to_owned();
        manager.set_leaf(Some(&root_id)).expect("leaf");
        let abandoned =
            manager.append_message(json!({ "role": "user", "content": "x", "timestamp": 3 }));
        assert!(manager.create_branched_session("missing").is_err());
        let file = manager
            .create_branched_session(&answer_id)
            .expect("branch")
            .expect("file");
        assert_eq!(
            manager.header().expect("header")["parentSession"],
            json!(previous)
        );
        assert!(
            manager
                .entry(abandoned["id"].as_str().unwrap_or_default())
                .is_none()
        );
        assert!(
            manager
                .entry(label["id"].as_str().unwrap_or_default())
                .is_none()
        );
        assert_eq!(
            manager.entry(&answer_id).expect("answer")["parentId"],
            json!(root_id)
        );
        assert_eq!(manager.label(&root_id), Some("start"));
        assert_eq!(load_entries_from_file(&file).len(), 4);

        let mut memory = SessionManager::in_memory("/tmp", None, None);
        let kept = memory.append_message(json!({ "role": "user", "content": "a", "timestamp": 1 }));
        memory.append_message(json!({ "role": "user", "content": "b", "timestamp": 2 }));
        assert_eq!(
            memory.create_branched_session(kept["id"].as_str().unwrap_or_default()),
            Ok(None)
        );
        assert_eq!(memory.entries().len(), 1);
        assert_eq!(memory.header().expect("header").get("parentSession"), None);
    }

    #[test]
    fn branch_queries_follow_the_current_leaf() {
        let mut manager = SessionManager::in_memory("/tmp", None, None);
        let info = manager.append_session_info(Some("n"));
        assert!(!manager.has_context_messages());
        let thinking = manager.append_thinking_level_change("high", None);
        assert!(manager.has_thinking_level_changes());
        let message =
            manager.append_message(json!({ "role": "user", "content": "a", "timestamp": 1 }));
        assert!(manager.has_context_messages());
        manager.append_compaction(
            "s",
            message["id"].as_str().unwrap_or_default(),
            1,
            None,
            None,
            None,
        );
        assert_eq!(manager.count_compactions(), 1);
        assert_eq!(
            manager.children(info["id"].as_str().unwrap_or_default()),
            vec![thinking.clone()]
        );
        assert!(
            manager.entry_order(message["id"].as_str().unwrap_or_default())
                > manager.entry_order(thinking["id"].as_str().unwrap_or_default())
        );
        manager.set_leaf(info["id"].as_str()).expect("leaf");
        assert!(!manager.has_context_messages());
        assert!(!manager.has_thinking_level_changes());
    }

    #[test]
    fn a_reset_leaf_yields_an_empty_current_context() {
        let mut manager = SessionManager::in_memory("/tmp", None, None);
        manager.append_message(json!({ "role": "user", "content": "a", "timestamp": 1 }));
        manager.append_message(json!({ "role": "user", "content": "b", "timestamp": 2 }));
        assert_eq!(manager.current_context().messages.len(), 2);
        manager.reset_leaf();
        assert!(manager.current_context().messages.is_empty());
        assert!(manager.current_branch().is_empty());
        assert!(manager.current_context_entries().is_empty());
    }

    #[test]
    fn configuration_updates_carry_the_reasoning_effort_into_context() {
        let mut manager = SessionManager::in_memory("/tmp", None, None);
        let entry = manager.append_configuration_update("high");
        assert_eq!(entry["reasoning"], json!({ "effort": "high" }));
        let context = manager.current_context();
        assert_eq!(
            context.configuration_update,
            Some(json!({ "effort": "high" }))
        );
        assert_eq!(context.messages[0]["role"], "configurationUpdate");
    }

    #[test]
    fn rejected_model_changes_are_recorded_but_stay_out_of_context() {
        let mut manager = SessionManager::in_memory("/tmp", None, None);
        let mut budget = Map::new();
        budget.insert("contextWindow".to_owned(), Value::from(1000));
        let entry = manager.append_model_change_rejected(
            "p",
            "m",
            "context-budget",
            "too big",
            Some(budget),
        );
        assert_eq!(entry["type"], "model_change_rejected");
        assert_eq!(entry["modelId"], "m");
        assert_eq!(entry["contextWindow"], 1000);
        let context = manager.current_context();
        assert!(context.messages.is_empty());
        assert_eq!(context.model, None);
    }

    #[test]
    fn tree_returns_orphans_as_roots_and_sorts_children_by_timestamp() {
        let entries = vec![
            header("s", Some(3), "/tmp"),
            json!({ "type": "custom", "id": "root", "parentId": null, "timestamp": "2024-01-01T00:00:00.000Z", "customType": "x" }),
            json!({ "type": "custom", "id": "late", "parentId": "root", "timestamp": "2024-01-01T00:00:09.000Z", "customType": "x" }),
            json!({ "type": "custom", "id": "early", "parentId": "root", "timestamp": "2024-01-01T00:00:01.000Z", "customType": "x" }),
            json!({ "type": "custom", "id": "orphan", "parentId": "gone", "timestamp": "2024-01-01T00:00:02.000Z", "customType": "x" }),
        ];
        let manager = SessionManager::in_memory("/tmp", None, Some(entries));
        let trees = manager.get_tree();
        assert_eq!(
            trees
                .iter()
                .map(|node| node.entry["id"].clone())
                .collect::<Vec<_>>(),
            vec![json!("root"), json!("orphan")]
        );
        assert_eq!(
            trees[0]
                .children
                .iter()
                .map(|node| node.entry["id"].clone())
                .collect::<Vec<_>>(),
            vec![json!("early"), json!("late")]
        );
    }

    #[test]
    fn session_ids_are_validated() {
        assert_valid_session_id("abc-123_x.y");
        assert_valid_session_id("a");
        let result = std::panic::catch_unwind(|| assert_valid_session_id("-bad"));
        assert!(result.is_err());
        let result = std::panic::catch_unwind(|| assert_valid_session_id("bad-"));
        assert!(result.is_err());
        let result = std::panic::catch_unwind(|| assert_valid_session_id("bad/id"));
        assert!(result.is_err());
        let result = std::panic::catch_unwind(|| assert_valid_session_id(""));
        assert!(result.is_err());
    }

    #[test]
    fn default_session_dir_encodes_the_cwd() {
        let agent_dir = resolve_path(
            "/home/indo/.omo/agent",
            "/home/indo/.omo/agent",
            &PathInputOptions::default(),
        );
        let expected = Path::new(&agent_dir)
            .join("sessions")
            .join("--home-indo-piratetalk--")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            get_default_session_dir_path("/home/indo/piratetalk", "/home/indo/.omo/agent"),
            expected
        );
    }

    #[test]
    fn find_most_recent_session_filters_by_cwd_and_picks_the_newest() {
        use std::time::{Duration, SystemTime};
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let older = tmp.path().join("a.jsonl");
        let newer = tmp.path().join("b.jsonl");
        let other = tmp.path().join("c.jsonl");
        // Stamp explicit, ordered mtimes from ONE fixed base: "newest" must be decided by the
        // fixture, not by how finely the filesystem distinguishes two back-to-back writes (a
        // sub-millisecond tie made the overall-newest pick depend on readdir order), and never
        // by the wall clock (a clock jump between writes would reorder them).
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let write_at = |path: &std::path::Path, id: &str, cwd: &str, offset: Duration| {
            std::fs::write(
                path,
                format!("{}\n", serialize_entry(&header(id, Some(3), cwd))),
            )
            .expect("write");
            std::fs::File::options()
                .write(true)
                .open(path)
                .expect("open")
                .set_modified(base + offset)
                .expect("mtime");
        };
        write_at(&older, "old", &dir, Duration::from_secs(10));
        write_at(&newer, "new", &dir, Duration::from_secs(20));
        write_at(&other, "other", "/somewhere/else", Duration::from_secs(30));
        assert_eq!(
            find_most_recent_session(&dir, Some(&dir)).as_deref(),
            Some(newer.to_string_lossy().as_ref())
        );
        assert_eq!(
            find_most_recent_session(&dir, None).as_deref(),
            Some(other.to_string_lossy().as_ref())
        );
        assert_eq!(
            find_most_recent_session(&dir, Some("/somewhere/else")).as_deref(),
            Some(other.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn an_empty_existing_file_is_initialized_and_a_corrupt_one_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let empty = tmp.path().join("empty.jsonl");
        std::fs::write(&empty, "").expect("write");
        let manager = SessionManager::open(
            &empty.to_string_lossy(),
            Some(&dir),
            None,
            Some(NewSessionOptions {
                id: Some("kept-id".to_owned()),
                ..Default::default()
            }),
        )
        .expect("open");
        assert_eq!(manager.session_id(), "kept-id");
        assert_eq!(load_entries_from_file(&empty.to_string_lossy()).len(), 1);
        let corrupt = tmp.path().join("corrupt.jsonl");
        std::fs::write(&corrupt, "not json\n").expect("write");
        let error = SessionManager::open(&corrupt.to_string_lossy(), Some(&dir), None, None)
            .expect_err("corrupt session refused");
        assert_eq!(
            error,
            format!(
                "Session file is not a valid {} session: {}",
                app_name(),
                corrupt.to_string_lossy()
            )
        );
        assert_eq!(
            std::fs::read_to_string(&corrupt).expect("unchanged"),
            "not json\n"
        );
    }

    #[test]
    fn load_entries_from_file_rejects_a_file_without_a_header() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("x.jsonl");
        std::fs::write(
            &path,
            format!(
                "{}\n",
                serialize_entry(&json!({ "type": "message", "id": "a" }))
            ),
        )
        .expect("write");
        assert!(load_entries_from_file(&path.to_string_lossy()).is_empty());
        assert!(load_entries_from_file("/definitely/not/here.jsonl").is_empty());
    }

    #[test]
    fn usage_totals_track_the_latest_cache_hit_rate() {
        let mut manager = SessionManager::in_memory("/tmp", None, None);
        assert_eq!(manager.usage_totals().latest_cache_hit_rate, None);
        manager.append_message(json!({ "role": "assistant", "content": [], "usage": { "input": 10, "output": 1, "cacheRead": 30, "cacheWrite": 0 } }));
        assert_eq!(manager.usage_totals().latest_cache_hit_rate, Some(75.0));
        let projected = manager.extension_usage_totals();
        let totals = manager.usage_totals();
        assert_eq!(projected["latestCacheHitRate"], json!(75.0));
        assert_eq!(projected["input"], json!(totals.input));
        assert_eq!(projected["output"], json!(totals.output));
        assert_eq!(projected["cacheRead"], json!(totals.cache_read));
        assert_eq!(projected["cacheWrite"], json!(totals.cache_write));
        assert_eq!(projected["cost"], json!(totals.cost));
        manager.append_message(json!({ "role": "assistant", "content": [], "usage": { "input": 0, "output": 1, "cacheRead": 0, "cacheWrite": 0 } }));
        assert_eq!(manager.usage_totals().latest_cache_hit_rate, None);
        assert_eq!(
            manager.extension_usage_totals()["latestCacheHitRate"],
            Value::Null
        );
    }

    #[test]
    fn every_persisted_write_path_reserves_the_session_file() {
        static RESERVED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let _ = crate::session_write_reservation::install_session_write_reservation(|path| {
            RESERVED
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(path.to_owned());
        });
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let mut manager = SessionManager::create(&dir, Some(&dir), None);
        let session_file = manager.session_file().expect("session file").to_owned();
        let count = |path: &str| {
            RESERVED
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .filter(|reserved| reserved.as_str() == path)
                .count()
        };
        assert_eq!(count(&session_file), 1);
        manager.append_message(json!({ "role": "assistant", "content": [] }));
        let after_flush = count(&session_file);
        assert!(after_flush >= 3);
        manager.append_message(json!({ "role": "user", "content": "next", "timestamp": 2 }));
        assert_eq!(count(&session_file), after_flush + 1);
        let before_open = count(&session_file);
        drop(SessionManager::open(&session_file, Some(&dir), None, None).expect("open"));
        assert!(count(&session_file) >= before_open + 2);
    }

    #[test]
    fn large_strings_stay_resident_out_of_line_but_persist_and_read_in_full() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let mut manager = SessionManager::create(&dir, Some(&dir), None);
        let large = "L".repeat(40 * 1024);
        let entry = manager.append_message(
            json!({ "role": "assistant", "content": [{ "type": "text", "text": large }] }),
        );
        let id = entry["id"].as_str().expect("id").to_owned();
        let resident = manager.file_entries.last().expect("resident entry");
        assert!(
            resident["message"]["content"][0]["text"]
                .as_str()
                .expect("text")
                .starts_with(crate::session_resident_store::RESIDENT_STRING_PREFIX)
        );
        assert_eq!(
            manager.entry(&id).expect("entry")["message"]["content"][0]["text"],
            Value::from(large.clone())
        );
        let persisted = load_entries_from_file(manager.session_file().expect("file"));
        assert_eq!(
            persisted.last().expect("persisted")["message"]["content"][0]["text"],
            Value::from(large)
        );
        assert_eq!(manager.resident_store_stats().blob_count, 1);
    }

    #[test]
    fn compaction_trims_the_resident_mirror_while_full_history_stays_readable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let mut manager = SessionManager::create(&dir, Some(&dir), None);
        manager.append_message(json!({ "role": "user", "content": "old", "timestamp": 1 }));
        manager.append_message(
            json!({ "role": "assistant", "content": [], "provider": "p", "model": "m" }),
        );
        manager.append_thinking_level_change("high", None);
        let kept =
            manager.append_message(json!({ "role": "user", "content": "kept", "timestamp": 2 }));
        let kept_id = kept["id"].as_str().expect("kept id").to_owned();
        manager.append_compaction("digest", &kept_id, 10, None, None, None);
        let after =
            manager.append_message(json!({ "role": "user", "content": "after", "timestamp": 3 }));
        // Pre-compaction messages leave the mirror; non-message settings entries stay.
        assert_eq!(manager.file_entries.len(), 5);
        assert_eq!(manager.entries().len(), 6);
        let first_id = manager.entries()[0]["id"]
            .as_str()
            .expect("first id")
            .to_owned();
        assert_eq!(
            manager.entry(&first_id).expect("full-history entry")["message"]["content"],
            "old"
        );
        let context = manager.build_context(after["id"].as_str());
        assert_eq!(context.messages.len(), 3);
        assert_eq!(context.thinking_level, "high");
        manager
            .set_leaf(Some(&first_id))
            .expect("reload trimmed history");
        assert_eq!(manager.leaf_id(), Some(first_id.as_str()));
        assert_eq!(manager.file_entries.len(), 7);
    }

    #[test]
    fn entries_after_refuses_a_skip_that_predates_a_mirror_trim() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let mut manager = SessionManager::create(&dir, Some(&dir), None);
        for _ in 0..4 {
            manager.append_thinking_level_change("high", None);
        }
        manager.append_message(json!({ "role": "user", "content": "a", "timestamp": 1 }));
        manager.append_message(
            json!({ "role": "assistant", "content": [], "provider": "p", "model": "m" }),
        );
        let skip = manager.get_entry_count();
        manager.append_message(json!({ "role": "user", "content": "dropped", "timestamp": 2 }));
        let kept =
            manager.append_message(json!({ "role": "user", "content": "kept", "timestamp": 3 }));
        manager.append_compaction(
            "digest",
            kept["id"].as_str().expect("kept id"),
            10,
            None,
            None,
            None,
        );
        assert!(manager.entries_after(skip).is_none());
        let after_trim = manager.get_entry_count();
        manager.append_message(json!({ "role": "user", "content": "after", "timestamp": 4 }));
        assert_eq!(
            manager
                .entries_after(after_trim)
                .expect("resident tail")
                .len(),
            1
        );
    }

    #[test]
    fn in_memory_managers_never_touch_disk() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let mut manager = SessionManager::in_memory(&dir, None, None);
        manager.append_message(json!({ "role": "user", "content": "a", "timestamp": 1 }));
        assert!(manager.session_file().is_none());
        assert_eq!(manager.entries().len(), 1);
        assert_eq!(std::fs::read_dir(tmp.path()).expect("read_dir").count(), 0);
    }

    #[test]
    fn has_context_messages_tracks_context_entries_on_the_current_branch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let mut manager = SessionManager::in_memory(&dir, None, None);
        assert!(!manager.has_context_messages());
        manager.append_thinking_level_change("high", None);
        assert!(!manager.has_context_messages());
        manager.append_message(json!({ "role": "user", "content": "a", "timestamp": 1 }));
        assert!(manager.has_context_messages());
    }

    #[test]
    fn session_file_name_id_reads_the_id_a_file_name_carries() {
        assert_eq!(
            session_file_name_id("/s/2026-01-02T03-04-05-678Z_abc-1.jsonl").as_deref(),
            Some("abc-1")
        );
        assert_eq!(
            session_file_name_id("/s/2026-01-02T03-04-05-678Z_-bad.jsonl"),
            None
        );
        assert_eq!(session_file_name_id("/s/notes.jsonl"), None);
        assert_eq!(
            session_file_name_id("/s/2026-01-02T03-04-05-678Z_abc.json"),
            None
        );
    }

    #[test]
    fn creating_a_session_at_a_named_path_adopts_the_file_name_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("2026-01-02T03-04-05-678Z_named-id.jsonl");
        let cwd = tmp.path().to_string_lossy().into_owned();
        let manager =
            SessionManager::open(&path.to_string_lossy(), None, Some(&cwd), None).expect("open");
        assert_eq!(manager.session_id(), "named-id");
        assert!(
            path.exists(),
            "a file-name id is caller-chosen, so the file is written eagerly"
        );
    }

    #[test]
    fn append_owned_message_stores_the_input_source_on_the_entry_only() {
        let mut manager = SessionManager::in_memory("/tmp", None, None);
        let entry = manager.append_owned_message(
            json!({ "role": "user", "content": "hi" }),
            Some(maho_ext_api::InputSource::Extension),
        );
        assert_eq!(entry["inputSource"], "extension");
        assert!(entry["message"].get("inputSource").is_none());
        let plain = manager.append_owned_message(json!({ "role": "user", "content": "hi" }), None);
        assert!(plain.get("inputSource").is_none());
    }

    static COUNT_MESSAGES: crate::session_history_reduce::HistoryReducer<usize> =
        crate::session_history_reduce::HistoryReducer {
            initial: || 0,
            add: |state, entry| {
                *state += usize::from(entry.get("type").and_then(Value::as_str) == Some("message"))
            },
        };

    #[test]
    fn reduce_history_folds_appended_entries_and_restarts_on_a_new_session() {
        let mut manager = SessionManager::in_memory("/tmp", None, None);
        manager.append_message(json!({ "role": "user", "content": "a" }));
        assert_eq!(manager.reduce_history(&COUNT_MESSAGES), 1);
        manager.append_message(json!({ "role": "user", "content": "b" }));
        assert_eq!(manager.reduce_history(&COUNT_MESSAGES), 2);
        manager.new_session(None);
        assert_eq!(manager.reduce_history(&COUNT_MESSAGES), 0);
    }
}

#[cfg(test)]
mod prune_sessions_tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn aged(path: &Path, days: u64) {
        let file = std::fs::OpenOptions::new().write(true).open(path).expect("open");
        file.set_modified(SystemTime::now() - Duration::from_secs(days * 86_400))
            .expect("mtime");
    }

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().expect("root");
        let project = root.path().join("--proj--");
        std::fs::create_dir(&project).expect("project");
        (root, project)
    }

    #[test]
    fn prune_sessions_removes_old_and_keeps_recent_and_current() {
        let (root, project) = fixture();
        let (old, recent, current) = (project.join("old.jsonl"), project.join("recent.jsonl"), project.join("cur.jsonl"));
        for path in [&old, &recent, &current] {
            std::fs::write(path, "{}\n").expect("write");
        }
        aged(&old, 40);
        aged(&current, 40);
        aged(&recent, 1);
        let removed = prune_sessions(root.path(), 30, &current, SystemTime::now()).expect("prune");
        assert_eq!(removed, 1);
        assert!(!old.exists() && recent.exists() && current.exists());
    }

    #[test]
    fn new_session_sidecar_is_locked_from_creation_and_survives_first_persist() {
        let dir = tempfile::tempdir().expect("session dir");
        let dir = dir.path().to_string_lossy();
        let mut manager = SessionManager::create(&dir, Some(&dir), None);
        let file = manager.session_file().expect("session file").to_owned();
        assert!(!Path::new(&file).exists());
        assert!(manager.retention_lock_error().is_none());
        manager.append_message(json!({ "role": "user", "content": "a" }));
        manager.append_message(json!({ "role": "assistant", "content": [] }));
        assert!(Path::new(&file).exists());
        // A separate handle on the sidecar cannot take the lock while the manager lives.
        let probe = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(retention_lock_path(Path::new(&file)))
            .expect("sidecar");
        assert!(probe.try_lock().is_err());
        // The session file itself is never locked, so writers on any platform are unaffected.
        let session = std::fs::OpenOptions::new().read(true).write(true).open(&file).expect("open");
        assert!(session.try_lock().is_ok());
        drop(session);
        drop(manager);
        assert!(probe.try_lock().is_ok());
    }

    #[test]
    fn prune_sessions_keeps_locked_and_non_jsonl_files() {
        let (root, project) = fixture();
        let (locked, text) = (project.join("locked.jsonl"), project.join("notes.txt"));
        std::fs::write(&locked, "{}\n").expect("write");
        std::fs::write(&text, "x").expect("write");
        aged(&locked, 40);
        aged(&text, 40);
        let holder = open_retention_lock(&locked).expect("sidecar lock");
        #[cfg(unix)]
        let sidecar = retention_lock_path(&locked);
        let removed = prune_sessions(root.path(), 30, Path::new("/nonexistent"), SystemTime::now()).expect("prune");
        assert_eq!(removed, 0);
        assert!(locked.exists() && text.exists());
        drop(holder);
        let removed = prune_sessions(root.path(), 30, Path::new("/nonexistent"), SystemTime::now()).expect("prune");
        assert_eq!(removed, 1);
        assert!(!locked.exists() && text.exists());
        #[cfg(unix)]
        assert!(!sidecar.exists(), "sidecar removed with its session");
    }

    #[cfg(unix)]
    #[test]
    fn prune_sessions_collects_only_orphan_sidecars() {
        let (root, project) = fixture();
        let (live, orphan_session) = (project.join("live.jsonl"), project.join("gone.jsonl"));
        std::fs::write(&live, "{}\n").expect("write");
        let live_lock = retention_lock_path(&live);
        std::fs::write(&live_lock, "").expect("sidecar");
        std::fs::write(retention_lock_path(&orphan_session), "").expect("orphan");
        let held = open_retention_lock(&project.join("held.jsonl")).expect("held");
        prune_sessions(root.path(), 30, Path::new("/nonexistent"), SystemTime::now()).expect("prune");
        assert!(live_lock.exists(), "sidecar of an existing session is kept");
        assert!(!retention_lock_path(&orphan_session).exists());
        assert!(held.path.exists(), "locked sidecar is kept");
    }

    #[cfg(unix)]
    #[test]
    fn lock_on_unlinked_sidecar_is_never_trusted() {
        let (_root, project) = fixture();
        let session = project.join("s.jsonl");
        let path = retention_lock_path(&session);
        let orphan = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .expect("sidecar");
        std::fs::remove_file(&path).expect("unlink");
        let replacement = open_retention_lock(&session).expect("new sidecar");
        // The orphan inode locks fine but is reported as not the path's inode.
        assert!(!lock_verified(&orphan, &path).expect("lock orphan"));
        // Locking the replacement through a second handle is contended, not trusted.
        let again = std::fs::OpenOptions::new().read(true).write(true).open(&path).expect("open");
        assert!(lock_verified(&again, &path).is_err());
        drop(replacement);
        // A path that vanished entirely is also a mismatch.
        std::fs::remove_file(&path).expect("unlink");
        assert!(!lock_verified(&again, &path).expect("lock"));
    }

    #[test]
    fn deletion_check_under_lock_skips_freshly_written_files() {
        let (_root, project) = fixture();
        let file = project.join("s.jsonl");
        std::fs::write(&file, "{}\n").expect("write");
        let cutoff = SystemTime::now() - Duration::from_secs(30 * 86_400);
        aged(&file, 40);
        assert!(is_idle_since(&file, cutoff));
        // The session wrote after the initial scan: the mtime is fresh again.
        aged(&file, 1);
        assert!(!is_idle_since(&file, cutoff));
        assert!(!is_idle_since(&project.join("missing.jsonl"), cutoff));
    }

    #[test]
    fn dropping_a_session_leaves_its_sidecar() {
        let dir = tempfile::tempdir().expect("session dir");
        let dir = dir.path().to_string_lossy();
        let manager = SessionManager::create(&dir, Some(&dir), None);
        let sidecar = retention_lock_path(Path::new(manager.session_file().expect("file")));
        assert!(sidecar.exists());
        drop(manager);
        assert!(sidecar.exists());
    }

    #[cfg(unix)]
    #[test]
    fn prune_sessions_does_not_follow_symlinks() {
        let (root, project) = fixture();
        let outside = tempfile::tempdir().expect("outside");
        let target = outside.path().join("victim.jsonl");
        std::fs::write(&target, "{}\n").expect("write");
        aged(&target, 40);
        std::os::unix::fs::symlink(&target, project.join("link.jsonl")).expect("file link");
        std::os::unix::fs::symlink(outside.path(), root.path().join("dirlink")).expect("dir link");
        let removed = prune_sessions(root.path(), 30, Path::new("/nonexistent"), SystemTime::now()).expect("prune");
        assert_eq!(removed, 0);
        assert!(target.exists());
    }

    #[test]
    fn prune_sessions_invalid_retention_value_is_a_no_op() {
        assert_eq!(parse_retention_days(None), Ok(None));
        assert_eq!(parse_retention_days(Some(30.0)), Ok(Some(30)));
        for bad in [-1.0, 0.0, 1.5, f64::NAN, f64::INFINITY] {
            assert!(parse_retention_days(Some(bad)).is_err(), "{bad}");
        }
    }
}
