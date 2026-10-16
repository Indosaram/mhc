use super::{archive_state::ThreadArchiveState,registry::{JsonRpcError,MethodRegistration,MethodScope},search_cache::{SearchSessionRecord,ThreadSearchCache},search_params::{ParsedSearchParams,parse_search_params},server_core::ServerCore,thread_registry::ThreadRegistry,turn_log::TurnLog,wire_thread::{build_disk_wire_thread,build_wire_thread}};
use serde::{Deserialize,Serialize};
use serde_json::{Value,json};
use std::{collections::{BTreeMap,BTreeSet},sync::Arc};
use tokio::sync::{Mutex,RwLock};

#[derive(Deserialize,Serialize)]
#[serde(rename_all="camelCase")]
struct SearchCursor {search_term:String,sort_key:String,sort_direction:String,source_kinds:Vec<String>,archived:bool,anchor_id:String,include_anchor:bool}
fn invalid(message: impl Into<String>) -> JsonRpcError {JsonRpcError::new(-32600,message)}
/// The fields `search_window` reads, so cached records can be windowed without copying them.
pub trait SearchWindowRecord {fn thread(&self) -> &Value;fn recency_at(&self) -> &str;fn searchable_text(&self) -> &str;}
impl SearchWindowRecord for SearchSessionRecord {
    fn thread(&self) -> &Value {&self.thread}
    fn recency_at(&self) -> &str {&self.recency_at}
    fn searchable_text(&self) -> &str {&self.searchable_text}
}
/// A cache-shared record whose thread a loaded thread's wire may replace.
struct SearchCandidate {thread: Option<Value>,record: Arc<SearchSessionRecord>}
impl SearchWindowRecord for SearchCandidate {
    fn thread(&self) -> &Value {self.thread.as_ref().unwrap_or(&self.record.thread)}
    fn recency_at(&self) -> &str {&self.record.recency_at}
    fn searchable_text(&self) -> &str {&self.record.searchable_text}
}
fn search_window_keeps(record: &impl SearchWindowRecord,params: &ParsedSearchParams,archived: &BTreeSet<String>) -> bool {
    params.source_kinds.iter().any(|kind|kind == "appServer") && archived.contains(record.thread()["id"].as_str().unwrap_or_default()) == params.archived && lowercase_contains(record.searchable_text(),&params.search_term)
}
/// `to_locale_lowercase(text).contains(term)` for an already-lowercased `term`, without
/// allocating the lowercase copy except for final-sigma or multi-char-lowercase text.
fn lowercase_contains(text: &str,term: &str) -> bool {
    if term.is_empty() {return true;}
    if text.is_ascii() {return text.as_bytes().windows(term.len()).any(|window|window.eq_ignore_ascii_case(term.as_bytes()));}
    // `str::to_lowercase` is per-char except final-sigma context and multi-char expansions
    // (a term could start mid-expansion); keep the exact allocating path for those texts only.
    if text.chars().any(|character|character == '\u{3a3}' || character.to_lowercase().len() != 1) {return super::js_semantics::to_locale_lowercase(text).contains(term);}
    text.char_indices().any(|(index,_)|{let mut lower = text[index..].chars().flat_map(char::to_lowercase);term.chars().all(|expected|lower.next() == Some(expected))})
}
pub fn search_window<R: SearchWindowRecord>(records: &mut Vec<R>,params: &ParsedSearchParams,archived: &BTreeSet<String>) -> Result<Value,JsonRpcError> {
    records.retain(|record|search_window_keeps(record,params,archived));
    search_window_kept(records,params)
}
/// `search_window` over records already filtered by `search_window_keeps`.
fn search_window_kept<R: SearchWindowRecord>(records: &mut [R],params: &ParsedSearchParams) -> Result<Value,JsonRpcError> {
    let time = |record: &R|{let field = match params.sort_key.as_str() {"created_at"=>record.thread()["createdAt"].as_str(),"updated_at"=>record.thread()["updatedAt"].as_str(),_=>Some(record.recency_at())};field.and_then(super::js_semantics::date_parse_ms).unwrap_or(0)};
    records.sort_by(|left,right|{let order = time(left).cmp(&time(right)).then_with(||super::js_semantics::locale_compare(left.thread()["id"].as_str().unwrap_or_default(),right.thread()["id"].as_str().unwrap_or_default()));if params.sort_direction == "asc" {order} else {order.reverse()}});
    let start = if let Some(value) = &params.cursor {
        let cursor: SearchCursor = serde_json::from_str(value).map_err(|_|invalid(format!("thread/search received an invalid cursor: {value}")))?;
        if !matches!(cursor.sort_direction.as_str(),"asc"|"desc") {return Err(invalid(format!("thread/search received an invalid cursor: {value}")));}
        if cursor.search_term != params.search_term || cursor.sort_key != params.sort_key || cursor.archived != params.archived || cursor.source_kinds != params.source_kinds {return Err(invalid("thread/search cursor does not match the requested search"));}
        let same_direction = cursor.sort_direction == params.sort_direction;
        if if cursor.include_anchor {same_direction} else {!same_direction} {return Err(invalid("thread/search cursor does not match the requested sort direction"));}
        records.iter().position(|record|record.thread()["id"] == cursor.anchor_id).ok_or_else(||invalid("thread/search received an invalid cursor anchor"))? + usize::from(!cursor.include_anchor)
    } else {0};
    let end = start.saturating_add(params.limit as usize).min(records.len());
    let cursor = |record: &R,include_anchor|serde_json::to_string(&SearchCursor {search_term:params.search_term.clone(),sort_key:params.sort_key.clone(),sort_direction:params.sort_direction.clone(),source_kinds:params.source_kinds.clone(),archived:params.archived,anchor_id:record.thread()["id"].as_str().unwrap_or_default().into(),include_anchor}).map_err(|error|JsonRpcError::new(-32603,error.to_string()));
    Ok(json!({"start":start,"end":end,"nextCursor":if end < records.len() {records.get(end-1).map(|record|cursor(record,false)).transpose()?} else {None},"backwardsCursor":records.get(start).filter(|_|end > start).map(|record|cursor(record,true)).transpose()?}))
}
pub async fn register_search_handler(core: &Arc<RwLock<ServerCore>>,threads: Arc<ThreadRegistry>,archive: Arc<ThreadArchiveState>,log: Arc<Mutex<TurnLog>>,version: String) {
    let cache = Arc::new(Mutex::new(ThreadSearchCache::default()));
    core.write().await.registry.register("thread/search".into(),MethodRegistration {requires_init:true,experimental:true,scope:MethodScope::Thread,handler:Arc::new(move |context| {
        let threads = threads.clone();let archive = archive.clone();let log = log.clone();let cache = cache.clone();let version = version.clone();
        Box::pin(async move {
            let params = parse_search_params(&context.request["params"])?;
            let archived = archive.list_archived_threads().await.map_err(|error|JsonRpcError::new(-32603,error.to_string()))?.iter().filter_map(|thread|thread["id"].as_str().map(str::to_owned)).collect::<BTreeSet<_>>();
            let mut records = Vec::new();
            let mut cache = cache.lock().await;
            for info in threads.list_session_infos().await {if let Some(record) = cache.load_file(std::path::Path::new(&info.path)).await.map_err(|error|JsonRpcError::new(-32603,error.to_string()))? {records.push(record);}}
            drop(cache);
            let mut by_id = records.into_iter().map(|record|(record.thread["id"].as_str().unwrap_or_default().to_owned(),SearchCandidate {thread:None,record})).collect::<BTreeMap<_,_>>();
            for wire in threads.list_loaded().await {
                let id = wire["id"].as_str().unwrap_or_default().to_owned();
                if let Some(candidate) = by_id.get_mut(&id) {candidate.thread = Some(wire);} else if let Ok(entry) = threads.get_loaded_thread(&id).await {
                    let entry = entry.lock().await;
                    by_id.insert(id,SearchCandidate {thread:None,record:Arc::new(SearchSessionRecord {recency_at:wire["updatedAt"].as_str().unwrap_or_default().into(),thread:wire,searchable_text:entry.session.get_user_messages_for_forking().iter().map(|(_,text)|text.as_str()).collect::<Vec<_>>().join(" ")})});
                }
            }
            // Filter once; kept records stay shared with the cache instead of being copied.
            let mut records = by_id.into_values().filter(|record|search_window_keeps(record,&params,&archived)).collect::<Vec<_>>();let window = search_window_kept(&mut records,&params)?;
            let mut data = Vec::new();
            for record in records.iter().skip(window["start"].as_u64().unwrap_or_default() as usize).take(window["end"].as_u64().unwrap_or_default() as usize-window["start"].as_u64().unwrap_or_default() as usize) {
                let id = record.thread()["id"].as_str().unwrap_or_default();
                let mut thread = if let Ok(entry) = threads.get_loaded_thread(id).await {let entry = entry.lock().await;build_wire_thread(&entry,&log,false,&version).await} else {build_disk_wire_thread(record.thread(),&version).await}.map_err(|error|JsonRpcError::new(-32603,error.to_string()))?;
                thread["recencyAt"] = json!(chrono::DateTime::parse_from_rfc3339(record.recency_at()).map_or(0.0,|time|time.timestamp_millis() as f64/1000.0));
                data.push(json!({"thread":thread,"snippet":literal_snippet(record.searchable_text(),&params.search_term)}));
            }
            Ok(json!({"data":data,"nextCursor":window["nextCursor"],"backwardsCursor":window["backwardsCursor"]}))
        })
    })});
}
pub fn literal_snippet(text: &str,term: &str) -> String {
    let lower = super::js_semantics::to_locale_lowercase(text);let Some(byte_index) = lower.find(term) else {return String::new()};
    let index = lower[..byte_index].encode_utf16().count();let text = text.encode_utf16().collect::<Vec<_>>();
    let start = index.saturating_sub(80);let end = (index+term.encode_utf16().count()+80).min(text.len());
    format!("{}{}{}",if start > 0 {"... "} else {""},String::from_utf16_lossy(&text[start.min(text.len())..end]),if end < text.len() {" ..."} else {""})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lowercase_contains_matches_allocating_lowercase() {
        let texts = ["안녕하세요 Rust 세계","ÉCOLE Café NAÏVE","MiXeD 한글 CaSe Ünïcödé","ΣΊΣΥΦΟΣ ΟΔΟΣ","İstanbul KİLİM","straße STRASSE","Ǆemal ǅ ǆ","plain ascii Text"];
        let terms = ["","안녕","rust 세","세계","école","café naï","é","mixed 한글 case","ünïcödé","σίσυφος","σ","ς","οδος","i̇stanbul","i̇","kilim","straße","ǆemal","ǆ","ascii text","absent","하세요 r"];
        for text in texts {
            for term in terms {
                assert_eq!(lowercase_contains(text,term),super::super::js_semantics::to_locale_lowercase(text).contains(term),"{text:?} {term:?}");
            }
        }
    }
}
