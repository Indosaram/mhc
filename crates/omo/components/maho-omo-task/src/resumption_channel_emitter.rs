use std::sync::Arc;
use maho_ext_api::EventBus;
use serde_json::{json, Value};
use senpi_task::state::TaskRecord;
use crate::status_row_format::task_status_description;

pub const RESUMPTION_CHANNEL_STATE_EVENT: &str = "wake_source_state";

pub trait ResumptionChannelManager: Send + Sync {
    fn list(&self, session_id: &str) -> Vec<TaskRecord>;
    fn was_background(&self, task_id: &str) -> bool;
    fn is_owned_team_member(&self, record: &TaskRecord, session_id: &str) -> bool;
}
pub struct TaskResumptionChannelManager {
    pub manager: Arc<senpi_task::manager::TaskManager>,
    pub ownership: senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps,
}
impl ResumptionChannelManager for TaskResumptionChannelManager {
    fn list(&self,session:&str)->Vec<TaskRecord> {
        self.manager.list(&senpi_task::manager::types::ListScope::ParentSession(session.into())).into_iter().map(|entry| entry.record).collect()
    }
    fn was_background(&self,id:&str)->bool { self.manager.was_background(id) }
    fn is_owned_team_member(&self,record:&TaskRecord,session:&str)->bool {
        senpi_task::team::liveness_ownership::is_owned_team_member_task(record.name.as_deref(),Some(session),&self.ownership)
    }
}

pub struct ResumptionChannelEmitter {
    pub events: EventBus,
    pub manager: Arc<dyn ResumptionChannelManager>,
    pub session_id: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    active: bool,
    last_count: usize,
}

impl ResumptionChannelEmitter {
    pub fn new(events: EventBus, manager: Arc<dyn ResumptionChannelManager>,
        session_id: Arc<dyn Fn() -> Option<String> + Send + Sync>) -> Self {
        Self { events, manager, session_id, active: false, last_count: 0 }
    }

    fn snapshot(&self) -> Vec<Value> {
        let Some(session) = (self.session_id)() else { return vec![] };
        self.manager.list(&session).into_iter().filter(|record| !record.status.is_terminal())
            .filter(|record| self.manager.was_background(&record.task_id)
                || self.manager.is_owned_team_member(record, &session))
            .map(|record| {
                let started = chrono::DateTime::parse_from_rfc3339(&record.created_at)
                    .map_or(Value::Null, |date| json!(date.timestamp_millis()));
                json!({"id": record.task_id, "description": task_status_description(&record), "startedAtMs": started})
            }).collect()
    }

    fn publish(&mut self, channels: Vec<Value>) {
        self.last_count = channels.len();
        self.events.emit(RESUMPTION_CHANNEL_STATE_EVENT,
            &json!({"source":"senpi-task", "activeCount":channels.len(), "channels":channels}));
    }

    pub fn emit_if_changed(&mut self) {
        if !self.active { return; }
        let channels = self.snapshot();
        if channels.len() != self.last_count { self.publish(channels); }
    }

    pub fn emit_session_start(&mut self) {
        self.active = true;
        self.publish(self.snapshot());
    }

    pub fn emit_shutdown(&mut self) {
        self.active = false;
        self.publish(vec![]);
    }
}
