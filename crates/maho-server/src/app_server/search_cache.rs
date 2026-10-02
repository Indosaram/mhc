use serde_json::{Value,json};
use std::{collections::VecDeque,path::Path};

#[derive(Clone,Debug)]
pub struct SearchSessionRecord {pub thread: Value,pub recency_at: String,pub searchable_text: String}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct SearchCacheStats {pub hits: usize,pub misses: usize,pub entries: usize}
pub struct ThreadSearchCache {
    entries: VecDeque<(String,i64,SearchSessionRecord)>,max_entries: usize,hits: usize,misses: usize,
}
impl Default for ThreadSearchCache {fn default() -> Self {Self::new(512)}}
impl ThreadSearchCache {
    pub fn new(max_entries: usize) -> Self {Self {entries:VecDeque::new(),max_entries:max_entries.max(1),hits:0,misses:0}}
    pub fn stats(&self) -> SearchCacheStats {SearchCacheStats {hits:self.hits,misses:self.misses,entries:self.entries.len()}}
    pub async fn load(&mut self,directory: &Path) -> std::io::Result<Vec<SearchSessionRecord>> {
        let mut names = match tokio::fs::read_dir(directory).await {Ok(names)=>names,Err(error) if error.kind() == std::io::ErrorKind::NotFound=>return Ok(Vec::new()),Err(error)=>return Err(error)};
        let mut records = Vec::new();
        while let Some(entry) = names.next_entry().await? {if entry.file_name().to_string_lossy().ends_with(".jsonl") && let Some(record) = self.load_file(&entry.path()).await? {records.push(record);}}
        Ok(records)
    }
    pub async fn load_file(&mut self,path: &Path) -> std::io::Result<Option<SearchSessionRecord>> {
        let key = path.display().to_string();
        let modified = match tokio::fs::metadata(path).await {Ok(metadata)=>metadata.modified()?,Err(error) if error.kind() == std::io::ErrorKind::NotFound=>{self.entries.retain(|(name,_,_)|name != &key);return Ok(None)},Err(error)=>return Err(error)};
        let mtime = chrono::DateTime::<chrono::Utc>::from(modified).timestamp_millis();
        if let Some(index) = self.entries.iter().position(|(name,time,_)|name == &key && *time == mtime) {
            self.hits += 1;
            if let Some(entry) = self.entries.remove(index) {let record = entry.2.clone();self.entries.push_back(entry);return Ok(Some(record));}
        }
        self.misses += 1;
        let contents = match tokio::fs::read_to_string(path).await {Ok(contents)=>contents,Err(error) if error.kind() == std::io::ErrorKind::NotFound=>{self.entries.retain(|(name,_,_)|name != &key);return Ok(None)},Err(error)=>return Err(error)};
        let record = parse_search_session(&key,mtime,&contents);
        self.entries.retain(|(name,_,_)|name != &key);
        if let Some(record) = &record {self.entries.push_back((key,mtime,record.clone()));while self.entries.len() > self.max_entries {self.entries.pop_front();}}
        Ok(record)
    }
}
fn timestamp(value: &Value,fallback: i64) -> i64 {value.as_i64().or_else(||value.as_str().and_then(|value|chrono::DateTime::parse_from_rfc3339(value).ok()).map(|value|value.timestamp_millis())).unwrap_or(fallback)}
fn iso(value: i64) -> Option<String> {chrono::DateTime::from_timestamp_millis(value).map(|time|time.to_rfc3339_opts(chrono::SecondsFormat::Millis,true))}
pub fn parse_search_session(path: &str,mtime: i64,contents: &str) -> Option<SearchSessionRecord> {
    let mut header = None;let mut name = None;let mut preview = None;let mut activity = mtime;let mut recency = mtime;let mut messages = Vec::new();
    for line in contents.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {continue};if !entry.is_object() {continue;}
        if header.is_none() {
            if entry["type"] != "session" || entry["id"].as_str().is_none_or(str::is_empty) || !entry["cwd"].is_string() || !entry["timestamp"].is_string() {return None;}
            activity = timestamp(&entry["timestamp"],mtime);recency = activity;header = Some(entry);continue;
        }
        if entry["type"] == "session_info" {name = entry["name"].as_str().map(str::trim).filter(|name|!name.is_empty()).map(str::to_owned);continue;}
        let message = &entry["message"];
        if entry["type"] != "message" || !matches!(message["role"].as_str(),Some("user"|"assistant")) {continue;}
        let time = timestamp(&entry["timestamp"],mtime);activity = activity.max(time);if message["role"] == "user" {recency = recency.max(time);}
        let text = match &message["content"] {Value::String(text)=>text.clone(),Value::Array(blocks)=>blocks.iter().filter(|block|block["type"] == "text").filter_map(|block|block["text"].as_str()).filter(|text|!text.is_empty()).collect::<Vec<_>>().join(" "),_=>String::new()};
        if text.is_empty() {continue;}
        if message["role"] == "user" && preview.is_none() {preview = Some(text.clone());}messages.push(text);
    }
    let header = header?;
    Some(SearchSessionRecord {thread:json!({"id":header["id"],"sessionId":header["id"],"sessionPath":path,"cwd":header["cwd"],"createdAt":iso(timestamp(&header["timestamp"],mtime))?,"updatedAt":iso(activity)?,"status":{"type":"notLoaded"},"preview":preview,"name":name}),recency_at:iso(recency)?,searchable_text:messages.join(" ")})
}
