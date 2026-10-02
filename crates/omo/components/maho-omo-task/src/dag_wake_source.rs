use std::sync::Arc;
use maho_ext_api::EventBus;
use serde_json::{Value, json};
use crate::dag_status_ui::DagStatusUiManager;

pub const DAG_WAKE_SOURCE_STATE_EVENT: &str = "wake_source_state";
pub const DAG_WAKE_SOURCE: &str = "omo-dag";
pub struct DagWakeSource { pub events: EventBus, pub manager: Arc<dyn DagStatusUiManager>, pub session_id: Arc<dyn Fn() -> Option<String> + Send + Sync> }
impl DagWakeSource {
    pub fn publish_live(&self) {
        let channels: Vec<Value> = (self.session_id)().map_or_else(Vec::new, |session| self.manager.list(&session).into_iter().filter(|run| !run.status.is_terminal()).filter_map(|run| self.manager.snapshot(&run.run_id, &session)).map(|run| {
            let date = run.started_at.as_deref().unwrap_or(&run.created_at);
            let started = chrono::DateTime::parse_from_rfc3339(date).map_or(Value::Null, |date| json!(date.timestamp_millis()));
            json!({"id":run.run_id,"description":run.name,"startedAtMs":started})
        }).collect());
        self.publish(channels);
    }
    pub fn emit_shutdown(&self) { self.publish(vec![]); }
    fn publish(&self, channels: Vec<Value>) { self.events.emit(DAG_WAKE_SOURCE_STATE_EVENT, &json!({"source":DAG_WAKE_SOURCE,"activeCount":channels.len(),"channels":channels})); }
}
