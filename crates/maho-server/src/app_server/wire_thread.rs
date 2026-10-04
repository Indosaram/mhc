use super::{metadata_state::{MetadataStateError, ThreadMetadataState}, thread_registry::ThreadEntry, turn_log::{LoggedTurn, TurnLog, TurnStatus}};
use serde_json::{Value, json};

fn iso_seconds(value: &str) -> Option<f64> {
    chrono::DateTime::parse_from_rfc3339(value).ok().map(|time| time.timestamp_millis() as f64 / 1000.0)
}
pub async fn build_disk_wire_thread(wire: &Value,version: &str) -> Result<Value,MetadataStateError> {
    let git_info = match wire["sessionPath"].as_str() {Some(path)=>ThreadMetadataState.read_git_info(std::path::Path::new(path)).await?,None=>None};
    let created = wire["createdAt"].as_str().and_then(iso_seconds);
    let updated = wire["updatedAt"].as_str().and_then(iso_seconds);
    Ok(json!({"id":wire["id"],"sessionId":wire["sessionId"],"forkedFromId":null,"parentThreadId":null,"preview":wire["preview"].as_str().unwrap_or_default(),"ephemeral":false,"modelProvider":"unknown","createdAt":created,"updatedAt":updated,"recencyAt":updated,"status":wire["status"],"path":wire["sessionPath"],"cwd":wire["cwd"],"cliVersion":version,"source":"appServer","threadSource":null,"agentNickname":null,"agentRole":null,"gitInfo":git_info,"name":wire["name"],"turns":[]}))
}
pub fn logged_turn_to_wire_turn(turn: &LoggedTurn) -> Value {
    logged_turn_with_view(turn,"full")
}
pub fn logged_turn_with_view(turn: &LoggedTurn,view: &str) -> Value {
    let items = turn.items.iter().map(|item|wire_item_to_thread_item(&Value::Object(item.clone()))).collect::<Vec<_>>();
    let items = match view {"notLoaded"=>Vec::new(),"summary"=>{
        let first = items.iter().find(|item|item["type"] == "userMessage");
        let last = items.iter().rev().find(|item|item["type"] == "agentMessage");
        match (first,last) {(Some(first),Some(last)) if first["id"] != last["id"]=>vec![first.clone(),last.clone()],(Some(first),_)=>vec![first.clone()],(_,Some(last))=>vec![last.clone()],_=>Vec::new()}
    },_=>items};
    json!({"id":turn.turn_id,"items":items,"itemsView":view,"status":match turn.status { TurnStatus::Running => "inProgress", TurnStatus::Completed => "completed", TurnStatus::Failed => "failed", TurnStatus::Interrupted => "interrupted" },"error":turn.error.as_ref().map(|message|json!({"message":message,"codexErrorInfo":"other","additionalDetails":null})),"startedAt":iso_seconds(&turn.started_at),"completedAt":turn.completed_at.as_deref().and_then(iso_seconds),"durationMs":turn.duration_ms})
}
pub fn wire_item_to_thread_item(item: &Value) -> Value {
    let mut item = item.clone();
    let kind = item["type"].as_str().unwrap_or("agentMessage").to_owned();
    item["id"] = json!(item["id"].as_str().filter(|id|!id.is_empty()).unwrap_or("item"));
    item["type"] = json!(kind);
    match kind.as_str() {
        "userMessage"=>{item["clientId"] = item["clientId"].as_str().map_or(Value::Null,|id|json!(id));if !item["content"].is_array() {item["content"] = json!([]);}},
        "reasoning"=>{
            if !item["summary"].is_array() {item["summary"] = json!([]);}
            if !item["content"].is_array() {item["content"] = json!([item["text"].as_str().unwrap_or_default()]);}
        },
        "plan"|"agentMessage"=>{item["text"] = json!(item["text"].as_str().unwrap_or_default());if kind == "agentMessage" {item["phase"] = item["phase"].clone();item["memoryCitation"] = item["memoryCitation"].clone();}},
        _=>{},
    }
    item
}
pub fn turns_from_session_entries(entries: &[Value],fallback: &str) -> Vec<LoggedTurn> {
    let mut turns: Vec<LoggedTurn> = Vec::new();
    for entry in entries {
        if entry["type"] != "message" {continue;}
        let message = &entry["message"];
        if message["role"] == "user" {
            let content = match &message["content"] {Value::String(text)=>vec![json!({"type":"text","text":text,"text_elements":[]})],Value::Array(blocks)=>blocks.iter().filter(|block|block["type"] == "text" && block["text"].is_string()).map(|block|json!({"type":"text","text":block["text"],"text_elements":[]})).collect(),_=>Vec::new()};
            let item = serde_json::Map::from_iter([("id".into(),entry["id"].clone()),("type".into(),json!("userMessage")),("content".into(),json!(content))]);
            turns.push(LoggedTurn {turn_id:format!("turn-{}",turns.len()+1),started_at:entry["timestamp"].as_str().filter(|value|!value.is_empty()).unwrap_or(fallback).into(),completed_at:None,duration_ms:None,error:None,status:TurnStatus::Completed,items:vec![item]});
        } else if message["role"] == "assistant" && let Some(current) = turns.last_mut() {
            let text = message["content"].as_array().into_iter().flatten().filter(|block|block["type"] == "text").filter_map(|block|block["text"].as_str()).collect::<String>();
            if !text.is_empty() {current.items.push(serde_json::Map::from_iter([("id".into(),entry["id"].clone()),("type".into(),json!("agentMessage")),("text".into(),json!(text)),("phase".into(),Value::Null),("memoryCitation".into(),Value::Null)]));}
        }
    }
    turns
}
pub async fn build_wire_thread(entry: &ThreadEntry, log: &mut TurnLog, include_turns: bool, version: &str) -> Result<Value, MetadataStateError> {
    let model = entry.session.model();
    let path = entry.session.session_file();
    let git_info = match &path {
        Some(path) => ThreadMetadataState.read_git_info(std::path::Path::new(path)).await?,
        None => None,
    };
    let turns = if include_turns {
        let logged = log.read_turns(&entry.id);
        let logged = if logged.is_empty() {entry.session.with_session_manager(|manager|turns_from_session_entries(&manager.entries(),&entry.created_at))} else {logged};
        logged.iter().map(logged_turn_to_wire_turn).collect::<Vec<_>>()
    } else { Vec::new() };
    Ok(json!({"id":entry.id,"sessionId":entry.session.session_id(),"forkedFromId":null,"parentThreadId":null,"preview":entry.session.get_user_messages_for_forking().first().map(|(_,text)|text.as_str()).unwrap_or_default(),"ephemeral":false,"modelProvider":model.provider,"createdAt":iso_seconds(&entry.created_at),"updatedAt":iso_seconds(&entry.updated_at),"recencyAt":iso_seconds(&entry.updated_at),"status":if entry.active_turn.is_some() {json!({"type":"active","activeFlags":[]})} else {json!({"type":"idle"})},"path":path,"cwd":entry.cwd,"cliVersion":version,"source":"appServer","threadSource":null,"agentNickname":null,"agentRole":null,"gitInfo":git_info,"name":entry.session.session_name(),"turns":turns}))
}
