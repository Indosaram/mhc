//! The pinned scheduler suite's FakeTaskManager, behind the scheduler's test-only port.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::dag::owner::{DagTaskOwner, OwnedStartResult};
use crate::dag::scheduler::TestTaskPort;
use crate::manager::types::{ManagerStartSpec, PlanResolutionCode, PlanResolutionError, StartFailure, StartResult, StartedTask};
use crate::manager::execution_mode::ExecutionMode;
use crate::state::{TaskRecord, TaskRecordInput, TaskRunStats, TaskStatus, create_task_record};

#[derive(Default)]
pub(crate) struct FakeOptions {
    pub residency_limit: Option<usize>,
    pub manual: bool,
    pub queued: BTreeSet<String>,
    pub reject_start: BTreeSet<String>,
    pub reject_wait: BTreeSet<String>,
    pub cancel_errors: BTreeMap<String, String>,
    pub start_failures: BTreeMap<String, &'static str>,
    pub cancel_gate: Option<Arc<Gate>>,
}

#[derive(Default)]
pub(crate) struct Gate {
    open: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    pub fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }

    pub fn wait(&self) {
        let state = self.open.lock().unwrap();
        let (state, timeout) = self.changed.wait_timeout_while(state, Duration::from_secs(5), |open| !*open).unwrap();
        assert!(*state && !timeout.timed_out(), "gate never released");
    }
}

#[derive(Default)]
pub(crate) struct FakeState {
    pub starts: Vec<String>,
    pub attempts: Vec<String>,
    pub specs: Vec<ManagerStartSpec>,
    pub denials: Vec<String>,
    pub cancellations: Vec<String>,
    pub waits: BTreeSet<String>,
    pub max_residents: usize,
    records: BTreeMap<String, TaskRecord>,
}

pub(crate) struct FakeTaskManager {
    pub options: FakeOptions,
    pub state: Mutex<FakeState>,
    changed: Condvar,
}

impl FakeTaskManager {
    pub fn new(options: FakeOptions) -> Arc<Self> {
        Arc::new(Self { options, state: Mutex::new(FakeState::default()), changed: Condvar::new() })
    }

    pub fn when_started(&self, node_id: &str) {
        self.wait_state(node_id, |state| state.starts.iter().any(|id| id == node_id));
    }

    pub fn when_cancelling(&self) {
        self.wait_state("cancellation", |state| !state.cancellations.is_empty());
    }

    pub fn when_waiting(&self, node_id: &str) {
        self.wait_state(node_id, |state| state.waits.contains(node_id));
    }

    pub fn when_denied(&self, node_id: &str) {
        self.wait_state(node_id, |state| state.denials.iter().any(|id| id == node_id));
    }

    fn wait_state(&self, what: &str, ready: impl Fn(&FakeState) -> bool) {
        let state = self.state.lock().unwrap();
        let (state, timeout) = self.changed.wait_timeout_while(state, Duration::from_secs(5), |state| !ready(state)).unwrap();
        assert!(ready(&state) && !timeout.timed_out(), "fake manager event {what} never arrived; starts={:?}; denials={:?}", state.starts, state.denials);
    }

    pub fn complete(&self, node_id: &str, status: TaskStatus) {
        let mut state = self.state.lock().unwrap();
        let record = state.records.values_mut().find(|record| record.owner.as_ref().is_some_and(|owner| owner.node_id == node_id)).expect("started task");
        record.status = status;
        if status == TaskStatus::Completed {
            record.final_response = Some(format!("done {node_id}"));
            record.run_stats = Some(TaskRunStats { runtime_ms: 25, turns: 2, tool_calls: 1, output_tokens: Some(8), ..Default::default() });
        } else {
            record.error_message = Some(format!("{} {node_id}", status.as_str()));
        }
        self.changed.notify_all();
    }
}

impl TestTaskPort for FakeTaskManager {
    fn start_owned(&self, spec: &ManagerStartSpec, owner: &DagTaskOwner) -> Result<OwnedStartResult, String> {
        let node_id = &owner.node_id;
        let mut state = self.state.lock().unwrap();
        state.attempts.push(node_id.clone());
        if self.options.reject_start.contains(node_id) {
            return Err(format!("start rejected {node_id}"));
        }
        state.specs.push(spec.clone());
        let failure = match self.options.start_failures.get(node_id).copied() {
            Some("plan") => Some(StartResult::PlanUnresolved(PlanResolutionError::new(PlanResolutionCode::UnknownTarget, format!("unresolved {node_id}")))),
            Some("depth") => Some(StartResult::DepthDenied { reason: format!("depth denied {node_id}"), child_depth: 2, max_depth: 1 }),
            Some("start") => Some(StartResult::StartFailed(StartFailure { task_id: format!("failed-{node_id}"), name: node_id.clone(), category: Some("quick".into()), subagent_type: None, execution_mode: ExecutionMode::InProcess, model: "fake-model".into(), resolved_model: None, run_in_background: true, error_message: format!("failed to start {node_id}") })),
            _ => None,
        };
        if let Some(failure) = failure { return Ok(OwnedStartResult::NotStarted(failure)); }
        let residents = state.records.values().filter(|record| !record.status.is_terminal()).count();
        if self.options.residency_limit.is_some_and(|limit| residents >= limit) {
            state.denials.push(node_id.clone());
            self.changed.notify_all();
            return Ok(OwnedStartResult::NotStarted(StartResult::ResidencyDenied { reason: "resident child cap reached".into() }));
        }
        let mut record = create_task_record(TaskRecordInput {
            name: Some(node_id.clone()), parent_session_id: spec.parent_session_id.clone(), root_session_id: spec.root_session_id.clone().unwrap_or_default(), depth: 1, category: spec.category.clone(), execution_mode: "in-process".into(), model: "fake-model".into(), notify_on_terminal: true, owner: Some(owner.clone()), ..Default::default()
        }, None).unwrap();
        record.status = if self.options.queued.contains(node_id) { TaskStatus::Pending } else { TaskStatus::Running };
        let task = StartedTask { task_id: record.task_id.clone(), status: record.status, name: node_id.clone(), resolved_model: None, queue_position: self.options.queued.contains(node_id).then_some(3), name_warning: None };
        state.max_residents = state.max_residents.max(residents + 1);
        state.starts.push(node_id.clone());
        state.records.insert(record.task_id.clone(), record);
        self.changed.notify_all();
        drop(state);
        if !self.options.manual { self.complete(node_id, TaskStatus::Completed); }
        Ok(OwnedStartResult::Started { task, reused: false })
    }

    fn wait_for(&self, task_id: &str) -> Result<TaskRecord, String> {
        let mut state = self.state.lock().unwrap();
        let node_id = state.records[task_id].owner.as_ref().unwrap().node_id.clone();
        state.waits.insert(node_id.clone());
        self.changed.notify_all();
        if self.options.reject_wait.contains(&node_id) { return Err(format!("wait rejected {node_id}")); }
        let (state, timeout) = self.changed.wait_timeout_while(state, Duration::from_secs(5), |state| !state.records[task_id].status.is_terminal()).unwrap();
        assert!(!timeout.timed_out(), "task never settled");
        Ok(state.records[task_id].clone())
    }

    fn cancel_task(&self, task_id: &str) -> Result<(), String> {
        let node_id = {
            let mut state = self.state.lock().unwrap();
            let node_id = state.records[task_id].owner.as_ref().unwrap().node_id.clone();
            state.cancellations.push(node_id.clone());
            self.changed.notify_all();
            node_id
        };
        if let Some(gate) = &self.options.cancel_gate { gate.wait(); }
        if let Some(error) = self.options.cancel_errors.get(&node_id) { return Err(error.clone()); }
        self.complete(&node_id, TaskStatus::Cancelled);
        Ok(())
    }
}
