use super::{archive_state::ThreadArchiveState,history_pagination::{HistoryKind,HistoryPaginationOptions,HistoryValue,SortDirection,paginate_history},registry::{JsonRpcError,MethodRegistration,MethodScope},server_core::ServerCore,thread_registry::ThreadRegistry,turn_log::{LoggedTurn,TurnLog},wire_thread::{logged_turn_with_view,turns_from_session_entries,wire_item_to_thread_item}};
use serde_json::{Value,json};
use std::{borrow::Cow,collections::VecDeque,sync::Arc,time::SystemTime};
use tokio::sync::{Mutex,RwLock};

/// (thread id, session file path, created_at fallback, file len, mtime) of one session-entry rebuild.
type RebuiltTurnsKey = (String,String,String,u64,SystemTime);
const REBUILT_TURNS_CAPACITY: usize = 4;
/// The last few session-entry rebuilds, so paging an unchanged session file does not re-read and
/// re-parse it for every page. Locked only from blocking threads, never across an `.await`.
static REBUILT_TURNS: std::sync::Mutex<VecDeque<(RebuiltTurnsKey,Arc<Vec<LoggedTurn>>)>> = std::sync::Mutex::new(VecDeque::new());
fn rebuilt_turns_cache() -> std::sync::MutexGuard<'static,VecDeque<(RebuiltTurnsKey,Arc<Vec<LoggedTurn>>)>> {
    REBUILT_TURNS.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
/// The turns `rebuild` builds from `path`, reused while the file's len and mtime are unchanged.
/// The stamp is read before the rebuild, so a write racing it changes the stamp and rebuilds next time.
fn rebuilt_turns(id: &str,path: Option<&str>,created_at: &str,rebuild: impl FnOnce() -> Vec<LoggedTurn>) -> Arc<Vec<LoggedTurn>> {
    let stamp = path.and_then(|path|{let metadata = std::fs::metadata(path).ok()?;Some((path,metadata.len(),metadata.modified().ok()?))});
    let Some((path,len,modified)) = stamp else {return Arc::new(rebuild())};
    let key = (id.to_owned(),path.to_owned(),created_at.to_owned(),len,modified);
    if let Some((_,turns)) = rebuilt_turns_cache().iter().find(|(cached,_)|cached == &key) {return turns.clone();}
    let turns = Arc::new(rebuild());
    let mut cache = rebuilt_turns_cache();
    cache.retain(|(cached,_)|(&cached.0,&cached.1,&cached.2) != (&key.0,&key.1,&key.2));
    if cache.len() >= REBUILT_TURNS_CAPACITY {cache.pop_front();}
    cache.push_back((key,turns.clone()));
    turns
}
/// Drops a thread's cached rebuilds (`thread/delete`, `thread/unarchive`).
pub fn forget_rebuilt_turns(id: &str) {
    rebuilt_turns_cache().retain(|(cached,_)|cached.0 != id);
}

pub async fn thread_history_turns(id: &str,threads: &ThreadRegistry,archive: &ThreadArchiveState,log: &Mutex<TurnLog>) -> Result<Vec<LoggedTurn>,JsonRpcError> {
    history_turns(id,threads,archive,log,|turns|turns.into_owned()).await
}
/// `thread_history_turns`, handing `read` the logged turns borrowed under the `TurnLog` lock
/// instead of cloning the whole log; only the session-entry rebuilds are owned.
pub async fn with_thread_history_turns<R>(id: &str,threads: &ThreadRegistry,archive: &ThreadArchiveState,log: &Mutex<TurnLog>,read: impl FnOnce(&[LoggedTurn]) -> R) -> Result<R,JsonRpcError> {
    history_turns(id,threads,archive,log,|turns|read(&turns)).await
}
/// Hands `read` the turns borrowed under the `TurnLog` lock, or owned when rebuilt from session
/// entries, so owned turns are never copied again.
async fn history_turns<R>(id: &str,threads: &ThreadRegistry,archive: &ThreadArchiveState,log: &Mutex<TurnLog>,read: impl FnOnce(Cow<'_,[LoggedTurn]>) -> R) -> Result<R,JsonRpcError> {
    if let Ok(entry) = threads.get_loaded_thread(id).await {
        {let log = log.lock().await;let logged = log.turns(id);if !logged.is_empty() {return Ok(read(Cow::Borrowed(logged)));}}
        // Release the entry guard before the synchronous history read/rebuild so streamed events are not stalled.
        let (session,created_at) = {let entry = entry.lock().await;(entry.session.clone(),entry.created_at.clone())};
        let id = id.to_owned();
        // Only a persisted, flushed session's file holds every resident entry, so only it is a cache stamp.
        let turns = tokio::task::spawn_blocking(move || session.with_session_manager(|manager|rebuilt_turns(&id,manager.session_file().filter(|_|manager.is_persisted() && manager.is_flushed()),&created_at,||turns_from_session_entries(&manager.entries(),&created_at)))).await.map_err(|error|JsonRpcError::new(-32603,error.to_string()))?;
        return Ok(read(Cow::Borrowed(turns.as_slice())));
    }
    if let Some(info) = threads.list_session_infos().await.into_iter().find(|info|info.id == id) {
        // senpi `turnsForEntry`: the process-lifetime TurnLog wins over the session-entry rebuild.
        {let log = log.lock().await;let logged = log.turns(id);if !logged.is_empty() {return Ok(read(Cow::Borrowed(logged)));}}
        // The session-file read, JSON parse and turn rebuild are synchronous; keep them off the async worker.
        let turns = tokio::task::spawn_blocking(move || {let created_at = info.created.to_rfc3339_opts(chrono::SecondsFormat::Millis,true);rebuilt_turns(&info.id,Some(&info.path),&created_at,||turns_from_session_entries(&maho_core::session_manager::load_entries_from_file(&info.path),&created_at))}).await.map_err(|error|JsonRpcError::new(-32603,error.to_string()))?;
        return Ok(read(Cow::Borrowed(turns.as_slice())));
    }
    if archive.list_archived_threads().await.map_err(|error|JsonRpcError::new(-32603,error.to_string()))?.iter().any(|thread|thread["id"] == id) {return Ok(read(Cow::Borrowed(log.lock().await.turns(id))));}
    Err(JsonRpcError::new(-32600,format!("thread not found: {id}")))
}
/// The turns `occurrences_response` scans: borrowed turns are copied with only their user messages
/// and final agent message (the only items it reads), so the result is unchanged.
fn occurrence_turns(turns: Cow<'_,[LoggedTurn]>) -> Vec<LoggedTurn> {
    match turns {
        Cow::Owned(turns)=>turns,
        Cow::Borrowed(turns)=>turns.iter().map(|turn|{
            let is = |item: &super::turn_log::WireItem,kind: &str|item.get("type").is_some_and(|value|value == kind);
            let final_agent = turn.items.iter().rposition(|item|is(item,"agentMessage"));
            let items = turn.items.iter().enumerate().filter(|&(index,item)|is(item,"userMessage") || Some(index) == final_agent).map(|(_,item)|item.clone()).collect();
            LoggedTurn {turn_id:turn.turn_id.clone(),started_at:turn.started_at.clone(),completed_at:turn.completed_at.clone(),duration_ms:turn.duration_ms,error:turn.error.clone(),status:turn.status,items}
        }).collect(),
    }
}
pub async fn register_history_handlers(core: &Arc<RwLock<ServerCore>>,threads: Arc<ThreadRegistry>,archive: Arc<ThreadArchiveState>,log: Arc<Mutex<TurnLog>>) {
    for method in ["thread/turns/list","thread/items/list","thread/searchOccurrences"] {
        let threads = threads.clone();let archive = archive.clone();let log = log.clone();
        core.write().await.registry.register(method.into(),MethodRegistration {requires_init:true,experimental:true,scope:MethodScope::Thread,handler:Arc::new(move |context| {
            let threads = threads.clone();let archive = archive.clone();let log = log.clone();
            Box::pin(async move {
                let params = &context.request["params"];
                if method == "thread/searchOccurrences" {
                    let (id,_,_,_) = super::search_occurrences::parse_occurrence_params(params)?;
                    // Copy only the searchable items, then scan them after the `TurnLog` lock is released
                    // so the O(history) search never stalls streaming `item/completed` writers.
                    let turns = history_turns(id,&threads,&archive,&log,occurrence_turns).await?;
                    return super::search_occurrences::occurrences_response(params,&turns);
                }
                let id = params["threadId"].as_str().filter(|id|!id.is_empty()).ok_or_else(||JsonRpcError::new(-32600,"thread history requires a non-empty threadId"))?;
                let cursor = nullable_string(params,"cursor","thread history received an invalid cursor")?;
                let is_turns = method == "thread/turns/list";
                let turn_id = if is_turns {None} else {nullable_string(params,"turnId","thread/items/list received an invalid turnId")?};
                let limit = match params.get("limit").filter(|value|!value.is_null()) {None=>25,Some(value)=>value.as_f64().filter(|value|value.fract() == 0.0 && *value >= 0.0 && *value <= f64::from(u32::MAX)).map(|value|value.clamp(1.0,100.0) as usize).ok_or_else(||JsonRpcError::new(-32600,format!("{method} received an invalid limit")))?};
                let direction = match params.get("sortDirection").filter(|value|!value.is_null()) {None=>if is_turns {SortDirection::Desc} else {SortDirection::Asc},Some(Value::String(value)) if value == "asc"=>SortDirection::Asc,Some(Value::String(value)) if value == "desc"=>SortDirection::Desc,_=>return Err(JsonRpcError::new(-32600,format!("{method} received an invalid sortDirection")))};
                let view = if is_turns {match params.get("itemsView").filter(|value|!value.is_null()) {None=>"summary",Some(Value::String(value)) if matches!(value.as_str(),"full"|"summary"|"notLoaded")=>value,_=>return Err(JsonRpcError::new(-32600,"thread/turns/list received an invalid itemsView"))}} else {"full"};
                // Paginate on keys only, then build wire values for the selected page alone, reading
                // the logged turns under the lock instead of cloning the thread's whole log.
                let options = HistoryPaginationOptions {kind:if is_turns {HistoryKind::Turn} else {HistoryKind::Item},thread_id:id.into(),turn_id,limit,sort_direction:direction,cursor};
                let page = |turns: &[LoggedTurn]| -> Result<Value,JsonRpcError> {
                let mut values = Vec::new();
                for (turn_index,turn) in turns.iter().enumerate() {
                    if is_turns {values.push(HistoryValue {key:turn.turn_id.clone(),value:(turn_index,0)});} else if options.turn_id.as_ref().is_none_or(|id|id == &turn.turn_id) {
                        for (item_index,item) in turn.items.iter().enumerate() {
                            // Same id `wire_item_to_thread_item` assigns: the non-empty string id, else "item".
                            let item_id = item.get("id").and_then(Value::as_str).filter(|id|!id.is_empty()).unwrap_or("item");
                            values.push(HistoryValue {key:format!("{}\0{item_id}",turn.turn_id),value:(turn_index,item_index)});
                        }
                    }
                }
                let page = paginate_history(&values,&options)?;
                let data = page.data.iter().map(|&(turn_index,item_index)|{
                    let turn = &turns[turn_index];
                    if is_turns {logged_turn_with_view(turn,view)} else {json!({"turnId":turn.turn_id,"item":wire_item_to_thread_item(&Value::Object(turn.items[item_index].clone()))})}
                }).collect::<Vec<_>>();
                Ok(json!({"data":data,"nextCursor":page.next_cursor,"backwardsCursor":page.backwards_cursor}))
                };
                with_thread_history_turns(id,&threads,&archive,&log,page).await?
            })
        })});
    }
}
fn nullable_string(params: &Value,key: &str,message: &str) -> Result<Option<String>,JsonRpcError> {
    match params.get(key) {None|Some(Value::Null)=>Ok(None),Some(Value::String(value))=>Ok(Some(value.clone())),_=>Err(JsonRpcError::new(-32600,message))}
}
#[cfg(test)]
mod tests {
    use super::{Arc,Cow,LoggedTurn,forget_rebuilt_turns,occurrence_turns,rebuilt_turns};
    use serde_json::json;
    #[test]
    fn rebuilt_turns_are_reused_until_the_session_file_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        std::fs::write(&path,"one\n").unwrap();
        let path = path.display().to_string();
        let rebuilds = std::cell::Cell::new(0);
        let rebuild = ||{rebuilds.set(rebuilds.get() + 1);Vec::new()};
        let first = rebuilt_turns("cache-thread",Some(&path),"c",rebuild);
        let second = rebuilt_turns("cache-thread",Some(&path),"c",rebuild);
        assert!(Arc::ptr_eq(&first,&second));
        assert_eq!(rebuilds.get(),1);
        // Same length, explicitly bumped mtime.
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(std::fs::metadata(&path).unwrap().modified().unwrap() + std::time::Duration::from_secs(5)).unwrap();
        drop(file);
        let bumped = rebuilt_turns("cache-thread",Some(&path),"c",rebuild);
        assert!(!Arc::ptr_eq(&second,&bumped));
        assert_eq!(rebuilds.get(),2);
        // Changed length.
        std::fs::write(&path,"one\ntwo\n").unwrap();
        let grown = rebuilt_turns("cache-thread",Some(&path),"c",rebuild);
        assert!(!Arc::ptr_eq(&bumped,&grown));
        assert_eq!(rebuilds.get(),3);
        assert!(Arc::ptr_eq(&grown,&rebuilt_turns("cache-thread",Some(&path),"c",rebuild)));
        forget_rebuilt_turns("cache-thread");
        rebuilt_turns("cache-thread",Some(&path),"c",rebuild);
        assert_eq!(rebuilds.get(),4);
        // No session file: never cached.
        rebuilt_turns("cache-thread",None,"c",rebuild);
        rebuilt_turns("cache-thread",None,"c",rebuild);
        assert_eq!(rebuilds.get(),6);
    }
    #[test]
    fn occurrence_turns_keep_search_results_identical() {
        let item = |value: serde_json::Value|value.as_object().unwrap().clone();
        let turn = LoggedTurn {turn_id:"t".into(),started_at:"s".into(),completed_at:None,duration_ms:None,error:None,status:super::super::turn_log::TurnStatus::Completed,items:vec![
            item(json!({"type":"userMessage","id":"u","content":"find me"})),item(json!({"type":"agentMessage","id":"a1","text":"find early"})),
            item(json!({"type":"commandExecution","id":"c","aggregatedOutput":"find"})),item(json!({"type":"agentMessage","id":"a2","text":"find late"})),
        ]};
        let turns = vec![turn];
        let reduced = occurrence_turns(Cow::Borrowed(&turns));
        assert_eq!(reduced[0].items.iter().map(|item|item["id"].as_str().unwrap()).collect::<Vec<_>>(),["u","a2"]);
        let params = json!({"threadId":"x","searchTerm":"find"});
        let response = super::super::search_occurrences::occurrences_response(&params,&reduced).unwrap();
        assert_eq!(response,super::super::search_occurrences::occurrences_response(&params,&turns).unwrap());
        assert_eq!(response["data"].as_array().unwrap().len(),2);
    }
}
