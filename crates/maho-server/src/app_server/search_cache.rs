use serde_json::{Value,json};
use std::{collections::VecDeque,path::Path,sync::Arc};

#[derive(Clone,Debug)]
pub struct SearchSessionRecord {pub thread: Value,pub recency_at: String,pub searchable_text: String,
    /// `to_locale_lowercase(searchable_text)` computed once per record for non-ASCII text (`None` when ASCII),
    /// so `thread/search` does not re-lowercase every transcript on every request.
    pub searchable_lower: Option<String>}
impl SearchSessionRecord {
    pub fn lowered(text: &str) -> Option<String> {(!text.is_ascii()).then(||super::js_semantics::to_locale_lowercase(text))}
    fn bytes(&self) -> usize {self.searchable_text.len()+self.searchable_lower.as_ref().map_or(0,String::len)}
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct SearchCacheStats {pub hits: usize,pub misses: usize,pub entries: usize}
pub struct ThreadSearchCache {
    entries: VecDeque<(String,std::time::SystemTime,Arc<SearchSessionRecord>)>,max_entries: usize,max_text_bytes: usize,text_bytes: usize,hits: usize,misses: usize,
}
/// Total `searchable_text` budget, the same as maho-core's session summary cache, so a normal
/// session set stays resident instead of thrashing on every `thread/search` walk.
pub const SEARCH_CACHE_MAX_TEXT_BYTES: usize = maho_core::session_summary_cache::SESSION_SUMMARY_CACHE_MAX_TEXT_BYTES;
impl Default for ThreadSearchCache {fn default() -> Self {Self::with_budget(usize::MAX,SEARCH_CACHE_MAX_TEXT_BYTES)}}
impl ThreadSearchCache {
    pub fn new(max_entries: usize) -> Self {Self::with_budget(max_entries,usize::MAX)}
    pub fn with_budget(max_entries: usize,max_text_bytes: usize) -> Self {Self {entries:VecDeque::new(),max_entries:max_entries.max(1),max_text_bytes,text_bytes:0,hits:0,misses:0}}
    pub fn stats(&self) -> SearchCacheStats {SearchCacheStats {hits:self.hits,misses:self.misses,entries:self.entries.len()}}
    pub async fn load(&mut self,directory: &Path) -> std::io::Result<Vec<Arc<SearchSessionRecord>>> {
        let mut names = match tokio::fs::read_dir(directory).await {Ok(names)=>names,Err(error) if error.kind() == std::io::ErrorKind::NotFound=>return Ok(Vec::new()),Err(error)=>return Err(error)};
        let mut records = Vec::new();
        while let Some(entry) = names.next_entry().await? {if entry.file_name().to_string_lossy().ends_with(".jsonl") && let Some(record) = self.load_file(&entry.path()).await? {records.push(record);}}
        Ok(records)
    }
    /// Records are shared with the cache, so a hit never copies the transcript.
    pub async fn load_file(&mut self,path: &Path) -> std::io::Result<Option<Arc<SearchSessionRecord>>> {
        let key = path.display().to_string();
        let modified = match tokio::fs::metadata(path).await {Ok(metadata)=>metadata.modified()?,Err(error) if error.kind() == std::io::ErrorKind::NotFound=>{self.remove(&key);return Ok(None)},Err(error)=>return Err(error)};
        let mtime = chrono::DateTime::<chrono::Utc>::from(modified).timestamp_millis();
        if let Some(index) = self.entries.iter().position(|(name,time,_)|name == &key && *time == modified) {
            self.hits += 1;
            if let Some(entry) = self.entries.remove(index) {let record = Arc::clone(&entry.2);self.entries.push_back(entry);return Ok(Some(record));}
        }
        self.misses += 1;
        // The read and the per-line JSON parse are synchronous; keep them off the async worker.
        let (owned_path,owned_key) = (path.to_path_buf(),key.clone());
        let parsed = match tokio::task::spawn_blocking(move || std::fs::read_to_string(&owned_path).map(|contents|parse_search_session(&owned_key,mtime,&contents))).await {
            Ok(parsed)=>parsed,
            Err(error) if error.is_panic()=>std::panic::resume_unwind(error.into_panic()),
            Err(error)=>return Err(std::io::Error::other(error)),
        };
        let record = match parsed {Ok(record)=>record.map(Arc::new),Err(error) if error.kind() == std::io::ErrorKind::NotFound=>{self.remove(&key);return Ok(None)},Err(error)=>return Err(error)};
        self.remove(&key);
        if let Some(record) = &record {
            self.text_bytes += record.bytes();self.entries.push_back((key,modified,Arc::clone(record)));
            while self.entries.len() > self.max_entries || (self.text_bytes > self.max_text_bytes && self.entries.len() > 1) {if let Some((_,_,evicted)) = self.entries.pop_front() {self.text_bytes -= evicted.bytes();}}
        }
        Ok(record)
    }
    fn remove(&mut self,key: &str) {
        let text_bytes = &mut self.text_bytes;
        self.entries.retain(|(name,_,record)|{let keep = name != key;if !keep {*text_bytes -= record.bytes();}keep});
    }
}
fn timestamp(value: &Value,fallback: i64) -> i64 {value.as_i64().or_else(||value.as_str().and_then(super::js_semantics::date_parse_ms)).unwrap_or(fallback)}
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
    Some(SearchSessionRecord {thread:json!({"id":header["id"],"sessionId":header["id"],"sessionPath":path,"cwd":header["cwd"],"createdAt":iso(timestamp(&header["timestamp"],mtime))?,"updatedAt":iso(activity)?,"status":{"type":"notLoaded"},"preview":preview,"name":name}),recency_at:iso(recency)?,searchable_lower:None,searchable_text:messages.join(" ")}).map(|mut record|{record.searchable_lower = SearchSessionRecord::lowered(&record.searchable_text);record})
}
#[cfg(test)]
mod tests {
    use super::ThreadSearchCache;
    #[tokio::test]
    async fn text_byte_budget_evicts_oldest_and_tracks_removals() {
        let directory = tempfile::tempdir().unwrap();
        let header = serde_json::json!({"type":"session","id":"s","cwd":"/work","timestamp":"2020-01-01T00:00:00.000Z"}).to_string();
        let message = serde_json::json!({"type":"message","timestamp":1,"message":{"role":"user","content":"0123456789"}}).to_string();
        let paths = ["a","b","c"].map(|name|directory.path().join(format!("{name}.jsonl")));
        for path in &paths {std::fs::write(path,format!("{header}\n{message}")).unwrap();}
        let mut cache = ThreadSearchCache::with_budget(usize::MAX,20);
        for path in &paths {cache.load_file(path).await.unwrap().unwrap();}
        assert_eq!((cache.stats().entries,cache.text_bytes),(2,20));
        cache.load_file(&paths[2]).await.unwrap();assert_eq!(cache.stats().hits,1);
        std::fs::remove_file(&paths[2]).unwrap();assert!(cache.load_file(&paths[2]).await.unwrap().is_none());
        assert_eq!((cache.stats().entries,cache.text_bytes),(1,10));
    }
}
