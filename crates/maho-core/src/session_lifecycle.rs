//! Session lifecycle controller: per-session event-driven lifecycle controller.
//!
//! Port of senpi `packages/coding-agent/src/core/session-lifecycle.ts`.

use std::sync::Arc;

use crate::session_activity::SessionActivitySnapshot;
use crate::session_lifecycle_reducer::{LifecycleFacts, lifecycle_state, lifecycle_terminal};
use crate::session_lifecycle_types::{
    ChildLifecycleReport, EpochOrigin, HumanWait, LifecycleFailure, LifecycleHold, LifecycleWaits,
    SelfSettleReason, SelfSettleResult, SessionLifecycle, SessionLifecycleState, SessionRole,
    SpawnedBy,
};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LifecycleRuntimeFacts {
    pub activity: SessionActivitySnapshot,
    pub pending_messages: usize,
    pub holds: Vec<LifecycleHold>,
    pub human: Vec<HumanWait>,
    pub input_admission_pending: bool,
}

pub struct SessionLifecycleOptions {
    pub session_id: String,
    pub spawned_by: Option<SpawnedBy>,
    pub read_runtime: Arc<dyn Fn() -> LifecycleRuntimeFacts + Send + Sync>,
    pub append_child: Option<Arc<dyn Fn(ChildLifecycleReport) + Send + Sync>>,
}

/// One event-driven reducer per session, shared by every client surface.
pub struct SessionLifecycleController {
    options: SessionLifecycleOptions,
    lifecycle: SessionLifecycle,
    run_active: bool,
    interrupted: bool,
    run_id: Option<String>,
    idle_boundary_passed: bool,
    pending_human: bool,
    admitting_human: bool,
    own_task: Option<ChildLifecycleReport>,
    child_reports: Vec<ChildLifecycleReport>,
    evaluating: bool,
    deferred: bool,
}

impl SessionLifecycleController {
    pub fn new(options: SessionLifecycleOptions) -> Self {
        let is_child = options.spawned_by.is_some();
        let role = if is_child {
            SessionRole::Child
        } else {
            SessionRole::Root
        };
        let root_session_id = options
            .spawned_by
            .as_ref()
            .map(|s| s.root_session_id.clone())
            .unwrap_or_else(|| options.session_id.clone());
        let parent_session_id = options
            .spawned_by
            .as_ref()
            .map(|s| s.parent_session_id.clone());
        let task_id = options.spawned_by.as_ref().map(|s| s.task_id.clone());

        let initial_lifecycle = SessionLifecycle {
            state: if is_child {
                SessionLifecycleState::Running
            } else {
                SessionLifecycleState::Done
            },
            seq: 0,
            epoch: 0,
            epoch_origin: EpochOrigin::Human,
            completion_id: None,
            human_input_revision: 0,
            self_settle_eligible: false,
            settle_decision: None,
            restoring: true,
            persisted: false,
            run_paused: false,
            since: chrono::Utc::now().to_rfc3339(),
            role,
            root_session_id,
            parent_session_id,
            task_id,
            waiting_on: None,
            holds: None,
            completion: None,
            failure: None,
        };

        let mut controller = Self {
            options,
            lifecycle: initial_lifecycle,
            run_active: false,
            interrupted: false,
            run_id: None,
            idle_boundary_passed: false,
            pending_human: false,
            admitting_human: false,
            own_task: None,
            child_reports: Vec::new(),
            evaluating: false,
            deferred: false,
        };

        controller.recompute();
        controller
    }

    pub fn snapshot(&mut self) -> SessionLifecycle {
        self.recompute();
        self.lifecycle.clone()
    }

    pub fn requires_human_continuation(&self) -> bool {
        self.interrupted && !self.pending_human
    }

    pub fn next_run_origin(&self) -> EpochOrigin {
        if self.pending_human {
            EpochOrigin::Human
        } else {
            EpochOrigin::Automatic
        }
    }

    pub fn restore_complete(&mut self) {
        self.lifecycle.restoring = false;
        self.idle_boundary_passed = true;
        self.recompute();
    }

    pub fn human_input(&mut self, _at: &str) {
        self.admitting_human = true;
        self.pending_human = self.pending_human || !self.run_active;
        self.lifecycle.human_input_revision += 1;
    }

    pub fn admission_settled(&mut self) {
        self.admitting_human = false;
        let runtime = (self.options.read_runtime)();
        if !self.run_active && !self.interrupted && runtime.pending_messages == 0 {
            self.pending_human = false;
        }
        self.recompute();
    }

    pub fn start_run(&mut self, origin: Option<EpochOrigin>) {
        self.admitting_human = false;
        self.idle_boundary_passed = false;
        self.interrupted = false;
        self.run_active = true;
        self.lifecycle.epoch += 1;
        self.lifecycle.epoch_origin = origin.unwrap_or(if self.pending_human {
            EpochOrigin::Human
        } else {
            EpochOrigin::Automatic
        });
        self.pending_human = false;
        self.recompute();
    }

    pub fn idle(&mut self) {
        self.run_active = false;
        self.idle_boundary_passed = true;
        self.recompute();
    }

    pub fn report_child(&mut self, report: ChildLifecycleReport) {
        if let Some(pos) = self
            .child_reports
            .iter()
            .position(|r| r.task_id == report.task_id && r.child_session_id == report.child_session_id)
        {
            self.child_reports[pos] = report.clone();
        } else {
            self.child_reports.push(report.clone());
        }

        if report.child_session_id == self.options.session_id
            && self
                .options
                .spawned_by
                .as_ref()
                .is_some_and(|s| s.task_id == report.task_id)
        {
            self.own_task = Some(report.clone());
        }

        if let Some(ref append_child) = self.options.append_child {
            append_child(report);
        }

        self.recompute();
    }

    pub fn mark_run_completed(&mut self, run_id: &str) {
        self.run_id = Some(run_id.to_owned());
        self.lifecycle.completion_id = Some(run_id.to_owned());
        self.recompute();
    }

    pub fn request_self_settle(&mut self) -> SelfSettleResult {
        let runtime = (self.options.read_runtime)();
        if self.admitting_human || runtime.input_admission_pending {
            return SelfSettleResult {
                recorded: false,
                reason: Some(SelfSettleReason::NotEligible),
                epoch: self.lifecycle.epoch,
                human_input_revision: self.lifecycle.human_input_revision,
            };
        }
        self.recompute();
        if !self.lifecycle.self_settle_eligible {
            return SelfSettleResult {
                recorded: false,
                reason: Some(SelfSettleReason::NotEligible),
                epoch: self.lifecycle.epoch,
                human_input_revision: self.lifecycle.human_input_revision,
            };
        }
        SelfSettleResult {
            recorded: true,
            reason: None,
            epoch: self.lifecycle.epoch,
            human_input_revision: self.lifecycle.human_input_revision,
        }
    }

    pub fn recompute(&mut self) {
        if self.lifecycle.restoring || self.admitting_human {
            return;
        }
        if self.evaluating {
            self.deferred = true;
            return;
        }
        self.evaluating = true;
        loop {
            self.deferred = false;
            self.evaluate();
            if !self.deferred {
                break;
            }
        }
        self.evaluating = false;
    }

    fn evaluate(&mut self) {
        let runtime = (self.options.read_runtime)();

        let child_count = self
            .child_reports
            .iter()
            .filter(|report| {
                report.child_session_id != self.options.session_id && report.terminal.is_none()
            })
            .count();

        let human = if self.lifecycle.role == SessionRole::Root {
            runtime.human.clone()
        } else {
            Vec::new()
        };

        let running = self.run_active
            || runtime.activity.is_streaming
            || runtime.activity.is_compacting
            || runtime.activity.is_bash_running
            || runtime.activity.has_session_work
            || runtime.pending_messages > 0;

        let failure: Option<LifecycleFailure> = self
            .own_task
            .as_ref()
            .and_then(|task| task.terminal)
            .and_then(|term| match term {
                crate::session_lifecycle_types::ChildTerminal::Error => {
                    Some(LifecycleFailure {
                        reason: crate::session_lifecycle_types::FailureReason::TaskError,
                        message: None,
                        retry_at_ms: None,
                        task_id: self.own_task.as_ref().map(|t| t.task_id.clone()),
                        run_epoch: self.own_task.as_ref().map(|t| t.run_epoch),
                    })
                }
                crate::session_lifecycle_types::ChildTerminal::Lost => {
                    Some(LifecycleFailure {
                        reason: crate::session_lifecycle_types::FailureReason::Lost,
                        message: None,
                        retry_at_ms: None,
                        task_id: self.own_task.as_ref().map(|t| t.task_id.clone()),
                        run_epoch: self.own_task.as_ref().map(|t| t.run_epoch),
                    })
                }
                crate::session_lifecycle_types::ChildTerminal::Interrupted => {
                    Some(LifecycleFailure {
                        reason: crate::session_lifecycle_types::FailureReason::Interrupted,
                        message: None,
                        retry_at_ms: None,
                        task_id: self.own_task.as_ref().map(|t| t.task_id.clone()),
                        run_epoch: self.own_task.as_ref().map(|t| t.run_epoch),
                    })
                }
                _ => None,
            });

        let waits = LifecycleWaits {
            children: child_count,
            background: Vec::new(),
            human,
        };

        let state = lifecycle_state(&LifecycleFacts {
            role: self.lifecycle.role,
            terminal: self.own_task.as_ref().and_then(|t| t.terminal),
            holds: runtime.holds.clone(),
            waits: waits.clone(),
            running,
            failure: failure.clone(),
        });

        if lifecycle_terminal(state)
            && self.own_task.as_ref().and_then(|t| t.terminal).is_none()
            && !self.idle_boundary_passed
        {
            return;
        }

        self.lifecycle.seq += 1;
        self.lifecycle.state = state;
        self.lifecycle.waiting_on = Some(waits);
        self.lifecycle.holds = if runtime.holds.is_empty() {
            None
        } else {
            Some(runtime.holds)
        };
        self.lifecycle.failure = failure;
        self.lifecycle.run_paused = self.lifecycle.state == SessionLifecycleState::Paused;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_controller_transitions_from_restore_to_done() {
        let mut controller = SessionLifecycleController::new(SessionLifecycleOptions {
            session_id: "root-1".into(),
            spawned_by: None,
            read_runtime: Arc::new(|| LifecycleRuntimeFacts::default()),
            append_child: None,
        });

        assert!(controller.snapshot().restoring);
        controller.restore_complete();
        assert!(!controller.snapshot().restoring);
        assert_eq!(controller.snapshot().state, SessionLifecycleState::Done);
    }

    #[test]
    fn run_lifecycle_flow() {
        let mut controller = SessionLifecycleController::new(SessionLifecycleOptions {
            session_id: "root-1".into(),
            spawned_by: None,
            read_runtime: Arc::new(|| LifecycleRuntimeFacts::default()),
            append_child: None,
        });

        controller.restore_complete();
        controller.start_run(Some(EpochOrigin::Human));
        assert_eq!(controller.snapshot().state, SessionLifecycleState::Running);
        assert_eq!(controller.snapshot().epoch, 1);

        controller.idle();
        assert_eq!(controller.snapshot().state, SessionLifecycleState::Done);
    }
}
