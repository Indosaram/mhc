use super::{archive_state::ThreadArchiveState,history_pagination::{HistoryKind,HistoryPaginationOptions,HistoryValue,SortDirection,paginate_history},registry::{JsonRpcError,MethodRegistration,MethodScope},server_core::ServerCore,thread_registry::ThreadRegistry,turn_log::{LoggedTurn,TurnLog},wire_thread::{logged_turn_with_view,turns_from_session_entries,wire_item_to_thread_item}};
use serde_json::{Value,json};
use std::{borrow::Cow,sync::Arc};
use tokio::sync::{Mutex,RwLock};

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
        let turns = tokio::task::spawn_blocking(move || session.with_session_manager(|manager|turns_from_session_entries(&manager.entries(),&created_at))).await.map_err(|error|JsonRpcError::new(-32603,error.to_string()))?;
        return Ok(read(Cow::Owned(turns)));
    }
    if let Some(info) = threads.list_session_infos().await.into_iter().find(|info|info.id == id) {
        // senpi `turnsForEntry`: the process-lifetime TurnLog wins over the session-entry rebuild.
        {let log = log.lock().await;let logged = log.turns(id);if !logged.is_empty() {return Ok(read(Cow::Borrowed(logged)));}}
        // The session-file read, JSON parse and turn rebuild are synchronous; keep them off the async worker.
        let turns = tokio::task::spawn_blocking(move || turns_from_session_entries(&maho_core::session_manager::load_entries_from_file(&info.path),&info.created.to_rfc3339_opts(chrono::SecondsFormat::Millis,true))).await.map_err(|error|JsonRpcError::new(-32603,error.to_string()))?;
        return Ok(read(Cow::Owned(turns)));
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
    use super::{Cow,LoggedTurn,occurrence_turns};
    use serde_json::json;
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
