use super::{archive_state::ThreadArchiveState,history_pagination::{HistoryKind,HistoryPaginationOptions,HistoryValue,SortDirection,paginate_history},registry::{JsonRpcError,MethodRegistration,MethodScope},server_core::ServerCore,thread_registry::ThreadRegistry,turn_log::{LoggedTurn,TurnLog},wire_thread::{logged_turn_with_view,turns_from_session_entries,wire_item_to_thread_item}};
use serde_json::{Value,json};
use std::sync::Arc;
use tokio::sync::{Mutex,RwLock};

pub async fn thread_history_turns(id: &str,threads: &ThreadRegistry,archive: &ThreadArchiveState,log: &Mutex<TurnLog>) -> Result<Vec<LoggedTurn>,JsonRpcError> {
    if let Ok(entry) = threads.get_loaded_thread(id).await {
        let logged = log.lock().await.read_turns(id);
        if !logged.is_empty() {return Ok(logged);}
        let entry = entry.lock().await;
        return Ok(entry.session.with_session_manager(|manager|turns_from_session_entries(&manager.entries(),&entry.created_at)));
    }
    if let Some(info) = threads.list_session_infos().await.into_iter().find(|info|info.id == id) {
        return Ok(turns_from_session_entries(&maho_core::session_manager::load_entries_from_file(&info.path),&info.created.to_rfc3339_opts(chrono::SecondsFormat::Millis,true)));
    }
    if archive.list_archived_threads().await.map_err(|error|JsonRpcError::new(-32603,error.to_string()))?.iter().any(|thread|thread["id"] == id) {return Ok(log.lock().await.read_turns(id));}
    Err(JsonRpcError::new(-32600,format!("thread not found: {id}")))
}
pub async fn register_history_handlers(core: &Arc<RwLock<ServerCore>>,threads: Arc<ThreadRegistry>,archive: Arc<ThreadArchiveState>,log: Arc<Mutex<TurnLog>>) {
    for method in ["thread/turns/list","thread/items/list","thread/searchOccurrences"] {
        let threads = threads.clone();let archive = archive.clone();let log = log.clone();
        core.write().await.registry.register(method.into(),MethodRegistration {requires_init:true,experimental:false,scope:MethodScope::Thread,handler:Arc::new(move |context| {
            let threads = threads.clone();let archive = archive.clone();let log = log.clone();
            Box::pin(async move {
                let params = &context.request["params"];
                let id = params["threadId"].as_str().filter(|id|!id.is_empty()).ok_or_else(||JsonRpcError::new(-32600,"thread history requires a non-empty threadId"))?;
                if method == "thread/searchOccurrences" {let turns = thread_history_turns(id,&threads,&archive,&log).await?;return super::search_occurrences::occurrences_response(params,&turns);}
                let cursor = nullable_string(params,"cursor","thread history received an invalid cursor")?;
                let is_turns = method == "thread/turns/list";
                let turn_id = if is_turns {None} else {nullable_string(params,"turnId","thread/items/list received an invalid turnId")?};
                let limit = match params.get("limit").filter(|value|!value.is_null()) {None=>25,Some(value)=>value.as_f64().filter(|value|value.fract() == 0.0 && *value >= 0.0 && *value <= f64::from(u32::MAX)).map(|value|value.clamp(1.0,100.0) as usize).ok_or_else(||JsonRpcError::new(-32600,format!("{method} received an invalid limit")))?};
                let direction = match params.get("sortDirection").filter(|value|!value.is_null()) {None=>if is_turns {SortDirection::Desc} else {SortDirection::Asc},Some(Value::String(value)) if value == "asc"=>SortDirection::Asc,Some(Value::String(value)) if value == "desc"=>SortDirection::Desc,_=>return Err(JsonRpcError::new(-32600,format!("{method} received an invalid sortDirection")))};
                let view = if is_turns {match params.get("itemsView").filter(|value|!value.is_null()) {None=>"summary",Some(Value::String(value)) if matches!(value.as_str(),"full"|"summary"|"notLoaded")=>value,_=>return Err(JsonRpcError::new(-32600,"thread/turns/list received an invalid itemsView"))}} else {"full"};
                let turns = thread_history_turns(id,&threads,&archive,&log).await?;
                let mut values = Vec::new();
                for turn in turns {
                    if is_turns {values.push(HistoryValue {key:turn.turn_id.clone(),value:logged_turn_with_view(&turn,view)});} else if turn_id.as_ref().is_none_or(|id|id == &turn.turn_id) {
                        for item in turn.items {let item = wire_item_to_thread_item(&Value::Object(item));values.push(HistoryValue {key:format!("{}\0{}",turn.turn_id,item["id"].as_str().unwrap_or_default()),value:json!({"turnId":turn.turn_id,"item":item})});}
                    }
                }
                let page = paginate_history(&values,&HistoryPaginationOptions {kind:if is_turns {HistoryKind::Turn} else {HistoryKind::Item},thread_id:id.into(),turn_id,limit,sort_direction:direction,cursor})?;
                Ok(json!({"data":page.data,"nextCursor":page.next_cursor,"backwardsCursor":page.backwards_cursor}))
            })
        })});
    }
}
fn nullable_string(params: &Value,key: &str,message: &str) -> Result<Option<String>,JsonRpcError> {
    match params.get(key) {None|Some(Value::Null)=>Ok(None),Some(Value::String(value))=>Ok(Some(value.clone())),_=>Err(JsonRpcError::new(-32600,message))}
}
