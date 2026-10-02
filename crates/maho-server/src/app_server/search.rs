use super::{archive_state::ThreadArchiveState,registry::{JsonRpcError,MethodRegistration,MethodScope},search_cache::{SearchSessionRecord,ThreadSearchCache},search_params::{ParsedSearchParams,parse_search_params},server_core::ServerCore,thread_registry::ThreadRegistry,turn_log::TurnLog,wire_thread::{build_disk_wire_thread,build_wire_thread}};
use serde::{Deserialize,Serialize};
use serde_json::{Value,json};
use std::{collections::{BTreeMap,BTreeSet},sync::Arc};
use tokio::sync::{Mutex,RwLock};

#[derive(Deserialize,Serialize)]
#[serde(rename_all="camelCase")]
struct SearchCursor {search_term:String,sort_key:String,sort_direction:String,source_kinds:Vec<String>,archived:bool,anchor_id:String,include_anchor:bool}
fn invalid(message: impl Into<String>) -> JsonRpcError {JsonRpcError::new(-32600,message)}
pub fn search_window(records: &mut Vec<SearchSessionRecord>,params: &ParsedSearchParams,archived: &BTreeSet<String>) -> Result<Value,JsonRpcError> {
    records.retain(|record|params.source_kinds.iter().any(|kind|kind == "appServer") && archived.contains(record.thread["id"].as_str().unwrap_or_default()) == params.archived && record.searchable_text.to_lowercase().contains(&params.search_term));
    let time = |record: &SearchSessionRecord|{let field = match params.sort_key.as_str() {"created_at"=>record.thread["createdAt"].as_str(),"updated_at"=>record.thread["updatedAt"].as_str(),_=>Some(record.recency_at.as_str())};field.and_then(|value|chrono::DateTime::parse_from_rfc3339(value).ok()).map_or(0,|time|time.timestamp_millis())};
    records.sort_by(|left,right|{let order = time(left).cmp(&time(right)).then_with(||left.thread["id"].as_str().cmp(&right.thread["id"].as_str()));if params.sort_direction == "asc" {order} else {order.reverse()}});
    let start = if let Some(value) = &params.cursor {
        let cursor: SearchCursor = serde_json::from_str(value).map_err(|_|invalid(format!("thread/search received an invalid cursor: {value}")))?;
        if !matches!(cursor.sort_direction.as_str(),"asc"|"desc") {return Err(invalid(format!("thread/search received an invalid cursor: {value}")));}
        if cursor.search_term != params.search_term || cursor.sort_key != params.sort_key || cursor.archived != params.archived || cursor.source_kinds != params.source_kinds {return Err(invalid("thread/search cursor does not match the requested search"));}
        let same_direction = cursor.sort_direction == params.sort_direction;
        if if cursor.include_anchor {same_direction} else {!same_direction} {return Err(invalid("thread/search cursor does not match the requested sort direction"));}
        records.iter().position(|record|record.thread["id"] == cursor.anchor_id).ok_or_else(||invalid("thread/search received an invalid cursor anchor"))? + usize::from(!cursor.include_anchor)
    } else {0};
    let end = start.saturating_add(params.limit as usize).min(records.len());
    let cursor = |record: &SearchSessionRecord,include_anchor|serde_json::to_string(&SearchCursor {search_term:params.search_term.clone(),sort_key:params.sort_key.clone(),sort_direction:params.sort_direction.clone(),source_kinds:params.source_kinds.clone(),archived:params.archived,anchor_id:record.thread["id"].as_str().unwrap_or_default().into(),include_anchor}).map_err(|error|JsonRpcError::new(-32603,error.to_string()));
    Ok(json!({"start":start,"end":end,"nextCursor":if end < records.len() {records.get(end-1).map(|record|cursor(record,false)).transpose()?} else {None},"backwardsCursor":records.get(start).filter(|_|end > start).map(|record|cursor(record,true)).transpose()?}))
}
pub async fn register_search_handler(core: &Arc<RwLock<ServerCore>>,threads: Arc<ThreadRegistry>,archive: Arc<ThreadArchiveState>,log: Arc<Mutex<TurnLog>>,version: String) {
    let cache = Arc::new(Mutex::new(ThreadSearchCache::default()));
    core.write().await.registry.register("thread/search".into(),MethodRegistration {requires_init:true,experimental:false,scope:MethodScope::Thread,handler:Arc::new(move |context| {
        let threads = threads.clone();let archive = archive.clone();let log = log.clone();let cache = cache.clone();let version = version.clone();
        Box::pin(async move {
            let params = parse_search_params(&context.request["params"])?;
            let archived = archive.list_archived_threads().await.map_err(|error|JsonRpcError::new(-32603,error.to_string()))?.iter().filter_map(|thread|thread["id"].as_str().map(str::to_owned)).collect::<BTreeSet<_>>();
            let mut records = Vec::new();
            let mut cache = cache.lock().await;
            for info in threads.list_session_infos().await {if let Some(record) = cache.load_file(std::path::Path::new(&info.path)).await.map_err(|error|JsonRpcError::new(-32603,error.to_string()))? {records.push(record);}}
            drop(cache);
            let mut by_id = records.into_iter().map(|record|(record.thread["id"].as_str().unwrap_or_default().to_owned(),record)).collect::<BTreeMap<_,_>>();
            for wire in threads.list_loaded().await {
                let id = wire["id"].as_str().unwrap_or_default().to_owned();
                if let Some(record) = by_id.get_mut(&id) {record.thread = wire;} else if let Ok(entry) = threads.get_loaded_thread(&id).await {
                    let entry = entry.lock().await;
                    by_id.insert(id,SearchSessionRecord {recency_at:wire["updatedAt"].as_str().unwrap_or_default().into(),thread:wire,searchable_text:entry.session.get_user_messages_for_forking().iter().map(|(_,text)|text.as_str()).collect::<Vec<_>>().join(" ")});
                }
            }
            let mut records = by_id.into_values().collect::<Vec<_>>();let window = search_window(&mut records,&params,&archived)?;
            let mut data = Vec::new();
            for record in records.iter().skip(window["start"].as_u64().unwrap_or_default() as usize).take(window["end"].as_u64().unwrap_or_default() as usize-window["start"].as_u64().unwrap_or_default() as usize) {
                let id = record.thread["id"].as_str().unwrap_or_default();
                let mut thread = if let Ok(entry) = threads.get_loaded_thread(id).await {let entry = entry.lock().await;let mut log = log.lock().await;build_wire_thread(&entry,&mut log,false,&version).await} else {build_disk_wire_thread(&record.thread,&version).await}.map_err(|error|JsonRpcError::new(-32603,error.to_string()))?;
                thread["recencyAt"] = json!(chrono::DateTime::parse_from_rfc3339(&record.recency_at).map_or(0.0,|time|time.timestamp_millis() as f64/1000.0));
                data.push(json!({"thread":thread,"snippet":literal_snippet(&record.searchable_text,&params.search_term)}));
            }
            Ok(json!({"data":data,"nextCursor":window["nextCursor"],"backwardsCursor":window["backwardsCursor"]}))
        })
    })});
}
pub fn literal_snippet(text: &str,term: &str) -> String {
    let lower = text.to_lowercase();let Some(byte_index) = lower.find(term) else {return String::new()};
    let index = lower[..byte_index].encode_utf16().count();let text = text.encode_utf16().collect::<Vec<_>>();
    let start = index.saturating_sub(80);let end = (index+term.encode_utf16().count()+80).min(text.len());
    format!("{}{}{}",if start > 0 {"... "} else {""},String::from_utf16_lossy(&text[start.min(text.len())..end]),if end < text.len() {" ..."} else {""})
}
