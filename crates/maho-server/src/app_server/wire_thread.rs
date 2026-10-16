use super::{metadata_state::{MetadataStateError, ThreadMetadataState}, thread_registry::ThreadEntry, turn_log::{LoggedTurn, TurnLog, TurnStatus}};
use serde_json::{Value, json};

/// Pinned `wire-thread.ts` `isoSeconds`: `Date.parse(value) / 1000`, or `0` when unparseable.
fn iso_seconds(value: &str) -> f64 {
    super::js_semantics::date_parse_ms(value).map_or(0.0, |millis| millis as f64 / 1000.0)
}
/// Pinned `wire-thread.ts` `toGeneratedStatus`: an active registry status gains `activeFlags: []`.
fn to_generated_status(status: &Value) -> Value {
    match status["type"].as_str() {
        Some("active") => json!({"type":"active","activeFlags":[]}),
        Some("idle") => json!({"type":"idle"}),
        _ => status.clone(),
    }
}
pub async fn build_disk_wire_thread(wire: &Value,version: &str) -> Result<Value,MetadataStateError> {
    let git_info = match wire["sessionPath"].as_str() {Some(path)=>ThreadMetadataState.read_git_info(std::path::Path::new(path)).await?,None=>None};
    let created = iso_seconds(wire["createdAt"].as_str().unwrap_or_default());
    let updated = iso_seconds(wire["updatedAt"].as_str().unwrap_or_default());
    Ok(json!({"id":wire["id"],"sessionId":wire["sessionId"],"forkedFromId":null,"parentThreadId":null,"preview":wire["preview"].as_str().unwrap_or_default(),"ephemeral":false,"modelProvider":"unknown","createdAt":created,"updatedAt":updated,"recencyAt":updated,"status":to_generated_status(&wire["status"]),"path":wire["sessionPath"],"cwd":wire["cwd"],"cliVersion":version,"source":"appServer","threadSource":null,"agentNickname":null,"agentRole":null,"gitInfo":git_info,"name":wire["name"],"turns":[]}))
}
pub fn logged_turn_to_wire_turn(turn: &LoggedTurn) -> Value {
    logged_turn_with_view(turn,"full")
}
pub fn logged_turn_with_view(turn: &LoggedTurn,view: &str) -> Value {
    // Selection reads the raw items with `wire_item_to_thread_item`'s type/id defaults, so only kept items are converted.
    let convert = |item: &serde_json::Map<String,Value>|wire_item_to_thread_item(&Value::Object(item.clone()));
    let items = match view {"notLoaded"=>Vec::new(),"summary"=>{
        fn kind(item: &serde_json::Map<String,Value>) -> &str {item.get("type").and_then(Value::as_str).unwrap_or("agentMessage")}
        fn id(item: &serde_json::Map<String,Value>) -> &str {item.get("id").and_then(Value::as_str).filter(|id|!id.is_empty()).unwrap_or("item")}
        let first = turn.items.iter().find(|item|kind(item) == "userMessage");
        let last = turn.items.iter().rev().find(|item|kind(item) == "agentMessage");
        match (first,last) {(Some(first),Some(last)) if id(first) != id(last)=>vec![convert(first),convert(last)],(Some(first),_)=>vec![convert(first)],(_,Some(last))=>vec![convert(last)],_=>Vec::new()}
    },_=>turn.items.iter().map(convert).collect::<Vec<_>>()};
    json!({"id":turn.turn_id,"items":items,"itemsView":view,"status":match turn.status { TurnStatus::Running => "inProgress", TurnStatus::Completed => "completed", TurnStatus::Failed => "failed", TurnStatus::Interrupted => "interrupted" },"error":turn.error.as_ref().map(|message|json!({"message":message,"codexErrorInfo":"other","additionalDetails":null})),"startedAt":iso_seconds(&turn.started_at),"completedAt":turn.completed_at.as_deref().map(iso_seconds),"durationMs":turn.duration_ms})
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
pub async fn build_wire_thread(entry: &ThreadEntry, log: &tokio::sync::Mutex<TurnLog>, include_turns: bool, version: &str) -> Result<Value, MetadataStateError> {
    let model = entry.session.model();
    let path = entry.session.session_file();
    let git_info = match &path {
        Some(path) => ThreadMetadataState.read_git_info(std::path::Path::new(path)).await?,
        None => None,
    };
    let turns = if include_turns {
        // The TurnLog lock covers only the probe; the session-entry rebuild is synchronous and runs
        // off the async worker so it stalls neither the worker nor other threads' TurnLog writers.
        let logged = {let log = log.lock().await;let logged = log.turns(&entry.id);(!logged.is_empty()).then(||logged.iter().map(logged_turn_to_wire_turn).collect::<Vec<_>>())};
        match logged {
            Some(turns) => turns,
            None => {
                let (session,created_at) = (entry.session.clone(),entry.created_at.clone());
                tokio::task::spawn_blocking(move || session.with_session_manager(|manager|turns_from_session_entries(&manager.entries(),&created_at)).iter().map(logged_turn_to_wire_turn).collect::<Vec<_>>()).await.map_err(|error|MetadataStateError::State {path:entry.id.clone(),message:error.to_string()})?
            }
        }
    } else { Vec::new() };
    Ok(json!({"id":entry.id,"sessionId":entry.session.session_id(),"forkedFromId":null,"parentThreadId":null,"preview":entry.preview().unwrap_or_default(),"ephemeral":false,"modelProvider":model.provider,"createdAt":iso_seconds(&entry.created_at),"updatedAt":iso_seconds(&entry.updated_at),"recencyAt":iso_seconds(&entry.updated_at),"status":if entry.active_turn.is_some() {json!({"type":"active","activeFlags":[]})} else {json!({"type":"idle"})},"path":path,"cwd":entry.cwd,"cliVersion":version,"source":"appServer","threadSource":null,"agentNickname":null,"agentRole":null,"gitInfo":git_info,"name":entry.session.session_name(),"turns":turns}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn old_logged_turn_with_view(turn: &LoggedTurn,view: &str) -> Value {
        let items = turn.items.iter().map(|item|wire_item_to_thread_item(&Value::Object(item.clone()))).collect::<Vec<_>>();
        let items = match view {"notLoaded"=>Vec::new(),"summary"=>{
            let first = items.iter().find(|item|item["type"] == "userMessage");
            let last = items.iter().rev().find(|item|item["type"] == "agentMessage");
            match (first,last) {(Some(first),Some(last)) if first["id"] != last["id"]=>vec![first.clone(),last.clone()],(Some(first),_)=>vec![first.clone()],(_,Some(last))=>vec![last.clone()],_=>Vec::new()}
        },_=>items};
        json!({"id":turn.turn_id,"items":items,"itemsView":view,"status":match turn.status { TurnStatus::Running => "inProgress", TurnStatus::Completed => "completed", TurnStatus::Failed => "failed", TurnStatus::Interrupted => "interrupted" },"error":turn.error.as_ref().map(|message|json!({"message":message,"codexErrorInfo":"other","additionalDetails":null})),"startedAt":iso_seconds(&turn.started_at),"completedAt":turn.completed_at.as_deref().map(iso_seconds),"durationMs":turn.duration_ms})
    }
    fn turn(items: Vec<Value>) -> LoggedTurn {
        LoggedTurn {turn_id:"turn-1".into(),started_at:"2026-01-01T00:00:00Z".into(),completed_at:Some("2026-01-01T00:00:05Z".into()),duration_ms:Some(5000),error:Some("boom".into()),status:TurnStatus::Failed,items:items.into_iter().filter_map(|item|match item {Value::Object(map)=>Some(map),_=>None}).collect()}
    }
    #[test]
    fn logged_turn_with_view_matches_convert_all_then_select() {
        let cases = vec![
            vec![],
            vec![json!({"id":"u1","type":"userMessage","content":"x"}),json!({"id":"r","type":"reasoning","text":"t"}),json!({"id":"a1","type":"agentMessage","text":"one"}),json!({"id":"c","type":"commandExecution","output":"big"}),json!({"id":"a2","text":"untyped is agent"})],
            vec![json!({"type":"userMessage"}),json!({"id":"","type":"agentMessage"})],
            vec![json!({"id":"same","type":"userMessage"}),json!({"id":"same","type":"agentMessage","text":"t"})],
            vec![json!({"id":"a","type":"agentMessage","text":"only agent"}),json!({"id":7,"type":"plan"})],
            vec![json!({"id":"u","type":"userMessage","clientId":"cid","content":[]}),json!({"id":"p","type":"plan"})],
            vec![json!({"type":"reasoning","summary":[]})],
        ];
        for items in cases {
            let turn = turn(items);
            for view in ["full","summary","notLoaded"] {
                assert_eq!(serde_json::to_string(&logged_turn_with_view(&turn,view)).ok(),serde_json::to_string(&old_logged_turn_with_view(&turn,view)).ok(),"view {view}");
            }
        }
    }
}
