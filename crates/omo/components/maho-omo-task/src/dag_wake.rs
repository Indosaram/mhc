use std::collections::BTreeMap;
use serde_json::{Value, json};
use senpi_task::{completion::ParentState, dag::types::DagNodeCounts};

pub const DAG_WAKE_MESSAGE_TYPE: &str = "omo-senpi.dag-run";
pub struct DagWakeInjection { pub key: String, pub source: &'static str, pub custom_type: &'static str, pub content: String, pub display: bool, pub details: Value }
pub trait DagWakeCoordinator {
    fn enqueue(&self, injection: DagWakeInjection);
    fn schedule_flush(&self);
    fn flush_soon(&self);
}
#[derive(Default)]
pub struct DagWake { buffered: BTreeMap<String, Vec<DagWakeInjection>> }
pub struct DagWakeRun<'a> { pub run_id: &'a str, pub name: &'a str, pub parent_session_id: &'a str }
pub struct DagWakeFailure<'a> { pub code: &'a str, pub message: &'a str, pub node_id: Option<&'a str> }
impl DagWake {
    pub fn on_run_event(&mut self, run: DagWakeRun<'_>, event_type: &str, counts: Option<DagNodeCounts>, failure: Option<DagWakeFailure<'_>>, state: ParentState, coordinator: &dyn DagWakeCoordinator) {
        let status = match event_type { "dag.run.completed" => "completed", "dag.run.failed" => "failed", "dag.run.cancelled" => "cancelled", _ => return };
        let Some(counts) = counts else { return };
        let mut content = format!("DAG \"{}\" {status}: {} completed, {} failed, {} cancelled, {} skipped ({} total)", run.name, counts.completed, counts.failed, counts.cancelled, counts.skipped, counts.total);
        let mut details = json!({"runId":run.run_id,"name":run.name,"status":status,"counts":counts});
        if let Some(failure) = failure {
            let node = failure.node_id.map_or_else(String::new, |node| format!(" at {node}"));
            content.push_str(&format!(". First failure{node} [{}]: {}", failure.code, failure.message));
            let mut value = json!({"code":failure.code,"message":failure.message});
            if let Some(node) = failure.node_id { value["nodeId"] = json!(node); }
            details["firstFailure"] = value;
        }
        let injection = DagWakeInjection { key: format!("dag-run:{}", run.run_id), source: "dag-run", custom_type: DAG_WAKE_MESSAGE_TYPE, content, display: false, details };
        match state {
            ParentState::Compacting | ParentState::SessionSwitching | ParentState::SessionShutdown => {
                let entries=self.buffered.entry(run.parent_session_id.into()).or_default();
                if let Some(previous)=entries.iter_mut().find(|previous| previous.key==injection.key) { *previous=injection; } else { entries.push(injection); }
            }
            ParentState::Idle => { coordinator.enqueue(injection); coordinator.flush_soon(); }
            ParentState::Streaming => { coordinator.enqueue(injection); coordinator.schedule_flush(); }
        }
    }
    pub fn on_session_start(&mut self, session_id: Option<&str>, coordinator: &dyn DagWakeCoordinator) {
        let Some(session) = session_id else { return };
        let Some(entries) = self.buffered.remove(session) else { return };
        for injection in entries { coordinator.enqueue(injection); }
        coordinator.flush_soon();
    }
    pub fn buffered_count(&self, session: &str) -> usize { self.buffered.get(session).map_or(0, Vec::len) }
}
