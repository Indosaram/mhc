//! Port of senpi packages/coding-agent/src/core/compaction/lifecycle.ts.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use maho_ai::utils::abort::{AbortController, AbortSignal};

/// senpi CompactionModelRef.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionModelRef {
    pub provider: String,
    pub id: String,
}

/// senpi stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionStage {
    Feedback,
    Execution,
}

/// senpi CompactionOperation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionOperation {
    pub generation: u64,
    pub operation_id: String,
    pub stage: CompactionStage,
    pub reason: String,
    pub model: Option<CompactionModelRef>,
    pub started_revision: i64,
}

/// senpi CompactionLifecycleState.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CompactionLifecycleState {
    #[default]
    Idle,
    Running(CompactionOperation),
    Completed(CompactionOperation, i64),
    Failed(CompactionOperation, i64, Option<String>, Option<String>),
    Aborted(CompactionOperation, i64, Option<String>),
}

impl CompactionLifecycleState {
    pub fn generation(&self) -> u64 {
        match self {
            Self::Idle => 0,
            Self::Running(operation)
            | Self::Completed(operation, _)
            | Self::Failed(operation, _, _, _)
            | Self::Aborted(operation, _, _) => operation.generation,
        }
    }

    pub fn operation(&self) -> Option<&CompactionOperation> {
        match self {
            Self::Idle => None,
            Self::Running(operation)
            | Self::Completed(operation, _)
            | Self::Failed(operation, _, _, _)
            | Self::Aborted(operation, _, _) => Some(operation),
        }
    }

    pub fn status(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running(_) => "running",
            Self::Completed(_, _) => "completed",
            Self::Failed(_, _, _, _) => "failed",
            Self::Aborted(_, _, _) => "aborted",
        }
    }
}

/// senpi BeginCompactionOperation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeginCompactionOperation {
    pub operation_id: String,
    pub stage: CompactionStage,
    pub reason: String,
    pub model: Option<CompactionModelRef>,
    pub started_revision: i64,
}

/// senpi FinishCompactionOperation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishCompactionOperation {
    pub operation_id: String,
    pub status: CompactionFinishStatus,
    pub ended_revision: i64,
    pub rejection_cause: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionFinishStatus {
    Completed,
    Failed,
    Aborted,
}

/// senpi initialCompactionLifecycleState.
pub fn initial_compaction_lifecycle_state() -> CompactionLifecycleState {
    CompactionLifecycleState::Idle
}

/// senpi beginCompactionOperation.
pub fn begin_compaction_operation(
    state: &CompactionLifecycleState,
    operation: BeginCompactionOperation,
) -> CompactionLifecycleState {
    CompactionLifecycleState::Running(CompactionOperation {
        generation: state.generation() + 1,
        operation_id: operation.operation_id,
        stage: operation.stage,
        reason: operation.reason,
        model: operation.model,
        started_revision: operation.started_revision,
    })
}

/// senpi promoteCompactionOperation.
pub fn promote_compaction_operation(state: &CompactionLifecycleState, operation_id: &str) -> CompactionLifecycleState {
    match state {
        CompactionLifecycleState::Running(operation)
            if operation.operation_id == operation_id && operation.stage == CompactionStage::Feedback =>
        {
            let mut promoted = operation.clone();
            promoted.stage = CompactionStage::Execution;
            CompactionLifecycleState::Running(promoted)
        }
        _ => state.clone(),
    }
}

/// senpi finishCompactionOperation.
pub fn finish_compaction_operation(
    state: &CompactionLifecycleState,
    event: &FinishCompactionOperation,
) -> CompactionLifecycleState {
    let CompactionLifecycleState::Running(operation) = state else {
        return state.clone();
    };
    if operation.operation_id != event.operation_id {
        return state.clone();
    }

    match event.status {
        CompactionFinishStatus::Completed => CompactionLifecycleState::Completed(operation.clone(), event.ended_revision),
        CompactionFinishStatus::Aborted => {
            CompactionLifecycleState::Aborted(operation.clone(), event.ended_revision, event.error_message.clone())
        }
        CompactionFinishStatus::Failed => CompactionLifecycleState::Failed(
            operation.clone(),
            event.ended_revision,
            event.rejection_cause.clone(),
            event.error_message.clone(),
        ),
    }
}

/// An abort controller carrying the reference identity JS compares with ===.
#[derive(Debug, Clone, Default)]
pub struct CompactionAbortController {
    inner: Arc<CompactionAbortInner>,
}

#[derive(Debug, Default)]
struct CompactionAbortInner {
    controller: AbortController,
    aborted: AtomicBool,
}

impl CompactionAbortController {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn abort(&self) {
        self.inner.aborted.store(true, Ordering::SeqCst);
        self.inner.controller.abort(None);
    }

    pub fn signal(&self) -> AbortSignal {
        self.inner.controller.signal()
    }

    pub fn aborted(&self) -> bool {
        self.inner.aborted.load(Ordering::SeqCst) || self.inner.controller.signal().aborted()
    }

    pub fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

/// senpi CompactionLifecycleCoordinator: guards state transitions from stale callers.
#[derive(Debug, Default)]
pub struct CompactionLifecycleCoordinator {
    state: CompactionLifecycleState,
    controller: Option<CompactionAbortController>,
}

impl CompactionLifecycleCoordinator {
    pub fn new() -> Self {
        Self { state: initial_compaction_lifecycle_state(), controller: None }
    }

    pub fn state(&self) -> &CompactionLifecycleState {
        &self.state
    }

    /// senpi begin: returns the operation id the coordinator settled on.
    pub fn begin(&mut self, operation: BeginCompactionOperation, controller: CompactionAbortController) -> String {
        if self.state.status() == "running" {
            let same_controller = self.controller.as_ref().is_some_and(|current| current.same(&controller));
            if same_controller {
                let running_operation_id =
                    self.state.operation().map(|operation| operation.operation_id.clone()).unwrap_or_default();
                if operation.stage == CompactionStage::Execution {
                    self.state = promote_compaction_operation(&self.state, &running_operation_id);
                }
                return running_operation_id;
            }
            if let Some(current) = &self.controller {
                current.abort();
            }
        }

        self.controller = Some(controller);
        self.state = begin_compaction_operation(&self.state, operation.clone());
        operation.operation_id
    }

    /// senpi isCurrent.
    pub fn is_current(&self, operation_id: &str, controller: &CompactionAbortController) -> bool {
        self.state.status() == "running"
            && self.state.operation().is_some_and(|operation| operation.operation_id == operation_id)
            && self.controller.as_ref().is_some_and(|current| current.same(controller))
            && !controller.aborted()
    }

    /// senpi hasCurrentSignal. maho-ai's AbortSignal exposes no reference identity, so the port
    /// compares the controller handle that owns the signal instead of the signal itself.
    pub fn has_current_signal(&self, controller: &CompactionAbortController) -> bool {
        self.state.status() == "running"
            && self.controller.as_ref().is_some_and(|current| current.same(controller))
            && !controller.aborted()
    }

    /// senpi finish: true when the event advanced the state.
    pub fn finish(&mut self, event: &FinishCompactionOperation) -> bool {
        let next = finish_compaction_operation(&self.state, event);
        if next == self.state {
            return false;
        }
        self.state = next;
        self.controller = None;
        true
    }

    /// senpi abort.
    pub fn abort(&mut self, ended_revision: i64) -> Option<(String, CompactionStage)> {
        if self.state.status() != "running" {
            return None;
        }
        let operation = self.state.operation().cloned()?;
        if let Some(controller) = &self.controller {
            controller.abort();
        }
        self.finish(&FinishCompactionOperation {
            operation_id: operation.operation_id,
            status: CompactionFinishStatus::Aborted,
            ended_revision,
            rejection_cause: None,
            error_message: Some("Compaction cancelled".to_string()),
        });
        Some((operation.reason, operation.stage))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn begin(operation_id: &str, stage: CompactionStage) -> BeginCompactionOperation {
        BeginCompactionOperation {
            operation_id: operation_id.to_string(),
            stage,
            reason: "manual".to_string(),
            model: None,
            started_revision: 1,
        }
    }

    fn finish(operation_id: &str, status: CompactionFinishStatus) -> FinishCompactionOperation {
        FinishCompactionOperation {
            operation_id: operation_id.to_string(),
            status,
            ended_revision: 7,
            rejection_cause: None,
            error_message: None,
        }
    }

    #[test]
    fn the_reducer_moves_idle_to_running_to_completed() {
        let state = initial_compaction_lifecycle_state();
        assert_eq!(state, CompactionLifecycleState::Idle);
        let running = begin_compaction_operation(&state, begin("op-1", CompactionStage::Feedback));
        assert_eq!(running.status(), "running");
        assert_eq!(running.generation(), 1);
        let completed = finish_compaction_operation(&running, &finish("op-1", CompactionFinishStatus::Completed));
        assert_eq!(completed.status(), "completed");
        assert_eq!(completed.generation(), 1);
        assert_eq!(completed, CompactionLifecycleState::Completed(running.operation().cloned().expect("operation"), 7));
    }

    #[test]
    fn a_stale_finish_event_leaves_the_state_untouched() {
        let running = begin_compaction_operation(&initial_compaction_lifecycle_state(), begin("op-1", CompactionStage::Feedback));
        assert_eq!(finish_compaction_operation(&running, &finish("op-2", CompactionFinishStatus::Completed)), running);
        assert_eq!(
            finish_compaction_operation(&CompactionLifecycleState::Idle, &finish("op-1", CompactionFinishStatus::Completed)),
            CompactionLifecycleState::Idle
        );
    }

    #[test]
    fn failed_and_aborted_keep_their_extra_fields() {
        let running = begin_compaction_operation(&initial_compaction_lifecycle_state(), begin("op-1", CompactionStage::Feedback));
        let mut event = finish("op-1", CompactionFinishStatus::Failed);
        event.rejection_cause = Some("too-small".to_string());
        event.error_message = Some("boom".to_string());
        assert_eq!(
            finish_compaction_operation(&running, &event),
            CompactionLifecycleState::Failed(
                running.operation().cloned().expect("operation"),
                7,
                Some("too-small".to_string()),
                Some("boom".to_string())
            )
        );

        let mut abort_event = finish("op-1", CompactionFinishStatus::Aborted);
        abort_event.error_message = Some("Compaction cancelled".to_string());
        assert_eq!(
            finish_compaction_operation(&running, &abort_event),
            CompactionLifecycleState::Aborted(running.operation().cloned().expect("operation"), 7, Some("Compaction cancelled".to_string()))
        );
    }

    #[test]
    fn promotion_only_moves_feedback_to_execution_once() {
        let running = begin_compaction_operation(&initial_compaction_lifecycle_state(), begin("op-1", CompactionStage::Feedback));
        let promoted = promote_compaction_operation(&running, "op-1");
        assert_eq!(promoted.operation().map(|operation| operation.stage), Some(CompactionStage::Execution));
        assert_eq!(promote_compaction_operation(&promoted, "op-1"), promoted);
        assert_eq!(promote_compaction_operation(&running, "op-2"), running);
    }

    #[test]
    fn a_second_begin_from_a_different_controller_aborts_the_first() {
        let mut coordinator = CompactionLifecycleCoordinator::new();
        let first = CompactionAbortController::new();
        let second = CompactionAbortController::new();
        let operation_id = coordinator.begin(begin("op-1", CompactionStage::Feedback), first.clone());
        assert_eq!(operation_id, "op-1");
        assert!(coordinator.is_current("op-1", &first));

        let next_id = coordinator.begin(begin("op-2", CompactionStage::Feedback), second.clone());
        assert_eq!(next_id, "op-2");
        assert!(first.aborted());
        assert!(!coordinator.is_current("op-1", &first));
        assert!(coordinator.is_current("op-2", &second));
        assert_eq!(coordinator.state().generation(), 2);
    }

    #[test]
    fn the_same_controller_restarts_promote_instead_of_starting_a_new_generation() {
        let mut coordinator = CompactionLifecycleCoordinator::new();
        let controller = CompactionAbortController::new();
        coordinator.begin(begin("op-1", CompactionStage::Feedback), controller.clone());
        let operation_id = coordinator.begin(begin("op-2", CompactionStage::Execution), controller.clone());
        assert_eq!(operation_id, "op-1");
        assert_eq!(coordinator.state().operation().map(|operation| operation.operation_id.clone()), Some("op-1".to_string()));
        assert_eq!(coordinator.state().operation().map(|operation| operation.stage), Some(CompactionStage::Execution));
        assert_eq!(coordinator.state().generation(), 1);
    }

    #[test]
    fn finishing_clears_the_controller_and_reports_whether_it_moved() {
        let mut coordinator = CompactionLifecycleCoordinator::new();
        let controller = CompactionAbortController::new();
        coordinator.begin(begin("op-1", CompactionStage::Feedback), controller);
        assert!(coordinator.finish(&finish("op-1", CompactionFinishStatus::Completed)));
        assert!(!coordinator.finish(&finish("op-1", CompactionFinishStatus::Completed)));
        assert_eq!(coordinator.state().status(), "completed");
        assert_eq!(coordinator.state().generation(), 1);
    }

    #[test]
    fn abort_reports_the_running_operation_and_marks_the_controller() {
        let mut coordinator = CompactionLifecycleCoordinator::new();
        let controller = CompactionAbortController::new();
        coordinator.begin(begin("op-1", CompactionStage::Execution), controller.clone());
        let aborted = coordinator.abort(9).expect("abort");
        assert_eq!(aborted.0, "manual");
        assert_eq!(aborted.1, CompactionStage::Execution);
        assert!(controller.aborted());
        assert_eq!(coordinator.state().status(), "aborted");
        assert!(coordinator.abort(10).is_none());
    }

    #[test]
    fn has_current_signal_tracks_the_live_controller() {
        let mut coordinator = CompactionLifecycleCoordinator::new();
        let controller = CompactionAbortController::new();
        coordinator.begin(begin("op-1", CompactionStage::Feedback), controller.clone());
        assert!(coordinator.has_current_signal(&controller));
        let other = CompactionAbortController::new();
        assert!(!coordinator.has_current_signal(&other));
        controller.abort();
        assert!(!coordinator.has_current_signal(&controller));
    }
}
