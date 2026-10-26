//! The pinned scheduler suite's FakeTaskManager, behind the scheduler's test-only port.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Duration;

use crate::dag::owner::{DagTaskOwner, OwnedStartResult};
use crate::dag::scheduler::TestTaskPort;
use crate::manager::execution_mode::ExecutionMode;
use crate::manager::types::{
    ManagerStartSpec, PlanResolutionCode, PlanResolutionError, ResidencyDeniedCause,
    ResidentSummary, StartFailure, StartResult, StartedTask,
};
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
    pub lease_denials: BTreeSet<String>,
    pub empty_resident_denials: BTreeSet<String>,
    pub owner_conflicts: BTreeSet<String>,
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
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(5), |open| !*open)
            .unwrap();
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
    residency_waiters: BTreeMap<String, Arc<(Mutex<bool>, Condvar)>>,
    lease_denial_used: BTreeSet<String>,
}

pub(crate) struct FakeTaskManager {
    pub options: Mutex<FakeOptions>,
    pub state: Mutex<FakeState>,
    changed: Condvar,
}

impl FakeTaskManager {
    pub fn new(options: FakeOptions) -> Arc<Self> {
        Arc::new(Self {
            options: Mutex::new(options),
            state: Mutex::new(FakeState::default()),
            changed: Condvar::new(),
        })
    }

    pub fn when_started(&self, node_id: &str) {
        self.wait_state(node_id, |state| state.starts.iter().any(|id| id == node_id));
    }

    pub fn when_cancelling(&self) {
        self.wait_state("cancellation", |state| !state.cancellations.is_empty());
    }

    pub fn add_owner_conflict(&self, node_id: &str) {
        self.options
            .lock()
            .unwrap()
            .owner_conflicts
            .insert(node_id.to_string());
    }

    pub fn when_waiting(&self, node_id: &str) {
        self.wait_state(node_id, |state| state.waits.contains(node_id));
    }

    pub fn when_denied(&self, node_id: &str) {
        self.wait_state(node_id, |state| {
            state.denials.iter().any(|id| id == node_id)
        });
    }

    fn wait_state(&self, what: &str, ready: impl Fn(&FakeState) -> bool) {
        let state = self.state.lock().unwrap();
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(5), |state| !ready(state))
            .unwrap();
        assert!(
            ready(&state) && !timeout.timed_out(),
            "fake manager event {what} never arrived; starts={:?}; denials={:?}",
            state.starts,
            state.denials
        );
    }

    pub fn complete(&self, node_id: &str, status: TaskStatus) {
        let mut state = self.state.lock().unwrap();
        let record = state
            .records
            .values_mut()
            .find(|record| record.name.as_deref() == Some(node_id) && !record.status.is_terminal())
            .expect("started task");
        record.status = status;
        if status == TaskStatus::Completed {
            record.final_response = Some(format!("done {node_id}"));
            record.run_stats = Some(TaskRunStats {
                runtime_ms: 25,
                turns: 2,
                tool_calls: 1,
                output_tokens: Some(8),
                ..Default::default()
            });
        } else {
            record.error_message = Some(format!("{} {node_id}", status.as_str()));
        }
        self.changed.notify_all();
        let waiters = std::mem::take(&mut state.residency_waiters);
        for waiter in waiters.into_values() {
            *waiter.0.lock().unwrap() = true;
            waiter.1.notify_all();
        }
    }

    fn residency_change(&self, session: &str) -> Arc<(Mutex<bool>, Condvar)> {
        self.state
            .lock()
            .unwrap()
            .residency_waiters
            .entry(session.to_owned())
            .or_insert_with(|| Arc::new((Mutex::new(false), Condvar::new())))
            .clone()
    }

    pub fn release_residency(&self, session: &str) {
        let waiter = self.state.lock().unwrap().residency_waiters.remove(session);
        if let Some(waiter) = waiter {
            *waiter.0.lock().unwrap() = true;
            waiter.1.notify_all();
        }
    }

    pub fn add_external_resident(&self, session: &str) -> String {
        let mut record = create_task_record(
            TaskRecordInput {
                name: Some("external".into()),
                parent_session_id: session.into(),
                root_session_id: session.into(),
                depth: 1,
                category: Some("quick".into()),
                execution_mode: "in-process".into(),
                model: "fake-model".into(),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        record.status = TaskStatus::Running;
        let id = record.task_id.clone();
        self.state
            .lock()
            .unwrap()
            .records
            .insert(id.clone(), record);
        id
    }
}

impl TestTaskPort for FakeTaskManager {
    fn start_owned(
        &self,
        spec: &ManagerStartSpec,
        owner: &DagTaskOwner,
    ) -> Result<OwnedStartResult, String> {
        let node_id = &owner.node_id;
        let mut state = self.state.lock().unwrap();
        state.attempts.push(node_id.clone());
        let options = self.options.lock().unwrap();
        if options.owner_conflicts.contains(node_id)
            && let Some(record) = state
                .records
                .values()
                .find(|record| {
                    record
                        .owner
                        .as_ref()
                        .is_some_and(|existing| existing.node_id == *node_id)
                })
                .cloned()
        {
            return Ok(OwnedStartResult::OwnerConflict {
                task_id: record.task_id,
                existing_fingerprint: "old".into(),
                requested_fingerprint: owner.fingerprint.clone(),
            });
        }
        if options.reject_start.contains(node_id) {
            return Err(format!("start rejected {node_id}"));
        }
        state.specs.push(spec.clone());
        let failure = match options.start_failures.get(node_id).copied() {
            Some("plan") => Some(StartResult::PlanUnresolved(PlanResolutionError::new(
                PlanResolutionCode::UnknownTarget,
                format!("unresolved {node_id}"),
            ))),
            Some("depth") => Some(StartResult::DepthDenied {
                reason: format!("depth denied {node_id}"),
                child_depth: 2,
                max_depth: 1,
            }),
            Some("start") => Some(StartResult::StartFailed(StartFailure {
                task_id: format!("failed-{node_id}"),
                name: node_id.clone(),
                category: Some("quick".into()),
                subagent_type: None,
                execution_mode: ExecutionMode::InProcess,
                model: "fake-model".into(),
                resolved_model: None,
                run_in_background: true,
                error_message: format!("failed to start {node_id}"),
            })),
            _ => None,
        };
        if let Some(failure) = failure {
            return Ok(OwnedStartResult::NotStarted(failure));
        }
        let residents = state
            .records
            .values()
            .filter(|record| !record.status.is_terminal())
            .count();
        if options.lease_denials.contains(node_id)
            && state.lease_denial_used.insert(node_id.clone())
        {
            state.denials.push(node_id.clone());
            self.changed.notify_all();
            return Ok(OwnedStartResult::NotStarted(StartResult::ResidencyDenied {
                reason: "lease busy".into(),
                cause: ResidencyDeniedCause::Lease,
            }));
        }
        if options.empty_resident_denials.contains(node_id) {
            state.denials.push(node_id.clone());
            self.changed.notify_all();
            return Ok(OwnedStartResult::NotStarted(StartResult::ResidencyDenied {
                reason: "resident child cap reached".into(),
                cause: ResidencyDeniedCause::Residents {
                    max_children: options
                        .residency_limit
                        .map(crate::manager::types::MaxChildren::Number),
                    residents: Vec::new(),
                },
            }));
        }
        if options
            .residency_limit
            .is_some_and(|limit| residents >= limit)
        {
            state.denials.push(node_id.clone());
            self.changed.notify_all();
            let residents: Vec<_> = state
                .records
                .values()
                .filter(|record| {
                    !record.status.is_terminal()
                        && record.parent_session_id == spec.parent_session_id
                })
                .map(|record| ResidentSummary {
                    task_id: record.task_id.clone(),
                    name: record
                        .name
                        .clone()
                        .unwrap_or_else(|| record.task_id.clone()),
                    status: record.status.as_str().into(),
                })
                .collect();
            return Ok(OwnedStartResult::NotStarted(StartResult::ResidencyDenied {
                reason: "resident child cap reached".into(),
                cause: ResidencyDeniedCause::Residents {
                    max_children: options
                        .residency_limit
                        .map(crate::manager::types::MaxChildren::Number),
                    residents,
                },
            }));
        }
        let mut record = create_task_record(
            TaskRecordInput {
                name: Some(node_id.clone()),
                parent_session_id: spec.parent_session_id.clone(),
                root_session_id: spec.root_session_id.clone().unwrap_or_default(),
                depth: 1,
                category: spec.category.clone(),
                execution_mode: "in-process".into(),
                model: "fake-model".into(),
                notify_on_terminal: true,
                owner: Some(owner.clone()),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        record.status = if options.queued.contains(node_id) {
            TaskStatus::Pending
        } else {
            TaskStatus::Running
        };
        let task = StartedTask {
            task_id: record.task_id.clone(),
            status: record.status,
            name: node_id.clone(),
            resolved_model: None,
            queue_position: options.queued.contains(node_id).then_some(3),
            name_warning: None,
        };
        state.max_residents = state.max_residents.max(residents + 1);
        state.starts.push(node_id.clone());
        state.records.insert(record.task_id.clone(), record);
        self.changed.notify_all();
        drop(options);
        drop(state);
        let manual = self.options.lock().unwrap().manual;
        if !manual {
            self.complete(node_id, TaskStatus::Completed);
        }
        Ok(OwnedStartResult::Started {
            task,
            reused: false,
        })
    }

    fn wait_for(&self, task_id: &str) -> Result<TaskRecord, String> {
        let mut state = self.state.lock().unwrap();
        let node_id = state.records[task_id]
            .owner
            .as_ref()
            .unwrap()
            .node_id
            .clone();
        state.waits.insert(node_id.clone());
        self.changed.notify_all();
        if self.options.lock().unwrap().reject_wait.contains(&node_id) {
            return Err(format!("wait rejected {node_id}"));
        }
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(5), |state| {
                !state.records[task_id].status.is_terminal()
            })
            .unwrap();
        assert!(!timeout.timed_out(), "task never settled");
        Ok(state.records[task_id].clone())
    }

    fn cancel_task(&self, task_id: &str) -> Result<(), String> {
        let node_id = {
            let mut state = self.state.lock().unwrap();
            let node_id = state.records[task_id]
                .owner
                .as_ref()
                .unwrap()
                .node_id
                .clone();
            state.cancellations.push(node_id.clone());
            self.changed.notify_all();
            node_id
        };
        let options = self.options.lock().unwrap();
        if let Some(gate) = &options.cancel_gate {
            gate.wait();
        }
        if let Some(error) = options.cancel_errors.get(&node_id) {
            return Err(error.clone());
        }
        self.complete(&node_id, TaskStatus::Cancelled);
        Ok(())
    }

    fn residency_changed(&self, parent_session_id: &str, sender: mpsc::Sender<()>) {
        let waiter = self.residency_change(parent_session_id);
        std::thread::spawn(move || {
            let mut fired = waiter.0.lock().unwrap();
            while !*fired {
                fired = waiter.1.wait(fired).unwrap();
            }
            let _ = sender.send(());
        });
    }

    fn get_task(&self, task_id: &str) -> Option<TaskRecord> {
        self.state.lock().unwrap().records.get(task_id).cloned()
    }
}
