use std::{panic::{AssertUnwindSafe, catch_unwind}, sync::Arc};
use maho_ext_api::EventBus;
use serde_json::{Value, json};
use crate::dag_status_ui::DagStatusUiManager;

pub const DAG_WAKE_SOURCE_STATE_EVENT: &str = "wake_source_state";
pub const DAG_WAKE_SOURCE: &str = "omo-dag";
pub struct DagWakeSourceDeps {
    pub events: EventBus,
    pub manager: Arc<dyn DagStatusUiManager>,
    pub session_id: Arc<dyn Fn() -> Option<String> + Send + Sync>,
}
pub struct DagWakeSource { pub events: EventBus, pub manager: Arc<dyn DagStatusUiManager>, pub session_id: Arc<dyn Fn() -> Option<String> + Send + Sync> }
pub fn create_dag_wake_source(deps: DagWakeSourceDeps) -> DagWakeSource {
    DagWakeSource { events: deps.events, manager: deps.manager, session_id: deps.session_id }
}
impl DagWakeSource {
    pub fn on_run_start(&self, _run_id: &str) { self.publish_live(); }
    pub fn on_run_terminal(&self, _run_id: &str) { self.publish_live(); }
    pub fn publish_live(&self) {
        let channels: Vec<Value> = (self.session_id)().map_or_else(Vec::new, |session| self.manager.list(&session).into_iter().filter(|run| !run.status.is_terminal()).filter_map(|run| self.read_snapshot(&run.run_id, &session)).map(|run| {
            let date = run.started_at.as_deref().unwrap_or(&run.created_at);
            let started = chrono::DateTime::parse_from_rfc3339(date).map_or(Value::Null, |date| json!(date.timestamp_millis()));
            json!({"id":run.run_id,"description":run.name,"startedAtMs":started})
        }).collect());
        self.publish(channels);
    }
    fn read_snapshot(&self, run_id: &str, session: &str) -> Option<senpi_task::dag::types::DagRunSnapshot> {
        catch_unwind(AssertUnwindSafe(|| self.manager.snapshot(run_id, session))).ok().flatten()
    }
    pub fn emit_shutdown(&self) { self.publish(vec![]); }
    /// senpi#3864: a live workflow run is children's work, not a background job, so the published
    /// state carries `kind: "children"` and continuation keeps the parent's lifecycle off "done".
    fn publish(&self, channels: Vec<Value>) {
        self.events.emit(DAG_WAKE_SOURCE_STATE_EVENT, &json!({"source":DAG_WAKE_SOURCE,"kind":"children","activeCount":channels.len(),"channels":channels}));
    }
}
