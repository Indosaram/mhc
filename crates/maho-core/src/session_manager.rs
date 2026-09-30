//! Port of senpi packages/coding-agent/src/core/session-manager.ts.
//!
//! Sessions are append-only JSONL trees. Entries are kept as serde_json::Value so that a file
//! read and re-written is byte-identical (key order preserved, unknown fields kept), which is what
//! makes maho able to read and resume omo/senpi v3 sessions.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

use serde_json::{json, Map, Value};

use crate::config::{app_name, get_agent_dir, get_sessions_dir};
use crate::messages::{create_branch_summary_message, create_compaction_summary_message, create_custom_message};
use crate::paths::{normalize_path, resolve_path, PathInputOptions};

pub const CURRENT_SESSION_VERSION: i64 = 3;

const SESSION_READ_BUFFER_SIZE: usize = 1024 * 1024;
const SESSION_HEADER_READ_BUFFER_SIZE: usize = 4096;
const MAX_SESSION_HEADER_SCAN_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Default)]
pub struct NewSessionOptions {
    pub id: Option<String>,
    pub parent_session: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct UsageTotals {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub cost: f64,
    pub latest_cache_hit_rate: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionContext {
    pub messages: Vec<Value>,
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

pub fn assert_valid_session_id(id: &str) {
    let bytes = id.as_bytes();
    let alnum = |b: u8| b.is_ascii_alphanumeric();
    let valid = !bytes.is_empty()
        && alnum(bytes[0])
        && alnum(bytes[bytes.len() - 1])
        && bytes.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'.' || *b == b'_' || *b == b'-');
    if !valid {
        panic!("Session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character");
    }
}

fn generate_id(by_id: &HashMap<String, Value>) -> String {
    for _ in 0..100 {
        let id = uuid::Uuid::new_v4().to_string()[..8].to_owned();
        if !by_id.contains_key(&id) {
            return id;
        }
    }
    uuid::Uuid::new_v4().to_string()
}

fn entry_type(entry: &Value) -> &str {
    entry.get("type").and_then(Value::as_str).unwrap_or_default()
}

fn migrate_v1_to_v2(entries: &mut [Value]) {
    let mut ids: HashMap<String, Value> = HashMap::new();
    let mut prev_id: Option<String> = None;
    for index in 0..entries.len() {
        if entry_type(&entries[index]) == "session" {
            entries[index]["version"] = Value::from(2);
            continue;
        }
        let id = generate_id(&ids);
        ids.insert(id.clone(), Value::Null);
        entries[index]["id"] = Value::from(id.clone());
        match &prev_id {
            Some(previous) => entries[index]["parentId"] = Value::from(previous.clone()),
            None => entries[index]["parentId"] = Value::Null,
        }
        prev_id = Some(id);

        if entry_type(&entries[index]) == "compaction" {
            let first_kept_index = entries[index].get("firstKeptEntryIndex").and_then(Value::as_i64);
            if let Some(first_kept_index) = first_kept_index {
                if let Some(target) = entries.get(first_kept_index as usize) {
                    if entry_type(target) != "session" {
                        let target_id = target.get("id").cloned().unwrap_or(Value::Null);
                        entries[index]["firstKeptEntryId"] = target_id;
                    }
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
        if entry_type(entry) == "message" {
            if let Some(message) = entry.get_mut("message") {
                if message.get("role").and_then(Value::as_str) == Some("hookMessage") {
                    message["role"] = Value::from("custom");
                }
            }
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
    entries.iter().rev().find(|entry| entry_type(entry) == "compaction").cloned()
}

fn build_entry_index(entries: &[Value]) -> HashMap<String, Value> {
    let mut index = HashMap::new();
    for entry in entries {
        if let Some(id) = entry.get("id").and_then(Value::as_str) {
            index.insert(id.to_owned(), entry.clone());
        }
    }
    index
}

pub fn build_session_path(entries: &[Value], leaf_id: Option<&str>) -> Vec<Value> {
    let index = build_entry_index(entries);
    let mut leaf = match leaf_id {
        None => entries.last().cloned(),
        Some("") => return Vec::new(),
        Some(id) => index.get(id).cloned().or_else(|| entries.last().cloned()),
    };
    let mut path = Vec::new();
    while let Some(current) = leaf {
        let parent_id = current.get("parentId").and_then(Value::as_str).map(str::to_owned);
        path.push(current);
        leaf = parent_id.and_then(|parent| index.get(&parent).cloned());
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

fn get_session_context_settings(path: &[Value]) -> ContextSettings {
    let mut thinking_level = "off".to_owned();
    let mut thinking_selection: Option<Value> = None;
    let mut configuration_update: Option<Value> = None;
    let mut model: Option<(String, String)> = None;
    let mut is_model_selection_explicit = false;
    let mut is_in_fallback_window = false;
    let mut pre_fallback_thinking_level = thinking_level.clone();
    let mut pre_fallback_thinking_selection: Option<Value> = None;

    for entry in path {
        match entry_type(entry) {
            "thinking_level_change" => {
                thinking_level = entry.get("thinkingLevel").and_then(Value::as_str).unwrap_or_default().to_owned();
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
                            let original_provider = entry.get("originalProvider").and_then(Value::as_str);
                            let original_model_id = entry.get("originalModelId").and_then(Value::as_str);
                            if let (Some(provider), Some(model_id)) = (original_provider, original_model_id) {
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
                let is_assistant = message.and_then(|m| m.get("role")).and_then(Value::as_str) == Some("assistant");
                if is_assistant && !is_in_fallback_window {
                    let provider = message.and_then(|m| m.get("provider")).and_then(Value::as_str).unwrap_or_default();
                    if is_model_selection_explicit && model.as_ref().map(|(p, _)| p.as_str()) == Some(normalize_provider(provider).as_str()) {
                        continue;
                    }
                    let model_id = message.and_then(|m| m.get("model")).and_then(Value::as_str).unwrap_or_default();
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

    ContextSettings { thinking_level, thinking_selection, configuration_update, model }
}

pub fn session_entry_to_context_messages(entry: &Value) -> Vec<Value> {
    match entry_type(entry) {
        "message" => {
            let Some(message) = entry.get("message") else { return Vec::new() };
            let role = message.get("role").and_then(Value::as_str).unwrap_or_default();
            if matches!(role, "user" | "assistant" | "toolResult") && message.get("content").is_none_or(Value::is_null) {
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
            let content = entry.get("content").cloned().unwrap_or_else(|| Value::Array(Vec::new()));
            let timestamp = entry.get("timestamp").and_then(Value::as_str).unwrap_or_default();
            vec![create_custom_message(
                entry.get("customType").and_then(Value::as_str).unwrap_or_default(),
                content,
                entry.get("display").and_then(Value::as_bool).unwrap_or(false),
                entry.get("details").cloned(),
                timestamp,
            )]
        }
        "branch_summary" => {
            let summary = entry.get("summary").and_then(Value::as_str).unwrap_or_default();
            if summary.is_empty() {
                return Vec::new();
            }
            vec![create_branch_summary_message(
                summary,
                entry.get("fromId").and_then(Value::as_str).unwrap_or_default(),
                entry.get("timestamp").and_then(Value::as_str).unwrap_or_default(),
            )]
        }
        "compaction" => vec![create_compaction_summary_message(
            entry.get("summary").and_then(Value::as_str).unwrap_or_default(),
            entry.get("tokensBefore").and_then(Value::as_i64).unwrap_or(0),
            entry.get("timestamp").and_then(Value::as_str).unwrap_or_default(),
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
    let path = build_session_path(entries, leaf_id);
    let mut compaction: Option<Value> = None;
    for entry in &path {
        if entry_type(entry) == "compaction" {
            compaction = Some(entry.clone());
        }
    }
    let Some(compaction) = compaction else { return path };
    let Some(compaction_index) = path.iter().position(|entry| entry.get("id") == compaction.get("id")) else {
        return path;
    };
    let first_kept_entry_id = compaction.get("firstKeptEntryId").and_then(Value::as_str).unwrap_or_default().to_owned();
    let mut context_entries = vec![compaction];
    let mut found_first_kept = false;
    for entry in path.iter().take(compaction_index) {
        if entry_type(entry) == "compaction" {
            continue;
        }
        if entry.get("id").and_then(Value::as_str) == Some(first_kept_entry_id.as_str()) {
            found_first_kept = true;
        }
        if found_first_kept {
            context_entries.push(entry.clone());
        }
    }
    context_entries.extend(path.iter().skip(compaction_index + 1).cloned());
    context_entries
}

pub fn build_session_context(entries: &[Value], leaf_id: Option<&str>) -> SessionContext {
    let path = build_session_path(entries, leaf_id);
    let settings = get_session_context_settings(&path);
    let messages = build_context_entries(entries, leaf_id)
        .iter()
        .flat_map(session_entry_to_context_messages)
        .collect();
    SessionContext {
        messages,
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
    let safe_path = format!("--{}--", resolved_cwd.trim_start_matches(['/', '\\']).replace(['/', '\\', ':'], "-"));
    Path::new(&resolved_agent_dir).join("sessions").join(safe_path).to_string_lossy().into_owned()
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
    let Ok(file) = std::fs::File::open(&resolved) else { return Vec::new() };
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
    let Some(header) = entries.first() else { return entries };
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
    let Some(entry) = parse_session_entry_line(line) else { return None };
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
        let Some(Some(header)) = header_candidate_file(&path_string) else { continue };
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
        let modified = std::fs::metadata(&path).and_then(|metadata| metadata.modified()).ok();
        if let Some(modified) = modified {
            candidates.push((modified, path_string));
        }
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    candidates.into_iter().next().map(|(_, path)| path)
}

fn header_candidate_file(path: &str) -> Option<Option<Value>> {
    read_session_header(path).map(Some)
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
    by_id: HashMap<String, usize>,
    labels_by_id: HashMap<String, String>,
    label_timestamps_by_id: HashMap<String, String>,
    leaf_id: Option<String>,
    session_name_cache: Option<String>,
    usage_totals: UsageTotals,
}

impl SessionManager {
    fn build(cwd: &str, session_dir: &str, session_file: Option<String>, persist: bool, options: Option<NewSessionOptions>) -> Self {
        let mut manager = Self {
            session_id: String::new(),
            session_file: None,
            session_dir: normalize_path(session_dir, &PathInputOptions::default()),
            cwd: resolve_path(cwd, cwd, &PathInputOptions::default()),
            persist,
            flushed: false,
            file_entries: Vec::new(),
            by_id: HashMap::new(),
            labels_by_id: HashMap::new(),
            label_timestamps_by_id: HashMap::new(),
            leaf_id: None,
            session_name_cache: None,
            usage_totals: UsageTotals::default(),
        };
        if persist && !manager.session_dir.is_empty() {
            let _ = std::fs::create_dir_all(&manager.session_dir);
        }
        match session_file {
            Some(session_file) => manager.set_session_file(&session_file, options),
            None => {
                manager.new_session(options);
            }
        }
        manager
    }

    pub fn create(cwd: &str, session_dir: Option<&str>, options: Option<NewSessionOptions>) -> Self {
        let dir = session_dir.map(str::to_owned).unwrap_or_else(|| get_default_session_dir(cwd));
        Self::build(cwd, &dir, None, true, options)
    }

    pub fn open(path: &str, session_dir: Option<&str>, cwd_override: Option<&str>, options: Option<NewSessionOptions>) -> Self {
        let entries = load_entries_from_file(path);
        let header_cwd = entries.first().and_then(|header| header.get("cwd")).and_then(Value::as_str).map(str::to_owned);
        let cwd = cwd_override.map(str::to_owned).or(header_cwd).unwrap_or_else(|| ".".to_owned());
        let dir = session_dir.map(str::to_owned).unwrap_or_else(|| get_default_session_dir(&cwd));
        let mut manager = Self::build(&cwd, &dir, Some(path.to_owned()), true, options);
        if !entries.is_empty() {
            manager.file_entries = entries;
            manager.session_id = manager
                .file_entries
                .first()
                .and_then(|header| header.get("id"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(create_session_id);
            if migrate_to_current_version(&mut manager.file_entries) {
                manager.rewrite_file();
            }
            manager.build_index();
            manager.flushed = true;
        }
        manager
    }

    pub fn continue_recent(cwd: &str, session_dir: Option<&str>) -> Self {
        let dir = session_dir.map(str::to_owned).unwrap_or_else(|| get_default_session_dir(cwd));
        match find_most_recent_session(&dir, Some(cwd)) {
            Some(path) => Self::open(&path, Some(&dir), None, None),
            None => Self::create(cwd, Some(&dir), None),
        }
    }

    pub fn in_memory(cwd: &str, options: Option<NewSessionOptions>, entries: Option<Vec<Value>>) -> Self {
        let mut manager = Self::build(cwd, "", None, false, options);
        if let Some(entries) = entries {
            if !entries.is_empty() {
                let header = entries.iter().find(|entry| entry_type(entry) == "session").cloned();
                if let Some(header) = header {
                    manager.file_entries = entries;
                    manager.session_id = header.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
                    migrate_to_current_version(&mut manager.file_entries);
                } else {
                    manager.new_session(None);
                    manager.file_entries.extend(entries);
                }
                manager.build_index();
            }
        }
        manager
    }

    pub fn reload_from_disk(&mut self) {
        let Some(session_file) = self.session_file.clone() else { return };
        if !Path::new(&session_file).exists() {
            return;
        }
        let file_entries = load_entries_from_file(&session_file);
        if file_entries.is_empty() {
            return;
        }
        self.set_session_file(&session_file, None);
    }

    pub fn set_session_file(&mut self, session_file: &str, options: Option<NewSessionOptions>) {
        self.session_file = Some(resolve_path(session_file, session_file, &PathInputOptions::default()));
        let Some(current) = self.session_file.clone() else { return };
        if Path::new(&current).exists() {
            let entries = load_entries_from_file(&current);
            if entries.is_empty() {
                let size = std::fs::metadata(&current).map(|metadata| metadata.len()).unwrap_or(0);
                if size > 0 {
                    panic!("Session file is not a valid {} session: {current}", app_name());
                }
                self.reset_to_new_session(options);
                self.rewrite_file();
                self.flushed = true;
                return;
            }
            self.file_entries = entries;
            self.session_id = self
                .file_entries
                .iter()
                .find(|entry| entry_type(entry) == "session")
                .and_then(|header| header.get("id"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(create_session_id);
            if migrate_to_current_version(&mut self.file_entries) {
                self.rewrite_file();
            }
            self.build_index();
            self.flushed = true;
        } else {
            let explicit_id = options.as_ref().and_then(|options| options.id.clone());
            self.reset_to_new_session(options);
            if explicit_id.is_some() {
                self.rewrite_file();
                self.flushed = true;
            }
        }
    }

    pub fn new_session(&mut self, options: Option<NewSessionOptions>) -> Option<String> {
        let timestamp = self.reset_to_new_session(options);
        if self.persist {
            let file_timestamp = timestamp.replace([':', '.'], "-");
            let path = Path::new(&self.session_dir).join(format!("{file_timestamp}_{}.jsonl", self.session_id));
            self.session_file = Some(path.to_string_lossy().into_owned());
        }
        self.session_file.clone()
    }

    fn reset_to_new_session(&mut self, options: Option<NewSessionOptions>) -> String {
        if let Some(id) = options.as_ref().and_then(|options| options.id.as_deref()) {
            assert_valid_session_id(id);
        }
        self.session_id = options.as_ref().and_then(|options| options.id.clone()).unwrap_or_else(create_session_id);
        let timestamp = now_iso_millis();
        let mut header = Map::new();
        header.insert("type".to_owned(), Value::from("session"));
        header.insert("version".to_owned(), Value::from(CURRENT_SESSION_VERSION));
        header.insert("id".to_owned(), Value::from(self.session_id.clone()));
        header.insert("timestamp".to_owned(), Value::from(timestamp.clone()));
        header.insert("cwd".to_owned(), Value::from(self.cwd.clone()));
        if let Some(parent_session) = options.as_ref().and_then(|options| options.parent_session.clone()) {
            header.insert("parentSession".to_owned(), Value::from(parent_session));
        }
        self.file_entries = vec![Value::Object(header)];
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
        self.by_id.clear();
        self.labels_by_id.clear();
        self.label_timestamps_by_id.clear();
        self.leaf_id = None;
        self.session_name_cache = None;
        self.usage_totals = UsageTotals::default();
        let entries = self.file_entries.clone();
        for (order, entry) in entries.iter().enumerate() {
            if entry_type(entry) == "session" {
                continue;
            }
            let Some(id) = entry.get("id").and_then(Value::as_str).map(str::to_owned) else { continue };
            self.by_id.insert(id.clone(), order);
            self.leaf_id = Some(id.clone());
            self.accumulate_usage(entry);
            if entry_type(entry) == "session_info" {
                self.session_name_cache = entry.get("name").and_then(Value::as_str).map(str::trim).filter(|name| !name.is_empty()).map(str::to_owned);
            }
            if entry_type(entry) == "label" {
                let target = entry.get("targetId").and_then(Value::as_str).unwrap_or_default().to_owned();
                match entry.get("label").and_then(Value::as_str) {
                    Some(label) => {
                        self.labels_by_id.insert(target.clone(), label.to_owned());
                        if let Some(timestamp) = entry.get("timestamp").and_then(Value::as_str) {
                            self.label_timestamps_by_id.insert(target, timestamp.to_owned());
                        }
                    }
                    None => {
                        self.labels_by_id.remove(&target);
                        self.label_timestamps_by_id.remove(&target);
                    }
                }
            }
        }
    }

    fn accumulate_usage(&mut self, entry: &Value) {
        if entry_type(entry) != "message" {
            return;
        }
        let Some(message) = entry.get("message") else { return };
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            return;
        }
        let Some(usage) = message.get("usage") else { return };
        self.usage_totals.input += usage.get("input").and_then(Value::as_i64).unwrap_or(0);
        self.usage_totals.output += usage.get("output").and_then(Value::as_i64).unwrap_or(0);
        self.usage_totals.cache_read += usage.get("cacheRead").and_then(Value::as_i64).unwrap_or(0);
        self.usage_totals.cache_write += usage.get("cacheWrite").and_then(Value::as_i64).unwrap_or(0);
        self.usage_totals.cost += usage.get("cost").and_then(|cost| cost.get("total")).and_then(Value::as_f64).unwrap_or(0.0);
    }

    fn rewrite_file(&mut self) {
        if !self.persist {
            return;
        }
        let Some(session_file) = self.session_file.clone() else { return };
        let mut content = String::new();
        for entry in &self.file_entries {
            content.push_str(&serialize_entry(entry));
            content.push('\n');
        }
        let _ = std::fs::write(&session_file, content);
    }

    fn persist_entry(&mut self, entry: &Value) {
        if !self.persist {
            return;
        }
        let Some(session_file) = self.session_file.clone() else { return };
        let has_assistant = self.file_entries.iter().any(|candidate| {
            entry_type(candidate) == "message"
                && candidate.get("message").and_then(|message| message.get("role")).and_then(Value::as_str) == Some("assistant")
        });
        if !has_assistant {
            if self.flushed {
                append_line(&session_file, entry);
            }
            return;
        }
        if !self.flushed {
            self.rewrite_file();
            self.flushed = true;
        } else {
            append_line(&session_file, entry);
        }
    }

    fn append_entry(&mut self, entry: Value) {
        let id = entry.get("id").and_then(Value::as_str).map(str::to_owned);
        self.file_entries.push(entry.clone());
        if let Some(id) = id {
            self.by_id.insert(id.clone(), self.file_entries.len() - 1);
            self.leaf_id = Some(id);
        }
        self.accumulate_usage(&entry);
        if entry_type(&entry) == "session_info" {
            self.session_name_cache = entry.get("name").and_then(Value::as_str).map(str::trim).filter(|name| !name.is_empty()).map(str::to_owned);
        }
        if entry_type(&entry) == "label" {
            let target = entry.get("targetId").and_then(Value::as_str).unwrap_or_default().to_owned();
            match entry.get("label").and_then(Value::as_str) {
                Some(label) => {
                    self.labels_by_id.insert(target.clone(), label.to_owned());
                    if let Some(timestamp) = entry.get("timestamp").and_then(Value::as_str) {
                        self.label_timestamps_by_id.insert(target, timestamp.to_owned());
                    }
                }
                None => {
                    self.labels_by_id.remove(&target);
                    self.label_timestamps_by_id.remove(&target);
                }
            }
        }
        self.persist_entry(&entry);
    }

    pub fn append_entry_raw(&mut self, entry: Value) {
        self.append_entry(entry);
    }

    fn next_entry_id(&self) -> String {
        let existing: HashMap<String, Value> = self.by_id.keys().map(|id| (id.clone(), Value::Null)).collect();
        generate_id(&existing)
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

    pub fn append_thinking_level_change(&mut self, thinking_level: &str, thinking_selection: Option<Value>) -> Value {
        let mut entry = self.base_entry("thinking_level_change");
        entry.insert("thinkingLevel".to_owned(), Value::from(thinking_level));
        if let Some(selection) = thinking_selection {
            entry.insert("thinkingSelection".to_owned(), selection);
        }
        let entry = Value::Object(entry);
        self.append_entry(entry.clone());
        entry
    }

    pub fn append_model_change(&mut self, provider: &str, model_id: &str, reason: Option<&str>, original: Option<(&str, &str)>) -> Value {
        let mut entry = self.base_entry("model_change");
        entry.insert("provider".to_owned(), Value::from(provider));
        entry.insert("modelId".to_owned(), Value::from(model_id));
        if let Some(reason) = reason {
            entry.insert("reason".to_owned(), Value::from(reason));
        }
        if let Some((original_provider, original_model_id)) = original {
            entry.insert("originalProvider".to_owned(), Value::from(original_provider));
            entry.insert("originalModelId".to_owned(), Value::from(original_model_id));
        }
        let entry = Value::Object(entry);
        self.append_entry(entry.clone());
        entry
    }

    pub fn append_compaction(&mut self, summary: &str, first_kept_entry_id: &str, tokens_before: i64, details: Option<Value>, usage: Option<Value>, from_hook: Option<bool>) -> Value {
        let mut entry = self.base_entry("compaction");
        entry.insert("summary".to_owned(), Value::from(summary));
        entry.insert("firstKeptEntryId".to_owned(), Value::from(first_kept_entry_id));
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
        entry
    }

    pub fn append_branch_summary(&mut self, from_id: &str, summary: &str, details: Option<Value>, usage: Option<Value>, from_hook: Option<bool>) -> Value {
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

    pub fn append_custom_message(&mut self, custom_type: &str, content: Value, display: bool, details: Option<Value>) -> Value {
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

    pub fn append_label(&mut self, target_id: &str, label: Option<&str>) -> Value {
        let mut entry = self.base_entry("label");
        entry.insert("targetId".to_owned(), Value::from(target_id));
        match label {
            Some(label) => entry.insert("label".to_owned(), Value::from(label)),
            None => entry.insert("label".to_owned(), Value::Null),
        };
        let entry = Value::Object(entry);
        self.append_entry(entry.clone());
        entry
    }

    pub fn append_session_info(&mut self, name: Option<&str>) -> Value {
        let mut entry = self.base_entry("session_info");
        if let Some(name) = name {
            entry.insert("name".to_owned(), Value::from(name));
        }
        let entry = Value::Object(entry);
        self.append_entry(entry.clone());
        entry
    }

    pub fn cwd(&self) -> &str {
        &self.cwd
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

    pub fn header(&self) -> Option<Value> {
        self.file_entries.iter().find(|entry| entry_type(entry) == "session").cloned()
    }

    pub fn entries(&self) -> Vec<Value> {
        self.file_entries.iter().filter(|entry| entry_type(entry) != "session").cloned().collect()
    }

    pub fn entry(&self, id: &str) -> Option<Value> {
        self.by_id.get(id).and_then(|order| self.file_entries.get(*order)).cloned()
    }

    pub fn leaf_entry(&self) -> Option<Value> {
        self.leaf_id.as_deref().and_then(|id| self.entry(id))
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

    pub fn branch(&self, leaf_id: Option<&str>) -> Vec<Value> {
        build_session_path(&self.entries(), leaf_id)
    }

    pub fn context_entries(&self, leaf_id: Option<&str>) -> Vec<Value> {
        build_context_entries(&self.entries(), leaf_id)
    }

    pub fn build_context(&self, leaf_id: Option<&str>) -> SessionContext {
        build_session_context(&self.entries(), leaf_id)
    }

    pub fn set_leaf(&mut self, leaf_id: Option<&str>) {
        match leaf_id {
            Some(id) => {
                if self.by_id.contains_key(id) {
                    self.leaf_id = Some(id.to_owned());
                }
            }
            None => self.leaf_id = None,
        }
    }

    pub fn get_tree(&self, leaf_id: Option<&str>) -> Option<SessionTreeNode> {
        let entries = self.entries();
        let index = build_entry_index(&entries);
        let root_id = match leaf_id {
            Some(id) => id.to_owned(),
            None => entries.first().and_then(|entry| entry.get("id")).and_then(Value::as_str)?.to_owned(),
        };
        let mut children_by_parent: HashMap<String, Vec<String>> = HashMap::new();
        for entry in &entries {
            if let Some(parent) = entry.get("parentId").and_then(Value::as_str) {
                if let Some(id) = entry.get("id").and_then(Value::as_str) {
                    children_by_parent.entry(parent.to_owned()).or_default().push(id.to_owned());
                }
            }
        }
        fn build(id: &str, index: &HashMap<String, Value>, children: &HashMap<String, Vec<String>>, labels: &HashMap<String, String>, timestamps: &HashMap<String, String>) -> Option<SessionTreeNode> {
            let entry = index.get(id)?.clone();
            let child_nodes = children
                .get(id)
                .map(|ids| ids.iter().filter_map(|child| build(child, index, children, labels, timestamps)).collect())
                .unwrap_or_default();
            Some(SessionTreeNode {
                entry,
                children: child_nodes,
                label: labels.get(id).cloned(),
                label_timestamp: timestamps.get(id).cloned(),
            })
        }
        build(&root_id, &index, &children_by_parent, &self.labels_by_id, &self.label_timestamps_by_id)
    }
}

pub fn now_iso_millis() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn serialize_entry(entry: &Value) -> String {
    serde_json::to_string(entry).unwrap_or_else(|_| "null".to_owned())
}

fn append_line(session_file: &str, entry: &Value) {
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(session_file) {
        let _ = file.write_all(serialize_entry(entry).as_bytes());
        let _ = file.write_all(b"\n");
    }
}

pub fn sessions_dir() -> String {
    get_sessions_dir()
}

pub fn seek_end_of_file(file: &mut std::fs::File) -> std::io::Result<u64> {
    file.seek(SeekFrom::End(0))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let reserialized: String = entries.iter().map(|entry| format!("{}\n", serialize_entry(entry))).collect();
        assert_eq!(reserialized, content);
    }

    #[test]
    fn round_trips_through_the_manager_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let mut manager = SessionManager::create(&dir, Some(&dir), Some(NewSessionOptions { id: Some("sess-1".to_owned()), ..Default::default() }));
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
        let reserialized: String = reloaded.iter().map(|entry| format!("{}\n", serialize_entry(entry))).collect();
        assert_eq!(reserialized, first_read);
        assert_eq!(reloaded.len(), 3);
        assert_eq!(reloaded[0]["id"], "sess-1");
        assert_eq!(reloaded[2]["id"], assistant["id"]);
        let totals = manager.usage_totals();
        assert_eq!((totals.input, totals.output, totals.cache_read, totals.cache_write), (3, 4, 5, 6));
        assert!((totals.cost - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn buffers_entries_until_the_first_assistant_message() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let mut manager = SessionManager::create(&dir, Some(&dir), None);
        manager.append_message(json!({ "role": "user", "content": "hi", "timestamp": 1 }));
        let session_file = manager.session_file().expect("session file").to_owned();
        assert!(!Path::new(&session_file).exists());
        manager.append_message(json!({ "role": "assistant", "content": [], "stopReason": "stop", "timestamp": 2 }));
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
        let manager = SessionManager::open(&path.to_string_lossy(), Some(&tmp.path().to_string_lossy()), None, None);
        assert_eq!(manager.header().expect("header")["version"], 3);
        let rewritten = std::fs::read_to_string(&path).expect("read");
        assert!(rewritten.contains("\"version\":3"));
        assert!(rewritten.contains("\"role\":\"custom\""));
        assert_eq!(manager.session_id(), "sess");
        assert_eq!(manager.cwd(), "/w");
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
        assert_eq!(context.model, Some(("anthropic".to_owned(), "claude".to_owned())));
        let branch = build_context_entries(&entries, Some("b"));
        assert_eq!(branch.len(), 2);
        assert_eq!(branch[0]["id"], "a");
        assert!(build_context_entries(&entries, Some("")).is_empty());
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
        assert_eq!(context.model, Some(("anthropic".to_owned(), "opus".to_owned())));
        assert_eq!(context.thinking_level, "high");
        let mut reverted = entries.clone();
        reverted.push(json!({ "type": "model_change", "id": "e", "parentId": "d", "timestamp": "t", "provider": "anthropic", "modelId": "opus", "reason": "fallback-revert" }));
        assert_eq!(build_session_context(&reverted, None).thinking_level, "high");
    }

    #[test]
    fn an_assistant_message_from_another_provider_restores_the_model() {
        let entries = vec![
            json!({ "type": "model_change", "id": "a", "parentId": null, "timestamp": "t", "provider": "anthropic", "modelId": "opus" }),
            json!({ "type": "message", "id": "b", "parentId": "a", "timestamp": "t", "message": { "role": "assistant", "content": [], "provider": "google", "model": "flash", "stopReason": "stop" } }),
            json!({ "type": "message", "id": "c", "parentId": "b", "timestamp": "t", "message": { "role": "assistant", "content": [], "provider": "anthropic", "model": "opus-wire", "stopReason": "stop" } }),
        ];
        assert_eq!(build_session_context(&entries, None).model, Some(("anthropic".to_owned(), "opus-wire".to_owned())));
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
        let first = manager.append_message(json!({ "role": "user", "content": "a", "timestamp": 1 }));
        manager.append_label(first["id"].as_str().unwrap_or_default(), Some("start"));
        assert_eq!(manager.label(first["id"].as_str().unwrap_or_default()), Some("start"));
        manager.append_label(first["id"].as_str().unwrap_or_default(), None);
        assert_eq!(manager.label(first["id"].as_str().unwrap_or_default()), None);
        manager.append_session_info(Some("  named  "));
        assert_eq!(manager.session_name(), Some("named"));
        manager.append_session_info(Some("   "));
        assert_eq!(manager.session_name(), None);
    }

    #[test]
    fn tree_children_follow_parent_ids() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let mut manager = SessionManager::create(&dir, Some(&dir), None);
        let root = manager.append_message(json!({ "role": "user", "content": "a", "timestamp": 1 }));
        let root_id = root["id"].as_str().unwrap_or_default().to_owned();
        manager.set_leaf(Some(&root_id));
        let left = manager.append_message(json!({ "role": "assistant", "content": [], "timestamp": 2 }));
        manager.set_leaf(Some(&root_id));
        let right = manager.append_message(json!({ "role": "assistant", "content": [], "timestamp": 3 }));
        let tree = manager.get_tree(None).expect("tree");
        assert_eq!(tree.entry["id"], root_id);
        assert_eq!(tree.children.len(), 2);
        assert_eq!(tree.children[0].entry["id"], left["id"]);
        assert_eq!(tree.children[1].entry["id"], right["id"]);
        assert_eq!(manager.leaf_id(), Some(right["id"].as_str().unwrap_or_default()));
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
        assert_eq!(get_default_session_dir_path("/home/indo/piratetalk", "/home/indo/.maho/agent"), "/home/indo/.maho/agent/sessions/--home-indo-piratetalk--");
    }

    #[test]
    fn find_most_recent_session_filters_by_cwd_and_picks_the_newest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let older = tmp.path().join("a.jsonl");
        let newer = tmp.path().join("b.jsonl");
        let other = tmp.path().join("c.jsonl");
        std::fs::write(&older, format!("{}\n", serialize_entry(&header("old", Some(3), &dir)))).expect("write");
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&newer, format!("{}\n", serialize_entry(&header("new", Some(3), &dir)))).expect("write");
        std::fs::write(&other, format!("{}\n", serialize_entry(&header("other", Some(3), "/somewhere/else")))).expect("write");
        assert_eq!(find_most_recent_session(&dir, Some(&dir)).as_deref(), Some(newer.to_string_lossy().as_ref()));
        assert_eq!(find_most_recent_session(&dir, None).as_deref(), Some(other.to_string_lossy().as_ref()));
        assert_eq!(find_most_recent_session(&dir, Some("/somewhere/else")).as_deref(), Some(other.to_string_lossy().as_ref()));
    }

    #[test]
    fn an_empty_existing_file_is_initialized_and_a_corrupt_one_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let empty = tmp.path().join("empty.jsonl");
        std::fs::write(&empty, "").expect("write");
        let manager = SessionManager::open(&empty.to_string_lossy(), Some(&dir), None, Some(NewSessionOptions { id: Some("kept-id".to_owned()), ..Default::default() }));
        assert_eq!(manager.session_id(), "kept-id");
        assert_eq!(load_entries_from_file(&empty.to_string_lossy()).len(), 1);
        let corrupt = tmp.path().join("corrupt.jsonl");
        std::fs::write(&corrupt, "not json\n").expect("write");
        let result = std::panic::catch_unwind(|| SessionManager::open(&corrupt.to_string_lossy(), Some(&dir), None, None));
        assert!(result.is_err());
    }

    #[test]
    fn load_entries_from_file_rejects_a_file_without_a_header() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("x.jsonl");
        std::fs::write(&path, format!("{}\n", serialize_entry(&json!({ "type": "message", "id": "a" })))).expect("write");
        assert!(load_entries_from_file(&path.to_string_lossy()).is_empty());
        assert!(load_entries_from_file("/definitely/not/here.jsonl").is_empty());
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
}
