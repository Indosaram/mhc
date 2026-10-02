use super::{metadata_state::{MetadataStateError, ThreadMetadataState}, thread_registry::ThreadEntry, turn_log::{LoggedTurn, TurnLog, TurnStatus}};
use serde_json::{Value, json};

fn iso_seconds(value: &str) -> Option<f64> {
    chrono::DateTime::parse_from_rfc3339(value).ok().map(|time| time.timestamp_millis() as f64 / 1000.0)
}
pub fn logged_turn_to_wire_turn(turn: &LoggedTurn) -> Value {
    json!({"id":turn.turn_id,"items":turn.items,"itemsView":"full","status":match turn.status { TurnStatus::Running => "inProgress", TurnStatus::Completed => "completed", TurnStatus::Failed => "failed", TurnStatus::Interrupted => "interrupted" },"error":turn.error.as_ref().map(|message|json!({"message":message,"codexErrorInfo":"other","additionalDetails":null})),"startedAt":iso_seconds(&turn.started_at),"completedAt":turn.completed_at.as_deref().and_then(iso_seconds),"durationMs":turn.duration_ms})
}
pub async fn build_wire_thread(entry: &ThreadEntry, log: &mut TurnLog, include_turns: bool, version: &str) -> Result<Value, MetadataStateError> {
    let model = entry.session.model();
    let path = entry.session.session_file();
    let git_info = match &path {
        Some(path) => ThreadMetadataState::default().read_git_info(std::path::Path::new(path)).await?,
        None => None,
    };
    let turns = if include_turns { log.read_turns(&entry.id).iter().map(logged_turn_to_wire_turn).collect::<Vec<_>>() } else { Vec::new() };
    Ok(json!({"id":entry.id,"sessionId":entry.session.session_id(),"forkedFromId":null,"parentThreadId":null,"preview":entry.session.get_user_messages_for_forking().first().map(|(_,text)|text.as_str()).unwrap_or_default(),"ephemeral":false,"modelProvider":model.provider,"createdAt":iso_seconds(&entry.created_at),"updatedAt":iso_seconds(&entry.updated_at),"recencyAt":iso_seconds(&entry.updated_at),"status":if entry.active_turn.is_some() {json!({"type":"active","activeFlags":[]})} else {json!({"type":"idle"})},"path":path,"cwd":entry.cwd,"cliVersion":version,"source":"appServer","threadSource":null,"agentNickname":null,"agentRole":null,"gitInfo":git_info,"name":entry.session.session_name(),"turns":turns}))
}
