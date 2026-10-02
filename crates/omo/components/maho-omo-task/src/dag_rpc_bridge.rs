use std::{collections::BTreeMap, sync::{Arc, Mutex, PoisonError}};
use serde_json::{Value, json};
use senpi_task::manager::Unsubscribe;
use crate::{dag_rpc_bridge_contract::DagRpcBridgeDeps, dag_snapshot_payload::dag_updated_payload};
pub const DAG_DEFAULT_HEARTBEAT_MS: u64 = 15000;
pub const DAG_ACTIVITY_COALESCE_MS: u64 = 150;
pub const DAG_SNAPSHOT_DEBOUNCE_MS: u64 = 50;
#[derive(Default)]
struct State {
    subscriptions: BTreeMap<String, Unsubscribe>, head_seq: BTreeMap<String, u64>, pending_activity: Vec<(String, Value)>,
    heartbeat: Option<u64>, activity_flush: Option<u64>, snapshot_flush: Option<u64>, fingerprint: Option<String>, attached: bool, disposed: bool,
}
pub struct DagRpcBridge { deps: DagRpcBridgeDeps, state: Mutex<State> }
pub fn create_dag_rpc_bridge(deps: DagRpcBridgeDeps) -> Arc<DagRpcBridge> { Arc::new(DagRpcBridge { deps, state: Mutex::new(State::default()) }) }
impl DagRpcBridge {
    pub fn attach(self: &Arc<Self>) {
        if self.state.lock().unwrap_or_else(PoisonError::into_inner).disposed { return; }
        self.detach(); self.state.lock().unwrap_or_else(PoisonError::into_inner).attached = true; self.sync();
    }
    pub fn forward(&self, event: &Value) {
        let Some(run) = event.get("runId").and_then(Value::as_str) else { return; };
        let Some(seq) = event.get("seq").and_then(Value::as_u64) else { return; };
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.attached || seq <= state.head_seq.get(run).copied().unwrap_or(0) { return; }
        state.head_seq.insert(run.to_owned(), seq); drop(state); (self.deps.emit)("omo.dag.event", event.clone());
    }
    pub fn sync(self: &Arc<Self>) {
        if !self.state.lock().unwrap_or_else(PoisonError::into_inner).attached { return; }
        for run in (self.deps.live_runs)() {
            if self.state.lock().unwrap_or_else(PoisonError::into_inner).subscriptions.contains_key(&run.run_id) { continue; }
            let weak = Arc::downgrade(self);
            let unsubscribe = (run.subscribe)(Arc::new(move |event| { if let Some(bridge) = weak.upgrade() { bridge.forward(event); } }));
            self.state.lock().unwrap_or_else(PoisonError::into_inner).subscriptions.insert(run.run_id, unsubscribe);
        }
        self.schedule_heartbeat(); self.notify_store_mutation();
    }
    fn schedule_heartbeat(self: &Arc<Self>) {
        let live = (self.deps.live_runs)().into_iter().any(|run| !matches!(run.status.as_str(), "completed" | "failed" | "cancelled"));
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.attached || state.heartbeat.is_some() || !live { return; }
        let weak = Arc::downgrade(self);
        state.heartbeat = Some(self.deps.timers.set(Box::new(move || { if let Some(bridge) = weak.upgrade() { bridge.beat(); } }), self.deps.heartbeat_ms.unwrap_or(DAG_DEFAULT_HEARTBEAT_MS)));
    }
    fn beat(self: &Arc<Self>) {
        { let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner); state.heartbeat = None; if !state.attached { return; } }
        let runs: Vec<_> = (self.deps.live_runs)().into_iter().filter(|run| !matches!(run.status.as_str(), "completed" | "failed" | "cancelled")).map(|run| { let seq = self.state.lock().unwrap_or_else(PoisonError::into_inner).head_seq.get(&run.run_id).copied().unwrap_or(0); json!({"runId":run.run_id,"headSeq":seq}) }).collect();
        if runs.is_empty() { return; }
        let at = chrono::DateTime::from_timestamp_millis((self.deps.now)()).map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
        (self.deps.emit)("omo.dag.heartbeat", json!({"schemaVersion":1,"at":at,"runs":runs})); self.schedule_heartbeat();
    }
    pub fn publish_activity(self: &Arc<Self>, event: Value) {
        let key = format!("{}\0{}", event.get("runId").and_then(Value::as_str).unwrap_or_default(), event.get("nodeId").and_then(Value::as_str).unwrap_or_default());
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner); if !state.attached { return; }
        if let Some((_, pending)) = state.pending_activity.iter_mut().find(|(pending_key, _)| pending_key == &key) { *pending = event; }
        else { state.pending_activity.push((key, event)); }
        if state.activity_flush.is_some() { return; }
        let weak = Arc::downgrade(self);
        state.activity_flush = Some(self.deps.timers.set(Box::new(move || { if let Some(bridge) = weak.upgrade() { bridge.flush_activity(); } }), self.deps.activity_coalesce_ms.unwrap_or(DAG_ACTIVITY_COALESCE_MS)));
    }
    fn flush_activity(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner); state.activity_flush = None;
        let events = std::mem::take(&mut state.pending_activity); if !state.attached { return; } drop(state);
        for (_, event) in events { (self.deps.emit)("omo.dag.activity", event); }
    }
    pub fn notify_store_mutation(self: &Arc<Self>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner); if !state.attached || state.snapshot_flush.is_some() { return; }
        let weak = Arc::downgrade(self);
        state.snapshot_flush = Some(self.deps.timers.set(Box::new(move || { if let Some(bridge) = weak.upgrade() { bridge.flush_snapshot(); } }), self.deps.snapshot_debounce_ms.unwrap_or(DAG_SNAPSHOT_DEBOUNCE_MS)));
    }
    fn flush_snapshot(&self) {
        { let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner); state.snapshot_flush = None; if !state.attached { return; } }
        let Some(session) = (self.deps.parent_session_id)() else { return; }; let Some(snapshots) = &self.deps.run_snapshots else { return; };
        let data = dag_updated_payload(&session, &snapshots()); let fingerprint = data.to_string();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner); if state.fingerprint.as_ref() == Some(&fingerprint) { return; } state.fingerprint = Some(fingerprint); drop(state);
        (self.deps.emit)("omo.dag.updated", data);
    }
    pub fn detach(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner); state.attached = false;
        let subscriptions = std::mem::take(&mut state.subscriptions); let timers = [state.heartbeat.take(), state.activity_flush.take(), state.snapshot_flush.take()];
        state.head_seq.clear(); state.pending_activity.clear(); state.fingerprint = None; drop(state);
        for unsubscribe in subscriptions.into_values() { unsubscribe(); } for handle in timers.into_iter().flatten() { self.deps.timers.clear(handle); }
    }
    pub fn dispose(&self) { self.state.lock().unwrap_or_else(PoisonError::into_inner).disposed = true; self.detach(); }
}
