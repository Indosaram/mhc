//! Port of senpi `packages/agent/src/harness/runtime/types.ts`.

use std::sync::{Arc, Mutex};

use maho_ai::types::BoxFuture;
use maho_ai::utils::abort::{AbortController, AbortSignal};
use maho_ai::utils::retry::RetryPolicy;

use crate::harness::agent_harness::{DriveOptions, DriveOutcome, Resources};
use crate::harness::events::HarnessEvent;
use crate::harness::compaction::compaction::CompactionSettings;
use crate::harness::context::{Context, without_abort_signal};
use crate::harness::execution::effect_gate::{Cancellation, Gate, GateControl, create_gate};
use crate::harness::session::types::{
    CommitResult, InboxItem, LaneConfiguration, Operation, OperationResultRecord, OperationState, Write,
};
use crate::harness::types::{AgentHarnessStreamOptions, AgentHarnessTool};
use crate::types::QueueMode;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{operation} is not implemented until its later AgentHarness slice")]
pub struct SliceNotImplemented {
    pub operation: String,
}

impl SliceNotImplemented {
    pub fn new(operation: impl Into<String>) -> Self {
        Self { operation: operation.into() }
    }
}

pub type SystemPromptFn = Arc<dyn Fn(&Context) -> BoxFuture<'static, String> + Send + Sync>;
pub type ToProviderMessagesFn = Arc<
    dyn Fn(Vec<crate::types::AgentMessage>, &Context) -> BoxFuture<'static, Vec<maho_ai::types::Message>>
        + Send
        + Sync,
>;
pub type MaterializeFn<TResult> = Arc<dyn Fn(&CommitResult) -> TResult + Send + Sync>;
pub type CommitEventsFn = Arc<dyn Fn(&CommitResult) -> Vec<HarnessEvent> + Send + Sync>;

pub struct Config<TContext> {
    pub tools: Vec<Arc<AgentHarnessTool<TContext>>>,
    pub resources: Resources,
    pub stream_options: AgentHarnessStreamOptions,
    pub retry_policy: RetryPolicy,
    pub compaction: CompactionSettings,
    pub steering_mode: QueueMode,
    pub follow_up_mode: QueueMode,
    pub tool_execution: crate::harness::session::types::ToolExecutionMode,
    pub tool_context: Option<TContext>,
    pub system_prompt: Option<SystemPromptFn>,
    pub to_provider_messages: ToProviderMessagesFn,
    pub entry_projectors: Vec<crate::harness::session::types::EntryProjector>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LaneState {
    pub tip_id: Option<String>,
    pub configuration: LaneConfiguration,
    pub inbox: Vec<InboxItem>,
    pub last_operation_id: Option<String>,
    pub operation: Option<Operation>,
}

pub struct CommitDecision<TResult> {
    pub writes: Vec<Write>,
    pub materialize: MaterializeFn<TResult>,
    pub events: Option<CommitEventsFn>,
}

pub enum LaneCommand<TResult> {
    Commit { decision: CommitDecision<TResult>, next: Box<LaneState> },
    Return { result: TResult },
    Reject { error: String },
}

impl<TResult> LaneCommand<TResult> {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Commit { .. } => "commit",
            Self::Return { .. } => "return",
            Self::Reject { .. } => "reject",
        }
    }
}

pub enum ContinueOperationResult<TResult> {
    CancelRequested,
    Result { value: TResult },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LanePatch {
    pub tip_id: Option<Option<String>>,
    pub configuration: Option<LaneConfiguration>,
    pub inbox: Option<Vec<InboxItem>>,
}

pub struct FinishDecision<TResult> {
    pub writes: Vec<Write>,
    pub record: OperationResultRecord,
    pub lane: Option<LanePatch>,
    pub materialize: MaterializeFn<TResult>,
    pub events: Option<CommitEventsFn>,
}

pub enum OperationCommand<TResult> {
    Commit { decision: CommitDecision<TResult>, operation_state: Box<OperationState>, lane: Option<LanePatch> },
    Finish { decision: Box<FinishDecision<TResult>> },
    Return { result: TResult },
}

impl<TResult> OperationCommand<TResult> {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Commit { .. } => "commit",
            Self::Finish { .. } => "finish",
            Self::Return { .. } => "return",
        }
    }
}

struct CompletionState {
    outcome: Option<Result<DriveOutcome, String>>,
}

pub struct Completion {
    state: Mutex<CompletionState>,
    notify: tokio::sync::Notify,
}

impl Completion {
    fn new() -> Self {
        Self { state: Mutex::new(CompletionState { outcome: None }), notify: tokio::sync::Notify::new() }
    }

    fn settle(&self, outcome: Result<DriveOutcome, String>) {
        {
            let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.outcome.is_none() {
                state.outcome = Some(outcome);
            }
        }
        self.notify.notify_waiters();
    }

    pub async fn wait(&self) -> Result<DriveOutcome, String> {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(outcome) = &state.outcome {
                    return outcome.clone();
                }
            }
            notified.await;
        }
    }
}

pub struct Drive {
    pub operation_id: String,
    pub completion: Arc<Completion>,
    pub gate: Gate,
    pub context: Context,
    pub wait_for_retry: bool,
    pub close_signal: AbortSignal,
    pub deferred_permits: Mutex<u32>,
    control: GateControl,
    close_controller: AbortController,
}

impl Drive {
    pub fn new(options: &DriveOptions, context: &Context) -> Self {
        let (gate, control) = create_gate();
        let close_controller = AbortController::new();
        let close_signal = close_controller.signal();
        Self {
            operation_id: options.operation_id.clone(),
            completion: Arc::new(Completion::new()),
            gate,
            context: without_abort_signal(context),
            wait_for_retry: options.wait_for_retry.unwrap_or(false),
            deferred_permits: Mutex::new(if options.poll_deferred == Some(true) { 1 } else { 0 }),
            control,
            close_controller,
            close_signal,
        }
    }

    pub fn settle(&self, outcome: DriveOutcome) {
        self.completion.settle(Ok(outcome));
    }

    pub fn fail(&self, error: String) {
        self.completion.settle(Err(error));
    }

    pub fn begin_abort(&self, cancellation: Cancellation) {
        self.control.begin_abort(cancellation);
    }

    pub fn signal_abort(&self) {
        self.control.signal_abort();
    }

    pub fn close_gate(&self, error: String) {
        self.control.close(error.clone());
        if !self.close_signal.aborted() {
            self.close_controller
                .abort(Some(maho_ai::utils::abort::AbortReason::new("Error", error.clone())));
        }
        self.completion.settle(Err(error));
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProcedureResult {
    Continue,
    Waiting { outcome: DriveOutcome },
    Settled { outcome: OperationResultRecord },
}

impl ProcedureResult {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Waiting { .. } => "waiting",
            Self::Settled { .. } => "settled",
        }
    }
}
