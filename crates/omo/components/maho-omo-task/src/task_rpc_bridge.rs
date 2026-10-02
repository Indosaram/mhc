use std::{collections::BTreeMap, sync::{Arc, Mutex, PoisonError}};
use maho_ext_api::{ExtensionApi, ExtensionFailure};
use serde_json::{Value, json};
use senpi_task::{manager::{TaskManager, Unsubscribe, types::ListScope}, state::{TaskStatus, ResidencyState}, progress::{create_child_progress, ChildProgressTarget}, tools::{control::{cancel::run_task_cancel, send::run_task_send}, output::{output::run_task_output, types::TaskOutputDeps}}};
use crate::task_rpc_codec::{task_snapshot, live_progress_snapshot, parse_task_send, parse_task_cancel, parse_task_output, invalid_arguments, bounded_task_output};
pub type TaskRpcEmit = Arc<dyn Fn(&str, Value) + Send + Sync>;
#[derive(Default)] struct State { session: Option<String>, fingerprint: Option<String>, disposed: bool, subscriptions: BTreeMap<String, Unsubscribe>, progress: BTreeMap<String, Value> }
pub struct TaskRpcBridge { manager: Arc<TaskManager>, session_id: Arc<dyn Fn() -> Option<String> + Send + Sync>, state_dir: String, emit: TaskRpcEmit, state: Mutex<State> }
pub fn wire_task_rpc_bridge(api: &mut ExtensionApi, manager: Arc<TaskManager>, session_id: Arc<dyn Fn() -> Option<String> + Send + Sync>, state_dir: String, emit: TaskRpcEmit) -> Result<Arc<TaskRpcBridge>, ExtensionFailure> {
    let bridge = Arc::new(TaskRpcBridge { manager, session_id, state_dir, emit, state: Mutex::new(State::default()) });
    for name in ["omo.task.send","omo.task.cancel","omo.task.output"] { let bridge = bridge.clone(); api.rpc_handle(name, Arc::new(move |value| { let result = bridge.request(name,&value); Box::pin(async move { result }) }))?; }
    Ok(bridge)
}
impl TaskRpcBridge {
    pub fn request(&self, name: &str, value: &Value) -> Result<Value, ExtensionFailure> {
        let Some(session) = self.state.lock().unwrap_or_else(PoisonError::into_inner).session.clone() else { return Ok(json!({"kind":"unavailable","reason":"No active parent session."})); };
        let error = |error: String| ExtensionFailure::new(error);
        match name {
            "omo.task.send" => {
                let input = match parse_task_send(value) { Ok(input) => input, Err(reason) => return Ok(invalid_arguments(&reason)) };
                if self.manager.get(&input.to).is_none_or(|record| record.parent_session_id != session) { return Ok(json!({"kind":"not_found","reason":"Task not found."})); }
                serde_json::to_value(run_task_send(self.manager.as_ref(), &input, Some(&session), None).map_err(|failure| error(failure.to_string()))?.details).map_err(|failure| error(failure.to_string()))
            },
            "omo.task.cancel" => {
                let input = match parse_task_cancel(value) { Ok(input) => input, Err(reason) => return Ok(invalid_arguments(&reason)) };
                if input.task_id.as_ref().and_then(|id| self.manager.get(id)).is_none_or(|record| record.parent_session_id != session) { return Ok(json!({"kind":"not_found","reason":"Task not found."})); }
                serde_json::to_value(run_task_cancel(self.manager.as_ref(), &input).details).map_err(|failure| error(failure.to_string()))
            },
            "omo.task.output" => {
                let input = match parse_task_output(value) { Ok(input) => input, Err(reason) => return Ok(invalid_arguments(&reason)) };
                let deps = TaskOutputDeps { manager: self.manager.clone(), state_dir: self.state_dir.clone(), transcript_reader: None, resolve_caller_session_id: None, now: None };
                bounded_task_output(&run_task_output(&deps,&input,Some(&session)).map_err(|failure| error(failure.to_string()))?.details).map_err(|failure| error(failure.to_string()))
            },
            _ => Ok(invalid_arguments("Unknown task query.")),
        }
    }
    pub fn attach(self: &Arc<Self>) { if self.state.lock().unwrap_or_else(PoisonError::into_inner).disposed { return; } self.detach(); self.state.lock().unwrap_or_else(PoisonError::into_inner).session = (self.session_id)(); self.sync(); }
    pub fn sync(self: &Arc<Self>) {
        let Some(session) = self.state.lock().unwrap_or_else(PoisonError::into_inner).session.clone() else { return; };
        let all: Vec<_> = self.manager.list(&ListScope::ParentSession(session)).into_iter().map(|entry| entry.record).collect(); let truncated = all.len().saturating_sub(256);
        let records = if truncated == 0 { all } else { let live: Vec<_> = all.iter().filter(|record| matches!(record.status,TaskStatus::Pending|TaskStatus::Running)).cloned().collect(); let live: Vec<_> = live.into_iter().rev().take(256).collect::<Vec<_>>().into_iter().rev().collect(); let slots = 256-live.len(); let terminal: Vec<_> = all.into_iter().filter(|record| !matches!(record.status,TaskStatus::Pending|TaskStatus::Running)).collect(); live.into_iter().chain(terminal.into_iter().rev().take(slots).collect::<Vec<_>>().into_iter().rev()).collect() };
        let wanted: Vec<_> = records.iter().filter(|record| matches!(record.status,TaskStatus::Pending|TaskStatus::Running) && record.residency_state == ResidencyState::Resident).map(|record| record.task_id.clone()).collect();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner); let stale: Vec<_> = state.subscriptions.keys().filter(|id| !wanted.contains(id)).cloned().collect();
        let mut removed = Vec::new(); for id in stale { if let Some(unsubscribe) = state.subscriptions.remove(&id) { removed.push(unsubscribe); } state.progress.remove(&id); } drop(state); for unsubscribe in removed { unsubscribe(); }
        for record in &records {
            if !wanted.contains(&record.task_id) || self.state.lock().unwrap_or_else(PoisonError::into_inner).subscriptions.contains_key(&record.task_id) { continue; }
            let started = chrono::DateTime::parse_from_rfc3339(&record.created_at).ok().and_then(|time| u64::try_from(time.timestamp_millis()).ok()).unwrap_or(0);
            let progress = Arc::new(Mutex::new(create_child_progress(&record.task_id,ChildProgressTarget { category:record.category.clone(),agent_type:record.agent_type.clone(),resolved_model:record.resolved_model.clone(),model:Some(record.model.clone()),name:record.name.clone(),description:record.description.clone(),task_summary:record.task_summary.clone() },started,|| u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or_default())));
            let weak = Arc::downgrade(self); let id = record.task_id.clone();
            let unsubscribe = self.manager.subscribe_child(&record.task_id,Arc::new(move |event| { let mut progress = progress.lock().unwrap_or_else(PoisonError::into_inner); if !progress.accept(event) { return; } let snapshot = live_progress_snapshot(&progress.details()); drop(progress); if let Some(bridge) = weak.upgrade() { bridge.state.lock().unwrap_or_else(PoisonError::into_inner).progress.insert(id.clone(),snapshot); bridge.emit_snapshot(); } }));
            self.state.lock().unwrap_or_else(PoisonError::into_inner).subscriptions.insert(record.task_id.clone(),unsubscribe);
        }
        self.emit_records(&records,truncated);
    }
    fn emit_snapshot(self: &Arc<Self>) { self.sync(); }
    fn emit_records(&self, records: &[senpi_task::state::TaskRecord], truncated: usize) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner); let Some(session) = &state.session else { return; }; if state.disposed { return; }
        let mut tasks = Vec::new();
        for record in records { let stats = if matches!(record.status,TaskStatus::Pending|TaskStatus::Running) { self.manager.run_stats_snapshot(&record.task_id) } else { None }; match task_snapshot(record,stats.as_ref(),state.progress.get(&record.task_id)) { Ok(snapshot) => tasks.push(snapshot), Err(error) => { eprintln!("task RPC snapshot serialization failed: {error}"); return; } } }
        let mut payload = json!({"parent_session_id":session,"tasks":tasks}); if truncated > 0 { payload["truncated_tasks"] = json!(truncated); } let fingerprint = payload.to_string(); if state.fingerprint.as_ref() == Some(&fingerprint) { return; } state.fingerprint = Some(fingerprint); drop(state); (self.emit)("omo.task.updated",payload);
    }
    pub fn detach(&self) { let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner); let subscriptions = std::mem::take(&mut state.subscriptions); state.progress.clear(); state.session = None; state.fingerprint = None; drop(state); for unsubscribe in subscriptions.into_values() { unsubscribe(); } }
    pub fn dispose(&self) { self.state.lock().unwrap_or_else(PoisonError::into_inner).disposed = true; self.detach(); }
}
