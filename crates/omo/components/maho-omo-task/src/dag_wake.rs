use std::{collections::BTreeMap, sync::Arc};
use maho_ext_api::{IdleInjection, IdleInjectionCoordinator, IdleInjectionSource};
use serde_json::{Value, json};
use senpi_task::{completion::ParentState, dag::types::{DagNodeCounts, DagRunEventPayload}};

/// The run reference every DAG wake carries, so a receiver can tell which run owns the injection.
pub fn dag_run_ref(run_id: &str) -> Value { json!({"kind":"dag","id":run_id}) }

/// The text a paused run shows. A user's Stop is NOT an automatic restart: only an explicit Resume
/// on the parent session continues it.
pub fn paused_wake_text(name: &str, reason: Option<&str>) -> String {
    match reason {
        Some("user_stop") => format!("DAG \"{name}\" paused by your Stop. Resume the run to continue."),
        Some(reason) => format!("DAG \"{name}\" paused ({reason}): it will resume when the session restarts."),
        None => format!("DAG \"{name}\" paused: it will resume when the session restarts."),
    }
}

pub const DAG_WAKE_MESSAGE_TYPE: &str = "omo-senpi.dag-run";
pub type DagWakeInjection = IdleInjection;
pub struct DagWake {
    buffered: BTreeMap<String, Vec<DagWakeInjection>>,
    coordinator: Arc<dyn IdleInjectionCoordinator>,
    parent_state: Arc<dyn Fn() -> ParentState + Send + Sync>,
}
pub struct DagWakeRun<'a> { pub run_id: &'a str, pub name: &'a str, pub parent_session_id: &'a str }
pub struct DagWakeFailure<'a> { pub code: &'a str, pub message: &'a str, pub node_id: Option<&'a str> }
pub const DAG_VERIFICATION_DIRECTIVE: &str = "DAG SUBAGENT COMPLETION - TREAT AS FALSE UNTIL YOU PROVE IT.\nThis completion arrived from a DAG subagent. Assume it overstated or fabricated its work. Its summary is a CLAIM, not evidence.\nBefore relying on this result you MUST, in order:\n1. RECONSTRUCT the node's full work scope from its prompt: every deliverable, file, and check it owed.\n2. READ the actual artifacts yourself - open every file it claims it changed, run the commands it claims pass. Transcripts and summaries prove NOTHING.\n3. VERIFY each deliverable against that scope with your own eyes and your own tool calls - in BOTH directions: nothing owed is missing, and nothing beyond the scope was done. Over-engineering, drive-by refactors, and edits outside the prompt's scope are defects, not bonus work.\nIf ANY deliverable is missing, partial, or unproven, or the node drifted out of scope: send precise corrective instructions to THIS node (dag action \"send\" with this run_id and node_id; \"retry\" when it cannot be continued) and demand the fix WITH evidence: complete what is missing, revert what fell outside the scope. Loop until your own verification passes.\nWork is done ONLY when you have verified it yourself.";

pub fn create_dag_wake(coordinator: Arc<dyn IdleInjectionCoordinator>, parent_state: Arc<dyn Fn() -> ParentState + Send + Sync>) -> DagWake {
    DagWake { buffered: BTreeMap::new(), coordinator, parent_state }
}

impl DagWake {
    fn handle_run_event(&mut self, run: DagWakeRun<'_>, event_type: &str, counts: Option<DagNodeCounts>, failure: Option<DagWakeFailure<'_>>, state: ParentState) {
        if event_type == "dag.run.paused" {
            let reason = failure.as_ref().map(|reason| reason.code);
            let mut details = json!({"runId":run.run_id,"name":run.name,"status":"paused","runRef":dag_run_ref(run.run_id)});
            if let Some(reason) = reason { details["reason"] = json!(reason); }
            let injection = make_injection(run.run_id, paused_wake_text(run.name, reason), details);
            self.route(run.parent_session_id, injection, state);
            return;
        }
        let status = match event_type { "dag.run.completed" => "completed", "dag.run.failed" => "failed", "dag.run.cancelled" => "cancelled", _ => return };
        let Some(counts) = counts else { return };
        let mut content = format!("DAG \"{}\" {status}: {} completed, {} failed, {} cancelled, {} skipped ({} total)", run.name, counts.completed, counts.failed, counts.cancelled, counts.skipped, counts.total);
        let mut details = json!({"runId":run.run_id,"name":run.name,"status":status,"counts":counts,"runRef":dag_run_ref(run.run_id)});
        if let Some(failure) = failure {
            let node = failure.node_id.map_or_else(String::new, |node| format!(" at {node}"));
            content.push_str(&format!(". First failure{node} [{}]: {}", failure.code, failure.message));
            let mut value = json!({"code":failure.code,"message":failure.message});
            if let Some(node) = failure.node_id { value["nodeId"] = json!(node); }
            details["firstFailure"] = value;
        }
        content.push_str(&format!("\n\n{DAG_VERIFICATION_DIRECTIVE}"));
        let injection = make_injection(run.run_id, content, details);
        self.route(run.parent_session_id, injection, state);
    }
    fn route(&mut self, parent_session: &str, injection: DagWakeInjection, state: ParentState) {
        match state {
            ParentState::Compacting | ParentState::SessionSwitching | ParentState::SessionShutdown => {
                let entries=self.buffered.entry(parent_session.into()).or_default();
                if let Some(previous)=entries.iter_mut().find(|previous| previous.key==injection.key) { *previous=injection; } else { entries.push(injection); }
            }
            ParentState::Idle => { self.coordinator.enqueue(injection); self.coordinator.flush_soon(); }
            ParentState::Streaming => { self.coordinator.enqueue(injection); self.coordinator.schedule_flush(); }
        }
    }
    pub fn on_run_event(&mut self, run: DagWakeRun<'_>, event_type: &str, counts: Option<DagNodeCounts>, failure: Option<DagWakeFailure<'_>>) {
        let state = (self.parent_state)();
        self.handle_run_event(run, event_type, counts, failure, state);
    }
    pub fn on_dag_event(&mut self, run: DagWakeRun<'_>, event: &DagRunEventPayload) {
        match event {
            DagRunEventPayload::RunPaused { reason, .. } => {
                let state = (self.parent_state)();
                let reason = reason.as_deref();
                let mut details = json!({"runId":run.run_id,"name":run.name,"status":"paused","runRef":dag_run_ref(run.run_id)});
                if let Some(reason) = reason { details["reason"] = json!(reason); }
                self.route(run.parent_session_id, make_injection(run.run_id, paused_wake_text(run.name, reason), details), state);
            }
            DagRunEventPayload::RunCompleted { counts } => {
                self.on_run_event(run, event.event_type().as_str(), Some(*counts), None);
            }
            DagRunEventPayload::RunCancelled { counts, .. } => {
                self.on_run_event(run, event.event_type().as_str(), Some(*counts), None);
            }
            DagRunEventPayload::RunFailed { error, counts } => {
                let code = match error.code {
                    senpi_task::dag::types::DagNodeErrorCode::LostWhilePaused => "lost_while_paused",
                    senpi_task::dag::types::DagNodeErrorCode::PlanUnresolved => "plan_unresolved",
                    senpi_task::dag::types::DagNodeErrorCode::DepthDenied => "depth_denied",
                    senpi_task::dag::types::DagNodeErrorCode::StartFailed => "start_failed",
                    senpi_task::dag::types::DagNodeErrorCode::ResidencyDenied => "residency_denied",
                    senpi_task::dag::types::DagNodeErrorCode::TaskError => "task_error",
                    senpi_task::dag::types::DagNodeErrorCode::TaskInterrupted => "task_interrupted",
                    senpi_task::dag::types::DagNodeErrorCode::TaskLost => "task_lost",
                    senpi_task::dag::types::DagNodeErrorCode::TaskCancelled => "task_cancelled",
                    senpi_task::dag::types::DagNodeErrorCode::ResumeTaskMissing => "resume_task_missing",
                    senpi_task::dag::types::DagNodeErrorCode::ResumeTaskOrphaned => "resume_task_orphaned",
                    senpi_task::dag::types::DagNodeErrorCode::JournalCorrupt => "journal_corrupt",
                };
                let failure = DagWakeFailure { code, message: &error.message, node_id: error.node_id.as_deref() };
                self.on_run_event(run, "dag.run.failed", Some(*counts), Some(failure));
            }
            _ => {}
        }
    }
    pub fn on_session_start(&mut self, session_id: Option<&str>) {
        let Some(session) = session_id else { return };
        let Some(entries) = self.buffered.remove(session) else { return };
        for injection in entries { self.coordinator.enqueue(injection); }
        self.coordinator.flush_soon();
    }
    pub fn buffered_count(&self, session: &str) -> usize { self.buffered.get(session).map_or(0, Vec::len) }
}

fn make_injection(run_id: &str, content: String, details: Value) -> IdleInjection {
    IdleInjection { key: format!("dag-run:{run_id}"), source: IdleInjectionSource::DagRun, custom_type: Some(DAG_WAKE_MESSAGE_TYPE.into()), content, display: Some(false), details: Some(details), passive: None, on_flushed: None, on_delivery_failed: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_user_stopped_run_requires_an_explicit_resume() {
        let text = paused_wake_text("release", Some("user_stop"));
        assert!(text.contains("paused by your Stop"));
        assert!(text.contains("Resume the run to continue"));
        // A shutdown pause is NOT a user stop: it resumes when the session restarts.
        let shutdown = paused_wake_text("release", Some("session_shutdown"));
        assert!(shutdown.contains("resume when the session restarts"));
        assert!(paused_wake_text("release", None).contains("resume when the session restarts"));
    }

    #[test]
    fn every_dag_wake_carries_its_run_reference() {
        assert_eq!(dag_run_ref("dag_1"), json!({"kind":"dag","id":"dag_1"}));
    }
}
