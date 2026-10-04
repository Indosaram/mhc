use std::{collections::{BTreeMap, BTreeSet}, path::PathBuf, sync::{Arc, Mutex, PoisonError}};
use maho_ext_api::{CustomMessage, ToolContent};
use serde_json::{Value, json};
use senpi_task::{state::{TaskRecord, TaskStatus, ResidencyState}, team::{liveness_ownership::parse_team_member_task_identity, messaging::session_marker_index::{SessionMarkerIndex, create_incremental_session_marker_index}}};
pub const TEAM_MEMBER_LIVENESS_MESSAGE_TYPE: &str = "senpi-task.team-member-liveness";
const PREFIX: &str = "team-member-liveness:";
pub type LivenessDelivery = Arc<dyn Fn(&str, CustomMessage, senpi_task::completion::DeliveryCallbacks) -> Result<(), String> + Send + Sync>;
pub type RecordCallback = Arc<dyn Fn(&TaskRecord) + Send + Sync>;
pub struct TeamMemberLivenessDeps {
    pub deliver: LivenessDelivery,
    pub was_delivered: Arc<dyn Fn(&TaskRecord) -> bool + Send + Sync>,
    pub mark_delivered: RecordCallback,
    pub on_error: Arc<dyn Fn(String) + Send + Sync>,
    pub timers: Arc<dyn crate::status_ui::StatusUiTimers>,
    pub max_delivery_retries: Option<usize>,
    pub max_persistence_retries: Option<usize>,
}
pub fn bind_liveness_store_markers(mut deps:TeamMemberLivenessDeps,store:senpi_task::store::TaskRecordStore)->TeamMemberLivenessDeps {
    let read_store=store.clone(); let read_error=deps.on_error.clone();
    deps.was_delivered=Arc::new(move |record| {
        match read_store.load(&record.task_id) {
            Ok(fresh) => fresh.as_ref().unwrap_or(record).notification.liveness_notified_epoch.unwrap_or(-1)>=record.notification.run_epoch,
            Err(error) => { read_error(format!("omo-senpi team liveness marker read failed: taskId={} error={error}",record.task_id)); false }
        }
    });
    let write_error=deps.on_error.clone();
    deps.mark_delivered=Arc::new(move |record| {
        let epoch=record.notification.run_epoch;
        if let Err(error)=store.mutate(&record.task_id,|fresh| {
            let mut updated=fresh.clone();
            if fresh.status==record.status && fresh.notification.run_epoch==epoch && fresh.notification.liveness_notified_epoch.unwrap_or(-1)<epoch { updated.notification.liveness_notified_epoch=Some(epoch); }
            updated
        }) { write_error(format!("omo-senpi team liveness marker write failed: taskId={} error={error}",record.task_id)); }
    });
    deps
}
#[derive(Default)] struct State { delivered: BTreeSet<String>, pending: BTreeMap<String, TaskRecord>, retries: BTreeMap<String, usize>, persistence_active: bool, session_file: Option<Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>> }
pub struct TeamMemberLivenessNotifier { deps: TeamMemberLivenessDeps, state: Mutex<State>, markers: SessionMarkerIndex }
pub fn liveness_details(record: &TaskRecord) -> Option<Value> {
    let member = parse_team_member_task_identity(record.name.as_deref())?.member_name;
    if (!matches!(record.status, TaskStatus::Error | TaskStatus::Lost) && record.killed != Some(true)) || matches!(record.residency_state, ResidencyState::PersistedOnly | ResidencyState::RpcDetached) { return None; }
    let mut details = json!({"memberName":member,"lastKnownState":record.status.as_str()});
    if let Some(reason) = &record.error_message { details["reason"] = json!(reason); }
    if record.killed == Some(true) { details["killed"] = json!(true); } Some(details)
}
pub fn liveness_content(details: &Value) -> String {
    let reason = details.get("reason").and_then(Value::as_str).map_or_else(String::new, |reason| format!(" Reason: {reason}"));
    format!("Team member liveness: {} exited abnormally; last known state: {}.{reason}", details["memberName"].as_str().unwrap_or_default(), details["lastKnownState"].as_str().unwrap_or_default())
}
fn message_keys(message: &Value) -> Vec<String> {
    let read = |details: &Value| details.get("deliveryKey").and_then(Value::as_str).filter(|key| key.starts_with(PREFIX)).map(str::to_owned);
    if message.get("customType").and_then(Value::as_str) == Some(TEAM_MEMBER_LIVENESS_MESSAGE_TYPE) { return message.get("details").and_then(read).into_iter().collect(); }
    if message.get("customType").and_then(Value::as_str) != Some("omo-senpi:wake") { return Vec::new(); }
    message.get("details").and_then(Value::as_array).into_iter().flatten().filter(|entry| entry.get("customType").and_then(Value::as_str) == Some(TEAM_MEMBER_LIVENESS_MESSAGE_TYPE)).filter_map(|entry| entry.get("details").and_then(read)).collect::<BTreeSet<_>>().into_iter().collect()
}
pub fn liveness_delivery_keys(payload: &Value) -> Vec<String> { payload.get("message").map_or_else(Vec::new, message_keys) }
pub fn liveness_delivery_keys_from_session_text(text: &str) -> Vec<String> { text.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok()).filter(|entry| entry.get("type").and_then(Value::as_str) == Some("custom_message")).flat_map(|entry| message_keys(&entry)).collect::<BTreeSet<_>>().into_iter().collect() }
pub fn create_team_member_liveness_notifier(deps: TeamMemberLivenessDeps) -> Arc<TeamMemberLivenessNotifier> { Arc::new(TeamMemberLivenessNotifier { deps, state: Mutex::new(State::default()), markers: create_incremental_session_marker_index(Box::new(liveness_delivery_keys_from_session_text), None) }) }
impl TeamMemberLivenessNotifier {
    pub fn notify_terminal(self: &Arc<Self>, record: &TaskRecord) {
        let Some(mut details) = liveness_details(record) else { return; }; if (self.deps.was_delivered)(record) { return; }
        let key = format!("{PREFIX}{}:{}", record.task_id, record.notification.run_epoch);
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner); if !state.delivered.insert(key.clone()) { return; } state.pending.insert(key.clone(), record.clone()); drop(state);
        let content = liveness_content(&details); details["deliveryKey"] = json!(key);
        let weak = Arc::downgrade(self); let failed_key = key.clone(); let failed_record = record.clone();
        let callbacks = senpi_task::completion::DeliveryCallbacks::new(move |result| {
            if let Err(error) = result && let Some(notifier) = weak.upgrade() { notifier.fail_delivery(&failed_key, &failed_record, error.message); }
        });
        if let Err(error) = (self.deps.deliver)(&key, CustomMessage { custom_type: TEAM_MEMBER_LIVENESS_MESSAGE_TYPE.into(), content: vec![ToolContent::text(content)], display: false, details: Some(details) }, callbacks.clone()) { callbacks.failed(senpi_task::host::HostError { message: error }); }
    }
    pub fn fail_delivery(self: &Arc<Self>, key: &str, record: &TaskRecord, error: String) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner); if !state.pending.contains_key(key) { return; } state.pending.remove(key); state.delivered.remove(key);
        let count = state.retries.get(key).copied().unwrap_or(0); if count >= self.deps.max_delivery_retries.unwrap_or(3) { state.retries.remove(key); drop(state); (self.deps.on_error)(error); return; }
        state.retries.insert(key.to_owned(), count+1); drop(state); (self.deps.on_error)(error);
        let weak = Arc::downgrade(self); let record = record.clone(); self.deps.timers.set(Box::new(move || { if let Some(notifier) = weak.upgrade() { notifier.notify_terminal(&record); } }),50);
    }
    pub fn acknowledge_persisted(self: &Arc<Self>, session_file: Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner); state.session_file = Some(session_file); if state.persistence_active || state.pending.is_empty() { return; } state.persistence_active = true; drop(state); self.check_persistence(self.deps.max_persistence_retries.unwrap_or(3));
    }
    fn check_persistence(self: &Arc<Self>, retries: usize) {
        let (file, pending) = { let state = self.state.lock().unwrap_or_else(PoisonError::into_inner); (state.session_file.as_ref().and_then(|file| file()), state.pending.clone()) };
        for (key, record) in pending { match self.markers.contains(file.as_deref(), &key) { Ok(true) => { (self.deps.mark_delivered)(&record); let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner); state.pending.remove(&key); state.delivered.remove(&key); state.retries.remove(&key); }, Ok(false) => {}, Err(error) => { (self.deps.on_error)(error.to_string()); break; } } }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner); if state.pending.is_empty() || retries == 0 { state.persistence_active = false; return; } drop(state);
        let weak = Arc::downgrade(self); self.deps.timers.set(Box::new(move || { if let Some(notifier) = weak.upgrade() { notifier.check_persistence(retries-1); } }),50);
    }
}
